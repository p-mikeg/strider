"""Type stubs for strider.reader: the code and read-only-memory readers."""

from __future__ import annotations

from typing import Optional, Union

__all__: list[str]

class BufferReader:
    """Raw byte reader for firmware or custom sources.

    Serves both roles: the `mem` (instruction fetch) and `rom` (read-only
    memory) arguments of `strider.lift.lifter`, and the `mem` argument of
    `strider.sleigh.Sleigh`. Cheap to copy; copies share one backing state.
    """
    def __init__(self, base_addr: int, data: bytes) -> None:
        """Serve `data` as the bytes mapped starting at `base_addr`. Raises
        `StriderError` for an invalid region."""
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
        is allowed), or `None` when `addr` is unmapped. Returning more than
        `size` bytes is an error. The base implementation raises
        `NotImplementedError`.

        An exception here fails the lift with `StriderError`, chained as its
        `__cause__`. `ReadOnlyMemory.read` is the opposite: an exception there
        is swallowed and the analysis succeeds. `KeyboardInterrupt` and
        `SystemExit` stop the run in both."""
        ...

class ReadOnlyMemory:
    """Read-only memory backed by Python, read by the constant-load fold
    (`LoadReadOnly`) and by the indirect-branch classifier's jump-table probes.

    Subclass and override `read(addr, size)` to return the raw bytes at
    `addr`. Only RAM loads reach it, so subclasses need not filter on space.
    """

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Return exactly `size` raw bytes at `addr`, or `None` when unmapped.
        Bytes are not byte-swapped; the optimizer decodes them per the run's
        endianness. A short return is an error, not a partial read. The base
        implementation raises `NotImplementedError`.

        An exception here is swallowed: the constant-load fold declines,
        indistinguishable from `None`, and the analysis succeeds. `MemReader.read`
        is the opposite: an exception there fails the lift with `StriderError`.
        `KeyboardInterrupt` and `SystemExit` stop the run in both."""
        ...

class Symbol:
    """One ELF symbol: where it is, what the ELF says it spans, and which
    loaded region it falls in."""

    #: The symbol name as spelled in the ELF symbol table.
    name: str
    #: The symbol's virtual address (`st_value`).
    address: int
    #: `None` for `st_size == 0`, which records no extent rather than an empty
    #: one: a hand-written `.S` entry point with no `.size` directive is still
    #: a whole function.
    size: Optional[int]

    @property
    def is_function(self) -> bool:
        """Whether the ELF types this `STT_FUNC` (or `STT_GNU_IFUNC`), rather
        than inferring it from the section the symbol lives in."""
        ...
    @property
    def end(self) -> Optional[int]:
        """One past the last byte, or `None` when `size` is."""
        ...
    @property
    def region(self) -> Optional[tuple[int, int]]:
        """The `(start, end)` bounds (end exclusive) of the loaded region this
        symbol maps into, such as the `.text` mapping, or `None` when its
        address falls in no mapped region."""
        ...
    def __repr__(self) -> str: ...

class SymbolIter:
    """Iterator over `Symbol`s."""

    def __iter__(self) -> "SymbolIter": ...
    def __next__(self) -> Symbol: ...
    def __len__(self) -> int:
        """How many symbols are LEFT, not the total: CPython reads this as a
        length hint, which a partly consumed iterator must not overstate."""
        ...


#: Source of instruction bytes: a `BufferReader`, or a `MemReader` subclass.
MemLike = Union[MemReader, BufferReader]

#: Read-only memory image for constant folding: a `ReadOnlyMemory` subclass
#: or a `BufferReader`.
RomLike = Union[ReadOnlyMemory, BufferReader]
