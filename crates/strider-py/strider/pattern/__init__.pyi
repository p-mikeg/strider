"""Type stubs for strider.pattern: the match-side pattern DSL.

`Pat` is the left-hand side of a `find` or `rewrite` query, with the
match-only affordances `.when()`, commutativity, `.of_width()`,
`.value_ty()` and the wildcards. For a rewrite right-hand side (`replace=`)
use `strider.template` and its `Template` type instead. A bare `Pat` is
still accepted there for compatibility, but only its build-valid subset
compiles: `.when`, wildcards and commutativity are rejected.
"""

from __future__ import annotations

from typing import (
    Callable,
    Generic,
    Literal,
    Optional,
    Protocol,
    Sequence,
    TypeVar,
    Union,
    runtime_checkable,
)

from ..ir import Node
from ..sleigh import Vn, VnSpace

# A pattern describes graph shape; a constraint is a relational predicate
# over the captures patterns bind. Separate namespaces so the two kinds
# cannot be mistaken for one another.
from . import constraints as constraints

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

#: Integer-comparison names accepted by `int_cmp`. The short aliases `"eq"`,
#: `"lt"` and `"slt"` are equivalent to `"Equal"`, `"Less"` and `"Sless"`.
#: Matched case-insensitively at runtime.
IntCmpOpName = Literal[
    "Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow", "eq", "lt", "slt",
]

#: Widening-operation names accepted by `int_extend`.
ExtendOpName = Literal[
    "zero", "zero_extend", "ZeroExtend", "sign", "sign_extend", "SignExtend",
]

class Match:
    """One result of a query: the nodes a pattern matched and what each
    capture bound to.

    Each typed reader (`op`, `uint`, `node`, ...) raises `StriderError`
    when `key` is unbound or its node lacks the requested aspect; the `_opt`
    counterpart returns `None` there instead. Guard with `has(key)` (or catch
    the error) when a capture may be unbound.
    """

    @property
    def root(self) -> int:
        """The node id where the top-level pattern matched. For a joined
        (list) query, see `roots`. Raises once `compact` / `optimize` has
        invalidated every outstanding id."""
        ...
    @property
    def roots(self) -> list[int]:
        """One root node id per pattern passed to the query (`[root]` for a
        single-pattern query). Raises when stale, as `root` does."""
        ...
    def uint(self, key: CaptureKey) -> int:
        """The unsigned constant value bound to `key`. Raises when unbound or
        not an integer-valued node."""
        ...
    def uint_opt(self, key: CaptureKey) -> Optional[int]:
        """`uint`, or `None` instead of raising."""
        ...
    def sint(self, key: CaptureKey) -> int:
        """The signed constant value bound to `key`. Raises when unbound or
        not an integer-valued node."""
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
        `"Less"`, `"Neg"`). Raises when `key` is unbound or its node carries
        no operation.

        One accessor covers every op family (integer and float; binary,
        unary and compare). Pair it with `value_type` to tell a boolean op
        (`Xor` at `I1`) from a wide bitwise one.
        """
        ...
    def op_opt(self, key: CaptureKey) -> Optional[str]:
        """`op`, or `None` instead of raising."""
        ...
    def value_type(self, key: CaptureKey) -> str:
        """The value-output type of the node bound to `key` (`"I1"`, `"I64"`,
        `"F64"`). Raises when `key` is unbound or its node has no value
        output."""
        ...
    def value_type_opt(self, key: CaptureKey) -> Optional[str]:
        """`value_type`, or `None` instead of raising."""
        ...
    def vn(self, key: CaptureKey) -> Vn:
        """The varnode the node bound by `key` names: an initial register
        read, the register a `Call` returns in, or whatever varnode a
        `CallOther`'s result lands in. Raises when the node names none, which
        includes most `CallOther`s.
        A capture binds a node, so a call with several clobber outputs answers
        for its first value output, not for one clobber in particular."""
        ...
    def vn_opt(self, key: CaptureKey) -> Optional[Vn]:
        """`vn`, or `None` instead of raising."""
        ...
    def asm_fingerprint(self, key: CaptureKey) -> list[int]:
        """The machine-instruction addresses recorded on the node bound to
        `key`; `[]` when `key` is unbound."""
        ...
    def node(self, key: CaptureKey) -> Node:
        """A `Node` handle on what `key` bound to (`key` is a `Capture` or a
        capture name). Raises when `key` is unbound. Every other reader here
        is built on this."""
        ...
    def node_opt(self, key: CaptureKey) -> Optional[Node]:
        """`node`, or `None` instead of raising."""
        ...
    def __getitem__(self, key: CaptureKey) -> "BoundCapture":
        """`m[c]` / `m["name"]`: capture `c` bound to this match, carrying the
        same readers as `Match` without repeating the capture."""
        ...
    def __contains__(self, key: CaptureKey) -> bool: ...

class BoundCapture:
    """A capture bound to one `Match`, from `m[c]` / `m["name"]`. Each reader
    mirrors the `Match` reader of the same name: the plain one raises when the
    capture is unbound or its node lacks that aspect, the `_opt` one returns
    `None`. A numeric capture also converts and compares directly (`int(m[c])`,
    `m[c] == 0x10`)."""

    has: bool
    uint: int
    uint_opt: Optional[int]
    sint: int
    sint_opt: Optional[int]
    boolean: bool
    boolean_opt: Optional[bool]
    float_bits: int
    float_bits_opt: Optional[int]
    op: str
    op_opt: Optional[str]
    value_type: str
    value_type_opt: Optional[str]
    vn: Vn
    vn_opt: Optional[Vn]
    node: Node
    node_opt: Optional[Node]
    asm_fingerprint: list[int]
    def __int__(self) -> int: ...
    def __index__(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __repr__(self) -> str: ...

class Capture:
    """A capture variable: attach it to a sub-pattern, then read the node it
    bound back off the `Match`."""
    def __init__(self, name: Optional[str] = ...) -> None:
        """A capture variable. `Capture()` is fresh and unique;
        `Capture("name")` interns the name, so it is the SAME capture as a
        rewrite RHS `"name"` and reads back with `match.uint("name")`. A bare
        string is not a match-pattern operand. Reserved names (`"_"`,
        `"any_"`) raise."""
        ...

#: Names which capture is meant, by the `Capture` itself or by its name.
#: A read-back key on a `Match` and the argument to `.capture()`; NOT a
#: pattern operand, where a bare string has no meaning.
CaptureKey = Union[Capture, str]

class CastMask:
    """Selects which value-passthrough casts the matcher walks through
    transparently. Compose with `|`; pass as
    `Function.find_all(pat, ignore_casts=...)`."""
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
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...

class Pat(OrderedPat):
    """A finalised match pattern, the left-hand side of a `find` or
    `rewrite`.

    Built by the free constructors below and chained via `.capture`,
    `.when`, `.ordered`, `.of_width`, `.value_ty` and `.bool_valued`.
    For a rewrite right-hand side, build a `strider.template.Template`
    instead.
    """
    def capture(self, c: CaptureKey) -> Pat:
        """Bind the matched node to `c`, a `Capture` or a name."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Keep a match only when `f` returns true for the partial match so
        far. A rejected binding backtracks, so other bindings are still
        tried. Raising is not a rejection: it aborts the whole query and
        re-raises out of `find_all` / `find_unique`."""
        ...
    def of_width(self, n: int) -> Pat:
        """Constrain this value's own output to `n` bits. The free-function
        form is `value_of_width(n)`. For the input side, see
        `inputs_of_width(n, inner)`."""
        ...
    def value_ty(self, ty: ValueTy) -> Pat:
        """Constrain this value's own output type by name."""
        ...
    def bool_valued(self) -> Pat:
        """Constrain this value's output to a boolean (1-bit `I1`). Exactly
        `of_width(1)`, named for intent. This INCLUDES comparisons; for
        "operates on booleans", see `bool_inputs(inner)`."""
        ...

#: What a sub-pattern slot accepts: a raw `int` (an `int_const`), a `Capture`, a
#: finished `Pat`, or any of the typed builders below (auto-finalised at the call
#: site, so an explicit `.into_pat()` is never required).
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

#: A builder method that chains returns the SAME builder, so the type is
#: threaded through rather than widened to the protocol.
_S = TypeVar("_S", bound="NodePat")

#: The same, for a mixin `Pat` carries too: `Pat` is the finished pattern,
#: not a builder.
_P = TypeVar("_P", bound="OrderedPat")

#: The builder an `OutputSlotPat` terminal hands back.
_B = TypeVar("_B", bound="NodePat")

@runtime_checkable
class NodePat(Protocol):
    """A node-pattern builder: captures, guards, and seals into a `Pat`.

    Every builder below is one. `Pat` is not: it is the finished pattern, so
    it has no `into_pat`.
    """
    def capture(self: _S, c: CaptureKey) -> _S:
        """Bind the matched node to `c`, a `Capture` or a name."""
        ...
    def when(self: _S, f: Callable[[Match], bool]) -> _S:
        """Keep a match only when `f` returns true. A rejected binding
        backtracks, so other bindings are still tried. Raising is not a
        rejection: it aborts the whole query and re-raises out of `find_all` /
        `find_unique`."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

@runtime_checkable
class InputPat(Protocol):
    """A builder whose node kind has input slots. Every builder but
    `EntryPat`, whose `Entry` is `inputs: []`."""
    def input(self: _S, idx: int, p: PatLike) -> _S:
        """Match `p` against raw input slot `idx`.

        Slot 0 is not uniform across kinds: `Call` is `[ctrl, mem, target,
        sp, arg0, ...]`, `Load` is `[mem, addr]`, `If` is `[ctrl, cond]`.
        The IR's `expected_signature` (`strider-ir/src/node_signature.rs`)
        is the source of truth. This is the escape hatch beneath the named
        accessors, not a replacement for them; `PhiPat.phi_input` and
        `MemPhiPat.phi_input` are the predecessor-indexed spelling.

        What a slot holds decides what can bind it: only an untyped wildcard
        (`var` / `anything`) reaches a Control, memory or phi-token edge,
        never a typed value sub-pattern.
        """
        ...
    def any_input(self: _S, p: PatLike) -> _S:
        """Require SOME input of the node to match `p`, without pinning a
        slot. A typed sub binds an input of its own kind; `var` / `anything`
        also reaches the control, memory and phi-token edges. Repeatable,
        each call taking a distinct slot."""
        ...

@runtime_checkable
class CtrlPat(Protocol):
    """A builder whose node kind has a control-predecessor input."""
    def ctrl(self: _S, p: PatLike) -> _S:
        """Match `p` against the node's direct ctrl predecessor
        (`inputs[0]`). The sub-pattern's root produces a control edge, so a
        typed value pattern never binds it."""
        ...

@runtime_checkable
class MemPat(Protocol):
    """A builder whose node kind has a memory input."""
    def mem(self: _S, p: PatLike) -> _S:
        """Match `p` against the node's memory predecessor; takes a memory
        producer (`store` / `mem_phi` / `call` / `call_other`)."""
        ...

@runtime_checkable
class MemAccessPat(Protocol):
    """A builder whose node kind addresses memory: `load()` and `store()`.

    The memory predecessor itself is `MemPat.mem`; this is the address side
    and the filters over what the address decomposes to.
    """
    def addr(self: _S, p: PatLike) -> _S:
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
        """Reject matches where the SP-relative offset is unknown."""
        ...
    def non_stack(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base or is
        proven not memory-rooted. An address whose class is unknown is
        rejected, not kept."""
        ...
    def heap_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base (a
        pure allocator's return pointer)."""
        ...

@runtime_checkable
class OrderedPat(Protocol):
    """A pattern over a commutative op, whose operand order `ordered` pins."""
    def ordered(self: _P) -> _P:
        """Stop the matcher retrying this op with its operands swapped; a
        no-op where the op's operands are ordered already, as on `int_le` or
        `int_shl`. Pins this node alone, so `int_add(int_mul(a, b),
        c).ordered()` leaves the inner `int_mul` commuting. A shape with no
        operands to order (a wildcard, a constant, a `one_of`) raises."""
        ...

@runtime_checkable
class OutputPat(Protocol):
    """A builder whose node kind has output slots. Every builder but the
    three sinks `RetPat`, `IndirectBranchPat` and `UnreachablePat`, all
    `outputs: []`."""
    def output(self: _S, slot: int) -> OutputSlotPat[_S]:
        """Bind or constrain the value at raw output `slot`.

        Slot numbering is per kind and asymmetric with the inputs: a `Call`
        is `[ctrl, mem, result, ...clobbers]` while a `Load` is `[value]`.
        The IR's `expected_signature` (`strider-ir/src/node_signature.rs`)
        is the source of truth.

        This names the output value itself; it does not recurse into
        whatever consumes that output.
        """
        ...
    def any_output(self: _S) -> OutputSlotPat[_S]:
        """Some output rather than a fixed slot; otherwise `output`."""
        ...

class OutputSlotPat(Generic[_B]):
    """One output slot of a builder, returned by `.output(slot)` /
    `.any_output()`. Each method records its constraint and returns the
    parent builder for further chaining."""
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
    """Builder for `bool_binary(op, l, r)`: the named boolean binary op.

    Boolean ops are the integer ones pinned to `I1` operands, so this
    matches `int_and` / `int_or` / `int_xor` on 1-bit values.
    """
class CallPat(NodePat, InputPat, CtrlPat, MemPat, OutputPat):
    """Builder for `Call` node patterns, returned by `call()`.

    Inputs are `[ctrl, mem, target, sp, arg0, ...]`, outputs `[ctrl, mem,
    result, ...clobbers]`.
    """
    def target(self, p: Union[PatLike, list[PatLike]]) -> "CallPat":
        """Constrain the call target. `p` is any pattern operand, including
        a raw int, which matches a call to that literal address. A list of
        them matches a call to any one entry; an empty list matches
        nothing."""
        ...
    def arg(self, idx: int, p: PatLike) -> "CallPat":
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
    def arg(self, idx: int, p: PatLike) -> "CallOtherPat":
        """Constrain raw `inputs[idx]`, unshifted."""
        ...
    def res(self) -> "CallOtherPat":
        """When nested as a value operand, pin it to the declared result
        output, excluding implicit-write clobbers."""
        ...

class RetPat(NodePat, InputPat, CtrlPat):
    """Builder for `Return` node patterns, returned by `ret()`.

    Inputs are `[ctrl, mem, retval0, ...]`; a `Return` has no outputs, so
    the pattern is rooted on the node itself.
    """
    def ret_val(self, idx: int, p: PatLike) -> "RetPat":
        """Constrain returned value `idx` (0-based, raw input slot
        `idx + 2`)."""
        ...

class IfPat(NodePat, InputPat, CtrlPat, OutputPat):
    """Builder for two-way branch (`If`) patterns, returned by `if_else()`.

    Inputs are `[ctrl, cond]`, outputs `[true, false]`, both control edges.
    """
    def cond(self, p: PatLike) -> "IfPat":
        """Constrain the branch condition (`inputs[1]`)."""
        ...
    def true_branch(self, p: PatLike) -> "IfPat":
        """Match `p` against the unique consumer of the true edge."""
        ...
    def false_branch(self, p: PatLike) -> "IfPat":
        """Match `p` against the unique consumer of the false edge."""
        ...
    def capture_true(self, c: Capture) -> "IfPat":
        """Bind the true control-output value to `c`, the edge operand
        `dominates` / `phi_input_from_edge` take."""
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
    def data(self, p: PatLike) -> "StorePat":
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
        """Require the phi to carry varnode `vn`."""
        ...
    def phi_input(self, idx: int, p: PatLike) -> "PhiPat":
        """Constrain the value merged from predecessor `idx`, raw input slot
        `idx + 1`."""
        ...
    def phi_token(self, p: PatLike) -> "PhiPat":
        """Constrain the region token tying this phi to its merge point
        (raw `inputs[0]`), which `input(0, p)` also names."""
        ...

class MemPhiPat(NodePat, InputPat, OutputPat):
    """Builder for memory-token phi patterns, returned by `mem_phi()`.

    Same slot layout as `PhiPat`, with a memory token per predecessor.
    """
    def phi_input(self, idx: int, p: PatLike) -> "MemPhiPat":
        """Constrain the memory token merged from predecessor `idx`, raw
        input slot `idx + 1`. Takes a memory producer."""
        ...
    def phi_token(self, p: PatLike) -> "MemPhiPat":
        """Constrain the region token tying this phi to its merge point
        (raw `inputs[0]`), which `input(0, p)` also names."""
        ...

class EntryPat(NodePat, OutputPat):
    """Builder for the function's unique `Entry` node, returned by
    `entry()`. `Entry` has no inputs and one control output, so it nests as
    a control operand, e.g. `region().any_input(entry())`."""

class RegionPat(NodePat, InputPat, OutputPat):
    """Builder for control-flow merge (`Region`) patterns, returned by
    `region()`.

    Inputs are one control edge per predecessor at raw slots `0..N`, with
    no fixed prefix, so `input(idx, p)` constrains predecessor `idx`. Every
    one is a Control edge, which only a control-rooted sub-pattern
    (`entry()` / `region()`) or an untyped wildcard (`var` / `anything`)
    binds.
    """

class IndirectBranchPat(NodePat, InputPat, CtrlPat, MemPat):
    """Builder for unresolved indirect-branch patterns, returned by
    `indirect_branch()`.

    Inputs are `[ctrl, mem, target]` plus the optional interworking ISA
    mode; there are no outputs, so the pattern is rooted on the node itself.
    """
    def target(self, p: Union[PatLike, list[PatLike]]) -> "IndirectBranchPat":
        """Constrain the dispatch target (`inputs[2]`). `p` is any pattern
        operand, including a raw int, which matches a branch to that literal
        address. A list of them matches any one entry; an empty list matches
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
    def selector(self, p: Union[PatLike, list[PatLike]]) -> "SwitchPat":
        """The value the switch dispatches on (`inputs[1]`). The arms'
        addresses are the control outputs, not this slot. `p` is any pattern
        operand, including a raw int, which matches that literal value. A
        list of them matches any one entry; an empty list matches
        nothing."""
        ...

class FunctionArgPat(NodePat):
    """Builder for incoming-function-argument patterns, returned by
    `function_arg(i)`, `function_arg_float(i)`, `any_function_arg()`,
    `function_arg_reg(vn)` and `function_arg_stack(space, offset)`.

    Integer and float arguments are numbered separately, so the constructor
    picks the index space: `function_arg(0)` is the first integer argument,
    `function_arg_float(0)` the first float one.

    The carrier node has no fixed kind (an `InitialVar` for a
    register-passed argument, a `Load` for a stack-passed one), so it
    declares no raw slot vocabulary.
    """
    def index(self, i: int) -> "FunctionArgPat":
        """Require the argument's position to be `i`."""
        ...
    def source_register(self, vn: Vn) -> "FunctionArgPat":
        """Require the argument to arrive in register varnode `vn`."""
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
    """Match any node for which `f` returns true; shorthand for
    `anything().when(f)`."""
def value_of_width(n: int) -> Pat:
    """Match any value whose own output is exactly `n` bits wide. Width 1
    means "produces a boolean", which INCLUDES comparisons, since a
    comparison's output is `I1` however wide its operands are. The chained
    form is `Pat.of_width(n)`."""
def inputs_of_width(n: int, inner: PatLike) -> Pat:
    """Match `inner` and require all of ITS value inputs to be `n` bits
    wide. Width 1 means "operates on booleans", which EXCLUDES comparisons,
    whose operands are typically wider than their `I1` result. The
    input-side counterpart of `value_of_width`."""
def bool_inputs(inner: PatLike) -> Pat:
    """Match `inner` whose value inputs are all booleans (1-bit `I1`).
    Exactly `inputs_of_width(1, inner)`, named for intent. EXCLUDES
    comparisons; see `inputs_of_width`."""
def int_const(value: int | list[int] | Capture | None = ...) -> Pat:
    """Match an integer constant whose value, masked to its output width,
    equals `value`, or is one of `value` when given a list. Given a `Capture`,
    or nothing, match any integer constant, binding it when there is a
    capture."""
def int_const_any_width(value: int | list[int]) -> Pat:
    """Match an integer constant holding `value` however it was width-extended
    into the constant's own type: exact, widened by zero extension, or widened
    by sign extension. Given a list, any member of it. More permissive than
    `int_const`, which is bit-exact at the output width."""
def bool_const(value: bool | Capture | None = ...) -> Pat:
    """Match a 1-bit boolean constant equal to `value`. Given a `Capture`, or
    nothing, match any boolean constant, binding it when there is a capture."""
def float_const(bits: int | Capture | None = ...) -> Pat:
    """Match a float constant whose raw bits equal `bits`. Given a `Capture`,
    or nothing, match any float constant, binding it when there is a
    capture."""
def any_int(c: Capture | None = ...) -> Pat:
    """Match any node with an integer output (`I1` through `I512`), constant
    or not, optionally binding it to `c`. `I1` is an integer type, so this
    includes booleans; `any_bool` is the `I1`-only form. `int_const()` is the
    constant-only form."""
def any_bool(c: Capture | None = ...) -> Pat:
    """Match any node with a 1-bit (`I1`) output, constant or not, optionally
    binding it to `c`. INCLUDES comparisons. `bool_const()` is the
    constant-only form. The chained form is `Pat.bool_valued()`."""
def any_float(c: Capture | None = ...) -> Pat:
    """Match any node with a float output (`F16` through `F128`), constant or
    not, optionally binding it to `c`. `float_const()` is the constant-only
    form."""
def initial_var() -> Pat:
    """Match any initial-state register read."""
def initial_var_for(vn: Vn) -> Pat:
    """Match the initial-state read of varnode `vn`."""
def one_of(patterns: Sequence[PatLike]) -> Pat:
    """Match if ANY of the listed sub-patterns matches (a logical OR).

    An arm is anything a top-level pattern is (a value shape,
    `store()`/`mem_phi()`, `call()`, ...), and the result nests in any slot
    (value, memory, control). Match-only; no alternatives matches nothing.

    A UNION, not an ordered choice: every arm that matches fires with its own
    bindings, so order carries no meaning and a wildcard arm does not shadow
    a narrower one. Both arms below fire on `base + K`::

        load(addr=one_of([var(x), int_add(var(base), int_const(off))]))

    A capture under an arm that did not fire stays unbound, so `Match.has(c)`
    reports which fired: `off = h.uint(o) if h.has(o) else 0`.
    Use `first_of` for an ordered choice that cuts to the first match.
    """
def first_of(patterns: Sequence[PatLike]) -> Pat:
    """Match the FIRST listed sub-pattern that matches (an ordered OR).

    Same generality as `one_of` (any arm, any slot), but a first-match
    cut rather than a union: the first arm that matches wins and the rest
    are not tried. Order the alternatives most-specific first, because a
    permissive leading arm shadows everything after it. `anything()` and
    `var(c)` match ANY node::

        # WRONG: var(base) also matches the Add, so `off` never binds and
        # every `base + K` load looks like a bare `base`.
        load(addr=first_of([var(base), int_add(var(base), int_const(off))]))

        # RIGHT: specific shape first, bare fallback last.
        load(addr=first_of([int_add(var(base), int_const(off)), var(base)]))

    No alternatives matches nothing; match-only.
    """
def function_arg(i: int) -> FunctionArgPat:
    """Start a function-argument pattern constrained to argument index
    `i`."""
def function_arg_float(i: int) -> FunctionArgPat:
    """Start a function-argument pattern constrained to float argument
    index `i`, counting only float parameters."""
def any_function_arg() -> FunctionArgPat:
    """Start a function-argument pattern matching any argument index of
    either class."""
def function_arg_reg(vn: Vn) -> FunctionArgPat:
    """Match a function argument arriving in register varnode `vn`."""
def function_arg_stack(space: VnSpace, offset: int) -> FunctionArgPat:
    """Match a function argument arriving on the stack at `(space,
    offset)`."""
def phi() -> PhiPat:
    """Start a value-phi pattern builder."""
def phi_for(vn: Vn) -> PhiPat:
    """Start a value-phi pattern builder for varnode `vn`."""
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

def int_add(l: PatLike, r: PatLike) -> Pat:
    """Match integer addition. Commutative: both operand orders are
    tried."""
def int_sub(l: PatLike, r: PatLike) -> Pat:
    """Match integer subtraction."""
def int_mul(l: PatLike, r: PatLike) -> Pat:
    """Match integer multiplication. Commutative."""
def int_div(l: PatLike, r: PatLike) -> Pat:
    """Match unsigned integer division."""
def int_sdiv(l: PatLike, r: PatLike) -> Pat:
    """Match signed integer division."""
def int_rem(l: PatLike, r: PatLike) -> Pat:
    """Match unsigned integer remainder."""
def int_srem(l: PatLike, r: PatLike) -> Pat:
    """Match signed integer remainder."""
def int_shl(l: PatLike, r: PatLike) -> Pat:
    """Match a left shift."""
def int_shr(l: PatLike, r: PatLike) -> Pat:
    """Match a logical (zero-filling) right shift."""
def int_sshr(l: PatLike, r: PatLike) -> Pat:
    """Match an arithmetic (sign-filling) right shift."""
def int_and(l: PatLike, r: PatLike) -> Pat:
    """Match a bitwise AND. Commutative."""
def int_or(l: PatLike, r: PatLike) -> Pat:
    """Match a bitwise OR. Commutative."""
def int_xor(l: PatLike, r: PatLike) -> Pat:
    """Match a bitwise XOR. Commutative."""
def int_cmp(op: IntCmpOpName, l: PatLike, r: PatLike) -> Pat:
    """Match a named integer comparison, e.g. `"Equal"`, `"Less"`,
    `"Sless"`, `"Carry"`."""
def int_eq(l: PatLike, r: PatLike) -> Pat:
    """Match an integer equality test. Commutative."""
def int_ne(l: PatLike, r: PatLike) -> Pat:
    """Match an integer inequality test."""
def int_lt(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned less-than test."""
def int_le(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned less-or-equal test."""
def int_slt(l: PatLike, r: PatLike) -> Pat:
    """Match a signed less-than test."""
def int_sle(l: PatLike, r: PatLike) -> Pat:
    """Match a signed less-or-equal test."""
def int_carry(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned-addition carry test. Commutative."""
def int_scarry(l: PatLike, r: PatLike) -> Pat:
    """Match a signed-addition overflow test. Commutative."""
def int_sborrow(l: PatLike, r: PatLike) -> Pat:
    """Match a signed-subtraction overflow test."""

def int_neg(operand: PatLike) -> Pat:
    """Match arithmetic negation (`-x`)."""
def int_not(operand: PatLike) -> Pat:
    """Match bitwise complement (`~x`)."""

def bool_and(l: PatLike, r: PatLike) -> Pat:
    """Match a logical AND on 1-bit values. Commutative."""
def bool_or(l: PatLike, r: PatLike) -> Pat:
    """Match a logical OR on 1-bit values. Commutative."""
def bool_xor(l: PatLike, r: PatLike) -> Pat:
    """Match a logical XOR on 1-bit values. Commutative."""
def bool_not(operand: PatLike) -> Pat:
    """Match a logical NOT on a 1-bit value."""

def float_add(l: PatLike, r: PatLike) -> Pat:
    """Match float addition. Commutative."""
def float_sub(l: PatLike, r: PatLike) -> Pat:
    """Match float subtraction."""
def float_mul(l: PatLike, r: PatLike) -> Pat:
    """Match float multiplication. Commutative."""
def float_div(l: PatLike, r: PatLike) -> Pat:
    """Match float division."""
def float_neg(operand: PatLike) -> Pat:
    """Match float negation."""
def float_abs(operand: PatLike) -> Pat:
    """Match float absolute value."""
def float_sqrt(operand: PatLike) -> Pat:
    """Match a float square root."""
def float_ceil(operand: PatLike) -> Pat:
    """Match a round-toward-positive-infinity."""
def float_floor(operand: PatLike) -> Pat:
    """Match a round-toward-negative-infinity."""
def float_round(operand: PatLike) -> Pat:
    """Match a round-to-nearest."""
def float_is_nan(operand: PatLike) -> Pat:
    """Match a NaN test, the IEEE 754 self-inequality `x != x`."""
def float_eq(l: PatLike, r: PatLike) -> Pat:
    """Match a float equality test. Commutative."""
def float_ne(l: PatLike, r: PatLike) -> Pat:
    """Match a float inequality test."""
def float_lt(l: PatLike, r: PatLike) -> Pat:
    """Match a float less-than test."""
def float_le(l: PatLike, r: PatLike) -> Pat:
    """Match a float less-or-equal test, NaN-aware."""

def int_to_float(operand: PatLike) -> Pat:
    """Match an integer-to-float numeric conversion."""
def float_to_int(operand: PatLike) -> Pat:
    """Match a float-to-integer numeric conversion."""
def float_to_float(operand: PatLike) -> Pat:
    """Match a float-to-float reprecision."""
def int_bits_to_float(operand: PatLike) -> Pat:
    """Match a same-width reinterpretation of integer bits as a float."""
def float_bits_to_int(operand: PatLike) -> Pat:
    """Match a same-width reinterpretation of float bits as an integer."""

def int_truncate(operand: PatLike) -> Pat:
    """Match a narrowing that keeps the low bits."""
def int_popcount(operand: PatLike) -> Pat:
    """Match a set-bit count."""
def int_lzcount(operand: PatLike) -> Pat:
    """Match a leading-zero count."""
def int_zero_extend(operand: PatLike) -> Pat:
    """Match a widening that fills the new high bits with zero."""
def int_sign_extend(operand: PatLike) -> Pat:
    """Match a widening that replicates the sign bit."""
def int_extend(op: ExtendOpName, operand: PatLike) -> Pat:
    """Match a widening of the kind named by `op`."""

def load(addr: PatLike = ...) -> LoadPat:
    """Start a `Load` pattern builder, optionally pinning the address."""
def store(addr: PatLike = ..., data: PatLike = ...) -> StorePat:
    """Start a `Store` pattern builder, optionally pinning the address and
    the stored value."""
def call() -> CallPat:
    """Start a `Call` pattern builder."""
def call_other() -> CallOtherPat:
    """Start a `CallOther` (architecture-specific user operation) pattern
    builder."""
def ret() -> RetPat:
    """Start a `Return` pattern builder."""
def if_else(cond: PatLike = ...) -> IfPat:
    """Start a conditional-branch pattern builder, optionally pinning the
    condition."""

def int_binary(op: str, l: PatLike, r: PatLike) -> IntBinaryPat:
    """Match a named integer binary op (`"Add"`, `"Shl"`, `"And"`),
    returning a chainable builder."""
def bool_binary(op: str, l: PatLike, r: PatLike) -> BoolBinaryPat:
    """Match a named boolean binary op (`"And"`, `"Or"`, `"Xor"`).

    Booleans are 1-bit integers, so this matches an integer op at `I1`,
    commutative and guarded so it never matches a same-shaped wide integer
    op. Returns a chainable builder; call `.ordered()` to disable
    commutative matching.
    """
def float_binary(op: str, l: PatLike, r: PatLike) -> FloatBinaryPat:
    """Match a named float binary op (`"Add"`, `"Mul"`, `"Div"`), returning
    a chainable builder."""

def any_int_binary(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match ANY integer binary op with these operands, binding the node to
    `c` so you can read the variant back with `Match.op(c)`."""
def any_int_unary(c: Capture, operand: PatLike) -> Pat:
    """Match any integer unary op on `operand`, binding the node to `c`."""
def any_int_cmp(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any integer comparison with these operands, binding the node to
    `c`."""
def any_bool_binary(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any boolean binary op with these operands, binding the node to
    `c`. A 1-bit logical NOT is an XOR with 1, so match it here with an
    all-ones 1-bit operand."""
def any_float_binary(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any float binary op with these operands, binding the node to
    `c`."""
def any_float_unary(c: Capture, operand: PatLike) -> Pat:
    """Match any float unary op on `operand`, binding the node to `c`."""
def any_float_cmp(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any float comparison with these operands, binding the node to
    `c`."""
