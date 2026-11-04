"""The lift, optimise and resolve handle (`Lifter` / `ElfLifter`), its
options, and the ELF loader."""

from __future__ import annotations

from typing import (
    Any,
    ClassVar,
    Literal,
    Mapping,
    NamedTuple,
    Optional,
    TypeVar,
    Union,
)

from .cfg import Cfg, CfgOptions
from .ir import Function
from .opt import OptimizerPipeline
from .reader import MemLike, RomLike
from .sleigh import CallingConvention, CallOtherAbi, SleighArch, Vn

__all__: list[str]

from ._api import ElfLifter as ElfLifter, StrPath as StrPath, load_elf as load_elf

_L = TypeVar("_L", bound="Lifter")

class AnalyzeResult(NamedTuple):
    """What `Lifter.analyze` returns: (cfg, function, unresolved).

    `unresolved` holds the machine addresses `cfg.incomplete` names
    `"unresolved_branch"`. Empty means fully resolved, NOT that the answer is
    complete: `cfg.incomplete` lists every reason.
    """

    cfg: Cfg
    function: Function
    unresolved: list[int]

class AssumptionOptions:
    """Unchecked claims about the analysed code, for
    `LifterOptions(assumptions=...)` and `Function.optimize(assumptions=...)`;
    a wrong claim makes the answer wrong.

    Five default `True`, the other four off or empty; clearing all nine is
    the sound floor (see `none`). Claims accumulate on a `Function`:
    `Function.optimize` adds, none withdraws, and it refuses to add a
    lift-time claim (`msr_register_keeps_mode`, `fp_rounding_mode_is_nearest`).
    """

    @property
    def stack_global_disjoint(self) -> bool:
        """The stack never overlaps constant-address memory (`.data`, `.rodata`,
        `.bss`, MMIO). Default `True`."""
        ...
    @property
    def assume_incoming_args_survive_calls(self) -> bool:
        """A call leaves the incoming stack-argument slots alone unless handed a
        pointer into them. Default `True`; read by incoming-argument detection."""
        ...
    @property
    def distinct_sp_bases_disjoint(self) -> bool:
        """A store rooted at an SP base other than the entry SP (`sp & -16`) misses
        the probed slot; read by incoming-argument detection."""
        ...
    @property
    def callee_preserves_stack_args(self) -> bool:
        """A callee leaves the outgoing-argument slots the caller wrote alone.
        Inert without `escape_analysis` or `noalias_allocators`."""
        ...
    @property
    def noalias_allocators(self) -> list[int]:
        """Callee addresses of pure `malloc`-like allocators, sorted. Each result
        overlaps no other allocation, stack slot or constant address, so an arena
        allocator does not qualify."""
        ...
    @property
    def escape_analysis(self) -> bool:
        """When no stack address escapes to a callee, forward a spill load across
        a call (and, under `stack_global_disjoint`, past an opaque store)."""
        ...
    @property
    def return_slots_survive_untracked_writes(self) -> bool:
        """Untracked stores, callees and user-ops leave the saved return address
        and entry SP slots alone. Default `True`; off, such a return is unverified."""
        ...
    @property
    def msr_register_keeps_mode(self) -> bool:
        """An ARM `msr cpsr_c, rN` writes the mode already in force; the
        immediate form and `cps` still switch. Default `True`; a lift-time claim."""
        ...
    @property
    def fp_rounding_mode_is_nearest(self) -> bool:
        """Float conversions reading the dynamic rounding mode round to nearest
        even; off, each selects among the four modes. Wrong after `fesetround`.
        Default `True`; a lift-time claim."""
        ...
    def __init__(
        self,
        *,
        stack_global_disjoint: Optional[bool] = ...,
        assume_incoming_args_survive_calls: Optional[bool] = ...,
        distinct_sp_bases_disjoint: Optional[bool] = ...,
        callee_preserves_stack_args: Optional[bool] = ...,
        noalias_allocators: Optional[list[int]] = ...,
        escape_analysis: Optional[bool] = ...,
        return_slots_survive_untracked_writes: Optional[bool] = ...,
        msr_register_keeps_mode: Optional[bool] = ...,
        fp_rounding_mode_is_nearest: Optional[bool] = ...,
    ) -> None:
        """A field omitted or `None` takes its default."""
        ...
    @staticmethod
    def none(
        *,
        stack_global_disjoint: Optional[bool] = ...,
        assume_incoming_args_survive_calls: Optional[bool] = ...,
        distinct_sp_bases_disjoint: Optional[bool] = ...,
        callee_preserves_stack_args: Optional[bool] = ...,
        noalias_allocators: Optional[list[int]] = ...,
        escape_analysis: Optional[bool] = ...,
        return_slots_survive_untracked_writes: Optional[bool] = ...,
        msr_register_keeps_mode: Optional[bool] = ...,
        fp_rounding_mode_is_nearest: Optional[bool] = ...,
    ) -> "AssumptionOptions":
        """Every claim cleared, then the ones named set; with no argument,
        the sound floor. `none(escape_analysis=True)` holds one claim where
        `AssumptionOptions(escape_analysis=True)` holds six.

        The floor still takes memory to be RAM (a load reads the last value
        stored) and float arithmetic to round to nearest with flush-to-zero
        and denormals-are-zero off, so `-ffast-math` code has no floor.
        """
        ...
    def union(self, other: AssumptionOptions) -> AssumptionOptions:
        """Every claim either holds, every allocator either lists; `a | b`."""
        ...
    def __or__(self, other: AssumptionOptions) -> AssumptionOptions: ...
    def __eq__(self, other: object) -> bool:
        """Equal claims and allocators."""
        ...
    __hash__: ClassVar[None]  # type: ignore[assignment]

class LifterOptions:
    """Per-call options for `Lifter.analyze`.

    `compact` drops unreachable nodes at the end. `per_address_ccs`
    overrides the calling convention of a callee, keyed by the direct-call
    target address (not the call site); on ppc64 ELFv2 `ElfLifter.analyze`
    also matches a call to `Symbol.local_entry` for a key at
    `Symbol.address`. An override's `stack_pointer` must be the analysed
    function's own, or `analyze` raises `StriderError`. `pipeline`, when
    set, replaces the default optimizer pipeline.
    """

    @property
    def cfg(self) -> CfgOptions: ...
    @property
    def assumptions(self) -> AssumptionOptions: ...
    @property
    def compact(self) -> bool: ...
    @property
    def per_address_ccs(self) -> Optional[Mapping[int, CallingConvention]]: ...
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
        per_address_ccs: Optional[Mapping[int, CallingConvention]] = ...,
        resolve_indirect_branches: bool = ...,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Every field defaults."""
        ...

class Lifter:
    """Lifts, optimises and resolves functions for one architecture. Build
    one with `strider.lift.lifter(arch, mem, rom=None)`."""

    def __init__(
        self, arch: SleighArch, mem: MemLike, rom: Optional[RomLike] = ...
    ) -> None:
        """A handle for `arch` reading code from `mem`, with optional
        read-only memory `rom`."""
        ...
    # A subclass (`ElfLifter`) constructs the base through `__new__`.
    def __new__(
        cls: type[_L],
        arch: SleighArch,
        mem: MemLike,
        rom: Optional[RomLike] = ...,
    ) -> _L: ...
    def _merge_elf(
        self,
        elf: Any,
        path: str,
        apply_relocations: bool,
        relocated_got_is_rom: bool,
    ) -> None:
        """Merge the ELF at `path` into `elf` and rebuild, refusing before
        anything is merged when the handle is in use."""
        ...
    def _build_cfg_elf(
        self, elf: Any, entry: int, opts: Optional[CfgOptions] = ...
    ) -> Cfg:
        """`ElfLifter.build_cfg` over `elf`."""
        ...
    def _analyze_elf(
        self,
        elf: Any,
        entry: Union[str, int],
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """`ElfLifter.analyze` over `elf`."""
        ...
    @property
    def arch(self) -> SleighArch:
        """The `SleighArch` this handle decodes with. With `reader()` and
        `rom()`, enough to build a second handle for another thread. Raises
        while an `analyze`, `build_cfg` or `add_elf` is in flight."""
        ...
    def reader(self) -> MemLike:
        """The `mem` this handle was built with."""
        ...
    def rom(self) -> Optional[RomLike]:
        """The `rom` this handle was built with, or `None`."""
        ...
    def user_op_names(self) -> list[str]:
        """Every Sleigh user-op name of this arch, indexed by user-op id: the
        keys `CfgOptions(call_other_abis=...)` takes."""
        ...
    def call_other_abi(
        self, name: str, opts: Optional[CfgOptions] = ...
    ) -> Optional[CallOtherAbi]:
        """The `CallOtherAbi` for user op `name`: the `opts` entry, else the
        built-in one, else `None` (a lift reaching it fails). Raises
        `StriderError` for an `opts` entry built for another arch."""
        ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg:
        """The control-flow graph of the function at `entry`, without
        lifting. Holds the GIL throughout."""
        ...
    def analyze(
        self,
        entry: int,
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift, optimise and resolve the function at address `entry`.

        `ElfLifter.analyze` also takes a symbol name and a default `cc`. A
        bad `entry` raises `StriderError`. Returns even when incomplete:
        check `Cfg.incomplete` before trusting an absence ("no store to X").

        Releases the GIL unless `mem` or `rom` is a Python object. A daemon
        thread inside this call at interpreter shutdown aborts the process;
        analyse on non-daemon threads and join them.
        """
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """The p-code (ops joined `"; "`, empty for none) of the instruction
        at `addr`, decoding linearly from `entry`.

        Raises `StriderError` unless `addr` is on the linear stream from
        `entry`, or once this `Lifter` has pinned more decode modes than a
        sweep can carry over (sweep from a fresh `Lifter`). On ARM or MIPS
        `entry`'s low bit selects the ISA mode and the sweep starts at
        `entry & ~1`.
        """
        ...

def lifter(
    arch: SleighArch,
    mem: MemLike,
    rom: Optional[RomLike] = ...,
) -> Lifter:
    """A `Lifter` for `arch` reading code from `mem`, with optional read-only
    memory `rom`."""
    ...
