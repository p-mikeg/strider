"""Type stubs for strider.lift — the lift+optimise+resolve handle
(`Lifter` / `ElfLifter`), its options, and the loaders."""

from __future__ import annotations

from typing import Any, Iterable, List, Optional, Tuple, Union

import os

from .cfg import Cfg, CfgOptions
from .ir import Function, Node
from .opt import OptimizerPipeline
from .reader import BufferReader
from .sleigh import CallingConvention, SleighArch, Vn

#: What the loaders accept as a filesystem path: a `str`, a `pathlib.Path`,
#: or any object implementing `__fspath__`.
StrPath = Union[str, "os.PathLike[str]"]

class LifterOptions:
    """Mirrors `strider_lift::LiftOptions` (nested `cfg`, exactly like
    the Rust struct) plus the optimize-side knobs, plus the per-function
    optimizer-pipeline override `pipeline`.  `pipeline`, when set,
    replaces the built-in default pipeline for THAT `analyze` call only
    (never on `strider.lift.lifter(...)` itself). Raises `ValueError` for an
    unrecognised `alias_mode` or a nested `function_max_size=0`."""

    cfg: CfgOptions
    compact: bool
    per_address_ccs: Optional[dict]
    calls_clobber: bool
    assume_distinct_sp_bases_disjoint: bool
    alias_mode: str
    pipeline: Optional[OptimizerPipeline]
    def __init__(
        self,
        *,
        cfg: Optional[CfgOptions] = ...,
        compact: bool = ...,
        per_address_ccs: Optional[dict] = ...,
        calls_clobber: bool = ...,
        assume_distinct_sp_bases_disjoint: bool = ...,
        alias_mode: str = ...,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None: ...
    def with_cfg(self, cfg: CfgOptions) -> LifterOptions: ...

class Lifter:
    """The single lift+optimise+resolve handle.  Build one with
    `strider.lift.lifter(arch, mem, rom=None)` (equivalently
    `strider.lift.Lifter(arch, mem, rom=None)`) — `cc` is NOT fixed at
    construction, it is a required argument of every `analyze` call, so
    one handle can analyse functions under different calling
    conventions.  `build_cfg` is structural-only (no lift/optimise/
    indirect-branch resolution); `analyze` drives the full fixed-point
    loop.  Subclassable from Python — see `ElfLifter`."""

    def __init__(self, arch: SleighArch, mem: Any, rom: Optional[Any] = ...) -> None: ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg: ...
    def analyze(
        self,
        entry: int,
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> Tuple[Cfg, Function, List[int]]: ...
    def optimize(
        self,
        function: Function,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Run an optimizer pipeline over `function`'s IR in place.
        `pipeline=None` (the default) builds and runs the canonical
        default pipeline (equivalent to the former
        `Function.reoptimize()`); passing an `OptimizerPipeline` runs
        that pipeline instead, draining it (equivalent to the former
        `Function.optimize(pipeline)`)."""
        ...
    def to_dot(self, function: Function, path: Optional[str] = ...) -> Optional[str]:
        """Render `function`'s IR graph to Graphviz DOT. Returns the DOT
        string when `path` is `None`, otherwise writes it to `path` and
        returns `None`.  Lives on `Lifter` (not `Function`) because the
        pretty renderer needs the Sleigh the Lifter owns to resolve
        register names."""
        ...
    def to_html(
        self,
        function: Function,
        path: Optional[str] = ...,
        style: Optional[str] = ...,
    ) -> Optional[str]:
        """Render `function`'s IR graph to a standalone HTML page.
        Returns the HTML string when `path` is `None`, otherwise writes
        it to `path` and returns `None`. `style` selects the dot theme
        (default `"dark"`)."""
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """Look up a register by Sleigh name; `None` when not a
        register.  Mirrors `Sleigh.reg`, using the register table this
        `Lifter` already owns."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """Reverse of `reg`: a varnode's Sleigh register name, or `None`
        when it names no register.  Decodes a varnode reached from a
        function this `Lifter` analysed (`function.node(m.root).vn()`)
        without constructing a separate `Sleigh`."""
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """Decode LINEARLY from `entry`, one machine instruction at a
        time (advancing by each instruction's machine byte length,
        replaying context-register state exactly as a real lift would),
        until the cursor reaches `addr`, and return that instruction's
        lifted p-code (ops joined `"; "`, empty for an instruction that
        lifts to no p-code, e.g. `endbr64`).

        Unlike `Cfg.pcode_at` / `Cfg.fingerprint_pcode` (an exact lookup
        against an already-built CFG's stored decodes), this is a
        stand-alone sweep — useful for an `addr` outside any CFG that
        was actually analysed.  It does NOT follow control flow: `addr`
        must be reachable via the LINEAR instruction stream starting at
        `entry` (the same assumption the lifter itself makes).

        Raises `StriderError` if `addr < entry`, or if the sweep steps
        PAST `addr` without landing exactly on it (misaligned target)."""
        ...
    def visualize(
        self,
        target: Any,  # Function | Cfg
        *,
        host: str = ...,
        port: int = ...,
        depth: int = ...,
    ) -> None:
        """Start the interactive explorer for `target` — a `Function`
        (from `analyze`) or a `Cfg` (from `build_cfg`/`analyze`).
        Prints the local URL to stdout and BLOCKS serving requests on
        this thread until interrupted (Ctrl-C)."""
        ...

def lifter(
    arch: SleighArch,
    mem: Any,
    rom: Optional[Any] = ...,
) -> Lifter:
    """Build a `Lifter` — the single lift+optimise+resolve handle — over
    a raw code reader (`BufferReader` or `MemReader`).  `rom` is the
    optional read-only memory image for `LoadReadOnly` constant folding.
    For an ELF, prefer `strider.lift.load_elf(path)` → `ElfLifter`, which
    wires `mem`/`rom` from the loaded sections and adds symbol lookups."""
    ...

# ── High-level facade (strider._api) ─────────────────────────────────────

class ElfLifter(Lifter):
    """The loaded ELF binary as a `Lifter` — `strider.lift.load_elf(path)`
    returns one.  `ElfLifter` IS a `Lifter`
    (`isinstance(x, strider.Lifter)` is true): it carries the same
    persistent lift+optimise+resolve state wired with the ELF's memory
    (as both code reader and ROM), plus the ELF symbol backend
    (symbols, sizes, the entry point, raw reads) and a name-aware
    `analyze(target)`.  Analyse many functions by calling `analyze`
    repeatedly."""

    def __init__(
        self,
        elf: Any,  # strider._LoadedElf (internal, returned by the loader)
        arch: SleighArch,
        cc: CallingConvention,
        mem: Any,
        rom: Optional[Any] = ...,
    ) -> None:
        """Do not construct directly — use `load_elf`.  This is
        `ElfLifter`'s real constructor (`elf` is the internal
        `_LoadedElf` the loader builds), NOT the inherited base
        `Lifter(arch, mem, rom=None)` shape."""
        ...
    @property
    def arch(self) -> SleighArch: ...
    @property
    def cc(self) -> CallingConvention: ...
    def functions(self) -> Iterable[str]: ...
    def symbol(self, name: str) -> int: ...
    def symbol_size(self, name: str) -> Optional[int]: ...
    def symbols(self) -> dict[str, int]: ...
    def entry_point(self) -> int: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...
    def reader(self) -> BufferReader:
        """The raw multi-region `BufferReader` assembled from the ELF's
        loaded sections — the low-level code reader for
        `strider.lift.lifter` / `strider.sleigh.Sleigh`."""
        ...
    def add_elf(self, path: str, *, apply_relocations: bool = ...) -> None: ...
    def analyze(  # type: ignore[override]
        self,
        target: Any,  # str | int
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> Tuple[Cfg, Function, List[int]]:
        """Lift the function at `target` (symbol name or absolute
        address), driving the full lift+optimise+resolve pipeline and
        returning the same `(Cfg, Function, unresolved_addrs)` tuple as
        the base `Lifter.analyze`.  `cc` defaults to the ELF-derived (or
        explicitly-passed at construction) calling convention."""
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
    """Load an ELF binary and return an `ElfLifter` — an `ElfLifter` IS a
    `Lifter` — with the arch and userland calling convention auto-picked
    from the ELF header.

    `from_segments=True` (the default) walks PT_LOAD program headers for
    ET_EXEC / ET_DYN binaries, falling back to the section-walker for
    ET_REL objects (which carry no program headers).  Pass
    `from_segments=False` to FORCE the section-header-walk strategy even
    for a linked binary that carries PT_LOAD segments (section-granular
    regions).  Override the auto-detected `arch=` / `cc=` for kernel /
    syscall / custom-ABI workflows.  `apply_relocations` (default `True`)
    widens the loaded section set and patches every understood relocation
    in-place."""
    ...
