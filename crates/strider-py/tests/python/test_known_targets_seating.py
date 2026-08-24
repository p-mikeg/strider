"""`known_targets` keyed by the address `unresolved` reports.

`strider_cfg` keys the map by `PcodeInsnAddr` (machine address + p-code index),
because one machine instruction lifts to several p-code instructions and the
`BRANCHIND` is rarely the first. Python only ever sees machine addresses, so the
address a caller reads out of `unresolved` has to be the address that seats.
"""

import strider

# `dispatch_value` dispatches through `jmp *0x4000ec(,%ecx,4)` at 0x40113e; the
# first two words of that table are the arms seeded below.
DISPATCH = 0x40113E
ARMS = [0x401145, 0x40114C]


def _resolution_off():
    return strider.lift.LifterOptions(resolve_indirect_branches=False)


def _program():
    return strider.lift.load_elf("fixtures/out/x86/switch.elf")


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
