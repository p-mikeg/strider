"""`known_targets` keyed by the address `unresolved` reports.

`strider_cfg` keys the map by `PcodeInsnAddr` (machine address + p-code index),
because one machine instruction lifts to several p-code instructions and the
`BRANCHIND` is rarely the first. Python only ever sees machine addresses, so the
address a caller reads out of `unresolved` has to be the address that seats.
"""

from typing import Dict, List, Literal, Union

import strider
from .conftest import fixture_path

# `dispatch_value` dispatches through `jmp *0x4000ec(,%ecx,4)` at 0x40113e; the
# first two words of that table are the arms seeded below.
DISPATCH = 0x40113E
ARMS = [0x401145, 0x40114C]


def _resolution_off():
    return strider.lift.LifterOptions(resolve_indirect_branches=False)


def _program():
    return strider.lift.load_elf(str(fixture_path("x86", "switch")))


def test_unresolved_reports_the_dispatch_machine_address():
    _, _, unresolved = _program().analyze("dispatch_value", opts=_resolution_off())
    assert unresolved == [DISPATCH], f"expected one dispatch site, got {unresolved!r}"


def test_seating_a_reported_site_as_a_return_resolves_it():
    prog = _program()
    _, _, unresolved = prog.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: "return"}),
        ),
    )
    assert unresolved == [], f"seating {DISPATCH:#x} as a return left it unresolved"


def test_seating_a_reported_site_with_targets_builds_those_edges():
    prog = _program()
    cfg, _, unresolved = prog.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: ARMS}),
        ),
    )
    assert unresolved == [], f"seating {DISPATCH:#x} with targets left it unresolved"
    for arm in ARMS:
        assert cfg.region_at(arm) is not None, f"no region decoded at seeded arm {arm:#x}"


def test_seating_survives_the_symbol_name_entry_path():
    """A symbol-name entry rebuilds `CfgOptions` to apply the symbol size; the
    seed has to survive that rebuild."""
    prog = _program()
    by_name = prog.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: "return"}),
        ),
    )
    by_addr = prog.analyze(
        DISPATCH - 0xE,
        opts=strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: "return"}),
        ),
    )
    assert by_name.unresolved == by_addr.unresolved == []


def test_seating_an_empty_target_set_defers_the_site():
    """`{addr: []}` is a well-defined answer, not an error: no targets seat, so
    the site closes as an unresolved indirect branch and comes back in
    `unresolved`."""
    _, _, unresolved = _program().analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            resolve_indirect_branches=False,
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: []}),
        ),
    )
    assert unresolved == [DISPATCH]


def test_an_empty_seed_leaves_the_classifier_free_to_resolve():
    """The caller's seed is UNIONED with what the classifier proves, so an empty
    seed adds nothing rather than suppressing resolution."""
    cfg, _, unresolved = _program().analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(known_targets={DISPATCH: []}),
        ),
    )
    assert unresolved == []
    for arm in ARMS:
        assert cfg.region_at(arm) is not None


def test_a_seed_that_displaces_the_classifier_is_reported_unverified():
    """A seed the classifier never confirmed is not `unresolved` (the caller
    asserted the answer), but nothing checked it, so it is reported.

    Seeding x64 `main`'s dispatch with only itself changes the CFG the
    classifier reads: the selector stops deriving and the site converges
    holding just the seed.
    """
    dispatch = 0x401042
    result = strider.lift.load_elf(str(fixture_path("x64", "switch"))).analyze(
        "main",
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(known_targets={dispatch: [dispatch]}),
        ),
    )
    assert result.unresolved == []
    assert result.cfg.unverified_seeded_sites() == [dispatch]


def test_an_unseeded_analysis_reports_nothing_unverified():
    result = _program().analyze("dispatch_value")
    assert result.cfg.unverified_seeded_sites() == []


def test_a_link_register_seed_is_reported_as_unverified():
    """A `LinkRegister` seed becomes a `Return` at CFG-build time, leaving no
    placeholder and no switch anchor. Nothing downstream can tell the caller's
    answer replaced what the classifier would have derived, so the seat itself
    is the report."""
    code = bytes([0xB8, 0x08, 0x10, 0x00, 0x00, 0xFF, 0xE0, 0x90, 0xC3])
    base, dispatch = 0x1000, 0x1005

    def analyze(seed):
        mem = strider.reader.BufferReader(base, code)
        lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
        targets: Dict[int, Union[List[int], Literal["return"]]] = (
            {dispatch: "return"} if seed else {}
        )
        return lift.analyze(
            base,
            strider.sleigh.CallingConvention.x86_64_systemv(),
            strider.lift.LifterOptions(
                cfg=strider.cfg.CfgOptions(known_targets=targets)
            ),
        )

    assert analyze(seed=False).cfg.unverified_seeded_sites() == []
    assert analyze(seed=True).cfg.unverified_seeded_sites() == [dispatch]
