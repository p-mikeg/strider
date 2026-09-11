"""Type stubs for strider.cfg: the control-flow graph and its options."""

from __future__ import annotations

from typing import Literal, Optional, Union

from .ir import Node
from .sleigh import CallOtherAbi

__all__: list[str]

#: Colour themes accepted by every `to_html` / `to_dot` renderer.
DotStyle = Literal["dark", "dark_cfg", "empty"]

class Cfg:
    """Control-flow graph of a single function, produced by
    `Lifter.build_cfg` and returned as `AnalyzeResult.cfg`."""

    def to_dot(
        self, path: Optional[str] = ..., style: Optional[DotStyle] = ...
    ) -> Optional[str]:
        """Render the CFG to DOT. Returns the DOT string when `path` is
        `None`, otherwise writes it to `path` and returns `None`.
        `style` selects the theme (default `"dark_cfg"`)."""
        ...
    def to_html(
        self, path: Optional[str] = ..., style: Optional[DotStyle] = ...
    ) -> Optional[str]:
        """Render the CFG to a standalone HTML page. Returns the HTML string
        when `path` is `None`, otherwise writes it and returns `None`.
        `style` selects the theme (default `"dark_cfg"`)."""
        ...
    def pcode_at(self, addr: int) -> Optional[str]:
        """The p-code lifted from the machine instruction at `addr`, joined
        with `"; "`, or `None` when this CFG stored no decode for `addr`.

        This is a lookup against decodes this CFG already made, so it is
        correct even on architectures where decoding depends on accumulated
        context (ARM/Thumb, MIPS16).

        An instruction that lifts to zero p-code operations (`endbr64`, say)
        leaves no trace here, so it is indistinguishable from an address that
        was never decoded: both give `None`. `Lifter.pcode_at` re-decodes and
        returns `""` for that case instead.
        """
        ...
    def fingerprint_pcode(self, node: Node) -> list[tuple[int, str]]:
        """The asm addresses recorded on `node` paired with their p-code
        text, sorted by address.

        The lookup companion to `Node.asm_fingerprint()`, which gives the
        addresses alone. An address not present in this CFG is skipped rather
        than emitted with empty text. Structural nodes (Entry, InitialMemory,
        InitialVar, Region, phis) carry no fingerprint and give `[]`.
        """
        ...
    def entry(self) -> int:
        """The region index of the CFG entry, the default explorer center."""
        ...
    def neighborhood_dot(
        self, center: int, depth: int = ..., max_nodes: int = ...
    ) -> str:
        """DOT for the regions within `depth` hops of region `center`
        (predecessors and successors), capped at `max_nodes`."""
        ...
    def isa_mode_conflicts(self) -> list[int]:
        """Machine addresses this CFG reached carrying two different ISA modes.

        One region owns the bytes, decoded in whichever mode was reached first,
        so the losing path's arm is not the instruction stream it believes. A
        direct edge can cause this, not only a resolved indirect branch, which
        is why it is reported here rather than in `unresolved`. Non-empty means
        part of this CFG is decoded in a mode some path into it disagrees with.

        Accumulated over every round `analyze` ran: a clash decodes the bytes
        twice and costs the site, whose next round rebuilds without the edge
        that raised it, so the final CFG alone would launder the report.
        """
        ...
    def interior_branch_targets(self) -> list[int]:
        """Branch targets interior to a region but off every instruction
        boundary.

        No region can start there -- decoding from inside an instruction yields
        a different stream -- so the edge is seated on the region owning the
        bytes, whose instructions start earlier. A direct edge can cause this,
        not only a resolved indirect branch. Non-empty means this CFG claims a
        successor the branch does not actually enter at.

        Accumulated over every round `analyze` ran: the inexact edge fed the
        classifier, whose derived targets outlive it, so a later round that no
        longer carries the edge does not undo what it cost.
        """
        ...
    def unverified_seeded_sites(self) -> list[int]:
        """Dispatch addresses nothing verified: a site seated with exactly the
        `known_targets` you supplied and nothing the classifier derived, plus
        every site the CFG consumed outright as a return or a tail call,
        whether that answer was seeded or derived.

        Not unresolved -- you asserted the answer -- but nothing checked it.
        Seating a seed changes the CFG the classifier reads, so a stale or
        wrong seed can stop it deriving and take the site's real arms with it.
        These are the sites where that cannot be ruled out. Always empty for a
        CFG from `build_cfg`, which runs no resolver.
        """
        ...
    def region_at(self, addr: int) -> Optional[int]:
        """The index of the region whose instruction range contains `addr`,
        else `None`."""
        ...
    def _region_texts(self) -> dict:
        """Disassembly text for every region, keyed by region index. The
        search corpus for the CFG explorer."""
        ...

class CfgOptions:
    """Options controlling how the CFG is built.

    `function_max_size` bounds how far past the entry to decode; omit it for
    unbounded (`0` raises `ValueError`). `allow_code_before_start_addr`
    permits blocks below the entry address.

    `call_other_abis` classifies Sleigh user-op names, winning over the
    built-in table. Each value is a `strider.sleigh.CallOtherAbi`: one of
    the four footprint-free classes, or `CallOtherAbi.custom(...)` naming
    implicit registers.
    """

    # Read-only: the options types are frozen, so a plain attribute
    # declaration would let a type checker accept `o.x = ...`, which raises at runtime.
    @property
    def function_max_size(self) -> Optional[int]: ...
    @property
    def allow_code_before_start_addr(self) -> bool: ...
    @property
    def known_targets(self) -> dict[int, Union[list[int], Literal["return"]]]: ...
    @property
    def call_other_abis(self) -> dict[str, CallOtherAbi]: ...
    def __init__(
        self,
        *,
        function_max_size: Optional[int] = ...,
        allow_code_before_start_addr: bool = ...,
        known_targets: dict[int, Union[list[int], Literal["return"]]] = ...,
        call_other_abis: dict[str, CallOtherAbi] = ...,
    ) -> None:
        """Build the options. Raises `ValueError` for
        `function_max_size=0`."""
        ...
