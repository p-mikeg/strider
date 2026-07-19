"""Type stubs for strider.lift: the lift, optimise and resolve handle
(`Lifter` / `ElfLifter`), its options, and the loaders."""

from __future__ import annotations

from typing import Iterable, Iterator, List, Literal, Optional, Union

import os

from .cfg import Cfg, CfgOptions, DotStyle
from .ir import Function, Node
from .opt import OptimizerPipeline
from .reader import BufferReader, MemReader, ReadOnlyMemory
from .sleigh import CallingConvention, SleighArch, Vn

#: What the loaders accept as a filesystem path.
StrPath = Union[str, "os.PathLike[str]"]

#: Source of instruction bytes: a `BufferReader`, or a `MemReader` subclass.
MemLike = Union[MemReader, BufferReader]

#: Read-only memory image for constant folding: a `ReadOnlyMemory` subclass
#: or a `BufferReader`.
RomLike = Union[ReadOnlyMemory, BufferReader]

#: Memory-aliasing model for the optimizer. Validated by the `LifterOptions`
#: constructor, which raises `ValueError` for anything else.
AliasMode = Literal["stack_global_disjoint", "strict"]

class AnalyzeResult:
    """What `Lifter.analyze` returns: the CFG, the lifted function, and any
    unresolved indirect branches. Unpacks as `(cfg, function, unresolved)`."""

    @property
    def cfg(self) -> Cfg:
        """The CFG `function` was lifted from."""
        ...
    @property
    def function(self) -> Function:
        """The lifted, optimised, indirect-branch-resolved IR."""
        ...
    @property
    def unresolved(self) -> List[int]:
        """Machine addresses of indirect branches that could not be
        resolved. Empty when the function resolved fully; a non-empty list is
        not an error."""
        ...
    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> Union[Cfg, Function, List[int]]: ...
    def __iter__(self) -> Iterator[Union[Cfg, Function, List[int]]]: ...
    def __repr__(self) -> str: ...

class LifterOptions:
    """Per-call options for `Lifter.analyze`.

    `cfg` holds the CFG-building options. `compact` drops unreachable nodes
    at the end. `per_address_ccs` overrides the calling convention at
    individual call sites. `pipeline`, when set, replaces the default
    optimizer pipeline for that one `analyze` call. `calls_clobber`,
    `assume_distinct_sp_bases_disjoint` and `alias_mode` tune what the
    optimizer may assume about memory.

    Raises `ValueError` for an unrecognised `alias_mode` or a nested
    `function_max_size=0`.
    """

    cfg: CfgOptions
    compact: bool
    per_address_ccs: Optional[dict]
    calls_clobber: bool
    assume_distinct_sp_bases_disjoint: bool
    alias_mode: AliasMode
    pipeline: Optional[OptimizerPipeline]
    def __init__(
        self,
        *,
        cfg: Optional[CfgOptions] = ...,
        compact: bool = ...,
        per_address_ccs: Optional[dict] = ...,
        calls_clobber: bool = ...,
        assume_distinct_sp_bases_disjoint: bool = ...,
        alias_mode: AliasMode = ...,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Build the options; every field defaults."""
        ...
    def with_cfg(self, cfg: CfgOptions) -> LifterOptions:
        """A copy of these options with `cfg` replaced."""
        ...

class Lifter:
    """Lifts, optimises and resolves functions for one architecture.

    Build one with `strider.lift.lifter(arch, mem, rom=None)`. The calling
    convention is an argument of every `analyze` call, so one handle can
    analyse functions under different conventions.
    """

    def __init__(
        self, arch: SleighArch, mem: MemLike, rom: Optional[RomLike] = ...
    ) -> None:
        """Build a handle for `arch` reading code from `mem`, with `rom` as
        the optional read-only memory for constant folding."""
        ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg:
        """Build the control-flow graph of the function at `entry`, without
        lifting or optimising."""
        ...
    def analyze(
        self,
        entry: Union[int, str],
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift, optimise and resolve the function at `entry`.

        A plain `Lifter` needs an address and a `cc`; it raises `StriderError`
        for a symbol name or a missing `cc`. (`ElfLifter` accepts a symbol
        name and supplies a default `cc`.)
        """
        ...
    def optimize(
        self,
        function: Function,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Run an optimizer pipeline over `function` in place. `pipeline=None`
        runs the default pipeline; a given pipeline is drained."""
        ...
    def neighborhood_dot(
        self,
        function: Function,
        center: int,
        depth: int = ...,
        hub_cap: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """Pretty DOT for the nodes within `depth` hops of node `center`,
        with register names resolved. `Function.neighborhood_dot` is the
        plain counterpart.

        A node whose degree exceeds `hub_cap` is shown but not expanded
        through, and `max_nodes` caps the total.
        """
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """The varnode for the register called `name`, or `None` when the
        name is not a register."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """The register name for `vn`, or `None` when it names no register."""
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """Decode from `entry` one instruction at a time until `addr`, and
        return that instruction's p-code (ops joined with `"; "`, empty for
        an instruction that lifts to none).

        A stand-alone sweep, so it works for an address outside any analysed
        CFG. `addr` must be reachable through the linear instruction stream
        from `entry`; raises `StriderError` otherwise.
        """
        ...
    def visualize(
        self,
        target: Union[Function, Cfg],
        *,
        host: str = ...,
        port: int = ...,
        depth: int = ...,
    ) -> None:
        """Start the interactive explorer for `target`, a `Function` or a
        `Cfg`. Prints the local URL and blocks on this thread until Ctrl-C.

        Off the main thread you MUST pair this with
        `strider.explore.shutdown(port)` and a thread join before the
        interpreter exits, or the process aborts.
        """
        ...

def lifter(
    arch: SleighArch,
    mem: MemLike,
    rom: Optional[RomLike] = ...,
) -> Lifter:
    """Create a lifter for `arch` that can lift and analyze functions. `mem`
    supplies the code bytes; `rom` is the optional read-only memory for
    constant folding."""
    ...

class ElfLifter(Lifter):
    """A loaded ELF binary as a `Lifter`, returned by
    `strider.lift.load_elf(path)`.

    It carries the ELF's memory as both code reader and read-only image, plus
    the symbol table and a name-aware `analyze(target)`. Analyse many
    functions by calling `analyze` repeatedly.
    """

    def __init__(
        self,
        elf,
        arch: SleighArch,
        cc: CallingConvention,
        mem: MemLike,
        rom: Optional[RomLike] = ...,
    ) -> None:
        """Do not call this directly; use `load_elf`."""
        ...
    @property
    def arch(self) -> SleighArch:
        """The architecture this binary was loaded as."""
        ...
    @property
    def cc(self) -> CallingConvention:
        """The calling convention `analyze` uses when none is passed."""
        ...
    def functions(self) -> Iterable[str]:
        """Every function and data symbol name in the loaded ELFs, sorted."""
        ...
    def symbol(self, name: str) -> int:
        """The address of `name`. Raises `StriderError` when it is not
        defined."""
        ...
    def symbol_size(self, name: str) -> Optional[int]:
        """The recorded size of `name` in bytes, or `None` when the size is
        recorded as zero. Raises `StriderError` when undefined."""
        ...
    def symbols(self) -> dict[str, int]:
        """Every symbol as a name-to-address dict."""
        ...
    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr`, or `None` when unmapped."""
        ...
    def reader(self) -> BufferReader:
        """The `BufferReader` over the ELF's loaded sections."""
        ...
    def add_elf(self, path: str, *, apply_relocations: bool = ...) -> None:
        """Merge another ELF, such as a shared library, into this handle.
        The earlier-loaded ELF wins on name collisions."""
        ...
    def analyze(
        self,
        entry: Union[int, str],
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift the function at `entry`, a symbol name or an address. `cc`
        defaults to this handle's convention."""
        ...
    def __repr__(self) -> str: ...

def load_elf(
    path: StrPath,
    *,
    from_segments: bool = ...,
    apply_relocations: bool = ...,
    arch: Optional[SleighArch] = ...,
    cc: Optional[CallingConvention] = ...,
) -> ElfLifter:
    """Load an ELF binary, picking the architecture and calling convention
    from its header, and return an `ElfLifter`.

    `from_segments=True` (the default) walks PT_LOAD program headers, falling
    back to sections for relocatable objects; `from_segments=False` forces
    the section walk for section-granular regions. Override the detected
    `arch=` / `cc=` for kernel or custom-ABI workflows. `apply_relocations`
    (default `True`) patches relocations in place, which shared objects and
    PIE binaries need.
    """
    ...
