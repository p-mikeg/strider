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

    None is checked, and each one's risky value is the positive one, so any of
    them can make the answer wrong on valid input. `stack_global_disjoint`,
    `assume_incoming_args_survive_calls`,
    `return_slots_survive_untracked_writes`, `msr_register_keeps_mode` and
    `fp_rounding_mode_is_nearest` default `True`, the other four off or empty;
    clearing all nine is the sound floor under any input whose memory
    behaves as RAM and whose floating-point controls are the IEEE default
    (see `none`). What each one claims is in
    `docs/python-api.md` section 2.

    Claims accumulate on a `Function` (`Function.assumptions`): `analyze`
    starts from the ones it is given, `Function.optimize` adds, none withdraws.
    The lift reads `msr_register_keeps_mode` and `fp_rounding_mode_is_nearest`,
    so `optimize` refuses to add either: analyse again with the claim.
    """

    # Read-only: these are getters with no setter, so a plain attribute
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
    def noalias_allocators(self) -> list[int]:
        """Callee addresses of pure `malloc`-like allocators, sorted. Each
        listed call's result overlaps no other live allocation, no stack slot
        and no global or other constant address, so an allocator carving from
        a static or a caller-supplied arena does not qualify."""
        ...
    @property
    def escape_analysis(self) -> bool: ...
    @property
    def return_slots_survive_untracked_writes(self) -> bool: ...
    @property
    def msr_register_keeps_mode(self) -> bool: ...
    @property
    def fp_rounding_mode_is_nearest(self) -> bool:
        """The SSE MXCSR.RC, x87 RC (for `fst` and `fstp`), PowerPC FPSCR.RN,
        ARM FPSCR.RMode, AArch64 FPCR.RMode and MIPS FCSR.RM rounding modes
        are round to nearest even at every conversion that rounds by them,
        narrowing and integer-to-float ones included. Off, each reads the mode
        and selects among its four; the fixed-mode forms (`cvtt*`, `fcti*z`,
        ARM `vcvt` to an integer, MIPS `round`, `trunc`, `floor`, `ceil`,
        AArch64 `fcvt{n,a,m,p,z}{s,u}` and `fcvtxn`) read none, and x87
        `fist` reads RC either way. Float arithmetic rounds to nearest either
        way."""
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
        """Build the claims; a field omitted or `None` takes its default, as
        documented above. `noalias_allocators` reads back sorted, without
        duplicates."""
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
        sound floor of these fields under any input, forwarding solely what
        the IR structurally proves.

        Five of the nine default `True`, so this is not
        `AssumptionOptions()`, and `AssumptionOptions(escape_analysis=True)`
        holds six claims where `none(escape_analysis=True)` holds one. Add
        one claim to a function's claims with
        `function.assumptions | AssumptionOptions.none(escape_analysis=True)`.

        Memory is still taken to be RAM: a load reads back the last value the
        analysed code stored at its address, with nothing outside that code
        (a device, another thread, a signal handler) changing it in between.
        A memory-mapped register polled after a write reads as the value
        written. The floating-point environment outside the conversion
        rounding mode `fp_rounding_mode_is_nearest` claims is still taken to
        be the IEEE default: float arithmetic rounds to nearest, and x86 MXCSR.FTZ and DAZ, PowerPC FPSCR.NI, OE and UE, ARM
        FPSCR.FZ, AArch64 FPCR.FZ and MIPS FCSR.FS are clear, so
        `-ffast-math` code, whose startup sets FTZ and DAZ, has no floor
        here. No option lifts either premise.
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
    claims about the analysed code, each of which can make the answer wrong;
    when it is not passed, `analyze` takes the defaults.
    `compact` drops unreachable nodes at the end. `per_address_ccs` overrides
    the calling convention for a callee, keyed by the direct-call target
    address rather than the call site: on ppc64 ELFv2 a call sharing the
    callee's TOC targets `Symbol.local_entry`, which only `ElfLifter.analyze`
    adds for a key at `Symbol.address`. `pipeline`, when set,
    replaces the default optimizer pipeline for the calls these options drive.
    """

    # Read-only: these are getters with no setter, so a plain attribute
    # declaration would let a type checker accept `o.x = ...`, which raises at runtime.
    @property
    def cfg(self) -> CfgOptions: ...
    @property
    def assumptions(self) -> AssumptionOptions: ...
    @property
    def compact(self) -> bool: ...
    # A read-only VIEW onto what the constructor stored, built once and handed
    # out unchanged. Unlike the `CfgOptions` tables, it is not shared with a
    # clone: `_with_cfg` resets it.
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
    def _with_cfg(self, cfg: CfgOptions) -> LifterOptions:
        """A copy of these options with `cfg` replaced."""
        ...
    def _per_address_ccs_table_id(self) -> Optional[int]:
        """The address of the `per_address_ccs` table an analyse resolves,
        or `None` without one."""
        ...
    def _with_elf(self, elf: object) -> LifterOptions:
        """These options as `ElfLifter.analyze` runs them on `elf`: its
        mapping-symbol data ranges unless `cfg.data_ranges` names some, and
        the local entry of every `no_return_targets` address and
        `per_address_ccs` key that is a ppc64 ELFv2 global entry."""
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
        """INTERNAL, for tests. Rebuild the handle's Sleigh and orchestrator
        state."""
        ...
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
    @property
    def arch(self) -> SleighArch:
        """The `SleighArch` this handle decodes with.

        Enough, with `reader()` and `rom()`, to build a second handle over the
        same memory, which is how two threads analyse in parallel; read all
        three before an `analyze`, `build_cfg` or `add_elf` on this handle is
        in flight, since they raise while one is."""
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
        lifting or optimising.

        Holds the GIL for the whole build, unlike `analyze`: decoding is short
        next to a full analysis."""
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

        A bad `entry` raises a `StriderError` naming the cause, as `build_cfg`
        does for the same argument.

        Returns even when it cannot vouch for every path: `Cfg.incomplete`
        lists every address it cannot, and an empty `unresolved` is not a
        complete answer. Check `Cfg.incomplete` before trusting an absence
        ("no store to X"): code behind a listed address may be missing.

        Runs the fixed-point loop with the GIL released, so other Python
        threads keep running -- except where `mem` or `rom` is a Python object,
        whose adapter takes the GIL back on every read anyway; there the GIL is
        held, so finalization waits for the analysis the way any other Python
        call does.

        A DAEMON thread inside this call when the interpreter starts
        finalizing aborts the process either way. With the GIL released it is
        killed holding no GIL, and the forced unwind out of the released
        region aborts. With a Python `mem` or `rom` it is terminated inside
        your `read` callback, and the unwind through the Rust frames below it
        aborts or segfaults. CPython hangs a pure-Python daemon thread safely
        instead, so this is a cost of the Rust frames and no exception can be
        raised for it. Analyse on non-daemon threads and join them, which is
        clean.
        """
        ...
    def pcode_at(self, entry: int, addr: int) -> str:
        """Decode from `entry` one instruction at a time until `addr`, and
        return that instruction's p-code (ops joined with `"; "`, empty for
        an instruction that lifts to none).

        A stand-alone sweep, so it works for an address outside any analysed
        CFG. `addr` must be reachable through the linear instruction stream
        from `entry`; raises `StriderError` otherwise.

        The sweep decodes on a copy of the engine, which carries over the
        decode modes the analyses pinned. Past the engine's commit limit it
        cannot, and this raises `StriderError` rather than answering `addr` in
        the pspec default mode: sweep from a fresh `Lifter` instead.
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
        """Byte order of the loaded binary, as the first ELF's `EI_DATA`
        gives it: `"little"` or `"big"`. Read off the ELF, so it answers while
        a call holds the handle."""
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
        """The `Symbol` named `name`: its definition in the first ELF that
        defines it, else an undefined symbol that still has an address (a PLT
        stub, or an object file's extern). Raises `StriderError` when there is
        neither."""
        ...
    def symbol_opt(self, name: str) -> Optional[Symbol]:
        """`symbol`, but `None` rather than raising when `name` is
        undefined."""
        ...
    def symbol_at(self, address: int) -> Optional[Symbol]:
        """The symbol covering `address`: the nearest one at or below it whose
        recorded extent reaches `address`. A symbol with no recorded size
        covers only its own address. Aliases sharing an address are ranked by
        having a recorded extent first, then by being code, so a sized DATA
        alias outranks an unsized CODE one; `functions()` filters to code
        instead. `None` when nothing covers `address`."""
        ...
    def symbols(self) -> Mapping[str, Symbol]:
        """Every symbol, keyed by the name it resolves under."""
        ...
    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        ...
    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr`, or `None` when `addr` is unmapped or
        inside a relocation this loader does not compute. In writable,
        non-executable data such a field is not cut out but serves the file's
        bytes, which the loader overwrites. `rom()` serves no writable data,
        except `.got` / `.got2` / `.data.rel.ro` under `relocated_got_is_rom`,
        where such a field is unmapped."""
        ...
    def reader(self) -> BufferReader:
        """The `BufferReader` over the regions `load_elf` mapped: PT_LOAD segments
        unless `from_segments=False`. The base `Lifter.reader`, narrowed."""
        ...
    def add_elf(
        self,
        path: StrPath,
        *,
        apply_relocations: bool = True,
        relocated_got_is_rom: bool = False,
    ) -> None:
        """Merge another ELF, such as a shared library, into this handle.
        The earlier-loaded ELF wins on name collisions.

        `apply_relocations` defaults to `True`, matching `load_elf`: a merged
        ELF is normally the ET_DYN case relocations exist for, and the flag
        also selects what is mapped, so `False` drops writable non-executable
        sections rather than serving their on-disk bytes, and serves every
        relocation field still mapped as its file bytes, as in `load_elf`.
        With it on, an image whose relocation section headers name more than
        twice the entries the file holds at its smallest entry, or whose code
        a dynamic relocation table strider cannot read or decode may relocate,
        raises `StriderError`, as in `load_elf`.

        `relocated_got_is_rom` is `load_elf`'s unchecked claim, made for THIS
        image only: it does not inherit the handle's, since one shared
        object's link says nothing about another's. It contradicts
        `apply_relocations=False` here too, and the pair raises `StriderError`.

        Raises `StriderError` if the new ELF's architecture, class or byte
        order, or on ARM its `EF_ARM_BE8`, differs from the first image's (one
        handle decodes one target), or if it maps code over an address already
        loaded with different bytes (two ELFs at the same base cannot merge),
        and `ValueError` for a microMIPS image, which no preset decodes."""
        ...
    def add_symbol_file(self, path: StrPath) -> None:
        """Take the symbols of `path` and none of its bytes.

        How a separate debug or symbol file attaches after loading: a
        `objcopy --only-keep-debug` companion and distro debuginfo are linked
        at the same addresses as the image they describe, so `add_elf` would
        refuse them as an overlap. Both `.symtab` and `.dynsym` are read, and
        the already-loaded ELFs keep a colliding name."""
        ...
    def add_symbols(
        self,
        symbols: dict[str, Union[int, tuple[int, int]]],
        *,
        is_function: bool = ...,
    ) -> None:
        """Add symbols directly, for names that live in no ELF: a map file, a
        kernel `System.map`, or your own naming. Each value is an address, or
        an `(address, size)` pair when the extent is known, which is what lets
        `symbol_at` resolve an interior address. An ELF already carrying a name
        keeps its own answer."""
        ...
    def build_cfg(
        self,
        entry: int,
        opts: Optional[CfgOptions] = ...,
    ) -> Cfg:
        """`Lifter.build_cfg`, with the options `analyze` derives from the
        ELF: its `$d` mapping symbols as `data_ranges` unless `opts` names
        some, and the local entry of every `no_return_targets` address that
        is a ppc64 ELFv2 symbol's `address`. `entry` is an address, and no
        symbol size bounds the build, as for `analyze(int)`."""
        ...
    def analyze(
        self,
        entry: Union[int, str],
        cc: Optional[CallingConvention] = ...,
        opts: Optional[LifterOptions] = ...,
    ) -> AnalyzeResult:
        """Lift the function at `entry`, a symbol name or an address. `cc`
        defaults to this handle's convention. A name with an ELF size bounds
        the lift to that size unless `opts.cfg.function_max_size` is set, and
        the bound drops `allow_code_before_start_addr`. Unless `opts.cfg.data_ranges`
        names some, the ELF's `$d` mapping symbols supply them, so a literal
        pool is never decoded as code. A `no_return_targets` address or
        `per_address_ccs` key that is a ppc64 ELFv2 symbol's `address` also
        covers its `Symbol.local_entry`, where a call sharing its TOC lands.

        Releases the GIL as `Lifter.analyze` does, and a daemon thread inside
        it at interpreter finalization aborts the process as it does there."""
        ...
    def __repr__(self) -> str: ...

def load_elf(
    path: StrPath,
    *,
    from_segments: bool = ...,
    apply_relocations: bool = ...,
    relocated_got_is_rom: bool = ...,
    arch: Optional[SleighArch] = ...,
    cc: Optional[CallingConvention] = ...,
) -> ElfLifter:
    """Load an ELF binary, picking the architecture and calling convention
    from its header, and return an `ElfLifter`.

    `from_segments=True` (the default) walks PT_LOAD program headers, falling
    back to sections for relocatable objects; `from_segments=False` forces
    the section walk for section-granular regions. Override the detected
    `arch=` / `cc=` for kernel or custom-ABI workflows. `arch=` alone keeps
    the header's convention, and `analyze` raises `StriderError` when that
    convention's stack geometry is another arch's.

    `apply_relocations` (default `True`) applies relocations, which shared
    objects and PIE binaries need; it also selects what is mapped, so `False`
    drops writable non-executable sections rather than serving their on-disk
    bytes. A writable-and-executable mapping is kept either way. With it off,
    every relocation field still mapped (in code, a `.rodata` table, a TEXTREL
    image's instructions) serves its file bytes: the addend or the link-time
    value, not the relocated one. With it on, a linked image is relocated from
    the tables its dynamic tags place, as the loader reads them, Android
    packed ones included, whatever its section headers say. An image whose
    relocation tables name more than twice the entries the file holds at their
    smallest entry raises `StriderError`, and so does one whose code
    (`DT_TEXTREL`, or a writable-and-executable mapping) a table the tags
    place but strider cannot read or decode may relocate. Any other image with
    such a table serves none of its writable mappings.

    `relocated_got_is_rom` (default `False`) additionally serves `.got`
    (PPC32's `.got2` too) and `.data.rel.ro` as READ-ONLY memory, so a load
    through a GOT slot folds to the address the slot holds. It is what
    resolves a PIC jump table whose base comes out of the GOT. PPC32 code
    that relocates its own `.got2` (Linux's `prom_init` `reloc_got2`,
    `-mrelocatable` `.fixup`) breaks the claim.

    An image with a table its dynamic tags place that strider cannot read or
    decode serves neither.

    A slot whose load-time value only the dynamic loader computes stays
    unmapped, and a load through it does not fold: one under a relocation
    strider does not compute, such as one against an imported symbol the
    image gives no address; an IFUNC's (`R_*_IRELATIVE`, or a relocation
    against a defined `STT_GNU_IFUNC` symbol), which holds what the resolver
    returns; every byte of an `R_*_COPY` object, which the loader copies from
    the library defining it; the two GOT header words ARM and secure-PLT
    PPC32 lazy binding writes (`DT_PLTGOT + 4` / `+ 8`, `DT_PPC_GOT + 4` /
    `+ 8`); and on MIPS, whose primary GOT no relocation covers, the
    lazy-resolver word, the module-pointer word when its top bit asks for
    one, and every global entry except those for symbols the image defines
    that already hold their address. `.got.plt` is never served, lazy binding
    rewriting those slots on the first call through each one.

    Inside an executable mapping a relocation against a defined IFUNC takes
    the symbol's own address instead, so the instruction still decodes. An
    address taken in code (`lea f(%rip)`, `mov $f`), or a data word in an
    executable mapping (FreeBSD 12's `.rodata`), then folds to the symbol,
    where at run time it holds what the resolver returns.

    The rest is an unchecked claim about the runtime: that nothing rewrites a
    slot after the loader has run, and that a symbol the image defines binds
    to that definition. Where either is false (a program writing its own GOT;
    a copy relocation, `LD_PRELOAD` or a `dlopen`ed library interposing the
    symbol) the fold is wrong too. It contradicts `apply_relocations=False`,
    which says the loader has NOT run, and the pair raises `StriderError`.

    Raises `FileNotFoundError` when `path` does not exist, and `ValueError`
    when the file is not an ELF, or its architecture or ABI is unsupported,
    a microMIPS image among them. Passing both `arch=` and `cc=` skips the
    detector, and with it the ABI and microMIPS refusals.
    """
    ...
