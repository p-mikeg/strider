"""Type stubs for strider.reader: the code and read-only-memory readers."""

from __future__ import annotations

from typing import Any, Optional

class BufferReader:
    """Low-level raw-byte reader for firmware / custom byte sources.

    Serves both the instruction-fetch (`mem=`) and read-only-memory (`rom=`)
    roles, so one `BufferReader` can be passed as either argument to
    `strider.lift.lifter` / `strider.sleigh.Sleigh`.
    """
    def __init__(self, base_addr: int, data: bytes) -> None:
        """Serve `data` as the bytes mapped starting at `base_addr`."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """The `size` bytes at `addr`, or `None` when the range is unmapped."""
        ...

class MemReader:
    """Instruction-fetch source backed by Python.

    Subclass and override `read(addr, size) -> Optional[bytes]` to feed the
    analysis pipeline from your own data source. Each `read` is a Python call
    made once per memory access.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Override this: return the `size` bytes at `addr`, or `None` when
        the range is unmapped."""
        ...

class ReadOnlyMemory:
    """Read-only memory image backed by Python, for the `LoadReadOnly` pass.

    Subclass and override `read(addr, size) -> Optional[bytes]` to return the
    `size` RAW bytes at `addr` (no endianness swap; the optimizer decodes them
    per the target byte order), or `None` for unmapped addresses.

    Only RAM loads reach `read`; other spaces (REGISTER, CONST, UNIQUE) never
    do, so subclasses need not filter on space.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Override this: return the `size` raw bytes at `addr`, or `None`
        when the range is unmapped."""
        ...
