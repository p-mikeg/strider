"""The sea-of-nodes IR: `Function` and `Node`."""

from __future__ import annotations

from typing import Optional, Sequence, Union

from .cfg import Cfg, DotStyle
from .lift import AssumptionOptions, StrPath
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

_ReplaceLike = Union[_Template, Capture, str]

class Node:
    """A handle on one IR node, from `Function.node(id)` or
    `Match.node(capture)`. Once `compact`, `optimize`, `rewrite` or
    `rewrite_all` has run, every accessor raises `StriderError`."""

    @property
    def id(self) -> int:
        """The node's integer id."""
        ...
    def kind(self) -> str:
        """The node's kind: an op or memory space carries its payload
        (`"IntBinaryOp(Add)"`, `"Load(RAM)"`), a float constant its raw bits
        (`"FloatConst(<bits>)"`), other kinds are bare (`"IntConst"`,
        `"InitialVar"`, `"CallOther"`)."""
        ...
    def inputs(self) -> list[Node]:
        """The producer behind each input edge, in input-slot order (once per
        slot)."""
        ...
    def sint(self) -> Optional[int]:
        """The integer constant, sign-extended at its width, else `None`. A
        boolean `True` reads as `-1`."""
        ...
    def uint(self) -> Optional[int]:
        """The integer constant, masked to its width, else `None`."""
        ...
    def boolean(self) -> Optional[bool]:
        """The boolean constant, else `None`."""
        ...
    def float_bits(self) -> Optional[int]:
        """A float constant's IEEE 754 bits, else `None`."""
        ...
    def vn(self) -> Optional[Vn]:
        """The varnode an `InitialVar` reads, a `Call`'s first return register
        (`None` when its convention has none), or a `CallOther` result is
        assigned to; else `None`."""
        ...
    def asm_fingerprint(self) -> list[int]:
        """The machine-instruction addresses recorded on this node, sorted
        and deduplicated: its lift's, each lift that reused it, and those of
        nodes folded into it. A superset of the instructions behind the
        value: a shared constant or initial register (the stack pointer, say)
        carries every address that used it, and so does a node folded with
        one, so the set can span most of the function and costs that much to
        read."""
        ...
    def outputs(self) -> list[Node]:
        """The consumers of this node's outputs, in output-slot then use order
        (once per edge)."""
        ...
    def call_other_name(self) -> Optional[str]:
        """A `CallOther`'s user-op name, else `None`. `"read_volatile"` reads
        a register that changes with nothing writing it (a cycle counter)."""
        ...
    def op(self) -> Optional[str]:
        """The operation of an op node (`"Add"`, `"Less"`, `"SignExtend"`),
        else `None`. A boolean op is an `IntBinaryOp` at `I1`."""
        ...
    def value_type(self) -> Optional[str]:
        """The value-output type (`"I1"`, `"I32"`, `"F64"`), or `None`
        without one. Accepted by `Pat.value_ty(...)`."""
        ...
    def __repr__(self) -> str: ...
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...

class Function:
    """A lifted function as a sea-of-nodes IR graph
    (`AnalyzeResult.function`)."""

    @property
    def cfg(self) -> "Cfg":
        """The `Cfg` this function was lifted from."""
        ...
    @property
    def assumptions(self) -> AssumptionOptions:
        """A copy of the claims in force: those `analyze` ran under plus every
        set `optimize` added since."""
        ...
    @property
    def optimizer_converged(self) -> bool:
        """False when the last optimizer run stopped at its iteration cap (the
        IR is valid but not fully folded) or raised."""
        ...
    def entry_node(self) -> int:
        """The id of the `Entry` node."""
        ...
    def optimize(
        self,
        assumptions: Optional[AssumptionOptions] = ...,
        pipeline: Optional[OptimizerPipeline] = ...,
    ) -> None:
        """Run `pipeline` (default: the default pipeline) over this function in
        place. `assumptions` must include every claim in force and may not add
        a lift-time one, else `StriderError`. Add a claim with::

            function.optimize(assumptions=function.assumptions
                              | AssumptionOptions.none(escape_analysis=True))

        Reads the producing `Lifter`'s `rom`, and raises `StriderError` while
        that `Lifter` is in `analyze`, `build_cfg` or `add_elf`. Stales `Node`
        / `Match` handles and node ids. Holds the GIL."""
        ...
    def to_dot(
        self,
        path: Optional[StrPath] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """The graph as DOT, or written to `path` (returning `None`).
        `pretty=False` renders it as stored; `pretty=True` inlines constants
        and names registers; a `DotStyle` name picks the pretty theme (an
        unknown one raises `StriderError`)."""
        ...
    def to_html(
        self,
        path: Optional[StrPath] = ...,
        *,
        pretty: Union[bool, DotStyle] = ...,
    ) -> Optional[str]:
        """`to_dot` wrapped in a self-contained HTML page."""
        ...
    def to_text(self, fingerprints: bool = ...) -> str:
        """The canonical text form: a header, then one line per node
        reachable from entry, `outs = op[payload] ins`. `fingerprints=True`
        appends each node's asm addresses `  @{..}`, which can make the text
        quadratic in the function's size. Values are numbered
        `%v0`, `%v1`, ... in print order, so equal graphs print equal text."""
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
        """DOT for the nodes within `depth` hops of `center`, at most
        `max_nodes`. A hub (fan-out over `hub_cap`, counting its inputs too
        when `count_producers`) is shown but not followed. `pretty` as in
        `to_dot`."""
        ...
    def node_count(self) -> int:
        """The number of node ids, reachable from entry or not."""
        ...
    def count_regions(self) -> int:
        """The number of `Region` nodes reachable from entry."""
        ...
    def node_ids(self) -> list[int]:
        """Every node id, reachable or not."""
        ...
    def node(self, node_id: int) -> Node:
        """A `Node` handle on `node_id`, else `StriderError`. A raw id is
        meaningful only until `compact`, `optimize` or `rewrite`; a `Node`
        then goes stale, while an int silently names something else."""
        ...
    def cfg_walk(self) -> list[Node]:
        """The nodes reachable from entry along control edges, by ascending
        id."""
        ...
    def data_walk(self) -> list[Node]:
        """Every node reachable from entry (data inputs, control outputs), in
        pre-order."""
        ...
    def walk(self, node_id: int) -> list[Node]:
        """Every node reachable from `node_id` (data inputs, control outputs),
        in pre-order."""
        ...
    def mem_walk(self) -> list[Node]:
        """The memory nodes (`Load`, `Store`, `Call`, `CallOther`, `MemPhi`,
        and the `InitialMemory` root) reached along memory edges, in
        pre-order, each once; empty when `InitialMemory` is unreachable. At a
        `MemPhi` a node can precede one of its memory predecessors."""
        ...
    def compact(self) -> None:
        """Drop every node unreachable from entry. Invalidates node ids."""
        ...
    def validate(self) -> Optional[str]:
        """Check the graph's invariants: `None` on success, else the error."""
        ...
    def find_all(
        self,
        pat: Union[_PatLike, Sequence[_PatLike]],
        ignore_root: bool = ...,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
    ) -> list[Match]:
        """Every deduplicated `Match` for `pat`, one per distinct binding:
        `int_add(anything().capture(k), anything())` on `int_add(x, y)` gives
        `k=x` and `k=y`; `.ordered()` pins operand slots. Each output
        `any_output()` binds is its own match; `m[c] ==` tells them apart.

        A list of patterns joins on shared `Capture`s, one merged `Match` per
        row, and `constraints` filter the rows. Rows differing only in the
        root of a pattern whose captures are all shared with an earlier one
        collapse to one. `ignore_root` keys dedup on captures alone.
        `ignore_casts` (`True`, or a `CastMask`) looks through extend /
        truncate / bit-reinterpret casts.
        """
        ...
    def find_unique(
        self,
        pat: Union[_PatLike, Sequence[_PatLike]],
        ignore_root: bool = ...,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
    ) -> Match:
        """The single `Match` for `pat` after dedup, else `StriderError`.
        Arguments as in `find_all`, so an ambiguous commutative capture
        raises; pin it with `.ordered()`. With `ignore_root=True` a pattern
        binding no captures succeeds on one or more occurrences, returning
        an arbitrary one."""
        ...
    def find_unique_value(
        self,
        pat: Union[_PatLike, Sequence[_PatLike]],
        capture: CaptureKey,
        ignore_casts: Union[bool, CastMask] = ...,
        constraints: Optional[Sequence[ConstraintLike]] = ...,
        signed: bool = ...,
    ) -> Optional[int]:
        """The one constant `capture` binds across the matches of `pat`: `None`
        when none binds one, `StriderError` for two distinct values or a
        capture no pattern binds. `signed=True` reads it as two's-complement.
        Arguments as in `find_all`."""
        ...
    def rewrite(self, find: _PatLike, replace: _ReplaceLike) -> int:
        """Apply one `find` -> `replace` rewrite in one walk (not a fixed
        point), returning how often it fired. `replace` is a `Template`, a
        `Capture` or a capture name (a `Pat` raises `TypeError`, one nested in
        a `Template` `StriderError`). Raises `StriderError` up front for a
        `.when()` guard on `find` or a capture `find` does not bind, and at a
        match holding no instruction-produced node (an `initial_var` or `phi`
        root) when `replace` builds nodes.

        A match is skipped when the replacement has another width, or, for a
        `find` through a phi or region, would read the matched value or is
        not available everywhere that value is read. Stales every `Node`
        handle, even on a return of 0."""
        ...
    def rewrite_all(self, pairs: Sequence[tuple[_PatLike, _ReplaceLike]]) -> int:
        """Apply the first matching `(find, replace)` pair at every reachable
        node, returning the total fire count. Stales `Node` handles."""
        ...
    def clone(self) -> "Function":
        """An independent copy, sharing the parent `Cfg`."""
        ...
