"""Type stubs for strider.reader: the code and read-only-memory readers."""

from __future__ import annotations

from typing import Optional

class BufferReader:
    """Raw byte reader for firmware or custom sources.

    Works as both the `mem` (instruction fetch) and `rom` (read-only memory)
    argument to `strider.lift.lifter` and `strider.sleigh.Sleigh`.
    """
    def __init__(self, base_addr: int, data: bytes) -> None:
        """Serve `data` as the bytes mapped starting at `base_addr`."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` bytes at `addr` (fewer near a region edge), or `None`
        when `addr` itself is unmapped."""
        ...

class MemReader:
    """Instruction source backed by Python.

    Subclass and override `read(addr, size)` to feed the pipeline from your
    own data source.
    """

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Return up to `size` bytes at `addr` (a short read near a region edge
        is allowed), or `None` when `addr` is unmapped."""
        ...

class ReadOnlyMemory:
    """Read-only memory backed by Python, used by the `LoadReadOnly` pass.

    Subclass and override `read(addr, size)` to return the raw bytes at
    `addr`. Only RAM loads reach it, so subclasses need not filter on space.
    """

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Return the `size` raw bytes at `addr`, or `None` when unmapped."""
        ...
