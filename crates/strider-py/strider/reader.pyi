"""Code and read-only-memory readers."""

from __future__ import annotations

from typing import Optional, Protocol, Sequence, Union

__all__: list[str]

class BufferReader:
    """Raw byte reader, usable as both `mem` and `rom`."""
    def __init__(self, base_addr: int, data: Union[bytes, bytearray, Sequence[int]]) -> None:
        """Map `data` at `base_addr`. Raises `ValueError` if the region runs
        past the address space, `TypeError` for a buffer whose items are not
        one byte."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` bytes at `addr` (fewer at a region edge), or `None`
        when unmapped. An `ElfLifter` reader also stops before, and gives
        `None` inside, an uncomputed relocation field (writable
        non-executable data serves the file bytes)."""
        ...

class MemReader:
    """Instruction source backed by Python: subclass and override `read`.
    Each subclass instance keeps a reference to its class (PyO3 0.22): define
    a subclass once, not per call."""

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Override to return up to `size` bytes at `addr`, or `None` for
        unmapped. Raising, another type, or more than `size` bytes fails the
        lift with `StriderError` (the exception as `__cause__`);
        `KeyboardInterrupt` and `SystemExit` propagate."""
        ...

class ReadOnlyMemory:
    """Read-only memory backed by Python: subclass and override `read`. Only
    RAM loads reach it. Each subclass instance keeps a reference to its class
    (PyO3 0.22): define a subclass once, not per call."""

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Override to return the `size` raw (not byte-swapped) bytes at
        `addr`, or `None` for unmapped. An exception, or another type or
        length, reads as `None`; `KeyboardInterrupt` and `SystemExit` stop
        the run."""
        ...

class Symbol:
    """One ELF symbol."""

    @property
    def name(self) -> str:
        """The name as spelled in the symbol table."""
        ...
    @property
    def address(self) -> int:
        """`st_value`; for a ppc64 ELFv1 function, the code its `.opd`
        descriptor names. A Thumb function keeps the Thumb bit, and a MIPS16
        one (`STO_MIPS16`) gets the ISA bit. An object file's undefined,
        common or defined IFUNC symbol has an unmapped address past the
        image."""
        ...
    @property
    def size(self) -> Optional[int]:
        """`st_size`; `None` when 0, for an object file's defined IFUNC, or
        when a ppc64 ELFv1 function's `st_size` measures its `.opd`
        descriptor."""
        ...

    @property
    def is_function(self) -> bool:
        """Whether the ELF types this `STT_FUNC` or `STT_GNU_IFUNC`, but
        `False` for an object file's defined IFUNC, which has no code at its
        extern address."""
        ...
    @property
    def end(self) -> Optional[int]:
        """One past the last byte, or `None` when `size` is. Ignores the ISA
        bit."""
        ...
    @property
    def is_thumb(self) -> bool:
        """An ARM Thumb function: `address` carries the Thumb bit, so
        `analyze` enters in Thumb mode at `address & ~1`."""
        ...
    @property
    def region(self) -> Optional[tuple[int, int]]:
        """`(start, end)` of the loaded region holding the symbol, or
        `None`."""
        ...
    @property
    def local_entry(self) -> int:
        """`address` plus the ppc64 ELFv2 local-entry offset, else
        `address`."""
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other) -> bool:
        """Equal when every field is."""
        ...
    def __hash__(self) -> int: ...

class SymbolIter:
    """Iterator over `Symbol`s."""

    def __iter__(self) -> "SymbolIter": ...
    def __next__(self) -> Symbol: ...
    def __len__(self) -> int:
        """Symbols left."""
        ...


class _Readable(Protocol):
    """Any object with a `read(addr, size)` method."""

    def read(self, addr: int, size: int, /) -> Optional[bytes]: ...

#: Instruction bytes: a `BufferReader`, `MemReader` or any `read(addr, size)`.
MemLike = Union[MemReader, BufferReader, _Readable]

#: Read-only memory: a `BufferReader`, `ReadOnlyMemory` or any
#: `read(addr, size)`.
RomLike = Union[ReadOnlyMemory, BufferReader, _Readable]
