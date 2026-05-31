"""Type stubs for the strider Python bindings.

These mirror the surface defined in crates/strider-py/src/.  Coverage
in v1 is not exhaustive — it covers the entry points that show up in
the README/usage examples.  Expand as the API grows.
"""

from __future__ import annotations

from typing import Any, Callable, ClassVar, Iterable, List, Optional, Tuple

__version__: str

# ── Architecture / calling convention ───────────────────────────────────

class SleighArch:
    @classmethod
    def x86_64(cls) -> SleighArch: ...
    @classmethod
    def x86(cls) -> SleighArch: ...
    @classmethod
    def mipsbe32(cls) -> SleighArch: ...
    @classmethod
    def mipsle32(cls) -> SleighArch: ...
    @classmethod
    def mipsbe64(cls) -> SleighArch: ...
    @classmethod
    def mipsle64(cls) -> SleighArch: ...
    @classmethod
    def arm(cls) -> SleighArch: ...
    @classmethod
    def arm_be(cls) -> SleighArch: ...
    @classmethod
    def arm_thumb(cls) -> SleighArch: ...
    @classmethod
    def aarch64(cls) -> SleighArch: ...
    @classmethod
    def aarch64be(cls) -> SleighArch: ...
    @classmethod
    def ppc32be(cls) -> SleighArch: ...
    @classmethod
    def ppc32le(cls) -> SleighArch: ...
    @classmethod
    def ppc64be(cls) -> SleighArch: ...
    @classmethod
    def ppc64le(cls) -> SleighArch: ...
    def name(self) -> str: ...

class CallingConvention:
    @classmethod
    def x86_64_systemv(cls) -> CallingConvention: ...
    @classmethod
    def aarch64_aapcs64(cls) -> CallingConvention: ...
    @classmethod
    def arm_aapcs(cls) -> CallingConvention: ...
    @classmethod
    def mips_o32(cls) -> CallingConvention: ...
    @classmethod
    def mips_n64(cls) -> CallingConvention: ...
    @classmethod
    def powerpc_sysv32(cls) -> CallingConvention: ...
    @classmethod
    def powerpc64_elf_v1(cls) -> CallingConvention: ...
    @classmethod
    def powerpc64_elf_v2(cls) -> CallingConvention: ...
    @classmethod
    def x86_cdecl(cls) -> CallingConvention: ...
    @classmethod
    def x86_64_all_preserving(cls) -> CallingConvention:
        """All-preserving x86_64 CC (every caller-clobbered register is
        listed callee-saved); a per-address override for transparent
        sites like `__fentry__` / `mcount`."""
        ...
    # Linux kernel presets.
    @classmethod
    def x86_linux_kernel(cls) -> CallingConvention: ...
    @classmethod
    def x86_64_linux_kernel(cls) -> CallingConvention: ...
    @classmethod
    def aarch64_linux_kernel(cls) -> CallingConvention: ...
    @classmethod
    def arm_linux_kernel(cls) -> CallingConvention: ...
    @classmethod
    def mips_linux_kernel_o32(cls) -> CallingConvention: ...
    @classmethod
    def mips_linux_kernel_n64(cls) -> CallingConvention: ...
    # Linux syscall ABIs.
    @classmethod
    def x86_linux_syscall(cls) -> CallingConvention: ...
    @classmethod
    def x86_64_linux_syscall(cls) -> CallingConvention: ...
    @classmethod
    def aarch64_linux_syscall(cls) -> CallingConvention: ...
    @classmethod
    def arm_linux_syscall(cls) -> CallingConvention: ...
    @classmethod
    def mips_linux_syscall_o32(cls) -> CallingConvention: ...
    @classmethod
    def mips_linux_syscall_n64(cls) -> CallingConvention: ...
    @classmethod
    def custom(
        cls,
        sleigh: "Sleigh",
        arg_passing_regs: list[str],
        callee_saved_regs: list[str],
        ret_val_regs: list[str],
        ret_val_regs_float: list[str],
        stack_pointer: str,
        stack_arg_offsets: list[int],
        ret_stack_pop: int,
        link_register: Optional[str] = ...,
        preserves_memory: bool = ...,
    ) -> CallingConvention: ...
    def name(self) -> str: ...

# ── Memory readers ──────────────────────────────────────────────────────

class MemoryMap:
    """Low-level reader of raw byte regions for non-ELF / custom-source
    / firmware-blob cases.  Implements both the sleigh fetch and
    ReadOnlyMemory roles internally; the same MemoryMap can serve `mem=`
    and `rom=` arguments to `strider.run`.

    For an ELF, prefer `strider.load(path)` → `Program`, which wires
    these readers up automatically and adds symbol/entry-point lookups.
    """
    def __init__(self) -> None: ...
    def add_region(self, start_addr: int, data: bytes) -> None: ...
    def set_endianness(self, endianness: str) -> None: ...
    def region_count(self) -> int: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class _LoadedElf:
    """Parsed ELF binary (internal; the user-facing face is `Program`).
    Built by `strider.load_elf(path)`; owns the parsed object(s) plus an
    internal raw `MemoryMap` assembled from the ELF sections.
    """
    def memory_map(self) -> MemoryMap: ...
    def symbol(self, name: str) -> int: ...
    def symbol_size(self, name: str) -> Optional[int]: ...
    def symbol_addr_and_size(self, name: str) -> tuple[int, Optional[int]]: ...
    def symbols(self) -> dict[str, int]: ...
    def entry_point(self) -> int: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...
    def endianness(self) -> str: ...
    def add_elf(self, path: str, apply_relocations: bool = ...) -> None: ...

def load_elf(path: str, apply_relocations: bool = ...) -> _LoadedElf:
    """Load an ELF binary into a `_LoadedElf` (the object `Program`
    wraps).  Prefer `strider.load(path)` for the high-level surface."""
    ...

class MemReader:
    """Subclass and override `read(addr, size) -> Optional[bytes]` to
    feed the analysis pipeline from a Python data source.  Each `read`
    crosses the Rust↔Python boundary; prefer `MemoryMap` for
    in-process bulk data.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class ReadOnlyMemory:
    """Subclass and override `read(addr, size) -> Optional[int]` to
    back a `LoadReadOnly` opt pass with Python data.  Returns up to 8
    bytes at `addr` decoded as a target-endian `int`; the implementation
    is responsible for byte order.  Returns `None` for unmapped
    addresses or sizes > 8.

    The pass only invokes `read` for RAM loads — non-RAM spaces
    (REGISTER, CONST, UNIQUE) are short-circuited by the adapter
    before reaching Python, so subclasses don't need to filter on
    space.
    """

    def __init__(self, *args: Any, **kwargs: Any) -> None: ...
    def read(self, addr: int, size: int) -> Optional[int]: ...

# ── Sleigh / CFG / Strider ──────────────────────────────────────────────

class VnSpace:
    """One of Sleigh's built-in address spaces."""
    @classmethod
    def ram(cls) -> VnSpace: ...
    @classmethod
    def register(cls) -> VnSpace: ...
    @classmethod
    def const_(cls) -> VnSpace: ...
    @classmethod
    def unique(cls) -> VnSpace: ...
    def name(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Vn:
    """A Sleigh varnode — `(space, offset, size_in_bytes)`."""
    def __init__(self, space: VnSpace, off: int, size: int) -> None: ...
    @property
    def space(self) -> VnSpace: ...
    @property
    def off(self) -> int: ...
    @property
    def size(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Sleigh:
    def __init__(self, arch: SleighArch, mem: Any) -> None: ...
    def arch_name(self) -> str: ...
    def reg(self, name: str) -> Optional[Vn]:
        """Look up a register by Sleigh name; `None` when not a register."""
        ...

class Cfg:
    def to_html(self, path: str, style: Optional[str] = ...) -> None: ...
    def to_dot(self, path: str) -> None: ...
    def html_str(self, style: Optional[str] = ...) -> str: ...

def build_cfg(
    sleigh: Sleigh,
    entry: int,
    allow_code_before_start_addr: bool = ...,
    function_max_size: Optional[int] = ...,
) -> Cfg: ...

class AnalyzeOutcome:
    function: Function
    unresolved_branch_count: int
    region_count: int

class Strider:
    def __init__(
        self, arch: SleighArch, sleigh: Sleigh, cc: CallingConvention
    ) -> None: ...
    def analyze_cfg(self, cfg: Cfg) -> AnalyzeOutcome: ...
    def build_optimizer_pipeline(self) -> OptimizerPipeline: ...
    def build_stable_optimizer_pipeline(self) -> OptimizerPipeline: ...
    def build_destructive_optimizer_pipeline(self) -> OptimizerPipeline: ...

class Node:
    """A handle on a single node in the IR graph.

    Returned by `Function.node(id)` and `Match.node(capture)`.  Lets you
    explore the sea-of-nodes IR beyond pattern matching: walk the
    data/control edges feeding a node (`inputs()`), read its kind
    (`kind()`), pull out constant values, and recover provenance
    (`fingerprint()`).  Snapshots the function generation at construction
    so a stale id (after `compact`/`optimize`) raises rather than
    dereferencing the wrong node."""

    @property
    def id(self) -> int:
        """The raw `u32` arena index of this node."""
        ...
    def kind(self) -> str:
        """The node's `NodeKind` formatted as a string."""
        ...
    def inputs(self) -> List[Node]:
        """The data/control nodes feeding this one, as a list of `Node`s."""
        ...
    def const_int(self) -> Optional[int]:
        """The node's unsigned integer constant value, else `None`."""
        ...
    def const_bool(self) -> Optional[bool]:
        """The node's boolean constant value, else `None`."""
        ...
    def fingerprint(self) -> List[int]:
        """Sorted, deduped asm-instruction addresses recorded on this node."""
        ...
    def wide_const_bytes(self) -> Optional[bytes]:
        """Raw LE bytes of an `IntConstWide` node, else `None`."""
        ...
    def call_other_name(self) -> Optional[str]:
        """Sleigh user-op name attached to a `CallOther` node, else `None`."""
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Function:
    def to_html(self, path: str, style: Optional[str] = ...) -> None: ...
    def to_dot(self, path: str) -> None: ...
    def html_str(self, style: Optional[str] = ...) -> str: ...
    def raw_dot_str(self) -> str: ...
    def raw_html_str(self) -> str: ...
    def to_raw_dot(self, path: str) -> None: ...
    def to_raw_html(self, path: str) -> None: ...
    def node_count(self) -> int: ...
    def count_regions(self) -> int:
        """Count of `Region` (control-flow join) nodes reachable from entry."""
        ...
    def node_ids(self) -> List[int]:
        """All reachable node ids as raw integers."""
        ...
    def node_kind(self, node_id: int) -> str:
        """The `NodeKind` of `node_id` formatted as a string."""
        ...
    def asm_fingerprint(self, node_id: int) -> List[int]:
        """Sorted, deduped asm-instruction addresses recorded on `node_id`."""
        ...
    def wide_const_bytes(self, node_id: int) -> Optional[bytes]:
        """Raw LE bytes of an `IntConstWide` node, else `None`."""
        ...
    def call_other_name(self, node_id: int) -> Optional[str]:
        """Sleigh user-op name attached to a `CallOther` node, else `None`."""
        ...
    def node(self, node_id: int) -> Node:
        """A discoverable `Node` handle on the node at `node_id`.  Raises
        `StriderError` for an invalid id."""
        ...
    def compact(self) -> None:
        """Drop every node unreachable from entry; invalidates node ids."""
        ...
    def validate(self) -> Optional[str]:
        """Re-validate the graph; `None` on success, else an error message."""
        ...
    def optimize(self, pipeline: OptimizerPipeline) -> None: ...
    def reoptimize(self, destructive: bool = ...) -> None: ...
    def find_all(
        self,
        pat: Any,  # strider.pattern.PatLike
        ignore_casts: bool = ...,
        ignore_regions: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
    ) -> List[Match]: ...
    def find_one(
        self,
        pat: Any,  # strider.pattern.PatLike
        ignore_casts: bool = ...,
        ignore_regions: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
    ) -> Optional[Match]:
        """Return the first `Match` for `pat`, or `None` if it does not
        match anywhere.  One-shot convenience over `find_all`."""
        ...
    def find_joined(
        self,
        pats: List[Any],  # list[strider.pattern.PatLike]
        ignore_casts: bool = ...,
        ignore_regions: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
    ) -> List[List[Match]]:
        """Run multiple patterns and return matched sets joined on shared
        `Capture`s — a cross-pattern join.  Each result is a tuple with
        one `Match` per input pattern (in input order) where every
        `Capture` shared between patterns binds to the same node."""
        ...
    def rewrite(self, find: Any, replace: Any) -> int: ...
    def rewrite_all(self, pairs: List[Tuple[Any, Any]]) -> int: ...

class Match:
    @property
    def root(self) -> int:
        """The root node where the top-level pattern matched, as a `u32`
        node id."""
        ...
    def uint(self, key: Any) -> Optional[int]: ...
    def int(self, key: Any) -> Optional[int]: ...
    def bool(self, key: Any) -> Optional[bool]: ...
    def float_bits(self, key: Any) -> Optional[int]: ...
    def has(self, key: Any) -> bool: ...
    def int_binary_op(self, key: Any) -> Optional[str]:
        """Recover the matched `IntBinaryOp` variant name from `key`."""
        ...
    def int_unary_op(self, key: Any) -> Optional[str]:
        """Recover the matched `IntUnaryOp` variant name from `key`."""
        ...
    def int_cmp_op(self, key: Any) -> Optional[str]:
        """Recover the matched `IntCmpOp` variant name from `key`."""
        ...
    def bool_binary_op(self, key: Any) -> Optional[str]:
        """Recover the matched boolean binary op (`IntBinaryOp` at `I1`) name."""
        ...
    # `bool_unary_op` was removed alongside `IntUnaryOp::BitNot`: a 1-bit
    # logical NOT is `Xor(_, IntConst(1)):I1`, so the op variant is
    # recovered via `bool_binary_op` (returns "Xor").
    def float_binary_op(self, key: Any) -> Optional[str]:
        """Recover the matched `FloatBinaryOp` variant name from `key`."""
        ...
    def float_unary_op(self, key: Any) -> Optional[str]:
        """Recover the matched `FloatUnaryOp` variant name from `key`."""
        ...
    def float_cmp_op(self, key: Any) -> Optional[str]:
        """Recover the matched `FloatCmpOp` variant name from `key`."""
        ...
    def vn(self, key: Any) -> Optional[Vn]:
        """Recover the varnode bound by `key` (InitialVar / tagged Phi /
        FunctionArg node), else `None`."""
        ...
    def asm_fingerprint(self, key: Any) -> List[int]: ...
    def node(self, key: Any) -> Optional[Node]:
        """A `Node` handle on the node bound to `key` (a `Capture` or
        string capture-name), or `None` when `key` is unbound."""
        ...
    def __getitem__(self, key: Any) -> Any: ...
    def __contains__(self, key: Any) -> bool: ...

class OptimizerPipeline:
    @classmethod
    def empty(cls) -> OptimizerPipeline: ...
    @classmethod
    def default(cls) -> OptimizerPipeline: ...
    @classmethod
    def stable_default(cls) -> OptimizerPipeline: ...
    @classmethod
    def destructive_default(cls) -> OptimizerPipeline: ...
    def add(self, pass_obj: Any) -> None: ...
    def add_post(self, pass_obj: Any) -> None: ...
    def pass_count(self) -> int: ...
    def post_pass_count(self) -> int: ...

class RunResult:
    cfg: Cfg
    function: Function
    sleigh: Sleigh

def run(
    arch: SleighArch,
    cc: CallingConvention,
    mem: Any,  # MemoryMap | MemReader subclass
    entry: int,
    rom: Optional[Any] = ...,  # MemoryMap | ReadOnlyMemory subclass
    pipeline: Optional[OptimizerPipeline] = ...,
    allow_code_before_start_addr: bool = ...,
    function_max_size: Optional[int] = ...,
    compact: bool = ...,
    per_address_ccs: Optional[dict[int, CallingConvention]] = ...,
) -> RunResult: ...

def pcode_at(
    arch: SleighArch,
    mem: MemoryMap,
    addr: int,
    count: int = ...,
) -> List[Tuple[int, str]]:
    """Lift the p-code of `count` machine instructions from `addr` over
    `mem`, returning `(insn_addr, text)` tuples in address order.  `text`
    is the instruction's p-code ops joined with `"; "` (empty for ops
    like `endbr64` that lift to no p-code).  rsleigh is a p-code lifter —
    this is the lifted semantics, NOT native assembly mnemonics.  Builds
    one Sleigh and decodes sequentially.  Raises `StriderError` on
    failure."""
    ...

def pcode_at_addrs(
    arch: SleighArch,
    mem: MemoryMap,
    addrs: List[int],
) -> List[Tuple[int, str]]:
    """Lift the p-code of a set of (possibly non-sequential) machine
    addresses, one instruction each, returning `(addr, text)` tuples in
    the order of `addrs`.  `text` is the instruction's p-code ops joined
    with `"; "` (empty for ops like `endbr64` that lift to no p-code).
    rsleigh is a p-code lifter — this is the lifted semantics, NOT native
    assembly mnemonics.  Builds the Sleigh only once."""
    ...

# ── High-level facade (strider._api) ─────────────────────────────────────

class Program:
    """The loaded ELF binary — `strider.load(path)` returns one.  Wires
    the code + ROM readers internally and exposes symbols, sizes, the
    entry point, raw reads, and `analyze()`."""
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
    def add_elf(self, path: str, *, apply_relocations: bool = ...) -> None: ...
    def pcode(self, addr: int, count: int = ...) -> List[Tuple[int, str]]:
        """Lift the p-code of `count` machine instructions from `addr`,
        returning `(insn_addr, text)` tuples in address order.  `text` is
        the instruction's p-code ops joined with `"; "` (empty for ops
        like `endbr64` that lift to no p-code).  rsleigh is a p-code
        lifter — this is the lifted semantics, NOT native assembly
        mnemonics."""
        ...
    def analyze(
        self,
        function: Any,  # str | int
        *,
        function_max_size: Optional[int] = ...,
        allow_code_before_start_addr: bool = ...,
        rom: Optional[Any] = ...,
    ) -> Analysis: ...
    def analyzer(
        self,
        *,
        allow_code_before_start_addr: bool = ...,
        function_max_size: Optional[int] = ...,
        rom: Optional[Any] = ...,  # MemoryMap | ReadOnlyMemory subclass
        compact: bool = ...,
        per_address_ccs: Optional[dict[int, CallingConvention]] = ...,
        pipeline_factory: Optional[Callable[[], OptimizerPipeline]] = ...,
    ) -> Analyzer:
        """Build a frozen, configure-once `Analyzer` over this Program —
        bundles the arch + cc + code reader + ELF symbol source once so
        `analyzer.analyze(target)` needs only the target function."""
        ...

class Analyzer:
    """A frozen configure-once analysis handle.  Bundles the arch +
    calling convention + code reader + a set of option defaults once;
    `analyze(target)` then needs only the target function (symbol name or
    address), and any frozen option can be overridden per call.

    Build one from a loaded ELF (`prog.analyzer(...)`) for symbol-name
    resolution, or standalone (`strider.analyzer(arch, cc, mem, ...)`)
    for a raw firmware blob / custom source."""
    @property
    def arch(self) -> SleighArch: ...
    @property
    def cc(self) -> CallingConvention: ...
    def pcode(self, addr: int, count: int = ...) -> List[Tuple[int, str]]:
        """Lift the p-code of `count` machine instructions from `addr`
        over this analyzer's memory + effective arch (parity with
        `Program.pcode`)."""
        ...
    def analyze(
        self,
        function: Any,  # str | int
        *,
        function_max_size: Any = ...,  # int | None override
        allow_code_before_start_addr: Any = ...,  # bool override
        rom: Any = ...,  # MemoryMap | ReadOnlyMemory | None override
        compact: Any = ...,  # bool override
        per_address_ccs: Any = ...,  # dict | None override
        pipeline: Any = ...,  # OptimizerPipeline override (one-off)
    ) -> Analysis:
        """Lift `function` (symbol name or address) into an `Analysis`
        using this analyzer's frozen config.  Each keyword, when passed,
        overrides the frozen default for this one call; `pipeline`
        overrides the frozen `pipeline_factory` for that call."""
        ...
    def __repr__(self) -> str: ...

class Analysis:
    """Wrapper around a `RunResult` — the lifted, optimized IR graph for
    a single function — with convenience methods for pattern queries and
    provenance lookup."""
    @property
    def function(self) -> Function: ...
    @property
    def cfg(self) -> Cfg: ...
    @property
    def sleigh(self) -> Sleigh: ...
    @property
    def entry(self) -> int: ...
    @property
    def name(self) -> Optional[str]: ...
    def find(self, pattern: Any, **matcher_options: Any) -> List[Match]: ...
    def find_one(
        self, pattern: Any, **matcher_options: Any
    ) -> Optional[Match]: ...
    def find_joined(
        self, patterns: List[Any], **matcher_options: Any
    ) -> List[List[Match]]: ...
    def fingerprint(self, node: Any) -> List[int]: ...
    def fingerprint_pcode(self, node: Any) -> List[Tuple[int, str]]:
        """The node's asm-fingerprint as `(addr, text)` pairs sorted by
        address, `text` being the lifted p-code (empty for ops like
        `endbr64` that lift to no p-code); `[]` for structural nodes with
        no fingerprint.  rsleigh is a p-code lifter — this is the lifted
        semantics, NOT native assembly mnemonics."""
        ...
    def dump_html(self, path: str, style: Optional[str] = ...) -> None: ...
    def dump_dot(self, path: str) -> None: ...

def load(
    path: str,
    *,
    arch: Optional[SleighArch] = ...,
    cc: Optional[CallingConvention] = ...,
    apply_relocations: bool = ...,
) -> Program:
    """Load an ELF binary and return a `Program` with the arch + calling
    convention auto-picked from the ELF header (override via `arch=` /
    `cc=` for kernel / syscall / custom-ABI workflows)."""
    ...

def analyzer(
    arch: SleighArch,
    cc: CallingConvention,
    mem: Any,  # MemoryMap | MemReader subclass
    *,
    rom: Optional[Any] = ...,  # MemoryMap | ReadOnlyMemory subclass
    allow_code_before_start_addr: bool = ...,
    function_max_size: Optional[int] = ...,
    compact: bool = ...,
    per_address_ccs: Optional[dict[int, CallingConvention]] = ...,
    pipeline_factory: Optional[Callable[[], OptimizerPipeline]] = ...,
) -> Analyzer:
    """Build a standalone (non-ELF / firmware) `Analyzer` over a raw code
    reader.  No ELF symbol source — only address targets are accepted;
    a name target raises a clear error."""
    ...

# ── Subpackages ────────────────────────────────────────────────────────

# Exception base class — also re-exported as strider.errors.StriderError.
class StriderError(Exception):
    """The single exception type raised by strider.  Every Rust error
    lands here carrying an informative message; the hierarchy is flat
    (no typed subclasses)."""
    ...
