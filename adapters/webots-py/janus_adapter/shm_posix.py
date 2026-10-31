"""Linux/POSIX backend placeholder.

Windows is the supported platform for the adapter in v1 (Webots + host + models run in one Windows session).
A POSIX backend (``shm_open`` + named semaphores via ctypes) is deliberately not implemented yet; the
interface it must provide is exactly the one in ``shm_win`` (``Segment.create/open/close``, ``read_bytes``,
``write_bytes``, ``zero``, ``read_json_area``, ``write_json_area``, ``Event.create/open/signal/wait/close``).
"""

_MSG = "POSIX shared-memory backend is not implemented (Windows only in v1)"


class ShmNotFound(OSError):
    pass


class ShmError(OSError):
    pass


class Segment:
    @classmethod
    def create(cls, name, size):
        raise NotImplementedError(_MSG)

    @classmethod
    def open(cls, name):
        raise NotImplementedError(_MSG)


class Event:
    @classmethod
    def create(cls, name):
        raise NotImplementedError(_MSG)

    @classmethod
    def open(cls, name):
        raise NotImplementedError(_MSG)
