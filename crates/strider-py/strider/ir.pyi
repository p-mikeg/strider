"""Type stubs for strider.ir: the sea-of-nodes IR surface (`Function`,
`Node`)."""

from __future__ import annotations

from typing import Optional, Sequence, Union

from .cfg import Cfg, DotStyle
from .lift import AssumptionOptions
from .opt import OptimizerPipeline
from .pattern import (
    Capture,
    CaptureKey,
    CastMask,
    Match,
    PatLike as _PatLike,
)
from .pattern.constraints import ConstraintLike
from .sleigh import Vn
from .template import Template as _Template

__all__: list[str]

#: What `rewrite` accepts as a right-hand side: a `Template`, a `Capture`, or a
#: capture name.
_ReplaceLike = Union[_Template, Capture, str]

class Node:
    """A handle on a single node in the IR graph, from `Function.node(id)` or
    `Match.node(capture)`.

    Once `compact`, `optimize`, `rewrite` or `rewrite_all` has run, every
    accessor raises `StriderError`.
    """

    @property
    def id(self) -> int:
        """This node's raw integer index in the graph. Raises once the handle
        is stale."""
        ...
    def kind(self) -> str:
        """The node's kind as a string. An op or memory space carries its
        payload (`"IntBinaryOp(Add)"`, `"Load(RAM)"`, the space named as
        `VnSpace` names it), a float constant its raw bits
        (`"FloatConst(<bits>)"`); other kinds are bare (`"IntConst"`,
        `"InitialVar"`, `"CallOther"`, `"Region"`). Read a constant with
        `uint()` / `sint()`, a varnode with `vn()`, a user-op with
        `call_other_name()`."""
        ...
    def inputs(self) -> list[Node]:
        """The producer behind each input edge, in input-slot order (once per
        slot)."""
        ...
    def sint(self) -> Optional[int]:
        """This node's integer constant, sign-extended at its declared width
        (any width), else `None`. A boolean `True` reads as `-1`."""
        ...
    def uint(self) -> Optional[int]:
        """This node's integer constant, masked to its declared width (any
        width), else `None`."""
        ...
    def boolean(self) -> Optional[bool]:
        """This node's boolean constant value, else `None`."""
        ...
    def float_bits(self) -> Optional[int]:
        """The raw IEEE 754 bit pattern of a float constant, else `None`."""
        ...
    def vn(self) -> Optional[Vn]:
        """The varnode this node names, else `None`: for `InitialVar` the
        register read at entry, for a `Call` its return register (its first
        value output, not a clobber), for a `CallOther` whatever varnode the
        sla assigns its result, if any."""
        ...
    def asm_fingerprint(self) -> list[int]:
        """The machine-instruction addresses recorded on this node, sorted
        and deduplicated."""
        ...
    def outputs(self) -> list[Node]:
        """The consumers of this node's outputs, in output-slot then use order
        (once per edge)."""
        ...
    def call_other_name(self) -> Optional[str]:
        """The user-op name attached to a `CallOther` node, else `None`.
        `"read_volatile"` is a read of a register that changes with nothing
        writing it, such as a cycle counter."""
        ...
    def op(self) -> Optional[str]:
        """The operation variant of an op-carrying node (`"Add"`, `"Less"`,
        `"Neg"`, `"Sqrt"`, `"SignExtend"`), or `None` for a node carrying no
        operation (`Region`, `Load`, `IntConst`, `Call`). A boolean op is an
        `IntBinaryOp` at `I1`; see `value_type()`.
        """
        ...
    def value_type(self) -> Optional[str]:
        """The type of this node's value output (`"I1"`, `"I32"`, `"F64"`), or
        `None` for a node with none (`Region`, `Store`, `Return`). Booleans are
        `"I1"`. Accepted verbatim by `Pat.value_ty(...)`.
        """
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...

class Function:
    """A lifted function as a sea-of-nodes IR graph.

    Returned as `AnalyzeResult.function`. Query it with `find_all` /
    `find_unique`, edit it with `rewrite` / `rewrite_all`, walk it with
    `data_walk` / `cfg_walk` / `mem_walk`, or render it with `to_dot` /
    `to_html`.
    """

    @property
    def cfg(self) -> "Cfg":
        """The `Cfg` this function was lifted from."""
        ...
    @property
    def assumptions(self) -> AssumptionOptions:
        """A copy of the claims in force: those `analyze` ran under plus every
        set `optimize` added since. Dropping one means analysing again."""
        ...
    def entry_node(self) -> int:
        """The node id of the function's `Entry` node."""
        ...
    def optimize(
        self,
        assumptions: Optional[AssumptionOptions] = ...,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Run an optimizer pipeline (default: the default pipeline) over this
        function in place. `assumptions` join `self.assumptions`; none is
        withdrawn. Raises `StriderError` before running if `assumptions`
        leaves out a claim in force or adds `msr_register_keeps_mode` /
        `fp_rounding_mode_is_nearest` (lift-time claims: analyse again). Add
        one with::

            function.optimize(assumptions=function.assumptions
                              | AssumptionOptions.none(escape_analysis=True))

        Reads the producing `Lifter`'s `rom`, and raises `StriderError` while
        that `Lifter` is in `analyze`, `build_cfg` or `add_elf`. Invalidates
        outstanding `Node` / `Match` handles and node ids. Holds the GIL
        throughout."""
        ...
    def to_dot(
        self,
        path: Optional[str] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """Render the IR graph to DOT. Returns the string when `path` is
        `None`, else writes it to `path` and returns `None`.

        `pretty=False` renders the graph exactly as stored (every node
        reachable from entry, one edge per input). `pretty=True` inlines
        constants, adds virtual nodes and names registers; a `DotStyle` name in
        place of `True` picks the theme, and an unknown name raises
        `StriderError`.
        """
        ...
    def to_html(
        self,
        path: Optional[str] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """Like `to_dot`, but wraps the DOT in a self-contained HTML page."""
        ...
    def to_text(self, fingerprints: bool = ...) -> str:
        """The canonical text form: a header (endianness, tracked varnodes,
        default convention, the claims on, a `frame_address_in_call_register`
        line if one applies, argument carriers), then one line per node
        reachable from entry, `outs = op[payload] ins`. `fingerprints=True`
        appends each node's sorted asm address set `  @{..}`.

        Values are named `%v0`, `%v1`, ... in print order, so equal graphs
        print equal text; diff two runs to see what changed.
        """
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
        """DOT for the nodes within `depth` hops of node `center`, at most
        `max_nodes`. A hub (consumer fan-out over `hub_cap`, plus its inputs
        when `count_producers`) is shown but its consumers are not followed.

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

        A raw id is meaningful only until `compact`, `optimize` or `rewrite`;
        a `Node` then goes stale, while a bare int silently names something
        else."""
        ...
    def cfg_walk(self) -> list[Node]:
        """The nodes reachable from entry following control edges only (the
        CFG skeleton), in ascending node-id order."""
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
        and the InitialMemory root) reached along memory-token edges from
        InitialMemory, in pre-order, each once; empty when InitialMemory is
        unreachable. At a `MemPhi` a node can precede one of its memory
        predecessors. `Return` and `IndirectBranch` are excluded."""
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

        In a joined query, rows differing only in the root of a pattern whose
        captures are all shared with an earlier one collapse to one; give it a
        capture of its own to report them separately.

        A root yields one match per distinct binding:
        `int_add(anything().capture(k), anything())` on `int_add(x, y)`
        returns both `k=x` and `k=y`. `.ordered()` on a binary op pins operand
        slots.
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
        not exactly one after dedup. Arguments behave as in `find_all`, so an
        ambiguous commutative capture raises; pin it with `.ordered()`.

        `ignore_root=True` with a pattern that binds no captures succeeds on
        any number of occurrences, returning an arbitrary one.
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
        """The constant bound to `capture` across all matches of `pat`,
        deduplicated by value: `None` when no match binds a constant there,
        `StriderError` when two or more distinct values are bound. Matches
        that differ structurally but agree on the value collapse to one.

        `signed=True` reads it as two's-complement (`-8`, not a large unsigned
        pattern). `pat` and `constraints` behave as in `find_all`."""
        ...
    def rewrite(self, find: _PatLike, replace: _ReplaceLike) -> int:
        """Apply one `find` to `replace` rewrite across the graph, returning
        how many times it fired. `replace` is a `strider.template.Template`,
        a `Capture` or a capture name; a `strider.pattern.Pat` raises
        `TypeError`, one nested in a `Template` `StriderError`. A `.when()`
        guard on `find` raises. A `replace` that builds nodes raises
        `StriderError` where the match holds no instruction-produced node (an
        `initial_var` or `phi` root), as the new nodes would carry no address.

        A match is skipped (not counted) when the replacement has another
        width, or, for a `find` through a phi or region, would read the
        matched value or is not available everywhere that value is read.

        One walk, not a fixed point. Every outstanding `Node` handle goes
        stale, even on a return of 0."""
        ...
    def rewrite_all(self, pairs: Sequence[tuple[_PatLike, _ReplaceLike]]) -> int:
        """Apply the first matching `(find, replace)` pair at every reachable
        node, returning the total fire count. Stales `Node` handles as
        `rewrite` does."""
        ...
    def clone(self) -> "Function":
        """A deep, independent copy of this function, sharing the parent
        `Cfg`."""
        ...
