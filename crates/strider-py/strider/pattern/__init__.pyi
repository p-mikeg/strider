"""Type stubs for strider.pattern: the match-side pattern DSL.

`Pat` is the left-hand side of a `find` or `rewrite` query. A rewrite
right-hand side (`replace=`) is a `strider.template.Template`: a `Pat` there
raises `TypeError`, and one nested in a `Template` operand raises
`StriderError`.

A pattern past the matcher's node cap, or nested deeper than the running
thread's stack allows, raises `StriderError`. A thread with a reduced
`stack_size` accepts less nesting than the main thread.
"""

from __future__ import annotations

from typing import (
    Callable,
    ClassVar,
    Generic,
    Literal,
    NoReturn,
    Optional,
    Protocol,
    Sequence,
    TypeVar,
    Union,
    runtime_checkable,
)

from ..ir import Node
from ..sleigh import Vn, VnSpace

from . import constraints as constraints
from .._api import FieldPat as FieldPat, code_ptr as code_ptr, field as field
from .constraints import ConstraintLike as _ConstraintLike

__all__: list[str]

#: Value-type names accepted by `Pat.value_ty` and returned by
#: `Node.value_type` / `Match.value_type`. Matched case-insensitively at
#: runtime; the readers emit the uppercase spelling.
ValueTy = Literal[
    "i1", "i8", "i16", "i24", "i32", "i40", "i48", "i56", "i64", "i72",
    "i80", "i96", "i112", "i128", "i256", "i512", "f16", "f32", "f64", "f80",
    "f128",
    "I1", "I8", "I16", "I24", "I32", "I40", "I48", "I56", "I64", "I72",
    "I80", "I96", "I112", "I128", "I256", "I512", "F16", "F32", "F64", "F80",
    "F128",
]
#: Every `ValueTy`, lowercase spellings then uppercase.
VALUE_TYPES: tuple[ValueTy, ...]

#: Integer-comparison names accepted by `int_cmp`, as `Match.op` reads them.
IntCmpOpName = Literal[
    "Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow",
]
#: Every `IntCmpOpName`, in the order `int_cmp` lists them.
INT_CMP_OPS: tuple[IntCmpOpName, ...]

#: Integer binary-op names accepted by `int_binary`: the `IntBinaryOp`
#: variants.
IntBinaryOpName = Literal[
    "Add", "And", "Or", "Xor", "Div", "Sdiv", "Rem", "Srem",
    "ShiftRight", "SShiftRight", "ShiftLeft", "Mul",
]
#: Every `IntBinaryOpName`, in the order `int_binary` lists them.
INT_BINARY_OPS: tuple[IntBinaryOpName, ...]

#: Boolean binary-op names accepted by `bool_binary`, the `I1` subset of
#: `IntBinaryOpName`.
BoolBinaryOpName = Literal[
    "And", "Or", "Xor",
]
#: Every `BoolBinaryOpName`, in the order `bool_binary` lists them.
BOOL_BINARY_OPS: tuple[BoolBinaryOpName, ...]

#: Float binary-op names accepted by `float_binary`: the `FloatBinaryOp`
#: variants.
FloatBinaryOpName = Literal[
    "Add", "Mul", "Div",
]
#: Every `FloatBinaryOpName`, in the order `float_binary` lists them.
FLOAT_BINARY_OPS: tuple[FloatBinaryOpName, ...]

#: Widening-operation names accepted by `int_extend`: the `ExtendOp` variants.
ExtendOpName = Literal[
    "ZeroExtend", "SignExtend",
]
#: Every `ExtendOpName`, in the order `int_extend` lists them.
EXTEND_OPS: tuple[ExtendOpName, ...]

class Match:
    """One result of a query: the nodes a pattern matched and what each
    capture bound to.

    Each typed reader (`op`, `uint`, `node`, ...) raises `StriderError`
    when `key` is unbound or its node lacks the requested aspect; the `_opt`
    counterpart returns `None` instead.

    Every reader, `_opt` ones included, raises once `compact`, `optimize`,
    `rewrite` or `rewrite_all` has run on the function.
    """

    @property
    def root(self) -> int:
        """The node id where the top-level pattern matched. For a joined
        (list) query, see `roots`. Raises when stale."""
        ...
    @property
    def roots(self) -> list[int]:
        """One root node id per pattern passed to the query (`[root]` for a
        single-pattern query). Raises when stale, as `root` does."""
        ...
    def uint(self, key: CaptureKey) -> int:
        """The unsigned constant value bound to `key`, at any declared width.
        Raises when unbound or not an integer-valued node."""
        ...
    def uint_opt(self, key: CaptureKey) -> Optional[int]:
        """`uint`, or `None` instead of raising."""
        ...
    def sint(self, key: CaptureKey) -> int:
        """The signed constant value bound to `key`, at any declared width.
        Raises when unbound or not an integer-valued node."""
        ...
    def sint_opt(self, key: CaptureKey) -> Optional[int]:
        """`sint`, or `None` instead of raising."""
        ...
    def boolean(self, key: CaptureKey) -> bool:
        """The boolean constant value bound to `key`. Raises when unbound or
        not a boolean-valued node."""
        ...
    def boolean_opt(self, key: CaptureKey) -> Optional[bool]:
        """`boolean`, or `None` instead of raising."""
        ...
    def float_bits(self, key: CaptureKey) -> int:
        """The raw float bit pattern bound to `key`. Raises when unbound or
        not a float-valued node."""
        ...
    def float_bits_opt(self, key: CaptureKey) -> Optional[int]:
        """`float_bits`, or `None` instead of raising."""
        ...
    def has(self, key: CaptureKey) -> bool:
        """Whether `key` bound to anything. Captures under an alternative
        that did not fire are left unbound."""
        ...
    def op(self, key: CaptureKey) -> str:
        """The operation variant of the node bound to `key` (`"Add"`,
        `"Less"`, `"Neg"`), for every op family. Raises when `key` is unbound
        or its node carries no operation."""
        ...
    def op_opt(self, key: CaptureKey) -> Optional[str]:
        """`op`, or `None` instead of raising."""
        ...
    def value_type(self, key: CaptureKey) -> str:
        """The value-output type of the node bound to `key` (`"I1"`, `"I64"`,
        `"F64"`): its first value output, whichever output `key` captured.
        Raises when `key` is unbound or its node has no value output."""
        ...
    def value_type_opt(self, key: CaptureKey) -> Optional[str]:
        """`value_type`, or `None` instead of raising."""
        ...
    def vn(self, key: CaptureKey) -> Vn:
        """The varnode the node bound by `key` names: an `InitialVar`'s
        entry-read varnode, the register a `Call` returns in, or a
        `CallOther`'s result varnode. Raises for any other kind, and for a
        result carrying no varnode. For a multi-output call, the first value
        output."""
        ...
    def vn_opt(self, key: CaptureKey) -> Optional[Vn]:
        """`vn`, or `None` instead of raising."""
        ...
    def asm_fingerprint(self, key: CaptureKey) -> list[int]:
        """Sorted, deduped machine-instruction addresses whose lift or later
        rewrite contributed to the node bound to `key`. `[]` when `key` is
        unbound, and possibly for a structural kind (`Entry`, `Region`,
        `Phi`, ...)."""
        ...
    def node(self, key: CaptureKey) -> Node:
        """A `Node` handle on what `key` bound to. Raises when `key` is
        unbound."""
        ...
    def node_opt(self, key: CaptureKey) -> Optional[Node]:
        """`node`, or `None` instead of raising."""
        ...
    def __getitem__(self, key: CaptureKey) -> "BoundCapture":
        """`m[c]` / `m["name"]`: capture `c` bound to this match; see
        `BoundCapture`."""
        ...
    def __contains__(self, key: CaptureKey) -> bool: ...

class BoundCapture:
    """A capture bound to one `Match`, from `m[c]` / `m["name"]`. `has` says
    whether it bound; an integer capture converts and compares directly, in
    signed or unsigned spelling (`int(m[c])`, `m[c] == 0x10`,
    `m[a] == m[b]`). Two non-integer captures are equal when they bound the
    same value of the same function; an unbound one equals nothing. The
    readers are `Match` methods."""

    @property
    def has(self) -> bool: ...
    def __int__(self) -> int: ...
    def __index__(self) -> int: ...
    def __bool__(self) -> NoReturn:
        """Raises `TypeError`: test `has` for whether it bound, `int()` for
        its value."""
        ...
    def __eq__(self, other: object) -> bool: ...
    __hash__: ClassVar[None]  # type: ignore[assignment]
    def __repr__(self) -> str: ...

class Capture:
    """A capture variable: attach it to a sub-pattern, then read the node it
    bound back off the `Match`."""
    def __init__(self, name: Optional[str] = ...) -> None:
        """A capture variable. `Capture()` is fresh and unique;
        `Capture("name")` interns the name, the same capture as a rewrite RHS
        `"name"`. A bare string is not a match-pattern operand. Reserved
        names (`"_"`, `"any_"`) raise."""
        ...

#: A `Capture` or its name: a `Match` key and the argument to `.capture()`,
#: not a pattern operand.
CaptureKey = Union[Capture, str]

class CastMask:
    """The value-passthrough casts the matcher walks through. Compose with
    `|`; pass as `Function.find_all(pat, ignore_casts=...)`."""
    @classmethod
    def zero_extend(cls) -> CastMask:
        """Zero-extending widenings only."""
        ...
    @classmethod
    def sign_extend(cls) -> CastMask:
        """Sign-extending widenings only."""
        ...
    @classmethod
    def extend(cls) -> CastMask:
        """Both widening kinds."""
        ...
    @classmethod
    def truncate(cls) -> CastMask:
        """Narrowings only."""
        ...
    @classmethod
    def int_bits_to_float(cls) -> CastMask:
        """Integer-bits-to-float reinterpretations only."""
        ...
    @classmethod
    def float_bits_to_int(cls) -> CastMask:
        """Float-bits-to-integer reinterpretations only."""
        ...
    @classmethod
    def all(cls) -> CastMask:
        """Every passthrough cast."""
        ...
    @classmethod
    def none(cls) -> CastMask:
        """No passthrough casts; the matcher walks through nothing."""
        ...
    def bits(self) -> int:
        """The mask as a raw bitset."""
        ...
    def __or__(self, other: CastMask) -> CastMask: ...
    def __and__(self, other: CastMask) -> CastMask: ...
    def __bool__(self) -> bool:
        """Whether any cast is selected."""
        ...
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...

class Pat(OrderedPat):
    """A finalised match pattern, the left-hand side of a `find` or
    `rewrite`. Chain `.capture`, `.when`, `.ordered`, `.of_width`,
    `.value_ty` and `.bool_valued`."""
    def capture(self, c: CaptureKey) -> Pat:
        """Bind the matched node to `c`, a `Capture` or a name."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Keep a match only when `f` returns `True` for the partial match so
        far; a rejected binding backtracks. A non-`bool` return is a
        `TypeError`. Raising aborts the query and re-raises out of
        `find_all` / `find_unique`. `f` may run on a partial match, more than
        once, or not at all."""
        ...
    def of_width(self, n: int) -> Pat:
        """Constrain this value's own output to `n` bits."""
        ...
    def value_ty(self, ty: ValueTy) -> Pat:
        """Constrain this value's own output type by name."""
        ...
    def bool_valued(self) -> Pat:
        """`of_width(1)`: a boolean output, comparisons included."""
        ...

#: A top-level pattern (`find_all`, `one_of`): a raw `int` (an `int_const`), a
#: `Capture`, a `Pat`, or any typed builder. Operand slots are narrower:
#: `ValueLike`, `MemLike`.
PatLike = Union[
    int,
    Capture,
    Pat,
    "CallPat",
    "CallOtherPat",
    "RetPat",
    "IfPat",
    "LoadPat",
    "StorePat",
    "PhiPat",
    "MemPhiPat",
    "EntryPat",
    "RegionPat",
    "IndirectBranchPat",
    "UnreachablePat",
    "SwitchPat",
    "FunctionArgPat",
    "IntBinaryPat",
    "FloatBinaryPat",
    "BoolBinaryPat",
]

#: What a value operand slot accepts. A builder producing no value raises at
#: query time.
ValueLike = Union[
    int,
    Capture,
    Pat,
    "CallPat",
    "CallOtherPat",
    "LoadPat",
    "PhiPat",
    "FunctionArgPat",
    "IntBinaryPat",
    "FloatBinaryPat",
    "BoolBinaryPat",
]

#: What a memory slot accepts: a memory producer, a wildcard (`anything()`,
#: `var(c)`, a `Capture`), or a `one_of` over them.
MemLike = Union[
    Capture,
    Pat,
    "StorePat",
    "MemPhiPat",
    "CallPat",
    "CallOtherPat",
]

#: A chaining builder method returns the same builder type.
_S = TypeVar("_S", bound="NodePat")

#: The same, for a mixin `Pat` carries too.
_P = TypeVar("_P", bound="OrderedPat")

#: The builder an `OutputSlotPat` terminal hands back.
_B = TypeVar("_B", bound="NodePat")

@runtime_checkable
class NodePat(Protocol):
    """A node-pattern builder: captures, guards, and seals into a `Pat`."""
    def capture(self: _S, c: CaptureKey) -> _S:
        """Bind the matched node to `c`, a `Capture` or a name."""
        ...
    def when(self: _S, f: Callable[[Match], bool]) -> _S:
        """Keep a match only when `f` returns `True`; a rejected binding
        backtracks. A non-`bool` return is a `TypeError`. Raising aborts the
        query and re-raises out of `find_all` / `find_unique`. `f` may run on
        a partial match, more than once, or not at all."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a reusable `Pat`. A wrong-typed operand raises here; one
        that can never match its slot raises at the first query. Rarely
        needed: a builder auto-finalises where a pattern is expected."""
        ...

@runtime_checkable
class InputPat(Protocol):
    """A builder whose node kind exposes raw input slots."""
    def input(self: _S, idx: int, p: PatLike) -> _S:
        """Match `p` against raw input slot `idx`: `Call` is `[ctrl, mem,
        target, sp, arg0, ...]`, `Load` `[mem, addr]`, `If` `[ctrl, cond]`.

        A typed value sub-pattern binds only a value edge; `var` /
        `anything` any edge; a control producer (`entry()`, `region()`, ...)
        its control edge; a `call()` / `call_other()` any of its outputs
        (`.res()` keeps it to the value).
        """
        ...
    def any_input(self: _S, p: PatLike) -> _S:
        """Require some input of the node to match `p`, without pinning a
        slot; binding follows `input`. Repeatable, each call a distinct
        slot."""
        ...

@runtime_checkable
class CtrlPat(Protocol):
    """A builder whose node kind has a control-predecessor input."""
    def ctrl(self: _S, p: PatLike) -> _S:
        """Match `p` against the node's direct ctrl predecessor
        (`inputs[0]`)."""
        ...

@runtime_checkable
class MemPat(Protocol):
    """A builder whose node kind has a memory input."""
    def mem(self: _S, p: MemLike) -> _S:
        """Match `p` against the node's memory predecessor; takes a memory
        producer (`store` / `mem_phi` / `call` / `call_other`)."""
        ...

@runtime_checkable
class MemAccessPat(Protocol):
    """A builder whose node kind addresses memory: `load()` and `store()`.

    `stack_offset`, `stack_only`, `non_stack` and `heap_only` set one region
    filter; the last one called wins, except that `stack_only` keeps an
    offset an earlier `stack_offset` pinned.
    """
    def addr(self: _S, p: ValueLike) -> _S:
        """Constrain the address operand (`inputs[1]`)."""
        ...
    def bit_width(self: _S, n: int) -> _S:
        """Filter by accessed-value width in bits."""
        ...
    def space(self: _S, s: VnSpace) -> _S:
        """Restrict the match to a specific memory space."""
        ...
    def stack_offset(self: _S, k: int) -> _S:
        """Match only accesses whose address decomposes to exactly
        `sp + k`."""
        ...
    def stack_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a stack base."""
        ...
    def non_stack(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base or is
        proven not memory-rooted; an undecided address is rejected."""
        ...
    def heap_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base (a
        pure allocator's return pointer)."""
        ...

@runtime_checkable
class OrderedPat(Protocol):
    """A pattern whose root takes an operand pair `ordered` can pin."""
    def ordered(self: _P) -> _P:
        """Stop the matcher retrying this op with its operands swapped. Pins
        this node alone (for `int_ne` / `float_ne`, the `Equal` under the
        `Xor`). On a `Pat` with no operands to order (a wildcard, a constant,
        a `one_of`) it raises."""
        ...

@runtime_checkable
class OutputPat(Protocol):
    """A builder whose node kind exposes raw output slots."""
    def output(self: _S, slot: int) -> OutputSlotPat[_S]:
        """Bind or constrain the value at raw output `slot` (a `Call` is
        `[ctrl, mem, result, ...clobbers]`, a `Load` `[value]`)."""
        ...
    def any_output(self: _S) -> OutputSlotPat[_S]:
        """Like `output`, over some output rather than a fixed slot."""
        ...

class OutputSlotPat(Generic[_B]):
    """One output slot of a builder, returned by `.output(slot)` /
    `.any_output()`."""
    def capture(self, c: CaptureKey) -> _B:
        """Bind this output slot to `c`."""
        ...
    def of_width(self, bits: int) -> _B:
        """Require this output to be `bits` wide."""
        ...
    def of_type(self, ty: ValueTy) -> _B:
        """Require this output's type to be `ty`."""
        ...

#: Alias of `OutputSlotPat`, the terminal `CallPat.output(slot)` returns.
CallOutputPat = OutputSlotPat

class IntBinaryPat(NodePat, OrderedPat):
    """Builder for `int_binary(op, l, r)`: the named `IntBinaryOp` variant."""
class FloatBinaryPat(NodePat, OrderedPat):
    """Builder for `float_binary(op, l, r)`: the named `FloatBinaryOp` variant."""
class BoolBinaryPat(NodePat, OrderedPat):
    """Builder for `bool_binary(op, l, r)`: the named integer op on `I1`
    operands."""
class CallPat(NodePat, InputPat, CtrlPat, MemPat, OutputPat):
    """Builder for `Call` node patterns, returned by `call()`.

    Inputs are `[ctrl, mem, target, sp, arg0, ...]`, outputs `[ctrl, mem,
    result, ...clobbers]`.
    """
    def target(self, p: Union[ValueLike, list[ValueLike]]) -> "CallPat":
        """Constrain the call target. A raw int matches that literal address;
        a list matches any one entry, an empty list nothing."""
        ...
    def arg(self, idx: int, p: ValueLike) -> "CallPat":
        """Constrain positional argument `idx` (0-based, raw input slot
        `idx + 4`)."""
        ...
    def res(self) -> "CallPat":
        """When nested as a value operand, pin it to the declared result
        output, excluding caller-saved clobbers."""
        ...

class CallOtherPat(NodePat, InputPat, CtrlPat, MemPat, OutputPat):
    """Builder for `CallOther` (Sleigh user-op) node patterns, returned by
    `call_other()`.

    Inputs are `[ctrl, mem, arg0, ...]`, outputs `[ctrl, mem, result,
    ...clobbers]`.
    """
    def user_op_id(self, v: int) -> "CallOtherPat":
        """Constrain the matched node's user-op id."""
        ...
    def name(self, n: str) -> "CallOtherPat":
        """Constrain the matched node's user-op name."""
        ...
    def arg(self, idx: int, p: ValueLike) -> "CallOtherPat":
        """Constrain raw `inputs[idx]`, unshifted."""
        ...
    def res(self) -> "CallOtherPat":
        """When nested as a value operand, pin it to the declared result
        output, excluding implicit-write clobbers."""
        ...

class RetPat(NodePat, InputPat, CtrlPat):
    """Builder for `Return` node patterns, returned by `ret()`.

    Inputs are `[ctrl, mem, retval0, ...]`; no outputs.
    """
    def ret_val(self, idx: int, p: ValueLike) -> "RetPat":
        """Constrain returned value `idx` (0-based, raw input slot
        `idx + 2`)."""
        ...

class IfPat(NodePat, InputPat, CtrlPat, OutputPat):
    """Builder for two-way branch (`If`) patterns, returned by `if_else()`.

    Inputs are `[ctrl, cond]`, outputs `[true, false]`, both control edges.
    """
    def cond(self, p: ValueLike) -> "IfPat":
        """Constrain the branch condition (`inputs[1]`)."""
        ...
    def capture_true(self, c: Capture) -> "IfPat":
        """Bind the true control-output value to `c`, the edge operand
        `dominates` / `phi_input_from_edge` take, as in
        `find_all([if_else().capture_true(c), region().any_input(c)])`."""
        ...
    def capture_false(self, c: Capture) -> "IfPat":
        """Bind the false control-output value to `c`. See
        `capture_true`."""
        ...

class LoadPat(NodePat, InputPat, MemPat, MemAccessPat, OutputPat):
    """Builder for `Load` node patterns, returned by `load()`.

    Inputs are `[mem, addr]`, the one output the loaded value.
    """

class StorePat(NodePat, InputPat, MemPat, MemAccessPat, OutputPat):
    """Builder for `Store` node patterns, returned by `store()`.

    Inputs are `[mem, addr, data]`, the one output the new memory token.
    """
    def data(self, p: ValueLike) -> "StorePat":
        """Constrain the stored value (`inputs[2]`)."""
        ...

class PhiPat(NodePat, InputPat, OutputPat):
    """Builder for value-phi patterns, returned by `phi()` /
    `phi_for(vn)`.

    Raw input slot 0 is the phi-token edge from the owning `Region`, so
    predecessor `i`'s value sits at raw slot `i + 1`. `phi_input` indexes
    predecessors, `input` the raw slots.
    """
    def for_vn(self, vn: Vn) -> "PhiPat":
        """Require the phi to be tagged `vn` or a register containing it, so
        `eax` matches a phi tagged `rax`."""
        ...
    def phi_input(self, idx: int, p: ValueLike) -> "PhiPat":
        """Constrain the value merged from predecessor `idx`, raw input slot
        `idx + 1`."""
        ...
    def phi_token(self, p: PatLike) -> "PhiPat":
        """Constrain the region token tying this phi to its merge point
        (raw `inputs[0]`). Only `var` / `anything` binds it."""
        ...
    def input_from(self, edge: CaptureKey, value: Union[ValueLike, CaptureKey]) -> "PhiPat":
        """Merge `value` in from the branch edge `edge` binds (an `if_else`
        `capture_true` / `capture_false`). A pattern `value` becomes an input;
        a `Capture` names one another pattern binds. Pass `constraints()` to
        the query."""
        ...
    def constraints(self) -> list[_ConstraintLike]:
        """One `phi_input_from_edge` per `input_from`, on this phi's current
        capture."""
        ...

class MemPhiPat(NodePat, InputPat, OutputPat):
    """Builder for memory-token phi patterns, returned by `mem_phi()`.

    Same slot layout as `PhiPat`, with a memory token per predecessor.
    """
    def phi_input(self, idx: int, p: MemLike) -> "MemPhiPat":
        """Constrain the memory token merged from predecessor `idx`, raw
        input slot `idx + 1`. Takes a memory producer."""
        ...
    def phi_token(self, p: PatLike) -> "MemPhiPat":
        """Constrain the region token tying this phi to its merge point
        (raw `inputs[0]`)."""
        ...
    def input_from(self, edge: CaptureKey, value: Union[MemLike, CaptureKey]) -> "MemPhiPat":
        """Merge `value` in from the branch edge `edge` binds. A memory
        producer `value` becomes an input; a `Capture` names one another
        pattern binds. Pass `constraints()` to the query."""
        ...
    def constraints(self) -> list[_ConstraintLike]:
        """One `phi_input_from_edge` per `input_from`, on this phi's current
        capture."""
        ...

class EntryPat(NodePat, OutputPat):
    """Builder for the function's unique `Entry` node, returned by
    `entry()`. Nests as a control operand, e.g.
    `region().any_input(entry())`."""

class RegionPat(NodePat, InputPat, OutputPat):
    """Builder for control-flow merge (`Region`) patterns, returned by
    `region()`.

    Input `i` is predecessor `i`'s control edge, bound only by a
    control-rooted sub-pattern (`entry()` / `region()`) or a wildcard.
    """

class IndirectBranchPat(NodePat, InputPat, CtrlPat, MemPat):
    """Builder for unresolved indirect-branch patterns, returned by
    `indirect_branch()`.

    Inputs are `[ctrl, mem, target]` plus the optional interworking ISA
    mode; no outputs.
    """
    def target(self, p: Union[ValueLike, list[ValueLike]]) -> "IndirectBranchPat":
        """Constrain the dispatch target (`inputs[2]`). A raw int matches that
        literal address; a list matches any one entry, an empty list
        nothing."""
        ...

class UnreachablePat(NodePat, InputPat, CtrlPat):
    """Builder for `Unreachable` (no-return sink) patterns, returned by
    `unreachable()`.

    Inputs are `[ctrl]` plus an optional memory edge; there are no outputs.
    """

class SwitchPat(NodePat, InputPat, CtrlPat, OutputPat):
    """Builder for multi-way dispatch (`Switch`) patterns, returned by
    `switch()`.

    Inputs are `[ctrl, selector]`, outputs one control edge per arm.
    """
    def selector(self, p: Union[ValueLike, list[ValueLike]]) -> "SwitchPat":
        """The value the switch dispatches on (`inputs[1]`). A raw int matches
        that literal value; a list matches any one entry, an empty list
        nothing."""
        ...

class FunctionArgPat(NodePat):
    """Builder for incoming-function-argument patterns, returned by
    `function_arg(i)`, `function_arg_float(i)`, `any_function_arg()`,
    `function_arg_reg(vn)` and `function_arg_stack(space, offset)`.

    Integer and float arguments are numbered separately: `function_arg(0)` is
    the first integer argument, `function_arg_float(0)` the first float one.
    """
    def index(self, i: int) -> "FunctionArgPat":
        """Require the argument's position to be `i`."""
        ...
    def source_register(self, vn: Vn) -> "FunctionArgPat":
        """Require the argument to arrive in register `vn` or one containing
        it."""
        ...
    def source_stack(self, space: VnSpace, offset: int) -> "FunctionArgPat":
        """Require the argument to arrive on the stack at `(space,
        offset)`."""
        ...

def anything() -> Pat:
    """Wildcard: matches any node without binding it."""
def var(c: Capture) -> Pat:
    """Wildcard that binds the matched node to capture `c`."""
def predicate(f: Callable[[Match], bool]) -> Pat:
    """`anything().when(f)`."""
def value_of_width(n: int) -> Pat:
    """Match any value whose own output is exactly `n` bits wide (width 1
    includes comparisons)."""
def inputs_of_width(n: int, inner: ValueLike) -> Pat:
    """Match `inner` and require all of its value inputs to be `n` bits wide.
    `inner` needs at least one value input and one value output, so a
    constant or a sink never matches."""
def bool_inputs(inner: ValueLike) -> Pat:
    """`inputs_of_width(1, inner)`."""
def int_const(value: int | list[int] | Capture | None = ...) -> Pat:
    """Match an integer constant equal to `value` (or any member of a list;
    an empty list matches nothing), both wrapped to the constant's width:
    `0x1234` matches `0x34` at `i8`, `-1` the all-ones constant of any
    width. A value wider than 512 bits raises. Given a `Capture`, or
    nothing, match any integer constant, binding it if given."""
def int_const_any_width(value: int | list[int]) -> Pat:
    """Match an integer constant holding `value` exactly, zero-extended or
    sign-extended (or any member of a list). `value` must fit the matched
    width: `0x1234` never matches `0x34` at `i8`. A value outside the signed
    64-bit range raises."""
def bool_const(value: bool | Capture | None = ...) -> Pat:
    """Match a 1-bit boolean constant equal to `value`. Given a `Capture`, or
    nothing, match any boolean constant, binding it if given."""
def float_const(bits: int | Capture | None = ...) -> Pat:
    """Match a float constant whose raw 64-bit pattern equals `bits`. Given a
    `Capture`, or nothing, match any float constant, binding it if given."""
def any_int(c: Capture | None = ...) -> Pat:
    """Match any node with an integer output (`I1` through `I512`, so
    booleans too), optionally binding it to `c`."""
def any_bool(c: Capture | None = ...) -> Pat:
    """Match any node with a 1-bit (`I1`) output, comparisons included,
    optionally binding it to `c`."""
def any_float(c: Capture | None = ...) -> Pat:
    """Match any node with a float output, optionally binding it to `c`."""
def initial_var() -> Pat:
    """Match any initial-state register read."""
def initial_var_for(vn: Vn) -> Pat:
    """Match the initial-state read of `vn` or of any register containing it,
    so `eax` matches a node tagged `rax`."""
def one_of(patterns: Sequence[PatLike]) -> Pat:
    """Match if any listed sub-pattern matches (a logical OR). Nests in any
    slot. Match-only; no alternatives matches nothing.

    A union, not an ordered choice: every matching arm fires with its own
    bindings, so a wildcard arm does not shadow a narrower one (use
    `first_of` to cut). Arms matching one node with the same bindings are one
    match. A capture under an arm that did not fire stays unbound:
    `off = h.uint(o) if h.has(o) else 0`.
    """
def first_of(patterns: Sequence[PatLike]) -> Pat:
    """Match the first listed sub-pattern that matches (an ordered OR).

    Like `one_of`, but at each value it is offered the first arm that yields
    a match wins and the rest are dropped. Each read of a `first_of` settles
    on its own; several settle in pattern order (operands left to right,
    inner before outer). Order arms most-specific first: `anything()` and
    `var(c)` match any node::

        # WRONG: var(base) also matches the Add, so `off` never binds and
        # every `base + K` load looks like a bare `base`.
        load(addr=first_of([var(base), int_add(var(base), int_const(off))]))

        # RIGHT: specific shape first, bare fallback last.
        load(addr=first_of([int_add(var(base), int_const(off)), var(base)]))

    No alternatives matches nothing; match-only.
    """
def function_arg(i: int) -> FunctionArgPat:
    """Start a function-argument pattern at integer argument index `i`."""
def function_arg_float(i: int) -> FunctionArgPat:
    """Start a function-argument pattern constrained to float argument
    index `i`, counting only float parameters."""
def any_function_arg() -> FunctionArgPat:
    """Start a function-argument pattern matching any argument index of
    either class."""
def function_arg_reg(vn: Vn) -> FunctionArgPat:
    """Match a function argument arriving in register `vn` or one containing
    it."""
def function_arg_stack(space: VnSpace, offset: int) -> FunctionArgPat:
    """Match a function argument arriving on the stack at `(space,
    offset)`."""
def phi() -> PhiPat:
    """Start a value-phi pattern builder."""
def phi_for(vn: Vn) -> PhiPat:
    """Start a value-phi pattern builder for `vn` or a register containing it."""
def mem_phi() -> MemPhiPat:
    """Start a memory-token phi pattern builder."""
def entry() -> EntryPat:
    """Match the function's unique `Entry` node. Nests as a control operand,
    e.g. `region().any_input(entry())`."""
def region() -> RegionPat:
    """Match any control-flow merge (`Region`) node."""
def indirect_branch() -> IndirectBranchPat:
    """Start an unresolved indirect-jump pattern builder."""
def unreachable() -> UnreachablePat:
    """Start an `Unreachable` terminator pattern builder."""
def switch() -> SwitchPat:
    """Start a resolved multi-way dispatch pattern builder."""

def int_add(l: ValueLike, r: ValueLike) -> Pat:
    """Match integer addition. Commutative."""
def int_sub(l: ValueLike, r: ValueLike) -> Pat:
    """Match integer subtraction. A constant `r` also matches the folded `int_add(l, -r)`."""
def int_mul(l: ValueLike, r: ValueLike) -> Pat:
    """Match integer multiplication. Commutative."""
def int_div(l: ValueLike, r: ValueLike) -> Pat:
    """Match unsigned integer division."""
def int_sdiv(l: ValueLike, r: ValueLike) -> Pat:
    """Match signed integer division."""
def int_rem(l: ValueLike, r: ValueLike) -> Pat:
    """Match unsigned integer remainder."""
def int_srem(l: ValueLike, r: ValueLike) -> Pat:
    """Match signed integer remainder."""
def int_shl(l: ValueLike, r: ValueLike) -> Pat:
    """Match a left shift."""
def int_shr(l: ValueLike, r: ValueLike) -> Pat:
    """Match a logical (zero-filling) right shift."""
def int_sshr(l: ValueLike, r: ValueLike) -> Pat:
    """Match an arithmetic (sign-filling) right shift."""
def int_and(l: ValueLike, r: ValueLike) -> Pat:
    """Match a bitwise AND. Commutative."""
def int_or(l: ValueLike, r: ValueLike) -> Pat:
    """Match a bitwise OR. Commutative."""
def int_xor(l: ValueLike, r: ValueLike) -> Pat:
    """Match a bitwise XOR. Commutative."""
def int_cmp(op: IntCmpOpName, l: ValueLike, r: ValueLike) -> Pat:
    """Match a named integer comparison, e.g. `"Equal"`, `"Less"`,
    `"Sless"`, `"Carry"`, `"Scarry"`, `"Sborrow"`."""
def int_eq(l: ValueLike, r: ValueLike) -> Pat:
    """Match an integer equality test. Commutative."""
def int_ne(l: ValueLike, r: ValueLike) -> Pat:
    """Match an integer inequality test, the lifter's `Xor(Equal(l, r), 1)`.
    `.ordered()` pins the `Equal`."""
def int_lt(l: ValueLike, r: ValueLike) -> Pat:
    """Match an unsigned less-than test."""
def int_le(l: ValueLike, r: ValueLike) -> Pat:
    """Match an unsigned less-or-equal test, the lifter's
    `Xor(Less(r, l), 1)`."""
def int_slt(l: ValueLike, r: ValueLike) -> Pat:
    """Match a signed less-than test."""
def int_sle(l: ValueLike, r: ValueLike) -> Pat:
    """Match a signed less-or-equal test, the lifter's `Xor(Sless(r, l), 1)`."""
def int_carry(l: ValueLike, r: ValueLike) -> Pat:
    """Match an unsigned-addition carry test. Commutative."""
def int_scarry(l: ValueLike, r: ValueLike) -> Pat:
    """Match a signed-addition overflow test. Commutative."""
def int_sborrow(l: ValueLike, r: ValueLike) -> Pat:
    """Match a signed-subtraction overflow test."""

def int_neg(operand: ValueLike) -> Pat:
    """Match arithmetic negation (`-x`)."""
def int_not(operand: ValueLike) -> Pat:
    """Match bitwise complement (`~x`)."""

def bool_and(l: ValueLike, r: ValueLike) -> Pat:
    """Match a logical AND on 1-bit values. Commutative."""
def bool_or(l: ValueLike, r: ValueLike) -> Pat:
    """Match a logical OR on 1-bit values. Commutative."""
def bool_xor(l: ValueLike, r: ValueLike) -> Pat:
    """Match a logical XOR on 1-bit values. Commutative."""
def bool_not(operand: ValueLike) -> Pat:
    """Match a logical NOT on a 1-bit value."""

def float_add(l: ValueLike, r: ValueLike) -> Pat:
    """Match float addition. Commutative."""
def float_sub(l: ValueLike, r: ValueLike) -> Pat:
    """Match float subtraction. A `float_const` `r` also matches the folded `float_add(l, -r)`."""
def float_mul(l: ValueLike, r: ValueLike) -> Pat:
    """Match float multiplication. Commutative."""
def float_div(l: ValueLike, r: ValueLike) -> Pat:
    """Match float division."""
def float_neg(operand: ValueLike) -> Pat:
    """Match float negation."""
def float_abs(operand: ValueLike) -> Pat:
    """Match float absolute value."""
def float_sqrt(operand: ValueLike) -> Pat:
    """Match a float square root."""
def float_ceil(operand: ValueLike) -> Pat:
    """Match a round-toward-positive-infinity."""
def float_floor(operand: ValueLike) -> Pat:
    """Match a round-toward-negative-infinity."""
def float_round(operand: ValueLike) -> Pat:
    """Match a round to nearest, ties away from zero."""
def float_is_nan(operand: ValueLike) -> Pat:
    """Match a NaN test, the IEEE 754 self-inequality `x != x`."""
def float_eq(l: ValueLike, r: ValueLike) -> Pat:
    """Match a float equality test. Commutative."""
def float_ne(l: ValueLike, r: ValueLike) -> Pat:
    """Match a float inequality test, the lifter's
    `Xor(FloatEqual(l, r), 1)`. `.ordered()` pins the `FloatEqual`."""
def float_lt(l: ValueLike, r: ValueLike) -> Pat:
    """Match a float less-than test."""
def float_le(l: ValueLike, r: ValueLike) -> Pat:
    """Match a float less-or-equal test, NaN-aware."""

def int_to_float(operand: ValueLike) -> Pat:
    """Match a signed-integer conversion to the nearest representable float."""
def float_to_int(operand: ValueLike) -> Pat:
    """Match a float-to-integer conversion, truncating toward zero."""
def float_to_float(operand: ValueLike) -> Pat:
    """Match a float-to-float reprecision."""
def int_bits_to_float(operand: ValueLike) -> Pat:
    """Match a same-width reinterpretation of integer bits as a float."""
def float_bits_to_int(operand: ValueLike) -> Pat:
    """Match a same-width reinterpretation of float bits as an integer."""

def int_truncate(operand: ValueLike) -> Pat:
    """Match a narrowing to the output width, keeping the low bits."""
def int_popcount(operand: ValueLike) -> Pat:
    """Match a set-bit count."""
def int_lzcount(operand: ValueLike) -> Pat:
    """Match a leading-zero count."""
def int_zero_extend(operand: ValueLike) -> Pat:
    """Match a widening that fills the new high bits with zero."""
def int_sign_extend(operand: ValueLike) -> Pat:
    """Match a widening that replicates the sign bit."""
def int_extend(op: ExtendOpName, operand: ValueLike) -> Pat:
    """Match a widening of the kind named by `op`: `"ZeroExtend"` or
    `"SignExtend"`."""

def load(addr: ValueLike | None = ...) -> LoadPat:
    """Start a `Load` pattern builder, optionally pinning the address."""
def store(addr: ValueLike | None = ..., data: ValueLike | None = ...) -> StorePat:
    """Start a `Store` pattern builder, optionally pinning the address and
    the stored value."""
def call() -> CallPat:
    """Start a `Call` pattern builder."""
def call_other() -> CallOtherPat:
    """Start a `CallOther` (architecture-specific user operation) pattern
    builder."""
def ret() -> RetPat:
    """Start a `Return` pattern builder."""
def if_else(cond: ValueLike | None = ...) -> IfPat:
    """Start a conditional-branch pattern builder, optionally pinning the
    condition."""

def int_binary(op: IntBinaryOpName, l: ValueLike, r: ValueLike) -> IntBinaryPat:
    """Match a named integer binary op (`"Add"`, `"ShiftLeft"`, `"And"`),
    returning a chainable builder."""
def bool_binary(op: BoolBinaryOpName, l: ValueLike, r: ValueLike) -> BoolBinaryPat:
    """Match a named integer op at `I1` (`"And"`, `"Or"`, `"Xor"`),
    commutatively, returning a chainable builder."""
def float_binary(op: FloatBinaryOpName, l: ValueLike, r: ValueLike) -> FloatBinaryPat:
    """Match a named float binary op (`"Add"`, `"Mul"`, `"Div"`), returning
    a chainable builder."""

def any_int_binary(c: Capture, l: ValueLike, r: ValueLike) -> Pat:
    """Match any integer binary op with these operands, binding the node to
    `c` (read the variant with `Match.op(c)`)."""
def any_int_unary(c: Capture, operand: ValueLike) -> Pat:
    """Match any `IntUnaryOp` on `operand`, binding the node to `c`. The enum
    holds only `Neg`; `Popcount` and `Lzcount` are their own kinds."""
def any_int_cmp(c: Capture, l: ValueLike, r: ValueLike) -> Pat:
    """Match any integer comparison with these operands, binding the node to
    `c`."""
def any_bool_binary(c: Capture, l: ValueLike, r: ValueLike) -> Pat:
    """Match any `IntBinaryOp` at `I1` with these operands, binding the node
    to `c`. A logical NOT is `Xor(x, 1)`, so it matches here."""
def any_float_binary(c: Capture, l: ValueLike, r: ValueLike) -> Pat:
    """Match any float binary op with these operands, binding the node to
    `c`."""
def any_float_unary(c: Capture, operand: ValueLike) -> Pat:
    """Match any float unary op on `operand`, binding the node to `c`."""
def any_float_cmp(c: Capture, l: ValueLike, r: ValueLike) -> Pat:
    """Match any float comparison with these operands, binding the node to
    `c`."""
