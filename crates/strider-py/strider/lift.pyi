"""Type stubs for strider.lift: the lift, optimise and resolve handle
(`Lifter` / `ElfLifter`), its options, and the loaders."""

from __future__ import annotations

from typing import Any, Iterable, List, Literal, Optional, Tuple, Union

import os

from .cfg import Cfg, CfgOptions, DotStyle
from .ir import Function, Node
from .opt import OptimizerPipeline
from .reader import BufferReader
from .sleigh import CallingConvention, SleighArch, Vn

#: What the loaders accept as a filesystem path: a `str`, a `pathlib.Path`,
#: or any object implementing `__fspath__`.
StrPath = Union[str, "os.PathLike[str]"]

#: Memory-aliasing model for the optimizer. Validated by the `LifterOptions`
#: constructor, which raises `ValueError` for anything else.
AliasMode = Literal["stack_global_disjoint", "strict"]

class AnalyzeResult:
    """What `Lifter.analyze` returns: named fields, and still unpackable as
    `(cfg, function, unresolved)` in that order."""

    @property
    def cfg(self) -> Cfg:
        """The CFG of the final resolve-and-relift iteration, the one
        `function` was actually lifted from."""
        ...
    @property
    def function(self) -> Function:
        """The lifted, optimised, indirect-branch-resolved IR."""
        ...
    @property
    def unresolved(self) -> List[int]:
        """Machine addresses of indirect branches that could not be
        resolved. Empty when the function resolved fully; a non-empty list is
        not an error, it is the honest report that some edges are unknown."""
        ...
    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> Any: ...
    def __iter__(self) -> Any: ...
    def __repr__(self) -> str: ...

class LifterOptions:
    """Per-call knobs for `Lifter.analyze`.

    `cfg` holds the nested CFG-building options. `compact` drops unreachable
    nodes at the end. `per_address_ccs` overrides the calling convention at
    individual call sites. `calls_clobber` and
    `assume_distinct_sp_bases_disjoint` loosen or tighten what the optimizer
    may assume about memory. `alias_mode` picks the aliasing model.
    `pipeline`, when set, replaces the default optimizer pipeline for that
    one `analyze` call.

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
    """The handle that lifts, optimises and resolves functions.

    Build one with `strider.lift.lifter(arch, mem, rom=None)` (equivalently
    `strider.lift.Lifter(...)`). The calling convention is not fixed at
    construction: it is an argument of every `analyze` call, so one handle
    can analyse functions under different conventions. `build_cfg` is
    structural only; `analyze` drives the full pipeline. Subclassable from
    Python, as `ElfLifter` shows.
    """

    def __init__(self, arch: SleighArch, mem: Any, rom: Optional[Any] = ...) -> None:
        """Build a handle for `arch` reading code from `mem`, with `rom` as
        the optional read-only memory image for constant folding."""
        ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg:
        """Build the control-flow graph of the function at `entry`, without
        lifting, optimising, or resolving indirect branches."""
        ...
    def analyze(
        self,
        entry: Union[int, str],
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift, optimise and indirect-branch-resolve the function at
        `entry`, returning an `AnalyzeResult` (named fields; also unpacks as
        `(cfg, function, unresolved)`).

        `entry` is typed `int | str` and `cc` is optional so that the
        `ElfLifter` subclass, which resolves a symbol name and derives a
        default convention from the ELF header, widens this signature rather
        than breaking it. A plain `Lifter` owns neither a symbol table nor a
        default convention, so it raises `StriderError` for a `str` entry or
        a missing `cc`.
        """
        ...
    def optimize(
        self,
        function: Function,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Run an optimizer pipeline over `function` in place.
        `pipeline=None` runs the canonical default pipeline; passing an
        `OptimizerPipeline` runs that one instead, draining it."""
        ...
    def neighborhood_dot(
        self,
        function: Function,
        center: int,
        depth: int = ...,
        hub_cap: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """Pretty DOT for the nodes within `depth` hops of node `center`:
        one DOT node per IR node, per-kind styling, `center` highlighted.
        Resolves register names, which needs this handle's disassembler;
        `Function.neighborhood_dot` is the plain counterpart.

        A node whose degree exceeds `hub_cap` is shown but not expanded
        through, and `max_nodes` caps the total (nearest nodes win), so a
        dense region cannot blow the render up.
        """
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """The varnode for the register called `name`, or `None` when the
        name is not a register."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """The register name for `vn`, or `None` when it names no register.
        Decodes a varnode reached from a function this handle analysed
        (`function.node(m.root).vn()`) without building a separate
        `Sleigh`."""
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """Decode linearly from `entry` one instruction at a time, replaying
        context state as a real lift would, until the cursor reaches `addr`,
        and return that instruction's p-code (operations joined with `"; "`,
        empty for an instruction that lifts to none, such as `endbr64`).

        Unlike `Cfg.pcode_at` and `Cfg.fingerprint_pcode`, which look up an
        already-built CFG's stored decodes, this is a stand-alone sweep,
        useful for an address outside any analysed CFG. It does not follow
        control flow: `addr` must be reachable through the linear instruction
        stream from `entry`.

        Raises `StriderError` if `addr < entry`, or if the sweep steps past
        `addr` without landing exactly on it.
        """
        ...
    def visualize(
        self,
        target: Any,  # Function | Cfg
        *,
        host: str = ...,
        port: int = ...,
        depth: int = ...,
    ) -> None:
        """Start the interactive explorer for `target`, a `Function` from
        `analyze` or a `Cfg` from `build_cfg` / `analyze`. Prints the local
        URL and blocks serving requests on this thread until interrupted with
        Ctrl-C.

        Off the main thread you MUST pair this with
        `strider.explore.shutdown(port)` and a thread join before the
        interpreter exits. A thread still parked here at interpreter shutdown
        is killed in a way strider cannot recover from, and the process
        aborts. Objects created inside such a thread are also leaked rather
        than freed if they outlive it.
        """
        ...

def lifter(
    arch: SleighArch,
    mem: Any,
    rom: Optional[Any] = ...,
) -> Lifter:
    """Build a `Lifter` over a raw code reader (`BufferReader` or
    `MemReader`). `rom` is the optional read-only memory image for constant
    folding. For an ELF, prefer `strider.lift.load_elf(path)`, which wires
    `mem` and `rom` from the loaded sections and adds symbol lookups."""
    ...

class ElfLifter(Lifter):
    """A loaded ELF binary as a `Lifter`, returned by
    `strider.lift.load_elf(path)`.

    An `ElfLifter` IS a `Lifter` (`isinstance(x, strider.lift.Lifter)` is
    true): it carries the same lift, optimise and resolve state, wired with
    the ELF's memory as both code reader and read-only image, plus the ELF
    symbol table (symbols, sizes, entry point, raw reads) and a name-aware
    `analyze(target)`. Analyse many functions by calling `analyze` repeatedly.
    """

    def __init__(
        self,
        elf: Any,  # the internal loaded-ELF handle the loader builds
        arch: SleighArch,
        cc: CallingConvention,
        mem: Any,
        rom: Optional[Any] = ...,
    ) -> None:
        """Do not call this directly; use `load_elf`. This is `ElfLifter`'s
        own constructor, not the inherited `Lifter(arch, mem, rom=None)`
        shape."""
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
        """Every function and data symbol name in the loaded ELFs, sorted.
        The first-loaded ELF wins on name collisions."""
        ...
    def symbol(self, name: str) -> int:
        """The address of `name`. Raises `StriderError` when it is not
        defined in any loaded ELF."""
        ...
    def symbol_size(self, name: str) -> Optional[int]:
        """The recorded size in bytes of `name`, or `None` when the symbol
        exists but its size is recorded as zero. Raises `StriderError` when
        the symbol is not defined."""
        ...
    def symbols(self) -> dict[str, int]:
        """Every symbol as a name-to-address dict. The first-loaded ELF wins
        on name collisions."""
        ...
    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr` from the loaded regions, or
        `None` when `addr` is unmapped."""
        ...
    def reader(self) -> BufferReader:
        """The multi-region `BufferReader` assembled from the ELF's loaded
        sections, for passing to `strider.lift.lifter` or
        `strider.sleigh.Sleigh` directly."""
        ...
    def add_elf(self, path: str, *, apply_relocations: bool = ...) -> None:
        """Merge another ELF, such as a shared library, into this handle,
        extending both the loaded regions and the symbol set. The
        earlier-loaded ELF wins on name collisions, and later `analyze`,
        `read` and `symbol` calls see the new ELF."""
        ...
    def analyze(
        self,
        entry: Union[int, str],
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift the function at `entry`, a symbol name or an absolute
        address, returning the same `AnalyzeResult` as `Lifter.analyze`. `cc`
        defaults to this handle's convention.

        This widens the base signature rather than breaking it: the base
        already declares `entry: int | str` with `cc` optional so that symbol
        lookup and a default convention are additions, not violations.
        """
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
    """Load an ELF binary and return an `ElfLifter`, with the architecture
    and userland calling convention picked from the ELF header.

    `from_segments=True` (the default) walks PT_LOAD program headers for
    executables and shared objects, falling back to the section walker for
    relocatable objects, which carry no program headers. Pass
    `from_segments=False` to force the section walk even for a linked binary
    that does carry PT_LOAD segments, giving section-granular regions.

    Override the detected `arch=` and `cc=` for kernel, syscall or custom-ABI
    workflows. `apply_relocations` (default `True`) widens the loaded section
    set and patches every understood relocation in place.
    """
    ...
