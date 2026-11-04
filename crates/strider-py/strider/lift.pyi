"""Type stubs for strider.lift: the lift, optimise and resolve handle
(`Lifter` / `ElfLifter`), its options, and the loaders."""

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
    """What `Lifter.analyze` returns: a `collections.namedtuple` of the CFG,
    the lifted function, and the unresolved indirect branches.

    `unresolved` holds the machine addresses `cfg.incomplete` names
    `"unresolved_branch"`. Empty means fully resolved, NOT that the answer is
    complete: `cfg.incomplete` lists every reason.
    """

    cfg: Cfg
    function: Function
    unresolved: list[int]

class AssumptionOptions:
    """Claims about the code being analysed, for `LifterOptions(assumptions=...)`
    and `Function.optimize(assumptions=...)`.

    None is checked, and each one's risky value is the positive one, so a
    wrong claim makes the answer wrong. `stack_global_disjoint`,
    `assume_incoming_args_survive_calls`,
    `return_slots_survive_untracked_writes`, `msr_register_keeps_mode` and
    `fp_rounding_mode_is_nearest` default `True`, the other four off or empty;
    clearing all nine is the sound floor (see `none`). What each one claims is
    in `docs/python-api.md` section 2.

    Claims accumulate on a `Function` (`Function.assumptions`): `analyze`
    starts from the ones it is given, `Function.optimize` adds, none withdraws.
    The lift reads `msr_register_keeps_mode` and `fp_rounding_mode_is_nearest`,
    so `optimize` refuses to add either: analyse again with the claim.
    """

    @property
    def stack_global_disjoint(self) -> bool: ...
    @property
    def assume_incoming_args_survive_calls(self) -> bool: ...
    @property
    def distinct_sp_bases_disjoint(self) -> bool: ...
    @property
    def callee_preserves_stack_args(self) -> bool: ...
    @property
    def noalias_allocators(self) -> list[int]:
        """Callee addresses of pure `malloc`-like allocators, sorted. Each
        listed call's result overlaps no other live allocation, no stack slot
        and no constant address, so an arena allocator does not qualify."""
        ...
    @property
    def escape_analysis(self) -> bool: ...
    @property
    def return_slots_survive_untracked_writes(self) -> bool: ...
    @property
    def msr_register_keeps_mode(self) -> bool: ...
    @property
    def fp_rounding_mode_is_nearest(self) -> bool:
        """Float conversions that read the dynamic rounding mode (x86 MXCSR.RC,
        x87 RC at `fst` / `fstp`, PowerPC FPSCR.RN, ARM FPSCR.RMode, AArch64
        FPCR.RMode, MIPS FCSR.RM) round to nearest even, so they fold.
        Fixed-mode forms (`cvtt*`, `fcti*z`, ...) read no mode, and x87 `fist`,
        `fistp` and `frndint` always read RC. Off, each selects among the four
        modes. Wrong after `fesetround` or a caller's mode change."""
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
        """Build the claims; a field omitted or `None` takes its default.
        `noalias_allocators` reads back sorted, without duplicates."""
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
        """Every claim cleared, then the ones named set: with no argument, the
        sound floor of these fields.

        Five claims default `True`, so `AssumptionOptions(escape_analysis=True)`
        holds six where `none(escape_analysis=True)` holds one. Add one claim
        to a function's with
        `function.assumptions | AssumptionOptions.none(escape_analysis=True)`.

        Two premises remain, and no option lifts them. Memory is RAM: a load
        reads back the last value the analysed code stored there (a polled
        MMIO register reads as the value written). Float arithmetic rounds to
        nearest, and x86 MXCSR.FTZ and DAZ, PowerPC FPSCR.NI, OE and UE, ARM
        FPSCR.FZ, AArch64 FPCR.FZ and MIPS FCSR.FS are clear, so `-ffast-math`
        code has no floor here.
        """
        ...
    def union(self, other: AssumptionOptions) -> AssumptionOptions:
        """Every claim either holds, and every allocator either lists. `a | b`
        is the same."""
        ...
    def __or__(self, other: AssumptionOptions) -> AssumptionOptions: ...
    def __eq__(self, other: object) -> bool:
        """Equal when both hold the same claims and list the same
        allocators."""
        ...
    __hash__: ClassVar[None]  # type: ignore[assignment]

class LifterOptions:
    """Per-call options for `Lifter.analyze`.

    `cfg` holds the CFG-building options. `assumptions` holds the unchecked
    claims about the analysed code (the defaults when not passed).
    `compact` drops unreachable nodes at the end. `per_address_ccs` overrides
    the calling convention for a callee, keyed by the direct-call target
    address rather than the call site; on ppc64 ELFv2 only `ElfLifter.analyze`
    also answers a call to `Symbol.local_entry` for a key at `Symbol.address`.
    An override's `stack_pointer` must be the analysed function's own, or
    `analyze` raises `StriderError`. `pipeline`, when set, replaces the
    default optimizer pipeline.
    """

    @property
    def cfg(self) -> CfgOptions: ...
    @property
    def assumptions(self) -> AssumptionOptions: ...
    @property
    def compact(self) -> bool: ...
    # A read-only view, built once.
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
        """Build the options; every field defaults."""
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
    # pyo3's `#[new]` fills `tp_new`, so a subclass (`ElfLifter`) constructs
    # the base through `__new__`.
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
        """INTERNAL. Merge the ELF at `path` into the loaded-ELF handle `elf`
        and rebuild over the merged image, refusing before anything is merged
        when the handle is in use."""
        ...
    def _build_cfg_elf(
        self, elf: Any, entry: int, opts: Optional[CfgOptions] = ...
    ) -> Cfg:
        """INTERNAL. `ElfLifter.build_cfg` over the loaded-ELF handle `elf`."""
        ...
    def _analyze_elf(
        self,
        elf: Any,
        entry: Union[str, int],
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """INTERNAL. `ElfLifter.analyze` over the loaded-ELF handle `elf`."""
        ...
    @property
    def arch(self) -> SleighArch:
        """The `SleighArch` this handle decodes with.

        With `reader()` and `rom()`, enough to build a second handle over the
        same memory for a parallel thread. Raises while an `analyze`,
        `build_cfg` or `add_elf` is in flight."""
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
        answer for (which fails the lift of any function containing it).
        Raises `StriderError` where `build_cfg` would over `opts`'
        `call_other_abis`, for an entry built against another arch's
        register table."""
        ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg:
        """Build the control-flow graph of the function at `entry`, without
        lifting or optimising. Holds the GIL throughout."""
        ...
    def analyze(
        self,
        entry: int,
        cc: CallingConvention,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift, optimise and resolve the function at address `entry`.

        `cc` is required here; `ElfLifter.analyze` also takes a symbol name
        and supplies a default `cc`. A bad `entry` raises a `StriderError`
        naming the cause.

        Returns even when it cannot vouch for every path: `Cfg.incomplete`
        lists every address it cannot, and an empty `unresolved` is not a
        complete answer. Check `Cfg.incomplete` before trusting an absence
        ("no store to X"): code behind a listed address may be missing.

        Releases the GIL for the analysis unless `mem` or `rom` is a Python
        object, which needs it on every read. A daemon thread inside this call
        at interpreter shutdown aborts the process; analyse on non-daemon
        threads and join them.
        """
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """Decode from `entry` one instruction at a time until `addr`, and
        return that instruction's p-code (ops joined with `"; "`, empty for
        an instruction that lifts to none).

        Works for an address outside any analysed CFG. `addr` must be
        reachable through the linear instruction stream from `entry`; raises
        `StriderError` otherwise. On an ISA-mode arch
        (ARM, MIPS) `entry`'s low bit selects the mode, as for `analyze`, and
        the sweep starts at `entry & ~1`.

        Also raises once this `Lifter` has pinned more decode modes than the
        sweep can carry over, rather than answer in the wrong mode: sweep from
        a fresh `Lifter` instead.
        """
        ...

def lifter(
    arch: SleighArch,
    mem: MemLike,
    rom: Optional[RomLike] = ...,
) -> Lifter:
    """Create a lifter for `arch`. `mem` supplies the code bytes; `rom` is the
    optional read-only memory for constant folding."""
    ...
