"""Type stubs for strider.reader: the code and read-only-memory readers."""

from __future__ import annotations

from typing import Optional, Protocol, Sequence, Union

__all__: list[str]

class BufferReader:
    """Raw byte reader for firmware or custom sources.

    Serves both roles: the `mem` (instruction fetch) and `rom` (read-only
    memory) arguments of `strider.lift.lifter`. Cheap to copy; copies share
    one backing state.
    """
    def __init__(self, base_addr: int, data: Union[bytes, bytearray, Sequence[int]]) -> None:
        """Serve `data` as the bytes mapped starting at `base_addr`. Raises
        `ValueError` for a region that runs past the end of the address
        space. A buffer (`memoryview`, `array`, numpy) serves its raw bytes
        and must have one-byte items, else `TypeError`."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` bytes at `addr` (fewer near a region edge, or before a
        relocation the ELF loader does not compute), or `None` when `addr`
        itself is unmapped or inside such a relocation's field. In writable,
        non-executable data such a field is not cut out: it serves the file's
        bytes."""
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
        is allowed), or `None` when `addr` is unmapped. A buffer must have
        one-byte items. The base implementation raises `NotImplementedError`.

        An exception here, a return of another type or of more than `size`
        bytes fails the lift with `StriderError`, an exception chained as its
        `__cause__`; `KeyboardInterrupt` and `SystemExit` propagate unchanged."""
        ...

class ReadOnlyMemory:
    """Read-only memory backed by Python, read by constant-load folding and
    jump-table probes.

    Subclass and override `read(addr, size)` to return the raw bytes at
    `addr`. Only RAM loads reach it.
    """

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Return exactly `size` raw bytes at `addr`, or `None` when unmapped.
        Bytes are not byte-swapped; the optimizer decodes them per the run's
        endianness. A short return is an error, not a partial read. The base
        implementation raises `NotImplementedError`.

        An exception here, or a return of another type or length, is
        swallowed as if `None` were returned, except `KeyboardInterrupt` and
        `SystemExit`, which stop the run."""
        ...

class Symbol:
    """One ELF symbol: where it is, what the ELF says it spans, and which
    loaded region it falls in."""

    @property
    def name(self) -> str:
        """The symbol name as spelled in the ELF symbol table."""
        ...
    @property
    def address(self) -> int:
        """The symbol's virtual address (`st_value`), except for a ppc64
        ELFv1 function, where it is the code the `.opd` descriptor at
        `st_value` names. An ARM Thumb function keeps the Thumb bit; see
        `is_thumb`. An undefined symbol of an object file has an address past
        the image that maps nothing."""
        ...
    @property
    def size(self) -> Optional[int]:
        """`None` for `st_size == 0` (no recorded extent), and for a ppc64
        ELFv1 function whose `st_size` measures its `.opd` descriptor rather
        than its code."""
        ...

    @property
    def is_function(self) -> bool:
        """Whether the ELF types this `STT_FUNC` (or `STT_GNU_IFUNC`), rather
        than inferring it from the section the symbol lives in."""
        ...
    @property
    def end(self) -> Optional[int]:
        """One past the last byte, or `None` when `size` is. Measured from the
        first instruction, so a Thumb function's ISA bit does not count."""
        ...
    @property
    def is_thumb(self) -> bool:
        """Whether this is an ARM Thumb function: `address` then carries the
        Thumb bit, on which `analyze` enters in Thumb mode, and the first
        instruction is at `address & ~1`, where `symbol_at` and `region`
        measure from."""
        ...
    @property
    def region(self) -> Optional[tuple[int, int]]:
        """The `(start, end)` bounds (end exclusive) of the loaded region this
        symbol maps into, such as the `.text` mapping, or `None` when its
        address falls in no mapped region."""
        ...
    @property
    def local_entry(self) -> int:
        """Where a call sharing the symbol's TOC enters: on ppc64 ELFv2,
        `address` plus the local-entry offset `st_other` encodes. `address`
        itself everywhere else."""
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
        """How many symbols are left, not the total."""
        ...


class _Readable(Protocol):
    """Any object with a `read(addr, size)` method."""

    def read(self, addr: int, size: int, /) -> Optional[bytes]: ...

#: Source of instruction bytes: a `BufferReader`, a `MemReader` subclass, or
#: any object with `read(addr, size)`.
MemLike = Union[MemReader, BufferReader, _Readable]

#: Read-only memory image for constant folding: a `BufferReader`, a
#: `ReadOnlyMemory` subclass, or any object with `read(addr, size)`.
RomLike = Union[ReadOnlyMemory, BufferReader, _Readable]
