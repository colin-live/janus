//! Owned mapping of one segment, plus the byte/JSON accessors of the protocol.
//!
//! This layer knows the *physical* layout (`Layout`, header offsets) but does not
//! parse JSON and implements no handshake state machine (B2 doc, requirements 7).
//!
//! # Bounds policy
//!
//! All accessors take absolute byte offsets from the segment start. Layout-derived
//! offsets are always in range, so out-of-range offsets are programming errors:
//!
//! * [`Segment::read_f64`] / [`Segment::write_f64`] panic (their frozen signature has
//!   no error channel).
//! * [`Segment::read_json`] / [`Segment::write_json`] and the [`Segment::try_*`] byte
//!   accessors return [`ShmError::TooSmall`].
//! * The frozen [`Segment::read_bytes`] / [`Segment::write_bytes`] return `()`, so they
//!   cannot report an error: a read out of range zero-fills the destination, a write out
//!   of range is a no-op. Use the `try_*` variants when the error matters.

use std::ptr;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use coupling_core::header::{SegmentHeader, HEADER_SIZE, MAGIC, PROTOCOL_MAJOR, PROTOCOL_MINOR};
use coupling_core::layout::Layout;
use coupling_core::{HandshakeState, SyncMode, Validity};

use crate::platform::Mapping;
use crate::ShmError;

/// Which UTF-8 JSON area of the segment (spec §3) an accessor refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonArea {
    /// Written by the host, read by the client.
    Manifest,
    /// Written by the client, read by the host.
    Hello,
    /// Written by the adapter.
    Report,
}

/// A mapped segment. Two handles (created and/or opened) refer to the same bytes.
pub struct Segment {
    map: Mapping,
}

// SAFETY: shared memory is explicitly designed to be reached from several threads;
// mutating access goes through atomics / the protocol's ownership rules.
unsafe impl Send for Segment {}
// SAFETY: see above.
unsafe impl Sync for Segment {}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Segment {
    /// Create a new named segment of `size` bytes (host side).
    ///
    /// Fails with [`ShmError::AlreadyExists`] if the name is already taken.
    pub fn create(name: &str, size: usize) -> Result<Segment, ShmError> {
        Ok(Segment {
            map: Mapping::create(name, size)?,
        })
    }

    /// Open an existing named segment (client / adapter side).
    ///
    /// Fails with [`ShmError::NotFound`] if no such segment exists.
    pub fn open(name: &str) -> Result<Segment, ShmError> {
        Ok(Segment {
            map: Mapping::open(name)?,
        })
    }

    /// Usable size of the mapping in bytes (logical size written into the header,
    /// or the OS capacity when the header is not initialised yet).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// `true` if the mapping has no bytes (never the case for a valid segment).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// View of the fixed 256-byte header.
    ///
    /// Dynamic fields are atomics and may be `load`ed/`store`d directly. The
    /// header is *not* validated here; call [`SegmentHeader::validate`] for that.
    pub fn header(&self) -> &SegmentHeader {
        assert!(
            self.len() >= HEADER_SIZE,
            "segment of {} bytes has no room for the {HEADER_SIZE}-byte header",
            self.len()
        );
        debug_assert_eq!(
            self.map.ptr() as usize % 8,
            0,
            "mapping must be 8-byte aligned"
        );
        // SAFETY: we checked that the mapping covers the whole header, the base pointer
        // is page aligned (hence 8-byte aligned) and the mapping outlives `&self`.
        unsafe { &*(self.map.ptr() as *const SegmentHeader) }
    }

    /// Zero the whole segment and fill the static header fields from `l`, then
    /// publish `hs_state = Created` (release).
    ///
    /// Must only be called by the creating process, before any peer uses the
    /// segment (spec §6 step 1); the static header fields are not atomic.
    pub fn init_from_layout(
        &self,
        l: &Layout,
        sim_id: u64,
        instance_id: u32,
        model_timeout_ms: u32,
    ) -> Result<(), ShmError> {
        let len = self.len() as u64;
        if len < HEADER_SIZE as u64 {
            return Err(ShmError::too_small("header", HEADER_SIZE as u64, len));
        }
        let regions = [
            ("manifest", l.manifest_off, l.manifest_len),
            ("hello", l.hello_off, l.hello_len),
            ("report", l.report_off, l.report_len),
            ("state", l.state_off, l.state_len),
            ("effort", l.effort_off, l.effort_len),
        ];
        for (what, off, size) in regions {
            let end = off.saturating_add(size);
            if end > len {
                return Err(ShmError::too_small(what, end, len));
            }
        }
        if l.total_size > len {
            return Err(ShmError::too_small("layout", l.total_size, len));
        }

        let h = self.map.ptr() as *mut SegmentHeader;
        // SAFETY: the mapping covers at least the header (checked above), is 8-byte
        // aligned and owned by us; the contract requires this to run before peers
        // observe the segment. `magic` is written last so an opener that maps the
        // segment concurrently never trusts a half-written header.
        unsafe {
            ptr::write_bytes(self.map.ptr(), 0, self.len());
            (*h).protocol_major = PROTOCOL_MAJOR;
            (*h).protocol_minor = PROTOCOL_MINOR;
            (*h).header_size = HEADER_SIZE as u32;
            (*h).total_size = l.total_size;
            (*h).simulation_id = sim_id;
            (*h).instance_id = instance_id;
            (*h).flags = 0;
            (*h).manifest_off = l.manifest_off;
            (*h).manifest_len = l.manifest_len;
            (*h).hello_off = l.hello_off;
            (*h).hello_len = l.hello_len;
            (*h).report_off = l.report_off;
            (*h).report_len = l.report_len;
            (*h).state_off = l.state_off;
            (*h).state_len = l.state_len;
            (*h).effort_off = l.effort_off;
            (*h).effort_len = l.effort_len;
            (*h).state_slot_count = l.state_slot_count;
            (*h).effort_slot_count = l.effort_slot_count;
            (*h).model_timeout_ms = model_timeout_ms;
            (*h).sync_mode
                .store(SyncMode::Lockstep.as_raw(), Ordering::Relaxed);
            (*h).magic = MAGIC;
            (*h).hs_state
                .store(HandshakeState::Created.as_raw(), Ordering::Release);
        }
        Ok(())
    }

    /// Read one validity byte. Unknown values (and out-of-range offsets) map to
    /// [`Validity::Error`] as required by the frozen interface.
    pub fn read_validity(&self, off: u64) -> Validity {
        match self.ptr_at(off, 1) {
            // SAFETY: at least one byte at an in-range offset; `AtomicU8` accepts any
            // alignment and the mapping is process-shared.
            Some(p) => {
                Validity::from_raw(unsafe { (*(p as *const AtomicU8)).load(Ordering::Acquire) })
                    .unwrap_or(Validity::Error)
            }
            None => Validity::Error,
        }
    }

    /// Write one validity byte (release). Out-of-range writes are ignored; see the
    /// module docs.
    pub fn write_validity(&self, off: u64, v: Validity) {
        if let Some(p) = self.ptr_at(off, 1) {
            // SAFETY: same as `read_validity`.
            unsafe { (*(p as *const AtomicU8)).store(v.as_raw(), Ordering::Release) };
        }
    }

    /// Read `out.len()` bytes at `off`.
    ///
    /// Out of range: `out` is zero-filled (the frozen signature cannot report an
    /// error). Prefer [`Segment::try_read_bytes`].
    pub fn read_bytes(&self, off: u64, out: &mut [u8]) {
        match self.ptr_at(off, out.len()) {
            // SAFETY: the checked range is inside the mapping and does not overlap
            // `out` (shared memory vs. caller-owned buffer).
            Some(p) => unsafe { ptr::copy_nonoverlapping(p, out.as_mut_ptr(), out.len()) },
            None => out.fill(0),
        }
    }

    /// Write `data` at `off`. Out-of-range writes are ignored.
    ///
    /// Prefer [`Segment::try_write_bytes`].
    pub fn write_bytes(&self, off: u64, data: &[u8]) {
        if let Some(p) = self.ptr_at(off, data.len()) {
            // SAFETY: checked range inside the mapping; `data` is a separate allocation.
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
        }
    }

    /// Fallible [`Segment::read_bytes`].
    pub fn try_read_bytes(&self, off: u64, out: &mut [u8]) -> Result<(), ShmError> {
        let p = self.ptr_at(off, out.len()).ok_or_else(|| {
            ShmError::too_small(
                "read_bytes",
                off.saturating_add(out.len() as u64),
                self.len() as u64,
            )
        })?;
        // SAFETY: checked range inside the mapping; `out` is a separate allocation.
        unsafe { ptr::copy_nonoverlapping(p, out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    /// Fallible [`Segment::write_bytes`].
    pub fn try_write_bytes(&self, off: u64, data: &[u8]) -> Result<(), ShmError> {
        let p = self.ptr_at(off, data.len()).ok_or_else(|| {
            ShmError::too_small(
                "write_bytes",
                off.saturating_add(data.len() as u64),
                self.len() as u64,
            )
        })?;
        // SAFETY: checked range inside the mapping; `data` is a separate allocation.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
        Ok(())
    }

    /// Read an `f64` slot value (bit-preserving, non-atomic fallback if misaligned).
    ///
    /// Panics if `off..off+8` is out of range; slot offsets from [`Layout`] are
    /// always valid.
    pub fn read_f64(&self, off: u64) -> f64 {
        let p = self.ptr_at(off, 8).unwrap_or_else(|| {
            panic!(
                "read_f64: offset {off} out of range (segment is {} bytes)",
                self.len()
            )
        });
        let bits = if (p as usize).is_multiple_of(8) {
            // SAFETY: in-range and 8-byte aligned, so the `AtomicU64` is well formed.
            unsafe { (*(p as *const AtomicU64)).load(Ordering::Relaxed) }
        } else {
            // SAFETY: in-range; `read_unaligned` tolerates any alignment.
            unsafe { ptr::read_unaligned(p as *const u64) }
        };
        f64::from_bits(bits)
    }

    /// Write an `f64` slot value. Panics if `off..off+8` is out of range.
    pub fn write_f64(&self, off: u64, v: f64) {
        let p = self.ptr_at(off, 8).unwrap_or_else(|| {
            panic!(
                "write_f64: offset {off} out of range (segment is {} bytes)",
                self.len()
            )
        });
        if (p as usize).is_multiple_of(8) {
            // SAFETY: in-range and 8-byte aligned, so the `AtomicU64` is well formed.
            unsafe { (*(p as *const AtomicU64)).store(v.to_bits(), Ordering::Relaxed) };
        } else {
            // SAFETY: in-range; `write_unaligned` tolerates any alignment.
            unsafe { ptr::write_unaligned(p as *mut u64, v.to_bits()) };
        }
    }

    /// Read a JSON area; `Ok(vec![])` for an empty (zeroed) area.
    pub fn read_json(&self, area: JsonArea) -> Result<Vec<u8>, ShmError> {
        let (off, cap) = self.json_bounds(area);
        if cap == 0 {
            return Ok(Vec::new());
        }
        if cap < 4 {
            return Err(ShmError::too_small("json area", 4, cap));
        }
        let mut prefix = [0u8; 4];
        self.try_read_bytes(off, &mut prefix)?;
        let n = u32::from_le_bytes(prefix) as u64;
        if n == 0 {
            return Ok(Vec::new());
        }
        if n + 4 > cap {
            return Err(ShmError::too_small("json length prefix", n + 4, cap));
        }
        let mut out = vec![0u8; n as usize];
        self.try_read_bytes(off + 4, &mut out)?;
        Ok(out)
    }

    /// Write a JSON area as `u32` little-endian length prefix + UTF-8 bytes.
    ///
    /// Fails with [`ShmError::TooSmall`] if `json` does not fit into the area
    /// (capacity minus the 4-byte prefix). The payload is written before the
    /// length prefix, so a concurrent reader can only see a consistent area or
    /// the previous one.
    pub fn write_json(&self, area: JsonArea, json: &[u8]) -> Result<(), ShmError> {
        let (off, cap) = self.json_bounds(area);
        if json.len() > u32::MAX as usize {
            return Err(ShmError::too_small("json", 4 + json.len() as u64, cap));
        }
        let needed = json.len() as u64 + 4;
        if needed > cap {
            return Err(ShmError::too_small("json area", needed, cap));
        }
        self.try_write_bytes(off + 4, json)?;
        self.try_write_bytes(off, &(json.len() as u32).to_le_bytes())
    }

    fn json_bounds(&self, area: JsonArea) -> (u64, u64) {
        let h = self.header();
        match area {
            JsonArea::Manifest => (h.manifest_off, h.manifest_len),
            JsonArea::Hello => (h.hello_off, h.hello_len),
            JsonArea::Report => (h.report_off, h.report_len),
        }
    }

    /// Checked pointer to `off..off+n`, or `None` when the range leaves the mapping.
    fn ptr_at(&self, off: u64, n: usize) -> Option<*mut u8> {
        let end = off.checked_add(n as u64)?;
        if end > self.len() as u64 {
            return None;
        }
        // SAFETY: `off < len` was checked and the mapping has `len` bytes.
        Some(unsafe { self.map.ptr().add(off as usize) })
    }
}
