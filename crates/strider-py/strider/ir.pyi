"""Type stubs for strider.ir — the sea-of-nodes IR surface (`Function`,
`Node`)."""

from __future__ import annotations

from typing import Any, List, Optional, Tuple

from .cfg import Cfg, DotStyle
from .pattern import Match, PatLike as _PatLike
from .sleigh import Vn
from .template import Template as _Template

class Node:
    """A handle on a single node in the IR graph.

    Returned by `Function.node(id)` and `Match.node(capture)`.  Lets you
    explore the sea-of-nodes IR beyond pattern matching: walk the
    data/control edges feeding a node (`inputs()`), read its kind
    (`kind()`), pull out constant values, and recover provenance
    (`asm_fingerprint()`).  Snapshots the function generation at construction
    so a stale id (after `compact`/`optimize`) raises rather than
    dereferencing the wrong node."""

    @property
    def id(self) -> int:
        """The raw `u32` arena index of this node."""
        ...
    def kind(self) -> str:
        """The node's `NodeKind` as a string. Payload-carrying kinds render
        WITH their payload, so this already names the op variant:
        `"IntBinaryOp(Add)"`, `"IntCmpOp(Less)"`, `"Load(Ram)"`. Kinds with
        no payload render bare: `"Region"`, `"Phi"`, `"Entry"`. See `op()`
        for just the operation variant."""
        ...
    def inputs(self) -> List[Node]:
        """The data/control nodes feeding this one, as a list of `Node`s."""
        ...
    def const_int(self) -> Optional[int]:
        """The node's signed integer constant value (sign-extended at
        its declared width), or `None` when its value output isn't an
        integer `IntConst` or the stored magnitude exceeds 128 bits
        (I256/I512 — use `wide_const_bytes()` for those)."""
        ...
    def const_uint(self) -> Optional[int]:
        """The node's unsigned integer constant value (masked to its
        declared width), or `None` when its value output isn't an
        integer `IntConst` or the stored magnitude exceeds 128 bits
        (I256/I512 — use `wide_const_bytes()` for those)."""
        ...
    def const_bool(self) -> Optional[bool]:
        """The node's boolean constant value, else `None`."""
        ...
    def float_bits(self) -> Optional[int]:
        """Raw IEEE 754 bit pattern as `u64`, else `None` when this
        node isn't a `FloatConst`."""
        ...
    def vn(self) -> Optional[Vn]:
        """The `Vn` associated with this node (`InitialVar` / `Call` /
        `CallOther` clobber output), else `None`."""
        ...
    def asm_fingerprint(self) -> List[int]:
        """Sorted, deduped asm-instruction addresses recorded on this node."""
        ...
    def outputs(self) -> List[Node]:
        """The nodes that consume this node's outputs, as a list of `Node`s —
        the forward counterpart to `inputs()`."""
        ...
    def wide_const_bytes(self) -> Optional[bytes]:
        """Raw LE bytes of an `IntConstWide` node, else `None`."""
        ...
    def call_other_name(self) -> Optional[str]:
        """Sleigh user-op name attached to a `CallOther` node, else `None`."""
        ...
    def op(self) -> Optional[str]:
        """The operation variant of an op-carrying node — `"Add"`,
        `"Less"`, `"Neg"`, `"Sqrt"` — or `None` for a node that carries no
        operation (`Region`, `Load`, `IntConst`, `Call`, ...).

        One accessor covers every op family; the family itself is already
        in `kind()`, so `kind() == "IntBinaryOp(Xor)"` and `op() == "Xor"`
        name the same node from two directions. A boolean op is an
        `IntBinaryOp` whose output is `I1`, so pair this with
        `value_type()` to tell `Xor:I1` (a logical NOT) from a wide
        bitwise `Xor`."""
        ...
    def value_type(self) -> Optional[str]:
        """The node's value-output type — `"I1"`, `"I32"`, `"I64"`,
        `"F64"`, ... — or `None` for a node with no value output
        (`Region`, `Store`, `Return`, ...).

        Booleans are the 1-bit integer `I1`, so `value_type() == "I1"` is
        the "this produces a boolean" test. The name is accepted verbatim
        by the pattern-side `Pat.value_ty(...)` filter."""
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Function:
    @property
    def cfg(self) -> "Cfg":
        """The snapshot `Cfg` this function was lifted from, kept alive so
        the pretty render (`to_dot(pretty=True)`) can reach its Sleigh."""
        ...
    def entry_node(self) -> int:
        """The IR node id of the function's `Entry` node — the natural
        starting center for a neighborhood view."""
        ...
    def to_dot(
        self,
        path: Optional[str] = ...,
        *,
        pretty: bool = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Render the IR graph to Graphviz DOT. Returns the string when
        `path` is `None`, else writes it to `path` and returns `None`.

        `pretty=False` (the default) renders the graph EXACTLY AS STORED:
        one node per `NodeId`, one edge per input edge, side-tables inline,
        no constant inlining or virtual nodes. It is the debugging view of
        the real graph shape, and it cannot fail.

        `pretty=True` inlines constants, adds virtual nodes and resolves
        register names. That needs a `Sleigh`, which this function reaches
        back through its parent `Cfg`'s `Lifter` — so it is available only
        for a function obtained from `analyze`, and raises `StriderError`
        if the graph has no entry. `style` themes the pretty render and is
        rejected without `pretty=True`."""
        ...
    def to_html(
        self,
        path: Optional[str] = ...,
        *,
        pretty: bool = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Like `to_dot` but wraps the DOT in a self-contained HTML page.
        Same `pretty` / `style` arguments and the same caveats."""
        ...
    def neighborhood_dot(
        self,
        center: int,
        depth: int = ...,
        hub_cap: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """Raw (structure-faithful) neighborhood DOT around node `center`;
        needs no Sleigh, so it lives on `Function`."""
        ...
    def node_count(self) -> int: ...
    def count_regions(self) -> int:
        """Count of `Region` (control-flow join) nodes reachable from entry."""
        ...
    def node_ids(self) -> List[int]:
        """All reachable node ids as raw integers."""
        ...
    def node(self, node_id: int) -> Node:
        """A discoverable `Node` handle on the node at `node_id` — the
        single source of truth for per-node reads (`kind()`,
        `asm_fingerprint()`, `wide_const_bytes()`,
        `call_other_name()`, …).  Raises `StriderError` for an invalid id."""
        ...
    def cfg_walk(self) -> List[Node]:
        """Control-only reachability (the CFG skeleton) from the entry, as
        `Node`s."""
        ...
    def data_walk(self) -> List[Node]:
        """Every node reachable from the entry (data-in + control-out),
        pre-order."""
        ...
    def walk(self, node_id: int) -> List[Node]:
        """Every node reachable from `node_id` (data-in + control-out),
        pre-order."""
        ...
    def mem_walk(self) -> List[Node]:
        """The memory-touching nodes (Load / Store / Call / CallOther /
        MemPhi and the InitialMemory root) reachable by following
        memory-token edges forward from the function's InitialMemory, in
        pre-order."""
        ...
    def compact(self) -> None:
        """Drop every node unreachable from entry; invalidates node ids."""
        ...
    def validate(self) -> Optional[str]:
        """Re-validate the graph; `None` on success, else an error message."""
        ...
    def find_all(
        self,
        pat: Any,  # strider.pattern.PatLike | list[strider.pattern.PatLike]
        ignore_root: bool = ...,
        ignore_casts: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
        constraints: Optional[List[Any]] = ...,  # list[strider.pattern.constraints.JoinConstraint]
    ) -> List[Match]:
        """Deduplicated `Match`es for `pat`.  `pat` is a single pattern or a
        `list` of patterns; a list joins on shared `Capture`s (every pattern
        matches and their captures unify), returning one merged `Match` per
        result.  Dedup keys on captures+root(s) by default; `ignore_root=True`
        keys on captures only (collapsing one binding reached from several
        roots and capture-less duplicates).  `constraints` filters a joined
        result by CFG relations (`dominates` / `phi_input_from_edge`) over
        captured entities; patterns linked only by a
        constraint still count as correlated for the shared-capture
        connectivity check.

        A single root can yield SEVERAL `Match`es — one per distinct
        capture-to-binding map.  A commutative node (`add`, `mul`, `int_and`,
        ...) whose operands both satisfy a captured sub-pattern binds that
        capture to each operand in turn, and every such binding is reported:
        `add(anything().capture(k), anything())` on `add(x, y)` returns TWO
        matches, `k=x` and `k=y`.  Patterns that bind identically under both
        orderings (`add(var(x), var(x))`) or that capture nothing on the
        operands collapse to one, so a capture-free pattern never duplicates.
        Use `.ordered()` on a binary builder to pin operand slots and suppress
        the commutative alternative.  Ordering is deterministic: natural operand
        order before swapped.  `one_of` stays first-match-wins — the arms are an
        ordered choice, so a later arm never adds a second binding."""
        ...
    def find_unique(
        self,
        pat: Any,  # strider.pattern.PatLike | list[strider.pattern.PatLike]
        ignore_root: bool = ...,
        ignore_casts: bool = ...,
        ignore_casts_mask: Optional[Any] = ...,  # strider.pattern.CastMask
        constraints: Optional[List[Any]] = ...,  # list[strider.pattern.constraints.JoinConstraint]
    ) -> Match:
        """Return the single `Match` for `pat`, raising `StriderError` if there
        is not exactly one (distinct messages for 0 and >1).  The count is
        taken after dedup, so `ignore_root`, a list `pat`, and `constraints`
        behave as in `find_all`.

        Fail-closed: because `find_all` reports EVERY distinct binding, an
        ambiguous pattern raises here instead of silently returning one of
        several.  `add(anything().capture(k), anything())` binds `k` to either
        operand and so is ambiguous — pin the intent with `.ordered()`, or
        narrow the operand sub-patterns, to make it unique."""
        ...
    def rewrite(self, find: _PatLike, replace: _Template) -> int:
        """Apply a single `find -> replace` rewrite rule across the graph,
        returning the fire count.  `replace` is a `strider.template.Template`
        (build it via the `strider.template` free functions) — a bare
        `strider.pattern.Pat` is still accepted for back-compat, but only
        its build-valid subset compiles."""
        ...
    def rewrite_all(self, pairs: List[Tuple[_PatLike, _Template]]) -> int:
        """Apply a list of `(find, replace)` pairs round-robin across every
        reachable node; returns the total fire count."""
        ...
    def clone(self) -> "Function":
        """Return a deep, fully independent copy of this function.

        The clone shares no mutable state with the original (IR graph,
        calling-convention overlay, side-tables, and constant interner are
        all duplicated), so mutating the clone — e.g.
        `g2 = fn.clone(); g2.rewrite(lhs, rhs)` — never affects the
        original.  The parent `Cfg` handle is shared by reference (it is
        read-only, kept alive only for dot rendering)."""
        ...
