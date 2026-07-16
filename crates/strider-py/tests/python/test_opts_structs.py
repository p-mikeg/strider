"""End-to-end Python smoke for `CfgOptions` / `LifterOptions` and the
per-function `LifterOptions.pipeline` override.

`Lifter.build_cfg` / `Lifter.analyze` (and `ElfLifter.analyze`) take a
single opts struct — `CfgOptions` / `LifterOptions` — instead of the old
kwargs pile.  See
`docs/superpowers/specs/2026-07-03-py-opts-pipelines-design.md`.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_analyze_takes_lifter_options():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(fixture))
    _cfg, g, unresolved = lift.analyze(
        "add", opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=4096))
    )
    assert g.node_count() > 0


def test_build_cfg_takes_cfg_options():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(fixture))
    cfg = lift.build_cfg(
        lift.symbol("add"), strider.CfgOptions(allow_code_before_start_addr=True)
    )
    assert cfg is not None


def test_analyze_default_opts_when_omitted():
    """Omitting `opts` entirely behaves like the all-defaults struct."""
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(fixture))
    _cfg, g, unresolved = lift.analyze("add")
    assert g.node_count() > 0


def test_build_cfg_default_opts_when_omitted():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(fixture))
    cfg = lift.build_cfg(lift.symbol("add"))
    assert cfg is not None


def test_cfg_options_rejects_zero_function_max_size():
    with pytest.raises(ValueError):
        strider.CfgOptions(function_max_size=0)


def test_lifter_options_rejects_bad_alias_mode():
    with pytest.raises(ValueError, match="alias_mode"):
        strider.LifterOptions(alias_mode="nonsense")


def test_lifter_options_defaults_are_not_shared_mutable():
    """Two default `LifterOptions()`/`CfgOptions()` instances must be
    independent — no shared mutable default object leaking across
    construction sites."""
    a = strider.LifterOptions()
    b = strider.LifterOptions()
    assert a.cfg is not b.cfg


def test_pipeline_override_runs_custom_pipeline():
    """`LifterOptions(pipeline=...)` overrides the default pipeline for
    this call only: an empty pipeline leaves the graph much less folded
    (more node ids) than the default pipeline does."""
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(fixture))

    _cfg, default_fn, _unresolved = lift.analyze("add")

    _cfg, empty_fn, _unresolved2 = lift.analyze(
        "add",
        opts=strider.LifterOptions(pipeline=strider.OptimizerPipeline.empty()),
    )

    # The empty pipeline runs no fixed-point passes at all, so it should
    # leave at least as many (and, on this fixture, strictly more) node
    # ids than the fully-optimised default pipeline.
    assert empty_fn.node_count() >= default_fn.node_count()
    assert empty_fn.node_count() > default_fn.node_count()


def test_with_cfg_carries_over_every_other_field():
    """`with_cfg` replaces only `cfg`.

    The fields are read-only, so overriding the nested `CfgOptions` used to
    mean re-listing all seven from Python — and anything the caller had set
    but forgot to re-list silently reverted to its default.  Every non-cfg
    field here is deliberately set away from its default, so a carry-over
    that drops one fails loudly.
    """
    pipeline = strider.OptimizerPipeline.empty()
    opts = strider.LifterOptions(
        cfg=strider.CfgOptions(function_max_size=64),
        compact=False,
        per_address_ccs={0x1000: strider.CallingConvention.x86_64_systemv()},
        calls_clobber=True,
        assume_distinct_sp_bases_disjoint=True,
        alias_mode="strict",
        pipeline=pipeline,
    )

    out = opts.with_cfg(strider.CfgOptions(function_max_size=128))

    # The replaced field.
    assert out.cfg.function_max_size == 128
    # ...and every other one, carried over intact.
    assert out.compact is False
    assert out.per_address_ccs is not None and 0x1000 in out.per_address_ccs
    assert out.calls_clobber is True
    assert out.assume_distinct_sp_bases_disjoint is True
    assert out.alias_mode == "strict"
    # Identity, not just presence: the carried-over pipeline must be the
    # SAME object, matching what passing `pipeline=opts.pipeline` did.
    assert out.pipeline is pipeline

    # The receiver is untouched — `with_cfg` copies, it does not mutate.
    assert opts.cfg.function_max_size == 64
