"""Type stubs for strider.cfg — the control-flow graph and its options."""

from __future__ import annotations

from typing import List, Literal, Optional, Tuple

from .ir import Node

#: Dot colour themes accepted by every `to_html` / `to_dot` renderer.
DotStyle = Literal["dark", "dark_cfg", "empty"]

class Cfg:
    """Control-flow graph of a single function, produced by
    `Lifter.build_cfg` / returned as element 0 of `Lifter.analyze`."""

    def to_dot(self, path: Optional[str] = ...) -> Optional[str]:
        """Render the CFG to DOT. Returns the DOT string when `path` is
        `None`, otherwise writes it to `path` and returns `None`."""
        ...
    def to_html(
        self, path: Optional[str] = ..., style: Optional[DotStyle] = ...
    ) -> Optional[str]:
        """Render the CFG to a standalone HTML page. Returns the HTML
        string when `path` is `None`, otherwise writes it and returns
        `None`. `style` selects the dot theme (default `"dark_cfg"`)."""
        ...
    def pcode_at(self, addr: int) -> Optional[str]:
        """Look up the lifted p-code for the machine instruction at
        `addr` — an exact LOOKUP against this CFG's own stored decodes
        (the exact lift-time context, correct even for context-dependent
        architectures like ARM/Thumb or MIPS16), never a fresh re-decode.

        Returns the joined p-code op text (ops rendered via their
        `rsleigh::Insn` `Display` impl, joined with `"; "`), or `None`
        when `addr` has no stored decode in this CFG.

        Known limitation: a machine instruction that lifts to ZERO
        p-code ops (e.g. `endbr64`) leaves no trace in the CFG at all,
        so such an address is indistinguishable from one never decoded
        — both return `None` here (`Lifter.pcode_at`, which re-decodes
        instead of looking up, still returns `""` for it)."""
        ...
    def fingerprint_pcode(self, node: Node) -> List[Tuple[int, str]]:
        """The asm-fingerprint of `node` as `(addr, text)` p-code pairs,
        sorted by address — the CFG-lookup companion to
        `Node.asm_fingerprint()` (addr-only).  Each fingerprint address is
        resolved via `pcode_at`; an address not present in this CFG is
        SKIPPED (not emitted with empty text).  `[]` for structural
        nodes with no fingerprint (Entry, InitialMemory, InitialVar,
        Region, phis)."""
        ...
    def entry(self) -> int:
        """The region index of the CFG entry — the default explorer
        center."""
        ...
    def neighborhood_dot(
        self, center: int, depth: int = ..., max_nodes: int = ...
    ) -> str:
        """Pretty neighborhood DOT around region `center` (BFS over
        predecessor+successor regions, capped at `max_nodes`; needs the
        Lifter's Sleigh to resolve register names)."""
        ...
    def region_at(self, addr: int) -> Optional[int]:
        """The region index whose instruction range contains `addr`,
        else `None`."""
        ...
    def _region_texts(self) -> dict:
        """Disassembly text for every region, keyed by region index —
        the text-search corpus for the CFG explorer's search bar."""
        ...

class CfgOptions:
    """Mirrors `strider_cfg::CfgOptions` (the user-facing subset — the
    orchestrator-internal `known_targets` feedback field is not
    exposed).  Raises `ValueError` for `function_max_size=0` (zero is
    meaningless — omit the argument for unbounded)."""

    function_max_size: Optional[int]
    allow_code_before_start_addr: bool
    def __init__(
        self,
        *,
        function_max_size: Optional[int] = ...,
        allow_code_before_start_addr: bool = ...,
    ) -> None: ...
