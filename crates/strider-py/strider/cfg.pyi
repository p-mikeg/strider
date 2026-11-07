"""The control-flow graph and its options."""

from __future__ import annotations

from typing import Iterable, Literal, Mapping, Optional, Sequence, Union

from .ir import Node
from .lift import Lifter, StrPath
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
        path: Optional[StrPath] = ...,
        style: Optional[DotStyle] = ...,
    ) -> Optional[str]:
        """Render to DOT: the string when `path` is `None`, else written to
        `path`. `style` is the theme (default `"dark_cfg"`)."""
        ...
    def to_html(
        self, path: Optional[StrPath] = ..., style: Optional[DotStyle] = ...
    ) -> Optional[str]:
        """Render to a standalone HTML page: the string when `path` is
        `None`, else written to `path`. `style` is the theme (default
        `"dark_cfg"`)."""
        ...
    def pcode_at(self, addr: int) -> Optional[str]:
        """The p-code ops this CFG decoded at `addr`, joined with `"; "`, or
        `None`. Correct across an ISA-mode switch; on ARM or MIPS `addr`'s low
        bit is ignored, as in `Lifter.pcode_at`. An instruction lifting to no
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

        Two reasons name a dispatch site:

        - `"unresolved_branch"`: an indirect branch with no complete successor
          set, including a return not provably to the caller nor seeded
          `"return"`.
        - `"unverified_seed"`: a seated answer nothing verified (a
          `known_targets` seat, a consumed tail call, a `"return"` seed).

        Five name a branch target:

        - `"isa_mode_conflict"`: reached in two ISA modes.
        - `"interior_target"`: off every instruction boundary, or a region
          start a later decode stepped over.
        - `"unmapped_target"`: the image does not back the instruction there.
        - `"undecodable_target"`: holds no instruction, or is in `data_ranges`.
        - `"past_function_end"`: past `function_max_size` or a
          `fragment_ranges` end.

        A seeded arm whose own decode reaches unmapped or undecodable bytes,
        or falls through past the end, is dropped unless a call precedes them,
        which then ends the arm; one landing on a stub an earlier edge or
        fall-through reported keeps it. From `analyze`, target
        reasons accumulate over the resolver's rounds; site reasons are the
        final round's.
        """
        ...
    def region_at(self, addr: int) -> Optional[int]:
        """The region holding `addr`, else `None`; of overlapping ones, the
        latest start."""
        ...
    def _region_texts(self) -> dict:
        """P-code text per region index."""
        ...

class CfgOptions:
    """Options controlling how the CFG is built.

    - `function_max_size`: how far past the entry to decode (omit for
      unbounded); a fall-through past it is `"past_function_end"`.
    - `allow_code_before_start_addr`: follow branches below the entry;
      `ValueError` beside a size.
    - `known_targets`: a dispatch address's targets, or `"return"` to seat
      it as a function return. An empty list seats nothing. A Thumb or
      MIPS16 target goes at `addr & ~1`; each decodes in the dispatch's mode.
    - `call_other_abis`: Sleigh user-op names to a `CallOtherAbi`, over the
      built-in table; a name the arch's sla does not declare is ignored, so
      one table serves several arches.
    - `no_return_targets`: direct-call targets that never return, a Thumb or
      MIPS16 one at `addr & ~1`. On ppc64 ELFv2 a plain `Lifter` also needs
      each `Symbol.local_entry`; `ElfLifter` adds it.
    - `data_ranges`: `(start, end)` data ranges, end exclusive (an ARM
      literal pool); a branch into one is `"undecodable_target"`, never
      decoded, except at the entry.
    - `fragment_ranges`: ranges outside the bound that are still this
      function's code (GCC `<name>.cold`), followed on a branch; running past
      one's end is `"past_function_end"`.

    `ElfLifter` fills empty `data_ranges` / `fragment_ranges` from the ELF's
    `$d` and sized `<name>.cold[.N]` symbols. A range ending at or below its
    start raises `ValueError`.
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
