"""Type stubs for strider.ir: the sea-of-nodes IR surface (`Function`,
`Node`)."""

from __future__ import annotations

from typing import Any, List, Optional, Tuple

from .cfg import Cfg, DotStyle
from .pattern import Match, PatLike as _PatLike
from .sleigh import Vn
from .template import Template as _Template

class Node:
    """A handle on a single node in the IR graph.

    Returned by `Function.node(id)` and `Match.node(capture)`. Lets you go
    beyond pattern matching: walk the edges feeding a node (`inputs()`), read
    its kind, pull out constant values, and recover which machine
    instructions it came from (`asm_fingerprint()`).

    A handle snapshots the function's generation, so a node id that
    `compact` or `optimize` has invalidated raises rather than silently
    dereferencing the wrong node.
    """

    @property
    def id(self) -> int:
        """This node's raw integer index in the graph."""
        ...
    def kind(self) -> str:
        """The node's kind as a string, including any payload:
        `"IntBinaryOp(Add)"`, `"IntCmpOp(Less)"`, `"Load(Ram)"`. Payload-free
        kinds render bare: `"Region"`, `"Phi"`, `"Entry"`. See `op()` for the
        operation variant alone."""
        ...
    def inputs(self) -> List[Node]:
        """The data and control nodes feeding this one."""
        ...
    def const_int(self) -> Optional[int]:
        """This node's signed constant value, sign-extended at its declared
        width. `None` when it is not an integer constant or its magnitude
        exceeds 128 bits (use `wide_const_bytes()` for those)."""
        ...
    def const_uint(self) -> Optional[int]:
        """This node's unsigned constant value, masked to its declared width.
        `None` when it is not an integer constant or its magnitude exceeds
        128 bits (use `wide_const_bytes()` for those)."""
        ...
    def const_bool(self) -> Optional[bool]:
        """This node's boolean constant value, else `None`."""
        ...
    def float_bits(self) -> Optional[int]:
        """The raw IEEE 754 bit pattern of a float constant, else `None`."""
        ...
    def vn(self) -> Optional[Vn]:
        """The varnode this node is associated with (an initial register
        read, or a call's return-value or clobber output), else `None`."""
        ...
    def asm_fingerprint(self) -> List[int]:
        """The machine-instruction addresses recorded on this node, sorted
        and deduplicated."""
        ...
    def outputs(self) -> List[Node]:
        """The nodes consuming this node's outputs, the forward counterpart
        to `inputs()`."""
        ...
    def wide_const_bytes(self) -> Optional[bytes]:
        """The little-endian bytes of a wider-than-128-bit integer constant,
        else `None`."""
        ...
    def call_other_name(self) -> Optional[str]:
        """The user-op name attached to a `CallOther` node, else `None`."""
        ...
    def op(self) -> Optional[str]:
        """The operation variant of an op-carrying node (`"Add"`, `"Less"`,
        `"Neg"`, `"Sqrt"`), or `None` for a node carrying no operation
        (`Region`, `Load`, `IntConst`, `Call`).

        One accessor covers every op family; the family itself is already in
        `kind()`, so `kind() == "IntBinaryOp(Xor)"` and `op() == "Xor"` name
        the same node from two directions. A boolean op is an `IntBinaryOp`
        whose output is `I1`, so pair this with `value_type()` to tell a
        logical NOT (`Xor` at `I1`) from a wide bitwise `Xor`.
        """
        ...
    def value_type(self) -> Optional[str]:
        """The type of this node's value output (`"I1"`, `"I32"`, `"I64"`,
        `"F64"`), or `None` for a node with no value output (`Region`,
        `Store`, `Return`).

        Booleans are the 1-bit integer `I1`, so `value_type() == "I1"` is the
        "this produces a boolean" test. The name is accepted verbatim by
        `Pat.value_ty(...)`.
        """
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Function:
    """A lifted, optimised function as a sea-of-nodes IR graph.

    Returned as `AnalyzeResult.function`. Query it with `find_all` /
    `find_unique`, edit it with `rewrite` / `rewrite_all`, walk it with
    `data_walk` / `cfg_walk` / `mem_walk`, or render it with `to_dot` /
    `to_html`.
    """

    @property
    def cfg(self) -> "Cfg":
        """The `Cfg` this function was lifted from, kept alive so the pretty
        render can reach its disassembler."""
        ...
    def entry_node(self) -> int:
        """The node id of the function's `Entry` node, the natural starting
        center for a neighborhood view."""
        ...
    def to_dot(
        self,
        path: Optional[str] = ...,
        *,
        pretty: bool = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Render the IR graph to DOT. Returns the string when `path` is
        `None`, else writes it to `path` and returns `None`.

        `pretty=False` (the default) renders the graph exactly as stored: one
        node per node id, one edge per input edge, side tables inline, no
        constant inlining or virtual nodes. It is the debugging view of the
        real graph shape and it cannot fail.

        `pretty=True` inlines constants, adds virtual nodes, and resolves
        register names. That needs a disassembler, which this function
        reaches through its parent `Cfg`, so it works only for a function
        obtained from `analyze` and raises `StriderError` if the graph has no
        entry. `style` themes the pretty render and is rejected without
        `pretty=True`.
        """
        ...
    def to_html(
        self,
        path: Optional[str] = ...,
        *,
        pretty: bool = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Like `to_dot`, but wraps the DOT in a self-contained HTML page.
        Same `pretty` and `style` arguments, same caveats."""
        ...
    def neighborhood_dot(
        self,
        center: int,
        depth: int = ...,
        hub_cap: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """DOT for the nodes within `depth` hops of node `center`, rendered
        exactly as stored. A node whose degree exceeds `hub_cap` is shown but
        not expanded through, and `max_nodes` caps the total.
        `Lifter.neighborhood_dot` is the prettier counterpart."""
        ...
    def node_count(self) -> int:
        """How many nodes are reachable from entry."""
        ...
    def count_regions(self) -> int:
        """How many control-flow join (`Region`) nodes are reachable from
        entry."""
        ...
    def node_ids(self) -> List[int]:
        """All reachable node ids."""
        ...
    def node(self, node_id: int) -> Node:
        """A `Node` handle on the node at `node_id`. Raises `StriderError`
        for an invalid id."""
        ...
    def cfg_walk(self) -> List[Node]:
        """The nodes reachable from entry following control edges only, the
        CFG skeleton."""
        ...
    def data_walk(self) -> List[Node]:
        """Every node reachable from entry (data inputs and control
        outputs), in pre-order."""
        ...
    def walk(self, node_id: int) -> List[Node]:
        """Every node reachable from `node_id` (data inputs and control
        outputs), in pre-order."""
        ...
    def mem_walk(self) -> List[Node]:
        """The memory-touching nodes (Load, Store, Call, CallOther, MemPhi,
        and the InitialMemory root) reached by following memory-token edges
        forward from InitialMemory, in pre-order."""
        ...
    def compact(self) -> None:
        """Drop every node unreachable from entry. Invalidates node ids."""
        ...
    def validate(self) -> Optional[str]:
        """Re-check the graph's invariants. `None` on success, else an error
        message."""
        ...
    def find_all(
        self,
        pat: Any,  # strider.pattern.PatLike | list[strider.pattern.PatLike]
        ignore_root: bool = ...,
        ignore_casts: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
        constraints: Optional[List[Any]] = ...,  # list[strider.pattern.constraints.JoinConstraint]
    ) -> List[Match]:
        """Every deduplicated `Match` for `pat`.

        `pat` is a single pattern or a list of them; a list joins on shared
        `Capture`s (every pattern must match and their captures must unify),
        returning one merged `Match` per result. Dedup keys on captures plus
        root(s) by default; `ignore_root=True` keys on captures alone,
        collapsing one binding reached from several roots. `constraints`
        filters a joined result by control-flow relations (`dominates`,
        `phi_input_from_edge`) over captured entities; patterns linked only
        by a constraint still count as correlated for the shared-capture
        connectivity check.

        A single root can yield several matches, one per distinct
        capture-to-binding map. A commutative node (`add`, `mul`, `int_and`)
        whose operands both satisfy a captured sub-pattern binds that capture
        to each operand in turn, and every binding is reported:
        `add(anything().capture(k), anything())` on `add(x, y)` returns TWO
        matches, `k=x` and `k=y`. Patterns that bind identically under both
        orderings (`add(var(x), var(x))`), or that capture nothing on the
        operands, collapse to one, so a capture-free pattern never
        duplicates. Call `.ordered()` on a binary builder to pin operand
        slots and suppress the commutative alternative. Ordering is
        deterministic: natural operand order before swapped. `one_of` stays
        first-match-wins, so a later arm never adds a second binding.
        """
        ...
    def find_unique(
        self,
        pat: Any,  # strider.pattern.PatLike | list[strider.pattern.PatLike]
        ignore_root: bool = ...,
        ignore_casts: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
        constraints: Optional[List[Any]] = ...,  # list[strider.pattern.constraints.JoinConstraint]
    ) -> Match:
        """The single `Match` for `pat`, raising `StriderError` if there is
        not exactly one (with distinct messages for none and for several).
        The count is taken after dedup, so `ignore_root`, a list `pat`, and
        `constraints` behave as in `find_all`.

        This fails closed. Because `find_all` reports every distinct binding,
        an ambiguous pattern raises here instead of silently returning one of
        several. `add(anything().capture(k), anything())` binds `k` to either
        operand and so is ambiguous: pin the intent with `.ordered()`, or
        narrow the operand sub-patterns, to make it unique.
        """
        ...
    def rewrite(self, find: _PatLike, replace: _Template) -> int:
        """Apply one `find` to `replace` rewrite across the graph, returning
        how many times it fired. `replace` is a `strider.template.Template`;
        a bare `strider.pattern.Pat` is accepted for compatibility, but only
        its build-valid subset compiles."""
        ...
    def rewrite_all(self, pairs: List[Tuple[_PatLike, _Template]]) -> int:
        """Apply `(find, replace)` pairs round-robin at every reachable node,
        returning the total fire count."""
        ...
    def clone(self) -> "Function":
        """A deep, fully independent copy of this function.

        The clone shares no mutable state with the original (graph,
        calling-convention overlay, side tables and constant interner are all
        duplicated), so `g2 = fn.clone(); g2.rewrite(lhs, rhs)` never touches
        the original. The parent `Cfg` handle is shared by reference; it is
        read-only and kept alive only for rendering.
        """
        ...
