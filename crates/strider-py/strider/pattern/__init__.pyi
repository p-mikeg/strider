"""Type stubs for strider.pattern: the match-side pattern DSL.

`Pat` is the left-hand side of a `find` or `rewrite` query, with the
match-only affordances `.when()`, commutativity, `.of_width()`,
`.value_ty()` and the wildcards. For a rewrite right-hand side (`replace=`)
use `strider.template` and its `Template` type instead. A bare `Pat` is
still accepted there for compatibility, but only its build-valid subset
compiles: no `.when`, no wildcard, no commutativity.
"""

from __future__ import annotations

from typing import Any, Callable, List, Literal, Optional, Union

from ..ir import Node
from ..sleigh import Vn

# A pattern describes graph shape; a constraint is a relational predicate
# over the captures patterns bind. Separate namespaces so the two kinds
# cannot be mistaken for one another.
from . import constraints as constraints

#: Value-type names accepted by `Pat.value_ty` and returned by
#: `Node.value_type` / `Match.value_type`. Matched case-insensitively at
#: runtime; these are the canonical spellings.
ValueTy = Literal[
    "i1", "i8", "i16", "i32", "i48", "i64", "i80", "i128", "i256", "i512",
    "f32", "f64", "f80",
]

#: Integer-comparison names accepted by `int_cmp`. The short aliases `"eq"`,
#: `"lt"` and `"slt"` are equivalent to `"Equal"`, `"Less"` and `"Sless"`.
#: Matched case-insensitively at runtime.
IntCmpOpName = Literal[
    "Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow", "eq", "lt", "slt",
]

#: Widening-operation names accepted by `extend`.
ExtendOpName = Literal[
    "zero", "zero_extend", "ZeroExtend", "sign", "sign_extend", "SignExtend",
]

class Match:
    """One result of a query: the nodes a pattern matched and what each
    capture bound to.

    `node(key)` is the single source of truth for per-node reads; every
    value and op reader below forwards to it, returning `None` when `key` is
    unbound.
    """

    @property
    def root(self) -> int:
        """The node id where the top-level pattern matched. For a joined
        (list) query, see `roots`."""
        ...
    @property
    def roots(self) -> List[int]:
        """One root node id per pattern passed to the query (`[root]` for a
        single-pattern query)."""
        ...
    def const_uint(self, key: Any) -> Optional[int]:
        """The unsigned constant value bound to `key`."""
        ...
    def const_int(self, key: Any) -> Optional[int]:
        """The signed constant value bound to `key`."""
        ...
    def const_bool(self, key: Any) -> Optional[bool]:
        """The boolean constant value bound to `key`."""
        ...
    def float_bits(self, key: Any) -> Optional[int]:
        """The raw float bit pattern bound to `key`."""
        ...
    def has(self, key: Any) -> bool:
        """Whether `key` bound to anything. Captures under an alternative
        that did not fire are left unbound."""
        ...
    def op(self, key: Any) -> Optional[str]:
        """The operation variant of the node bound to `key` (`"Add"`,
        `"Less"`, `"Neg"`), or `None` when `key` is unbound or names a node
        carrying no operation.

        One accessor covers every op family (integer and float; binary,
        unary and compare). Pair it with `value_type` to tell a boolean op
        (`Xor` at `I1`) from a wide bitwise one.
        """
        ...
    def value_type(self, key: Any) -> Optional[str]:
        """The value-output type of the node bound to `key` (`"I1"`,
        `"I64"`, `"F64"`), or `None` when `key` is unbound or names a node
        with no value output."""
        ...
    def vn(self, key: Any) -> Optional[Vn]:
        """The varnode bound by `key` (an initial register read, or a call's
        return-value or clobber output), else `None`."""
        ...
    def asm_fingerprint(self, key: Any) -> List[int]:
        """The machine-instruction addresses recorded on the node bound to
        `key`; `[]` when `key` is unbound."""
        ...
    def node(self, key: Any) -> Optional[Node]:
        """A `Node` handle on what `key` bound to (`key` is a `Capture` or a
        capture name), or `None` when unbound. Every other reader here is
        built on this."""
        ...
    def __getitem__(self, key: Any) -> Any: ...
    def __contains__(self, key: Any) -> bool: ...

class Capture:
    """A capture variable: attach it to a sub-pattern, then read the node it
    bound back off the `Match`."""
    def __init__(self) -> None:
        """Create a fresh capture, distinct from every other."""
        ...

class CastMask:
    """Selects which value-passthrough casts the matcher walks through
    transparently. Compose with `|`; pass as
    `Function.find_all(pat, ignore_casts_mask=...)`."""
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
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Pat:
    """A finalised match pattern, the left-hand side of a `find` or
    `rewrite`.

    Built by the free constructors below and chained via `.capture`, `.cap`,
    `.when`, `.ordered`, `.of_width`, `.value_ty` and `.bool_valued`. For a
    rewrite right-hand side, build a `strider.template.Template` instead.
    """
    def capture(self, c: Capture) -> Pat:
        """Bind the matched node to capture `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Bind the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Keep a match only when `f` returns true for the partial match so
        far. A rejected binding backtracks, so other bindings are still
        tried."""
        ...
    def ordered(self) -> Pat:
        """Pin operand slots, suppressing the commutative alternative for a
        commutative op."""
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

#: What a sub-pattern slot accepts: a capture name, a `Capture`, a finished
#: `Pat`, or any of the typed builders below (auto-finalised at the call
#: site, so an explicit `.into_pat()` is never required).
PatLike = Union[
    str,
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

class IntBinaryPat:
    """Builder for an integer binary-op pattern, returned by `int_binary`.

    `.capture`, `.cap`, `.when` and `.ordered` return the builder, which
    nests directly as a value operand; `.into_pat()` finalises to a `Pat`.
    """
    def ordered(self) -> IntBinaryPat:
        """Pin operand slots, disabling commutative matching."""
        ...
    def capture(self, c: Capture) -> IntBinaryPat:
        """Bind the matched node to capture `c`."""
        ...
    def cap(self, name: str) -> IntBinaryPat:
        """Bind the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> IntBinaryPat:
        """Keep a match only when `f` returns true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class FloatBinaryPat:
    """Builder for a float binary-op pattern, returned by `float_binary`.

    `.capture`, `.cap`, `.when` and `.ordered` return the builder, which
    nests directly as a value operand; `.into_pat()` finalises to a `Pat`.
    """
    def ordered(self) -> FloatBinaryPat:
        """Pin operand slots, disabling commutative matching."""
        ...
    def capture(self, c: Capture) -> FloatBinaryPat:
        """Bind the matched node to capture `c`."""
        ...
    def cap(self, name: str) -> FloatBinaryPat:
        """Bind the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> FloatBinaryPat:
        """Keep a match only when `f` returns true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class BoolBinaryPat:
    """Builder for a boolean binary-op pattern, returned by `bool_binary`.

    Booleans are the 1-bit integer `I1`, so this matches an integer `And`,
    `Or` or `Xor` whose output is `I1`, guarded so it never matches a
    same-shaped wide integer op. `.capture`, `.cap`, `.when` and `.ordered`
    return the builder, which nests directly as a value operand;
    `.into_pat()` finalises to a `Pat`.
    """
    def ordered(self) -> BoolBinaryPat:
        """Pin operand slots, disabling commutative matching."""
        ...
    def capture(self, c: Capture) -> BoolBinaryPat:
        """Bind the matched node to capture `c`."""
        ...
    def cap(self, name: str) -> BoolBinaryPat:
        """Bind the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> BoolBinaryPat:
        """Keep a match only when `f` returns true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class CallPat:
    """Builder for `Call` node patterns, returned by `call()`.

    Field setters (`at`, `at_any`, `target`, `arg`) return the same builder
    for chaining; finalisers (`capture`, `cap`, `when`, `into_pat`) return a
    `Pat`. Queries accept an unfinalised builder, so
    `fn.find_all(call().arg(0, int_const(8)))` works as written.
    """
    def at(self, addr: int) -> "CallPat":
        """Require the call to sit at machine address `addr`."""
        ...
    def at_any(self, addrs: List[int]) -> "CallPat":
        """Require the call to sit at one of `addrs`."""
        ...
    def target(self, p: PatLike) -> "CallPat":
        """Constrain the value naming the callee."""
        ...
    def arg(self, idx: int, p: PatLike) -> "CallPat":
        """Constrain the argument at position `idx`."""
        ...
    def any_input(self, p: PatLike) -> "CallPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def mem(self, p: PatLike) -> "CallPat":
        """Constrain the incoming memory token."""
        ...
    def res(self) -> "CallPat":
        """Match the call's primary return value rather than the call node
        itself."""
        ...
    def output(self, slot: int) -> "CallOutputPat":
        """Address output `slot` so it can be captured or type-constrained."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class CallOtherPat:
    """Builder for `CallOther` (architecture-specific user operation)
    patterns, returned by `call_other()`."""
    def user_op_id(self, v: int) -> "CallOtherPat":
        """Require the user-op id to be `v`."""
        ...
    def name(self, n: str) -> "CallOtherPat":
        """Require the user-op name to be `n`."""
        ...
    def arg(self, idx: int, p: PatLike) -> "CallOtherPat":
        """Constrain the argument at position `idx`."""
        ...
    def ctrl(self, p: PatLike) -> "CallOtherPat":
        """Constrain the incoming control edge."""
        ...
    def mem(self, p: PatLike) -> "CallOtherPat":
        """Constrain the incoming memory token."""
        ...
    def any_input(self, p: PatLike) -> "CallOtherPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def res(self) -> "CallOtherPat":
        """Match the operation's result value rather than the node itself."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class RetPat:
    """Builder for `Return` node patterns, returned by `ret()`."""
    def preceded_by(self, p: PatLike) -> "RetPat":
        """Require the return's direct control predecessor to match `p`."""
        ...
    def ret_val(self, idx: int, p: PatLike) -> "RetPat":
        """Constrain the value returned at ABI position `idx`."""
        ...
    def any_input(self, p: PatLike) -> "RetPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class IfPat:
    """Builder for conditional-branch patterns, returned by `if_else()`.

    When `cond` is set the matcher also tries the compiler-inverted layout,
    a negated condition with the two arms swapped.
    """
    def cond(self, p: PatLike) -> "IfPat":
        """Constrain the branch condition."""
        ...
    def true_branch(self, p: PatLike) -> "IfPat":
        """Constrain what the taken edge leads to."""
        ...
    def false_branch(self, p: PatLike) -> "IfPat":
        """Constrain what the not-taken edge leads to."""
        ...
    def capture_true(self, c: Capture) -> "IfPat":
        """Bind the taken control EDGE to `c`, for use with `dominates` and
        `phi_input_from_edge`."""
        ...
    def capture_false(self, c: Capture) -> "IfPat":
        """Bind the not-taken control EDGE to `c`, for use with `dominates`
        and `phi_input_from_edge`."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class LoadPat:
    """Builder for `Load` node patterns, returned by `load()`."""
    def addr(self, p: PatLike) -> "LoadPat":
        """Constrain the address loaded from."""
        ...
    def mem_in(self, p: PatLike) -> "LoadPat":
        """Constrain the incoming memory token."""
        ...
    def any_input(self, p: PatLike) -> "LoadPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def bit_width(self, n: int) -> "LoadPat":
        """Require the loaded value to be `n` bits wide."""
        ...
    def space(self, s: object) -> "LoadPat":
        """Require the load to target address space `s`."""
        ...
    def stack_offset(self, k: int) -> "LoadPat":
        """Require the address to be the frame pointer plus `k` bytes."""
        ...
    def stack_only(self) -> "LoadPat":
        """Require the address to be stack-relative, at any offset."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class StorePat:
    """Builder for `Store` node patterns, returned by `store()`."""
    def addr(self, p: PatLike) -> "StorePat":
        """Constrain the address stored to."""
        ...
    def data(self, p: PatLike) -> "StorePat":
        """Constrain the value stored."""
        ...
    def mem_in(self, p: PatLike) -> "StorePat":
        """Constrain the incoming memory token."""
        ...
    def any_input(self, p: PatLike) -> "StorePat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def bit_width(self, n: int) -> "StorePat":
        """Require the stored value to be `n` bits wide."""
        ...
    def space(self, s: object) -> "StorePat":
        """Require the store to target address space `s`."""
        ...
    def stack_offset(self, k: int) -> "StorePat":
        """Require the address to be the frame pointer plus `k` bytes."""
        ...
    def stack_only(self) -> "StorePat":
        """Require the address to be stack-relative, at any offset."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class PhiPat:
    """Builder for value-phi patterns, returned by `phi()` /
    `phi_for(vn)`."""
    def for_vn(self, vn: object) -> "PhiPat":
        """Require the phi to carry varnode `vn`."""
        ...
    def input(self, idx: int, p: PatLike) -> "PhiPat":
        """Constrain the value merged from predecessor `idx`."""
        ...
    def any_input(self, p: PatLike) -> "PhiPat":
        """Match `p` against any one merged value, yielding one match per
        qualifying arm. Anchored at the phi's own inputs, so it costs one
        step per arm rather than ranging over the whole function."""
        ...
    def phi_token(self, p: PatLike) -> "PhiPat":
        """Constrain the region token tying this phi to its merge point."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class MemPhiPat:
    """Builder for memory-token phi patterns, returned by `mem_phi()`."""
    def input(self, idx: int, p: PatLike) -> "MemPhiPat":
        """Constrain the memory token merged from predecessor `idx`."""
        ...
    def any_input(self, p: PatLike) -> "MemPhiPat":
        """Match `p` against any one merged token, yielding one match per
        qualifying arm."""
        ...
    def phi_token(self, p: PatLike) -> "MemPhiPat":
        """Constrain the region token tying this phi to its merge point."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class CallOutputPat:
    """One sibling output slot of a `Call`, returned by
    `CallPat.output(slot)`. Each method records its constraint and returns
    the parent `CallPat` for further chaining."""
    def capture(self, c: Capture) -> CallPat:
        """Bind this output slot to `c`."""
        ...
    def of_width(self, bits: int) -> CallPat:
        """Require this output to be `bits` wide."""
        ...
    def of_type(self, ty: str) -> CallPat:
        """Require this output's type to be `ty`."""
        ...

class EntryPat:
    """Builder for the function's unique `Entry` node, returned by
    `entry()`. `Entry` has no inputs and one control output, so it nests as
    a control operand, e.g. `region().any_input(entry())`."""
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class RegionPat:
    """Builder for control-flow merge (`Region`) patterns, returned by
    `region()`."""
    def input(self, idx: int, p: PatLike) -> "RegionPat":
        """Constrain control predecessor `idx`."""
        ...
    def any_input(self, p: PatLike) -> "RegionPat":
        """Match `p` against any one control predecessor."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class IndirectBranchPat:
    """Builder for unresolved indirect-jump patterns, returned by
    `indirect_branch()`."""
    def target(self, p: PatLike) -> "IndirectBranchPat":
        """Constrain the computed jump target."""
        ...
    def mem(self, p: PatLike) -> "IndirectBranchPat":
        """Constrain the incoming memory token."""
        ...
    def any_input(self, p: PatLike) -> "IndirectBranchPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def preceded_by(self, p: PatLike) -> "IndirectBranchPat":
        """Require the direct control predecessor to match `p`."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class UnreachablePat:
    """Builder for `Unreachable` terminator patterns, returned by
    `unreachable()`."""
    def any_input(self, p: PatLike) -> "UnreachablePat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def preceded_by(self, p: PatLike) -> "UnreachablePat":
        """Require the direct control predecessor to match `p`."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class SwitchPat:
    """Builder for resolved multi-way dispatch patterns, returned by
    `switch()`."""
    def address(self, p: PatLike) -> "SwitchPat":
        """Constrain the dispatch address."""
        ...
    def any_input(self, p: PatLike) -> "SwitchPat":
        """Match `p` against any one input, whichever slot it is in."""
        ...
    def preceded_by(self, p: PatLike) -> "SwitchPat":
        """Require the direct control predecessor to match `p`."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
        ...

class FunctionArgPat:
    """Builder for incoming-function-argument patterns, returned by
    `function_arg(i)`, `function_arg_any()`, `function_arg_reg(vn)` and
    `function_arg_stack(space, offset)`."""
    def index(self, i: int) -> "FunctionArgPat":
        """Require the argument's position to be `i`."""
        ...
    def source_register(self, vn: object) -> "FunctionArgPat":
        """Require the argument to arrive in register varnode `vn`."""
        ...
    def source_stack(self, space: object, offset: int) -> "FunctionArgPat":
        """Require the argument to arrive on the stack at `(space,
        offset)`."""
        ...
    def capture(self, c: Capture) -> Pat:
        """Finalise, binding the matched node to `c`."""
        ...
    def cap(self, name: str) -> Pat:
        """Finalise, binding the matched node to the capture named `name`."""
        ...
    def when(self, f: Callable[[Match], bool]) -> Pat:
        """Finalise with a predicate that must return true."""
        ...
    def into_pat(self) -> Pat:
        """Finalise to a `Pat`."""
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
def bool_value() -> Pat:
    """Match any value producing a boolean (1-bit `I1`). Exactly
    `value_of_width(1)`, named for intent. INCLUDES comparisons. The chained
    form is `Pat.bool_valued()`."""
def inputs_of_width(n: int, inner: PatLike) -> Pat:
    """Match `inner` and require all of ITS value inputs to be `n` bits
    wide. Width 1 means "operates on booleans", which EXCLUDES comparisons,
    whose operands are typically wider than their `I1` result. The
    input-side counterpart of `value_of_width`."""
def bool_inputs(inner: PatLike) -> Pat:
    """Match `inner` whose value inputs are all booleans (1-bit `I1`).
    Exactly `inputs_of_width(1, inner)`, named for intent. EXCLUDES
    comparisons; see `inputs_of_width`."""
def int_const(value: int) -> Pat:
    """Match an integer constant whose value, masked to its output width,
    equals `value`."""
def signed_int_const(value: int) -> Pat:
    """Match an integer constant equal to the signed `value` across both
    sign- and zero-extended storage forms."""
def int_const_any_of(values: List[int]) -> Pat:
    """Match an integer constant equal to any of `values`, the
    set-membership form of `int_const`."""
def bool_const(value: bool) -> Pat:
    """Match a 1-bit boolean constant equal to `value`."""
def float_const(bits: int) -> Pat:
    """Match a float constant whose raw bits equal `bits`."""
def any_int_const(c: Capture | None = ...) -> Pat:
    """Match any integer constant, optionally binding it to `c`. Omit `c` to
    use it purely as a structural constraint."""
def any_bool_const(c: Capture | None = ...) -> Pat:
    """Match any 1-bit boolean constant, optionally binding it to `c`."""
def any_float_const(c: Capture | None = ...) -> Pat:
    """Match any float constant, optionally binding it to `c`."""
def initial_var() -> Pat:
    """Match any initial-state register read."""
def initial_var_for(vn: object) -> Pat:
    """Match the initial-state read of varnode `vn`."""
def one_of(patterns: List[PatLike]) -> Pat:
    """Match a value if ANY of the listed sub-patterns matches it.

    An alternation, for the optional-wrapper case, e.g. an address that may
    or may not be masked:
    `one_of([add(base, off), int_and(add(base, off), mask)])`. Match-only
    (not usable as a rewrite replacement); requires at least one
    alternative.

    Order the alternatives most-specific first. They are tried in order and
    the first match wins, so a permissive alternative placed before a
    narrower one shadows it, and because the shadowing arm still matches the
    query silently returns the wrong binding rather than failing.
    `anything()` and `var(c)` match ANY node, including the operator a later
    arm was meant to catch::

        # WRONG: var(base) also matches the Add, so `off` never binds and
        # every `base + K` load silently looks like a bare `base`.
        load(addr=one_of([var(base), add(var(base), any_int_const(off))]))

        # RIGHT: specific shape first, bare fallback last.
        load(addr=one_of([add(var(base), any_int_const(off)), var(base)]))

    Captures under an alternative that did not fire are left unbound, not
    defaulted, so `Match.has(c)` tells you which arm fired and lets you
    supply your own default: `off = h.const_uint(o) if h.has(o) else 0`.
    """
def function_arg(i: int) -> FunctionArgPat:
    """Start a function-argument pattern constrained to argument index
    `i`."""
def function_arg_any() -> FunctionArgPat:
    """Start a function-argument pattern matching any argument index."""
def function_arg_reg(vn: object) -> FunctionArgPat:
    """Match a function argument arriving in register varnode `vn`."""
def function_arg_stack(space: object, offset: int) -> FunctionArgPat:
    """Match a function argument arriving on the stack at `(space,
    offset)`."""
def phi() -> PhiPat:
    """Start a value-phi pattern builder."""
def phi_for(vn: object) -> PhiPat:
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

def add(l: PatLike, r: PatLike) -> Pat:
    """Match integer addition. Commutative: both operand orders are
    tried."""
def sub(l: PatLike, r: PatLike) -> Pat:
    """Match integer subtraction, which the lifter stores as
    `add(a, neg(b))`."""
def mul(l: PatLike, r: PatLike) -> Pat:
    """Match integer multiplication. Commutative."""
def div(l: PatLike, r: PatLike) -> Pat:
    """Match unsigned integer division."""
def sdiv(l: PatLike, r: PatLike) -> Pat:
    """Match signed integer division."""
def rem(l: PatLike, r: PatLike) -> Pat:
    """Match unsigned integer remainder."""
def srem(l: PatLike, r: PatLike) -> Pat:
    """Match signed integer remainder."""
def shl(l: PatLike, r: PatLike) -> Pat:
    """Match a left shift."""
def shr(l: PatLike, r: PatLike) -> Pat:
    """Match a logical (zero-filling) right shift."""
def sshr(l: PatLike, r: PatLike) -> Pat:
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
    """Match an integer inequality test, stored as a negated equality."""
def int_lt(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned less-than test."""
def int_le(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned less-or-equal test, stored as a negated
    less-than with the operands swapped."""
def int_slt(l: PatLike, r: PatLike) -> Pat:
    """Match a signed less-than test."""
def int_sle(l: PatLike, r: PatLike) -> Pat:
    """Match a signed less-or-equal test, stored as a negated signed
    less-than with the operands swapped."""
def int_carry(l: PatLike, r: PatLike) -> Pat:
    """Match an unsigned-addition carry test. Commutative."""
def int_scarry(l: PatLike, r: PatLike) -> Pat:
    """Match a signed-addition overflow test. Commutative."""
def int_sborrow(l: PatLike, r: PatLike) -> Pat:
    """Match a signed-subtraction overflow test."""

def neg(operand: PatLike) -> Pat:
    """Match arithmetic negation (`-x`)."""
def int_not(operand: PatLike) -> Pat:
    """Match bitwise complement (`~x`), stored as an XOR with all ones."""

def bool_and(l: PatLike, r: PatLike) -> Pat:
    """Match a logical AND on 1-bit values. Commutative."""
def bool_or(l: PatLike, r: PatLike) -> Pat:
    """Match a logical OR on 1-bit values. Commutative."""
def bool_xor(l: PatLike, r: PatLike) -> Pat:
    """Match a logical XOR on 1-bit values. Commutative."""
def bool_not(operand: PatLike) -> Pat:
    """Match a logical NOT on a 1-bit value, stored as an XOR with 1."""

def float_add(l: PatLike, r: PatLike) -> Pat:
    """Match float addition. Commutative."""
def float_sub(l: PatLike, r: PatLike) -> Pat:
    """Match float subtraction, stored as `float_add(a, float_neg(b))`."""
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
    """Match a NaN test, stored as a negated self-equality."""
def float_eq(l: PatLike, r: PatLike) -> Pat:
    """Match a float equality test. Commutative."""
def float_ne(l: PatLike, r: PatLike) -> Pat:
    """Match a float inequality test, stored as a negated equality."""
def float_lt(l: PatLike, r: PatLike) -> Pat:
    """Match a float less-than test."""
def float_le(l: PatLike, r: PatLike) -> Pat:
    """Match a float less-or-equal test, stored as less-than OR equal so
    NaN behaves correctly."""

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

def truncate(operand: PatLike) -> Pat:
    """Match a narrowing that keeps the low bits."""
def popcount(operand: PatLike) -> Pat:
    """Match a set-bit count."""
def lzcount(operand: PatLike) -> Pat:
    """Match a leading-zero count."""
def zero_extend(operand: PatLike) -> Pat:
    """Match a widening that fills the new high bits with zero."""
def sign_extend(operand: PatLike) -> Pat:
    """Match a widening that replicates the sign bit."""
def extend(op: ExtendOpName, operand: PatLike) -> Pat:
    """Match a widening of the kind named by `op`."""

def load(addr: PatLike = ...) -> LoadPat:
    """Start a `Load` pattern builder, optionally pinning the address."""
def store(addr: PatLike = ..., data: PatLike = ...) -> StorePat:
    """Start a `Store` pattern builder, optionally pinning the address and
    the stored value."""
def call(at: Optional[int] = ...) -> CallPat:
    """Start a `Call` pattern builder, optionally pinning the call site's
    machine address."""
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

def int_bin_any(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match ANY integer binary op with these operands, binding the node to
    `c` so you can read the variant back with `Match.op(c)`."""
def int_un_any(c: Capture, operand: PatLike) -> Pat:
    """Match any integer unary op on `operand`, binding the node to `c`."""
def int_cmp_any(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any integer comparison with these operands, binding the node to
    `c`."""
def bool_bin_any(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any boolean binary op with these operands, binding the node to
    `c`. A 1-bit logical NOT is an XOR with 1, so match it here with an
    all-ones 1-bit operand."""
def float_bin_any(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any float binary op with these operands, binding the node to
    `c`."""
def float_un_any(c: Capture, operand: PatLike) -> Pat:
    """Match any float unary op on `operand`, binding the node to `c`."""
def float_cmp_any(c: Capture, l: PatLike, r: PatLike) -> Pat:
    """Match any float comparison with these operands, binding the node to
    `c`."""
