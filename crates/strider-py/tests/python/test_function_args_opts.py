"""The alias tuning knobs must stay reachable from `analyze()`.

`AssumptionOptions.distinct_sp_bases_disjoint` and
`assume_incoming_args_survive_calls` were once hardcoded to their defaults
at the Python boundary, so no caller could set them. These pin them as
settable keyword arguments.
"""

from __future__ import annotations

import pytest

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
            ),
            assume_incoming_args_survive_calls=False,
        ),
    )
    assert function.node_count() > 0


def test_function_args_knobs_default_off_still_analyzes():
    """Omitting the knobs keeps the previous default behaviour."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    assert function.node_count() > 0


def test_analyze_accepts_alias_mode_strict():
    """The global SP-alias precision knob must be settable to the
    always-sound `strict` floor from the high-level surface."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add", opts=strider.lift.LifterOptions(alias_mode="strict"))
    assert function.node_count() > 0


def test_analyze_accepts_explicit_default_alias_mode():
    """Passing the default `stack_global_disjoint` explicitly behaves
    the same as omitting it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add", opts=strider.lift.LifterOptions(alias_mode="stack_global_disjoint")
    )
    assert function.node_count() > 0


def test_analyze_rejects_unknown_alias_mode():
    """An unrecognised `alias_mode` is a typed ValueError, not a silent
    fall-through to the default."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    with pytest.raises(ValueError, match="alias_mode"):
        strider.lift.LifterOptions(alias_mode="nonsense")
