"""Type stubs for strider.cfg: the control-flow graph and its options."""

from __future__ import annotations

from typing import Literal, Optional, Union

from .ir import Node
from .sleigh import CallOtherAbi

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

    function_max_size: Optional[int]
    allow_code_before_start_addr: bool
    known_targets: dict[int, Union[list[int], Literal["return"]]]
    call_other_abis: dict[str, CallOtherAbi]
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
