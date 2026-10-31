"""Windows backend: named file mapping + named auto-reset events via ctypes/kernel32.

Python's ``mmap(tagname=...)`` silently *creates* a mapping when the name does not exist, which would hide a
"host not started" error, so the mapping is opened with ``OpenFileMappingW`` directly.
"""

from __future__ import annotations

import ctypes
import sys
from ctypes import wintypes

from . import protocol as P

if sys.platform != "win32":  # pragma: no cover
    raise ImportError("shm_win is Windows-only")

_k32 = ctypes.WinDLL("kernel32", use_last_error=True)

FILE_MAP_ALL_ACCESS = 0xF001F
PAGE_READWRITE = 0x04
EVENT_MODIFY_STATE = 0x0002
SYNCHRONIZE = 0x00100000
EVENT_ALL_ACCESS = 0x1F0003
INVALID_HANDLE_VALUE = wintypes.HANDLE(-1)
WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 0x102
ERROR_FILE_NOT_FOUND = 2
ERROR_ALREADY_EXISTS = 183

_k32.OpenFileMappingW.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR]
_k32.OpenFileMappingW.restype = wintypes.HANDLE
_k32.CreateFileMappingW.argtypes = [
    wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.LPCWSTR,
]
_k32.CreateFileMappingW.restype = wintypes.HANDLE
_k32.MapViewOfFile.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.c_size_t]
_k32.MapViewOfFile.restype = ctypes.c_void_p
_k32.UnmapViewOfFile.argtypes = [ctypes.c_void_p]
_k32.UnmapViewOfFile.restype = wintypes.BOOL
_k32.CloseHandle.argtypes = [wintypes.HANDLE]
_k32.CloseHandle.restype = wintypes.BOOL
_k32.OpenEventW.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR]
_k32.OpenEventW.restype = wintypes.HANDLE
_k32.CreateEventW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.BOOL, wintypes.LPCWSTR]
_k32.CreateEventW.restype = wintypes.HANDLE
_k32.SetEvent.argtypes = [wintypes.HANDLE]
_k32.SetEvent.restype = wintypes.BOOL
_k32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
_k32.WaitForSingleObject.restype = wintypes.DWORD


class ShmNotFound(OSError):
    pass


class ShmError(OSError):
    pass


class Segment:
    """A mapped shared-memory segment. ``buf`` is a writable ctypes byte array covering the whole mapping."""

    def __init__(self, handle, base: int, size: int, owner: bool):
        self._handle = handle
        self.base = base
        self.size = size
        self.buf = (ctypes.c_ubyte * size).from_address(base)
        self._owner = owner
        self.header = P.Header(self.buf)

    # -- construction --
    @classmethod
    def create(cls, name: str, size: int) -> "Segment":
        hi, lo = (size >> 32) & 0xFFFFFFFF, size & 0xFFFFFFFF
        h = _k32.CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, hi, lo, name)
        if not h:
            raise ShmError("CreateFileMappingW failed: %d" % ctypes.get_last_error())
        if ctypes.get_last_error() == ERROR_ALREADY_EXISTS:
            _k32.CloseHandle(h)
            raise ShmError("segment %r already exists" % name)
        base = _k32.MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, 0)
        if not base:
            err = ctypes.get_last_error()
            _k32.CloseHandle(h)
            raise ShmError("MapViewOfFile failed: %d" % err)
        return cls(h, base, size, True)

    @classmethod
    def open(cls, name: str) -> "Segment":
        h = _k32.OpenFileMappingW(FILE_MAP_ALL_ACCESS, False, name)
        if not h:
            err = ctypes.get_last_error()
            if err == ERROR_FILE_NOT_FOUND:
                raise ShmNotFound(name)
            raise ShmError("OpenFileMappingW failed: %d" % err)
        # Map the whole object (length 0), then read the real size from the header.
        base = _k32.MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, 0)
        if not base:
            err = ctypes.get_last_error()
            _k32.CloseHandle(h)
            raise ShmError("MapViewOfFile failed: %d" % err)
        probe = (ctypes.c_ubyte * P.HEADER_SIZE).from_address(base)
        if bytes(probe[0:8]) != P.MAGIC:
            _k32.UnmapViewOfFile(base)
            _k32.CloseHandle(h)
            raise P.ProtocolError("bad segment magic (segment not initialised yet?)")
        total = P.U64.unpack_from(probe, P.OFF["total_size"])[0]
        return cls(h, base, total, False)

    def close(self) -> None:
        if self.base:
            self.buf = None
            self.header = None
            _k32.UnmapViewOfFile(self.base)
            _k32.CloseHandle(self._handle)
            self.base = 0

    # -- byte helpers --
    def read_bytes(self, off: int, n: int) -> bytes:
        return ctypes.string_at(self.base + off, n)

    def write_bytes(self, off: int, data: bytes) -> None:
        ctypes.memmove(self.base + off, data, len(data))

    def zero(self, off: int, n: int) -> None:
        ctypes.memset(self.base + off, 0, n)

    # -- JSON areas: u32 little-endian length prefix + UTF-8 JSON --
    def read_json_area(self, off: int, cap: int) -> bytes:
        n = P.U32.unpack_from(self.buf, off)[0]
        if n > cap - 4:
            raise P.ProtocolError("JSON area length %d exceeds capacity %d" % (n, cap))
        return self.read_bytes(off + 4, n)

    def write_json_area(self, off: int, cap: int, data: bytes) -> None:
        if len(data) > cap - 4:
            raise ShmError("JSON too large: %d > %d" % (len(data), cap - 4))
        self.write_bytes(off + 4, data)
        P.U32.pack_into(self.buf, off, len(data))  # length last


class Event:
    """Named auto-reset event."""

    def __init__(self, handle):
        self._h = handle

    @classmethod
    def create(cls, name: str) -> "Event":
        h = _k32.CreateEventW(None, False, False, name)
        if not h:
            raise ShmError("CreateEventW failed: %d" % ctypes.get_last_error())
        return cls(h)

    @classmethod
    def open(cls, name: str) -> "Event":
        h = _k32.OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, False, name)
        if not h:
            err = ctypes.get_last_error()
            if err == ERROR_FILE_NOT_FOUND:
                raise ShmNotFound(name)
            raise ShmError("OpenEventW failed: %d" % err)
        return cls(h)

    def signal(self) -> None:
        _k32.SetEvent(self._h)

    def wait(self, timeout_ms: int) -> bool:
        """True if signalled, False on timeout."""
        r = _k32.WaitForSingleObject(self._h, max(0, int(timeout_ms)))
        if r == WAIT_OBJECT_0:
            return True
        if r == WAIT_TIMEOUT:
            return False
        raise ShmError("WaitForSingleObject returned %d" % r)

    def close(self) -> None:
        if self._h:
            _k32.CloseHandle(self._h)
            self._h = None
