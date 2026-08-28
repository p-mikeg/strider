"""Type stubs for strider.lift: the lift, optimise and resolve handle
(`Lifter` / `ElfLifter`), its options, and the loaders."""

from __future__ import annotations

from typing import (
    Any,
    Literal,
    Mapping,
    NamedTuple,
    Optional,
    TypeVar,
    Union,
)

import os

from .cfg import Cfg, CfgOptions
from .ir import Function
from .opt import OptimizerPipeline
from .reader import BufferReader, MemLike, RomLike, Symbol, SymbolIter
from .sleigh import CallingConvention, CallOtherAbi, SleighArch, Vn

__all__: list[str]

#: What the loaders accept as a filesystem path.
StrPath = Union[str, "os.PathLike[str]"]

_L = TypeVar("_L", bound="Lifter")

class AnalyzeResult(NamedTuple):
    """What `Lifter.analyze` returns: a named 3-tuple of the CFG, the lifted
    function, and the unresolved indirect branches. A real
    `collections.namedtuple`, so `isinstance(result, tuple)` holds and it
    compares equal to the plain tuple.

    `unresolved` holds the machine addresses of indirect branches that could
    not be resolved, and a non-empty list is not an error.

    Empty means fully resolved, NOT that the answer is complete: it is one of
    four incompleteness channels, and a dispatch the CFG consumed as a return
    or a tail call is reported only through `cfg.unverified_seeded_sites()`.
    The other two are `cfg.isa_mode_conflicts()` and
    `cfg.interior_branch_targets()`; `cfg.is_complete()` tests all four.
    """

    cfg: Cfg
    function: Function
    unresolved: list[int]

class AssumptionOptions:
    """Claims about the code being analysed, for `LifterOptions(assumptions=...)`.

    None of these is checked, and each one's risky value is the positive one,
    so any of them can make the answer wrong on valid input. Clearing all six
    (and leaving `noalias_allocators` empty) is the only configuration sound
    under any input.

    Two default `True`, both honoured by every compiler this analyses and
    without which the alias oracle answers may-alias almost everywhere.
    `stack_global_disjoint` says the stack and global/constant memory
    (`.data`, `.rodata`, `.bss`, MMIO) never overlap at runtime, so a memory
    walk steps through a constant-address store when looking back from a stack
    load and vice-versa; it is wrong only where a constant address
    coincidentally equals `sp + K`. `assume_incoming_args_survive_calls` lets
    a call on an incoming stack-argument slot's memory chain leave the slot
    alone, so the argument is still detectable afterwards; it reaches
    incoming-argument detection only, where it holds for any conforming
    callee, those slots being the caller's memory above the entry SP.

    `distinct_sp_bases_disjoint` treats a store rooted at a different SP base
    than the entry SP, an alignment-masked frame local say, as disjoint from
    the probed location.

    `callee_preserves_stack_args` empties the outgoing-argument window, so a
    value spilled at the stack top forwards across a call. The psABIs let a
    callee write the argument slots that hold its own parameters; this asserts
    compiler output does not. It is inert on its own: the window is consulted
    only under `escape_analysis` or at a call to a `noalias_allocators`
    address, so set it together with one of those.

    `noalias_allocators` lists callee addresses of pure `malloc`-like
    allocators: a size in, a fresh non-overlapping pointer out, no pointer
    arguments. Distinct allocations are then disjoint and a load steps through
    such a call. An address that can return an existing or interior pointer
    (`realloc`, `free`, an aligned-alloc taking a pointer) breaks that.

    `escape_analysis` forwards a spill load across a call and past an opaque
    store when the frame is provably private, meaning no stack address escapes
    to a callee. The proof is sound; the claim is that no callee returns a
    struct by value, an sret hidden pointer being an escape the analysis may
    not see.
    """

    # Read-only: the options types are frozen, so a plain attribute
    # declaration would let a type checker accept `o.x = ...`, which raises at runtime.
    @property
    def stack_global_disjoint(self) -> bool: ...
    @property
    def assume_incoming_args_survive_calls(self) -> bool: ...
    @property
    def distinct_sp_bases_disjoint(self) -> bool: ...
    @property
    def callee_preserves_stack_args(self) -> bool: ...
    @property
    def noalias_allocators(self) -> list[int]: ...
    @property
    def escape_analysis(self) -> bool: ...
    def __init__(
        self,
        *,
        stack_global_disjoint: bool = ...,
        assume_incoming_args_survive_calls: bool = ...,
        distinct_sp_bases_disjoint: bool = ...,
        callee_preserves_stack_args: bool = ...,
        noalias_allocators: list[int] = ...,
        escape_analysis: bool = ...,
    ) -> None:
        """Build the claims; every field defaults as documented above."""
        ...

class LifterOptions:
    """Per-call options for `Lifter.analyze`.

    `cfg` holds the CFG-building options. `assumptions` holds the unchecked
    claims about the analysed code, each of which can make the answer wrong.
    `compact` drops unreachable nodes at the end. `per_address_ccs` overrides
    the calling convention for a callee, keyed by the direct-call target
    address rather than the call site. `pipeline`, when set,
    replaces the default optimizer pipeline for the calls these options drive.

    Raises `ValueError` for a nested `function_max_size=0`.
    """

    # Read-only: the options types are frozen, so a plain attribute
    # declaration would let a type checker accept `o.x = ...`, which raises at runtime.
    @property
    def cfg(self) -> CfgOptions: ...
    @property
    def assumptions(self) -> AssumptionOptions: ...
    @property
    def compact(self) -> bool: ...
    @property
    def per_address_ccs(self) -> Optional[dict[int, CallingConvention]]: ...
    @property
    def resolve_indirect_branches(self) -> bool: ...
    @property
    def pipeline(self) -> Optional[OptimizerPipeline]: ...
    def __init__(
        self,
        *,
        cfg: Optional[CfgOptions] = ...,
        assumptions: Optional[AssumptionOptions] = ...,
        compact: bool = ...,
        per_address_ccs: Optional[dict[int, CallingConvention]] = ...,
        resolve_indirect_branches: bool = ...,
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
    # pyo3's `#[new]` fills `tp_new`, so a Python subclass constructs the base
    # through `__new__`; `ElfLifter` does.
    def __new__(
        cls: type[_L],
        arch: SleighArch,
        mem: MemLike,
        rom: Optional[RomLike] = ...,
    ) -> _L: ...
    def _rebuild(
        self, arch: SleighArch, mem: MemLike, rom: Optional[RomLike] = ...
    ) -> None:
        """INTERNAL. Rebuild the handle's Sleigh and orchestrator state, so a
        newly merged-in ELF becomes visible."""
        ...
    def reader(self) -> MemLike:
        """The code source (`BufferReader` or `MemReader`) this handle was
        built with."""
        ...
    def rom(self) -> Optional[RomLike]:
        """The `rom` (read-only memory for constant folding) this handle was
        built with, or `None` if none was supplied."""
        ...
    def user_op_names(self) -> list[str]:
        """Every Sleigh user-op name this architecture can emit, indexed by
        user-op id. These are the names `CfgOptions(call_other_abis=...)`
        classifies."""
        ...
    def call_other_abi(
        self, name: str, opts: Optional[CfgOptions] = ...
    ) -> Optional[CallOtherAbi]:
        """How `name` is classified: the `opts` entry for it when there is
        one, else the built-in table, else `None` for a name strider has no
        answer for (which fails the lift of any function containing it)."""
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
        entry: int,
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift, optimise and resolve the function at address `entry`.

        `cc` is required here; `ElfLifter.analyze` also takes a symbol name
        and supplies a default `cc`.

        An empty `unresolved` is not a complete answer: see `AnalyzeResult`
        for the four channels, and `Cfg.is_complete` to test them all.
        """
        ...
    def optimize(
        self,
        function: Function,
        pipeline: Optional[OptimizerPipeline] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> None:
        """Run an optimizer pipeline over `function` in place. `pipeline=None`
        runs the default pipeline; a given pipeline is copied, so it stays
        usable. `opts=None` takes the `LifterOptions` defaults.

        Runs against this handle's `rom`, so `LoadReadOnly` folds here exactly
        as it does inside `analyze`. Invalidates outstanding `Node` / `Match`
        handles and every node id for `function`."""
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
        host: str = ...,
        port: int = ...,
        depth: Optional[int] = ...,
    ) -> None:
        """Start the interactive explorer for `target`, a `Function` or a
        `Cfg`. It renders the NEIGHBORHOOD around a node you pick (inputs and
        outputs out to `depth` hops), never the whole graph, so it scales to
        large functions. Prints the local URL and blocks on this thread until
        Ctrl-C.

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

    #: INTERNAL. The loaded-ELF handle: the code and read-only readers, the
    #: symbol table, and the raw `read`. Its class is not in the module
    #: namespace, so there is no name to give it here.
    _elf: Any

    def __init__(
        self,
        elf: Any,
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
    @property
    def is_arm_be8(self) -> bool:
        """Whether the binary sets ARM's `EF_ARM_BE8`: instructions stored
        little-endian, data big-endian. `EI_DATA` marks BE8 and BE32 images
        alike, so this is what `load_elf` picks `arm_be_kernel` over `arm_be`
        on. `False` off ARM."""
        ...
    @property
    def endianness(self) -> str:
        """Byte order of the loaded binary: `"little"` or `"big"`."""
        ...
    def functions(self) -> SymbolIter:
        """The function symbols in address order, one per address: aliases of
        an address already listed are excluded, preferring the one whose size
        the ELF records. A function with no recorded size is still yielded,
        with `Symbol.size` `None`.

        A cursor, not a container: `len()` reports what is LEFT to pull."""
        ...
    def iter_symbols(self) -> SymbolIter:
        """Every symbol pulled one at a time, so the `Symbol` objects are never
        all live at once. The Rust table is built up front. `len()` reports
        what is LEFT to pull, so a consumed cursor shrinks."""
        ...
    def symbol(self, name: str) -> Symbol:
        """The `Symbol` named `name`. Raises `StriderError` when it is not
        defined."""
        ...
    def symbol_opt(self, name: str) -> Optional[Symbol]:
        """`symbol`, but `None` rather than raising when `name` is
        undefined."""
        ...
    def symbol_at(self, address: int) -> Optional[Symbol]:
        """The symbol covering `address`: the nearest one at or below it whose
        recorded extent reaches `address`. A symbol with no recorded size
        covers only its own address, and aliases sharing an address resolve to
        the code symbol among them. `None` when nothing covers `address`."""
        ...
    def symbols(self) -> Mapping[str, Symbol]:
        """Every symbol, keyed by the name it resolves under."""
        ...
    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr`, or `None` when unmapped."""
        ...
    def reader(self) -> BufferReader:
        """The `BufferReader` over the regions `load_elf` mapped: PT_LOAD segments
        unless `from_segments=False`."""
        ...
    def add_elf(self, path: str, *, apply_relocations: bool = ...) -> None:
        """Merge another ELF, such as a shared library, into this handle.
        The earlier-loaded ELF wins on name collisions.

        Raises `StriderError` if the new ELF maps code over an address already
        loaded with different bytes (two ELFs at the same base cannot merge)."""
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
    (default `True`) applies relocations, which shared objects and PIE
    binaries need; it also selects what is mapped, so `False` drops writable
    non-executable sections rather than serving their on-disk bytes. A
    writable-and-executable mapping is kept either way.
    """
    ...
