"""The alias tuning knobs must stay reachable from `analyze()`.

`AssumptionOptions.distinct_sp_bases_disjoint` and
`assume_incoming_args_survive_calls` were once hardcoded to their defaults
at the Python boundary, so no caller could set them. These pin them as
settable keyword arguments.
"""

from __future__ import annotations


import strider

from .conftest import fixture_path


def test_analyze_accepts_function_args_alias_knobs():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add",
        opts=strider.lift.LifterOptions(
            assumptions=strider.lift.AssumptionOptions(
                distinct_sp_bases_disjoint=True,
                assume_incoming_args_survive_calls=False,
            ),
        ),
    )
    assert function.node_count() > 0


def test_function_args_knobs_default_off_still_analyzes():
    """Omitting the knobs keeps the previous default behaviour."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    assert function.node_count() > 0


def test_analyze_accepts_every_assumption_cleared():
    """The sound floor -- every claim cleared -- must be reachable from the
    high-level surface."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add",
        opts=strider.lift.LifterOptions(
            assumptions=strider.lift.AssumptionOptions(
                stack_global_disjoint=False,
                assume_incoming_args_survive_calls=False,
            )
        ),
    )
    assert function.node_count() > 0


def test_analyze_accepts_the_explicit_defaults():
    """Passing the two default-on claims explicitly behaves the same as
    omitting them."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add",
        opts=strider.lift.LifterOptions(
            assumptions=strider.lift.AssumptionOptions(
                stack_global_disjoint=True,
                assume_incoming_args_survive_calls=True,
            )
        ),
    )
    assert function.node_count() > 0
