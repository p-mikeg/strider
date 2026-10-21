"""Type stubs for strider.cfg: the control-flow graph and its options."""

from __future__ import annotations

from typing import Iterable, Literal, Mapping, Optional, Sequence, Union

from .ir import Node
from .lift import Lifter
from .sleigh import CallOtherAbi

__all__: list[str]

#: Colour themes accepted by every `to_html` / `to_dot` renderer.
DotStyle = Literal["dark", "dark_cfg", "empty"]

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
        and deduplicated. Empty is complete. `analyze` and `build_cfg` return
        whatever this names, so check it before trusting an absence ("no
        store to X"): code behind a listed address may be missing.

        `"unresolved_branch"` and `"unverified_seed"` name a dispatch site, the
        address `CfgOptions(known_targets=...)` seeds; the other five a branch
        target, which a direct edge reaches too:

        - `"unresolved_branch"`: an indirect branch with no complete successor
          set, among them a return whose target is not provably the caller's
          and that you did not seed `"return"`.
        - `"unverified_seed"`: a seated answer nothing verified, such as a site
          seated with only your `known_targets`, every tail call the CFG
          consumed, your own `"return"` seat, and a return your `"return"`
          seed covers whose target is not provably the caller's.
        - `"isa_mode_conflict"`: reached in two ISA modes; the losing path
          decodes in the other.
        - `"interior_target"`: off every instruction boundary, or a region
          start a later decode stepped over.
        - `"unmapped_target"`: no byte of the image backs it. An unmapped
          seeded arm is named here and dropped.
        - `"undecodable_target"`: holds no instruction, or `data_ranges` marks
          it as data. A seeded arm leading to such bytes is dropped, and so is
          one whose decode fails for a reason no other entry names, named here
          at its start.
        - `"past_function_end"`: where code falls through past
          `function_max_size`, and so tail-calls this address, or where a call
          not to a `no_return_targets` entry would return; a seeded arm whose
          code falls through here is dropped and named here.

        From `build_cfg` this is that one build: every dispatch it left without
        an edge and every seed it seated. From `analyze` the target reasons
        accumulate over the resolver's rounds and the site reasons are the
        final round's.
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

    `call_other_abis` classifies Sleigh user-op names, winning over the
    built-in table. Each value is a `strider.sleigh.CallOtherAbi`: one of
    the four footprint-free classes, or `CallOtherAbi.custom(...)` naming
    implicit registers.

    `no_return_targets` lists direct-call targets that never return (`exit`,
    `panic`): a call to one ends its block, under `build_cfg` and `analyze`
    alike. A target is the address the call instruction names. A Thumb or
    MIPS16 one is listed at its decoded, even address: mask the ISA bit off a
    symbol value (`addr & ~1`). A ppc64 ELFv2 call sharing the callee's TOC
    lands at `Symbol.local_entry`, usually 8 bytes past `Symbol.address`.
    `ElfLifter.analyze` and `ElfLifter.build_cfg` add that local entry for
    every listed `address`; a plain `Lifter` takes the set as given, so list
    `local_entry` there.

    `data_ranges` lists `(start, end)` address ranges, end exclusive, that are
    data rather than code (an ARM literal pool): a branch or fall-through into
    one is reported in `Cfg.incomplete` as `"undecodable_target"`, never
    decoded. The instruction at the entry address decodes even inside one.
    `ElfLifter.analyze` and `ElfLifter.build_cfg` fill it from the ELF's
    mapping symbols when empty.
    A range that ends at or below its start raises `StriderError`: it names no
    address, and accepting it silently dropped the pool the caller meant to
    mark.
    """

    # Read-only: these are getters with no setter, so a plain attribute
    # declaration would let a type checker accept `o.x = ...`, which raises at
    # runtime.
    @property
    def function_max_size(self) -> Optional[int]: ...
    @property
    def allow_code_before_start_addr(self) -> bool: ...
    # The two tables are read-only VIEWS onto what the constructor stored,
    # built once and handed out unchanged: a seeded table can hold tens of
    # thousands of entries, and `analyze` reads it. `no_return_targets` is
    # likewise built once.
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
    def _with_function_max_size(self, function_max_size: int) -> "CfgOptions":
        """These options bounded to `function_max_size`, sharing both tables.
        Raises `ValueError` for `0`."""
        ...
    def _with_elf(self, elf: object) -> "CfgOptions":
        """These options as `ElfLifter` runs them on `elf`: its
        mapping-symbol data ranges unless `data_ranges` names some, and the
        local entry of every `no_return_targets` address that is a ppc64
        ELFv2 global entry."""
        ...
    def _seated_table_id(self) -> int:
        """The address of the seated `known_targets` table every analyse
        borrows, shared by every object derived from this one."""
        ...
    def _call_other_override_builds(self) -> int:
        """How many times `call_other_abis` was resolved for an analyse, by
        this object and every one derived from it."""
        ...
