"""The FunctionArgDetect alias tuning knobs must be reachable from the
high-level `analyze()` surface.

`MemAliasOptions` carries two real optimizer knobs
(`assume_distinct_sp_bases_disjoint` and `calls_clobber`) that were fully
plumbed through the Rust alias core but hardcoded to the `OptOptions`
default at the Python boundary, so no Python caller could ever set them.
These tests pin them as settable keyword arguments.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_analyze_accepts_function_args_alias_knobs():
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    function, _unresolved = s.analyze(
        "add",
        opts=strider.LifterOptions(
            assume_distinct_sp_bases_disjoint=True,
            calls_clobber=True,
        ),
    )
    assert function.node_count() > 0


def test_function_args_knobs_default_off_still_analyzes():
    """Omitting the knobs keeps the previous default behaviour."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    function, _unresolved = s.analyze("add")
    assert function.node_count() > 0


def test_analyze_accepts_alias_mode_strict():
    """The global SP-alias precision knob must be settable to the
    always-sound `strict` floor from the high-level surface."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    function, _unresolved = s.analyze("add", opts=strider.LifterOptions(alias_mode="strict"))
    assert function.node_count() > 0


def test_analyze_accepts_explicit_default_alias_mode():
    """Passing the default `stack_global_disjoint` explicitly behaves
    the same as omitting it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    function, _unresolved = s.analyze(
        "add", opts=strider.LifterOptions(alias_mode="stack_global_disjoint")
    )
    assert function.node_count() > 0


def test_analyze_rejects_unknown_alias_mode():
    """An unrecognised `alias_mode` is a typed ValueError, not a silent
    fall-through to the default."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    with pytest.raises(ValueError, match="alias_mode"):
        strider.LifterOptions(alias_mode="nonsense")
