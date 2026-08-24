"""Type stubs for strider.ir: the sea-of-nodes IR surface (`Function`,
`Node`)."""

from __future__ import annotations

from typing import Optional, Sequence, Union

from .cfg import Cfg, DotStyle
from .pattern import (
    Capture,
    CaptureKey,
    CastMask,
    Match,
    Pat as _Pat,
    PatLike as _PatLike,
)
from .pattern.constraints import ConstraintLike
from .sleigh import Vn
from .template import Template as _Template

#: What `rewrite` accepts as a right-hand side: a `Template`, a build-valid
#: `Pat`, a `Capture`, or a capture name.
_ReplaceLike = Union[_Template, _Pat, Capture, str]

class Node:
    """A handle on a single node in the IR graph.

    Returned by `Function.node(id)` and `Match.node(capture)`.

    A handle is tied to the function's current shape, so a node id that
    `compact` or `optimize` has invalidated raises rather than silently
    dereferencing the wrong node.
    """

    @property
    def id(self) -> int:
        """This node's raw integer index in the graph. Raises once `compact`
        / `optimize` has invalidated every outstanding id."""
        ...
    def kind(self) -> str:
        """The node's kind as a string, including any payload:
        `"IntBinaryOp(Add)"`, `"IntCmpOp(Less)"`, `"Load(Ram)"`. Payload-free
        kinds render bare: `"Region"`, `"Phi"`, `"Entry"`. See `op()` for the
        operation variant alone."""
        ...
    def inputs(self) -> list[Node]:
        """The data and control nodes feeding this one."""
        ...
    def sint(self) -> Optional[int]:
        """This node's signed constant value, sign-extended at its declared
        width. `None` when it is not an integer constant or its magnitude
        exceeds 128 bits (use `wide_const_bytes()` for those)."""
        ...
    def uint(self) -> Optional[int]:
        """This node's unsigned constant value, masked to its declared width.
        `None` when it is not an integer constant or its magnitude exceeds
        128 bits (use `wide_const_bytes()` for those)."""
        ...
    def boolean(self) -> Optional[bool]:
        """This node's boolean constant value, else `None`."""
        ...
    def float_bits(self) -> Optional[int]:
        """The raw IEEE 754 bit pattern of a float constant, else `None`."""
        ...
    def vn(self) -> Optional[Vn]:
        """The varnode this node is associated with (an initial register
        read, or a call's return-value or clobber output), else `None`."""
        ...
    def asm_fingerprint(self) -> list[int]:
        """The machine-instruction addresses recorded on this node, sorted
        and deduplicated."""
        ...
    def outputs(self) -> list[Node]:
        """The nodes consuming this node's outputs, the forward counterpart
        to `inputs()`."""
        ...
    def wide_const_bytes(self) -> Optional[bytes]:
        """The little-endian bytes of a wide integer constant (`I80`, `I128`,
        `I256`, `I512`), else `None`."""
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
    def __eq__(self, other) -> bool: ...
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
        """The `Cfg` this function was lifted from."""
        ...
    def entry_node(self) -> int:
        """The node id of the function's `Entry` node."""
        ...
    def to_dot(
        self,
        path: Optional[str] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """Render the IR graph to DOT. Returns the string when `path` is
        `None`, else writes it to `path` and returns `None`.

        `pretty=False` (the default) renders the graph exactly as stored: one
        node per node id, one edge per input edge, side tables inline, no
        constant inlining or virtual nodes. It never raises.

        `pretty=True` inlines constants, adds virtual nodes, and resolves
        register names. That needs a disassembler, which this function
        reaches through its parent `Cfg`, so it works only for a function
        obtained from `analyze` and raises `StriderError` if the graph has no
        entry. A `DotStyle` name in place of `True` picks the theme.
        """
        ...
    def to_html(
        self,
        path: Optional[str] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """Like `to_dot`, but wraps the DOT in a self-contained HTML page.
        Same `pretty` argument, same caveats."""
        ...
    def neighborhood_dot(
        self,
        center: int,
        depth: int = ...,
        hub_cap: int = ...,
        max_nodes: int = ...,
        count_producers: bool = ...,
        *,
        pretty: bool = ...,
    ) -> str:
        """DOT for the nodes within `depth` hops of node `center`. A hub
        (degree over `hub_cap`) is shown and its inputs are followed, but its
        consumer fan-out is not, and `max_nodes` caps the total. The hub degree
        is the consumer fan-out; `count_producers` folds a node's inputs in too.

        `pretty=False` draws the nodes exactly as stored; `pretty=True` inlines
        constants, adds virtual nodes and resolves register names."""
        ...
    def node_count(self) -> int:
        """Total number of node ids in the graph, whether or not they are
        reachable from entry."""
        ...
    def count_regions(self) -> int:
        """How many control-flow join (`Region`) nodes are reachable from
        entry."""
        ...
    def node_ids(self) -> list[int]:
        """Every node id in the graph, reachable or not."""
        ...
    def node(self, node_id: int) -> Node:
        """A `Node` handle on the node at `node_id`. Raises `StriderError`
        for an invalid id.

        A raw id is meaningful only in the graph generation it came from:
        `compact` and `Lifter.optimize` renumber, and an id held across
        either names a different node. A `Node` carries its generation and
        goes stale instead; a bare int cannot."""
        ...
    def cfg_walk(self) -> list[Node]:
        """The nodes reachable from entry following control edges only, the
        CFG skeleton."""
        ...
    def data_walk(self) -> list[Node]:
        """Every node reachable from entry (data inputs and control
        outputs), in pre-order."""
        ...
    def walk(self, node_id: int) -> list[Node]:
        """Every node reachable from `node_id` (data inputs and control
        outputs), in pre-order."""
        ...
    def mem_walk(self) -> list[Node]:
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
        pat: Union[_PatLike, Sequence[_PatLike]],
        ignore_root: bool = ...,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
    ) -> list[Match]:
        """Every deduplicated `Match` for `pat`.

        `pat` is a single pattern or a list of them; a list joins on shared
        `Capture`s and returns one merged `Match` per result. `ignore_root`
        keys dedup on captures alone. `constraints` filters joined results by
        control-flow relations (`dominates`, `phi_input_from_edge`).

        A single root can yield several matches, one per distinct binding. A
        commutative op whose operands both satisfy a captured sub-pattern
        reports one match per operand: `int_add(anything().capture(k),
        anything())` on `int_add(x, y)` returns both `k=x` and `k=y`. Call
        `.ordered()` on a binary op to pin operand slots and suppress the
        commutative alternative.
        """
        ...
    def find_unique(
        self,
        pat: Union[_PatLike, Sequence[_PatLike]],
        ignore_root: bool = ...,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
    ) -> Match:
        """The single `Match` for `pat`, raising `StriderError` if there is
        not exactly one. The count is taken after dedup, so `ignore_root`, a
        list `pat`, and `constraints` behave as in `find_all`.

        An ambiguous pattern raises here rather than returning one of several:
        `int_add(anything().capture(k), anything())` binds `k` to either operand,
        so pin the intent with `.ordered()` or narrow the operands.
        """
        ...
    def find_unique_value(
        self,
        pat: Union[_PatLike, Sequence[_PatLike]],
        capture: CaptureKey,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
        signed: bool = ...,
    ) -> Optional[int]:
        """The single distinct constant bound to `capture` across all matches
        of `pat`, deduplicated by VALUE: `None` when no match binds a constant
        there, the value when every match agrees, and `StriderError` when two
        or more distinct values are bound.

        Unlike `find_unique`, matches that differ only structurally but agree
        on the value collapse to one, so this is the tool for "the offset in
        `base + K`", where several distinct `IntConst` nodes may hold the same
        `K`.

        `signed=True` reads the constant as two's-complement (`-8` rather than
        a large unsigned bit pattern), the right choice for a stack or struct
        offset. `pat` and `constraints` behave as in `find_all`: a list `pat`
        joins on shared `Capture`s and `constraints` filters the joined
        results."""
        ...
    def rewrite(self, find: _PatLike, replace: _ReplaceLike) -> int:
        """Apply one `find` to `replace` rewrite across the graph, returning
        how many times it fired. `replace` is a `strider.template.Template`;
        a bare `strider.pattern.Pat` is accepted for compatibility, but only
        its build-valid subset compiles. A `.when()` guard on `find` raises:
        select the sites with `find_all` instead."""
        ...
    def rewrite_all(self, pairs: Sequence[tuple[_PatLike, _ReplaceLike]]) -> int:
        """Apply `(find, replace)` pairs round-robin at every reachable node,
        returning the total fire count."""
        ...
    def clone(self) -> "Function":
        """A deep, fully independent copy of this function. The parent `Cfg`
        handle is shared by reference."""
        ...
