"""The control-flow graph and its options."""

from __future__ import annotations

from typing import Iterable, Literal, Mapping, Optional, Sequence, Union

from .ir import Node
from .lift import Lifter
from .sleigh import CallOtherAbi

__all__: list[str]

#: Colour themes accepted by every `to_html` / `to_dot` renderer.
DotStyle = Literal["dark", "dark_cfg", "empty"]
#: Every `DotStyle`.
DOT_STYLES: tuple[DotStyle, ...]

#: Why `Cfg.incomplete` names an address.
IncompleteReason = Literal[
    "unresolved_branch",
    "unverified_seed",
    "isa_mode_conflict",
    "interior_target",
    "unmapped_target",
    "undecodable_target",
    "past_function_end",
]
#: Every `IncompleteReason`, in the order `Cfg.incomplete` sorts one address's.
INCOMPLETE_REASONS: tuple[IncompleteReason, ...]

class Cfg:
    """Control-flow graph of one function, from `Lifter.build_cfg` or
    `AnalyzeResult.cfg`."""
    @property
    def lifter(self) -> Lifter:
        """The `Lifter` that built this CFG."""
        ...

    def to_dot(
        self,
        path: Optional[str] = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Render to DOT: the string when `path` is `None`, else written to
        `path`. `style` is the theme (default `"dark_cfg"`)."""
        ...
    def to_html(
        self, path: Optional[str] = ..., style: Optional[DotStyle] = ...
    ) -> Optional[str]:
        """Render to a standalone HTML page: the string when `path` is
        `None`, else written to `path`. `style` is the theme (default
        `"dark_cfg"`)."""
        ...
    def pcode_at(self, addr: int) -> Optional[str]:
        """The p-code ops this CFG decoded at `addr`, joined with `"; "`, or
        `None`. Right across an ISA-mode switch. An instruction lifting to no
        ops (`endbr64`) also gives `None`; `Lifter.pcode_at` returns `""`.
        """
        ...
    def fingerprint_pcode(self, node: Node) -> list[tuple[int, str]]:
        """`node`'s asm addresses paired with their p-code, sorted, skipping
        an address this CFG has no decode for. Structural nodes (Entry,
        InitialMemory, InitialVar, Region, phis) give `[]`. Raises
        `StriderError` for a `node` of another `Cfg`'s function.
        """
        ...
    def entry(self) -> int:
        """The region index of the CFG entry."""
        ...
    def neighborhood_dot(
        self,
        center: int,
        depth: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """DOT of the regions within `depth` edges of region `center`, capped
        at `max_nodes`."""
        ...
    @property
    def incomplete(self) -> list[tuple[int, IncompleteReason]]:
        """Sorted `(machine_addr, reason)` pairs this CFG cannot vouch for;
        empty is complete. Check it before trusting an absence ("no store to
        X").

        `"unresolved_branch"` and `"unverified_seed"` name a dispatch site; the
        other five a branch target:

        - `"unresolved_branch"`: an indirect branch with no complete successor
          set, including a return not provably to the caller and not seeded
          `"return"`.
        - `"unverified_seed"`: a seated answer nothing verified: a
          `known_targets` seat, a consumed tail call, or a `"return"` seed.
        - `"isa_mode_conflict"`: reached in two ISA modes; the losing path
          decodes in the other.
        - `"interior_target"`: off every instruction boundary, or a region
          start a later decode stepped over.
        - `"unmapped_target"`: no byte of the image backs it.
        - `"undecodable_target"`: holds no instruction, or `data_ranges` marks
          it as data.
        - `"past_function_end"`: code falls through past `function_max_size`
          or a `fragment_ranges` end (a tail call to this address), or a call
          not to a `no_return_targets` entry would return here.

        A seeded arm that falls through into an unmapped, undecodable or
        past-the-end target is dropped and named there, unless a call precedes
        those bytes (the call then does not return). A branch from an arm's
        code to an unmapped target leaves through a stub and keeps the arm.

        From `analyze`, target reasons accumulate over the resolver's rounds
        and site reasons are the final round's.
        """
        ...
    def region_at(self, addr: int) -> Optional[int]:
        """The region holding `addr`, else `None`; of overlapping ones, the
        latest start."""
        ...
    def _region_texts(self) -> dict:
        """Disassembly text per region index."""
        ...

class CfgOptions:
    """Options controlling how the CFG is built.

    `function_max_size` bounds how far past the entry to decode (omit for
    unbounded); a fall-through past it is `"past_function_end"`.
    `allow_code_before_start_addr` permits blocks below the entry; it raises
    `ValueError` beside a size.

    `known_targets` maps a dispatch address to its targets, or to `"return"`
    to seat it as a function return. An empty list seats nothing.

    `call_other_abis` maps Sleigh user-op names to a
    `strider.sleigh.CallOtherAbi`, winning over the built-in table.

    `no_return_targets` lists direct-call targets that never return (`exit`,
    `panic`). A Thumb or MIPS16 one is listed at `addr & ~1`. A ppc64 ELFv2
    call sharing the callee's TOC lands at `Symbol.local_entry`, which
    `ElfLifter` adds for each listed `address`; on a plain `Lifter`, list
    it yourself.

    `data_ranges` lists `(start, end)` ranges, end exclusive, of data (an
    ARM literal pool): a branch or fall-through into one is
    `"undecodable_target"`, never decoded, except at the entry.
    `fragment_ranges` lists ranges outside the bound that are still this
    function's code (GCC `<name>.cold`): a branch into one is followed, and
    a fall-through past one's end is `"past_function_end"`. `ElfLifter`
    fills an empty one from the ELF's `$d` mapping symbols or sized
    `<name>.cold[.N]` symbols. A range ending at or below its start raises
    `ValueError`.
    """

    @property
    def function_max_size(self) -> Optional[int]: ...
    @property
    def allow_code_before_start_addr(self) -> bool: ...
    @property
    def known_targets(self) -> Mapping[int, Union[tuple[int, ...], Literal["return"]]]: ...
    @property
    def call_other_abis(self) -> Mapping[str, CallOtherAbi]: ...
    @property
    def data_ranges(self) -> list[tuple[int, int]]: ...
    @property
    def fragment_ranges(self) -> list[tuple[int, int]]: ...
    @property
    def no_return_targets(self) -> frozenset[int]: ...
    def __init__(
        self,
        *,
        function_max_size: Optional[int] = ...,
        allow_code_before_start_addr: bool = ...,
        known_targets: Mapping[int, Union[Sequence[int], Literal["return"]]] = ...,
        call_other_abis: Mapping[str, CallOtherAbi] = ...,
        data_ranges: Sequence[tuple[int, int]] = ...,
        fragment_ranges: Sequence[tuple[int, int]] = ...,
        no_return_targets: Iterable[int] = ...,
    ) -> None:
        """Raises `ValueError` for `function_max_size=0`, or a size with
        `allow_code_before_start_addr=True`."""
        ...
