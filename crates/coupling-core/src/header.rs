//! Fixed 256-byte segment header (spec §3). Little-endian targets only (x86-64 / aarch64).
//! Dynamic fields are atomics so the shm layer can cast the mapping to `&SegmentHeader`.

use crate::error::CoreError;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64};

pub const MAGIC: [u8; 8] = *b"WCOUPLE1";
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;
pub const HEADER_SIZE: usize = 256;

#[repr(C)]
pub struct SegmentHeader {
    // ---- static after Created (host-owned) ----
    pub magic: [u8; 8],              // 0
    pub protocol_major: u16,         // 8
    pub protocol_minor: u16,         // 10
    pub header_size: u32,            // 12
    pub total_size: u64,             // 16
    pub simulation_id: u64,          // 24
    pub instance_id: u32,            // 32
    pub flags: u32,                  // 36
    // ---- lifecycle ----
    pub hs_state: AtomicU32,         // 40  HandshakeState
    pub sync_mode: AtomicU32,        // 44  SyncMode (final value set by host at Ready)
    // ---- step protocol ----
    pub epoch: AtomicU64,            // 48  host/adapter-owned, bumped on reset
    pub model_epoch_ack: AtomicU64,  // 56  model-owned
    pub step_id: AtomicU64,          // 64  adapter-owned: written LAST (release) after state slots
    pub effort_step_id: AtomicU64,   // 72  model-owned: written LAST (release) after effort slots
    pub sim_time_ns: AtomicI64,      // 80  sim time at the state sample for step_id
    pub dt_ns: AtomicI64,            // 88  step length the model must advance
    pub adapter_heartbeat: AtomicU64, // 96
    pub model_heartbeat: AtomicU64,  // 104
    // ---- regions (offsets from segment start) ----
    pub manifest_off: u64,           // 112  JSON, host-owned
    pub manifest_len: u64,           // 120  capacity of the area
    pub hello_off: u64,              // 128  JSON, client-owned
    pub hello_len: u64,              // 136
    pub state_off: u64,              // 144  ToModel region
    pub state_len: u64,              // 152
    pub effort_off: u64,             // 160  ToWebots region
    pub effort_len: u64,             // 168
    pub state_slot_count: u32,       // 176
    pub effort_slot_count: u32,      // 180
    pub model_timeout_ms: u32,       // 184
    pub _reserved0: u32,             // 188
    pub report_off: u64,             // 192  JSON, adapter-owned (binding validation report)
    pub report_len: u64,             // 200
    pub _reserved: [u8; 48],         // 208
}

const _: () = assert!(std::mem::size_of::<SegmentHeader>() == HEADER_SIZE);
const _: () = assert!(std::mem::align_of::<SegmentHeader>() == 8);

impl SegmentHeader {
    /// Check magic, version and size of a header read from shared memory.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.magic != MAGIC {
            return Err(CoreError::BadMagic);
        }
        if self.protocol_major != PROTOCOL_MAJOR {
            return Err(CoreError::UnsupportedVersion {
                major: self.protocol_major,
                minor: self.protocol_minor,
            });
        }
        if self.header_size as usize != HEADER_SIZE {
            return Err(CoreError::BadHeader("header_size"));
        }
        Ok(())
    }
}

/// Byte offsets for non-Rust implementations (Python adapter, C clients). A unit test checks
/// them against the struct so docs, Python and Rust cannot drift apart.
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const PROTOCOL_MAJOR: usize = 8;
    pub const HEADER_SIZE_FIELD: usize = 12;
    pub const TOTAL_SIZE: usize = 16;
    pub const SIMULATION_ID: usize = 24;
    pub const INSTANCE_ID: usize = 32;
    pub const HS_STATE: usize = 40;
    pub const SYNC_MODE: usize = 44;
    pub const EPOCH: usize = 48;
    pub const MODEL_EPOCH_ACK: usize = 56;
    pub const STEP_ID: usize = 64;
    pub const EFFORT_STEP_ID: usize = 72;
    pub const SIM_TIME_NS: usize = 80;
    pub const DT_NS: usize = 88;
    pub const ADAPTER_HEARTBEAT: usize = 96;
    pub const MODEL_HEARTBEAT: usize = 104;
    pub const MANIFEST_OFF: usize = 112;
    pub const STATE_OFF: usize = 144;
    pub const EFFORT_OFF: usize = 160;
    pub const STATE_SLOT_COUNT: usize = 176;
    pub const EFFORT_SLOT_COUNT: usize = 180;
    pub const MODEL_TIMEOUT_MS: usize = 184;
    pub const REPORT_OFF: usize = 192;
}
