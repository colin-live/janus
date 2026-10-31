"""Platform selector for the shared-memory backend."""

import sys

if sys.platform == "win32":
    from .shm_win import Event, Segment, ShmError, ShmNotFound  # noqa: F401
    PLATFORM = "windows"
else:  # pragma: no cover
    from .shm_posix import Event, Segment, ShmError, ShmNotFound  # noqa: F401
    PLATFORM = "linux"
