"""Type stubs for strider.cfg: the control-flow graph and its options."""

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
    """Control-flow graph of a single function, produced by
    `Lifter.build_cfg` and returned as `AnalyzeResult.cfg`."""
    @property
    def lifter(self) -> Lifter:
        """The `Lifter` that built this CFG."""
        ...

    def to_dot(
        self,
        path: Optional[str] = ...,
        style: Optional[DotStyle] = ...,
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

        A lookup against this CFG's own decodes, so it is right across an
        ISA-mode switch (ARM/Thumb, MIPS16). An instruction that lifts to no
        p-code ops (`endbr64`) also gives `None`; `Lifter.pcode_at` returns
        `""` for it.
        """
        ...
    def fingerprint_pcode(self, node: Node) -> list[tuple[int, str]]:
        """The asm addresses recorded on `node` paired with their p-code
        text, sorted by address.

        An address this CFG has no decode for is skipped. Structural nodes
        (Entry, InitialMemory, InitialVar, Region, phis) give `[]`.

        Raises `StriderError` for a `node` from a function another `Cfg`
        lifted.
        """
        ...
    def entry(self) -> int:
        """The region index of the CFG entry, the default explorer center."""
        ...
    def neighborhood_dot(
        self,
        center: int,
        depth: int = ...,
        max_nodes: int = ...,
    ) -> str:
        """DOT for the regions within `depth` hops of region `center`
        (predecessors and successors), capped at `max_nodes`."""
        ...
    @property
    def incomplete(self) -> list[tuple[int, IncompleteReason]]:
        """Every machine address this CFG cannot vouch for, with why, sorted
        and deduplicated. Empty is complete. Check it before trusting an
        absence ("no store to X"): code behind a listed address may be missing.

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
          (a tail call to this address), or a call not to a
          `no_return_targets` entry would return here.

        A seeded arm that falls through into an unmapped, undecodable or
        past-the-end target is dropped and named there, unless a call precedes
        those bytes (the call then does not return). A branch from an arm's
        code to an unmapped target leaves through a stub and keeps the arm.

        From `analyze`, target reasons accumulate over the resolver's rounds
        and site reasons are the final round's.
        """
        ...
    def region_at(self, addr: int) -> Optional[int]:
        """The index of the region whose instruction range contains `addr`,
        else `None`. Where regions overlap, the one with the greatest start."""
        ...
    def _region_texts(self) -> dict:
        """Disassembly text for every region, keyed by region index. The
        search corpus for the CFG explorer."""
        ...

class CfgOptions:
    """Options controlling how the CFG is built.

    `function_max_size` bounds how far past the entry to decode; omit it for
    unbounded (`0` raises `ValueError`). Code that falls through past the
    bound tail-calls the address there, which `Cfg.incomplete` names
    `"past_function_end"`. `allow_code_before_start_addr`
    permits blocks below the entry address; it raises `ValueError` beside a
    size, which already fixes the extent.

    `known_targets` maps a dispatch address to its targets, or to `"return"`
    to seat it as a function return, instead of deferring to the classifier.
    An empty list seats nothing.

    `call_other_abis` classifies Sleigh user-op names, winning over the
    built-in table. Each value is a `strider.sleigh.CallOtherAbi`: one of
    the four footprint-free classes, or `CallOtherAbi.custom(...)` naming
    implicit registers.

    `no_return_targets` lists direct-call targets that never return (`exit`,
    `panic`): a call to one ends its block, under `build_cfg` and `analyze`
    alike. A target is the address the call instruction names. A Thumb or
    MIPS16 one is listed at its even address (`addr & ~1`). A ppc64 ELFv2
    call sharing the callee's TOC lands at `Symbol.local_entry`, which
    `ElfLifter.analyze` and `ElfLifter.build_cfg` add for every listed
    `address`; on a plain `Lifter`, list `local_entry` yourself.

    `data_ranges` lists `(start, end)` address ranges, end exclusive, that are
    data rather than code (an ARM literal pool): a branch or fall-through into
    one is reported in `Cfg.incomplete` as `"undecodable_target"`, never
    decoded. The instruction at the entry address decodes even inside one.
    `ElfLifter.analyze` and `ElfLifter.build_cfg` fill it from the ELF's
    mapping symbols when empty. A range that ends at or below its start
    raises `ValueError`.
    """

    @property
    def function_max_size(self) -> Optional[int]: ...
    @property
    def allow_code_before_start_addr(self) -> bool: ...
    # Read-only views, built once.
    @property
    def known_targets(self) -> Mapping[int, Union[tuple[int, ...], Literal["return"]]]: ...
    @property
    def call_other_abis(self) -> Mapping[str, CallOtherAbi]: ...
    @property
    def data_ranges(self) -> list[tuple[int, int]]: ...
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
        no_return_targets: Iterable[int] = ...,
    ) -> None:
        """Build the options. Raises `ValueError` for
        `function_max_size=0`, and for a size with
        `allow_code_before_start_addr=True`."""
        ...
