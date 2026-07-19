"""Type stubs for strider.reader: the code and read-only-memory readers."""

from __future__ import annotations

from typing import Any, Optional

class BufferReader:
    """Single-region raw-byte reader for non-ELF and firmware-blob cases.

    Serves both the instruction-fetch (`mem=`) and read-only-memory (`rom=`)
    roles, so one `BufferReader` can be passed as either argument to
    `strider.lift.lifter` / `strider.sleigh.Sleigh`. For an ELF, prefer
    `strider.lift.load_elf(path)`, which wires a multi-region reader up
    automatically and adds symbol and entry-point lookups.
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
    analysis pipeline from your own data source. Each `read` crosses the
    language boundary, so prefer `BufferReader` for in-process bulk data.
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

    Only RAM loads reach `read`. Non-RAM spaces (REGISTER, CONST, UNIQUE) are
    short-circuited before Python, so subclasses need not filter on space.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Override this: return the `size` raw bytes at `addr`, or `None`
        when the range is unmapped."""
        ...
