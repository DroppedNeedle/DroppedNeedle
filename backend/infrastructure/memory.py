from __future__ import annotations

import ctypes
import logging
import os

logger = logging.getLogger(__name__)

try:
    _PAGE_SIZE = os.sysconf("SC_PAGE_SIZE")
except (AttributeError, ValueError, OSError):
    _PAGE_SIZE = 4096


def get_rss_bytes() -> int | None:
    """Current process RSS in bytes from /proc/self/statm, or None off Linux."""
    try:
        with open("/proc/self/statm", encoding="ascii") as f:
            resident_pages = int(f.read().split()[1])
    except (OSError, ValueError, IndexError):
        return None
    return resident_pages * _PAGE_SIZE


def _init_libc() -> ctypes.CDLL | None:
    try:
        libc = ctypes.CDLL("libc.so.6", use_errno=True)
    except OSError:
        return None
    if not hasattr(libc, "malloc_trim"):
        return None
    libc.malloc_trim.argtypes = [ctypes.c_size_t]
    libc.malloc_trim.restype = ctypes.c_int
    return libc


_LIBC = _init_libc()


def trim_malloc() -> bool:
    """Return freed glibc heap memory to the OS; no-op without glibc malloc_trim."""
    if _LIBC is None:
        return False
    try:
        return bool(_LIBC.malloc_trim(0))
    except OSError:
        return False
