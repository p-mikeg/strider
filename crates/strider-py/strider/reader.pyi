"""Type stubs for strider.reader — the code / read-only-memory readers."""

from __future__ import annotations

from typing import Any, Optional

class BufferReader:
    """Single-region raw-byte reader for non-ELF / firmware-blob cases.
    Serves both the sleigh-fetch (`mem=`) and ReadOnlyMemory (`rom=`)
    roles, so one `BufferReader` can be passed as either argument to
    `strider.lift.lifter` / `strider.sleigh.Sleigh`.

    For an ELF, prefer `strider.lift.load_elf(path)` -> `ElfLifter`, which
    wires a multi-region reader up automatically and adds symbol/
    entry-point lookups.
    """
    def __init__(self, base_addr: int, data: bytes) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class MemReader:
    """Subclass and override `read(addr, size) -> Optional[bytes]` to
    feed the analysis pipeline from a Python data source.  Each `read`
    crosses the Rust↔Python boundary; prefer `BufferReader` for
    in-process bulk data.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class ReadOnlyMemory:
    """Subclass and override `read(addr, size) -> Optional[bytes]` to
    back a `LoadReadOnly` opt pass with Python data.  Returns the `size`
    RAW bytes at `addr` (NO endianness swap — the optimizer decodes them
    per the run's target byte order), or `None` for unmapped addresses.

    The pass only invokes `read` for RAM loads — non-RAM spaces
    (REGISTER, CONST, UNIQUE) are short-circuited by the adapter
    before reaching Python, so subclasses don't need to filter on
    space.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...
