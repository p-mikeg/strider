from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_analyze_takes_lifter_options():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))
    _cfg, g, unresolved = lift.analyze(
        "add", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=4096))
    )
    assert g.node_count() > 0


def test_build_cfg_takes_cfg_options():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))
    cfg = lift.build_cfg(
        lift.symbol("add").address, strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    )
    assert cfg is not None


def test_analyze_default_opts_when_omitted():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))
    _cfg, g, unresolved = lift.analyze("add")
    assert g.node_count() > 0


def test_build_cfg_default_opts_when_omitted():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))
    cfg = lift.build_cfg(lift.symbol("add").address)
    assert cfg is not None


def test_cfg_options_rejects_zero_function_max_size():
    with pytest.raises(ValueError):
        strider.cfg.CfgOptions(function_max_size=0)


def test_lifter_options_defaults_are_not_shared_mutable():
    """Two default `LifterOptions()` must not share one mutable default
    `CfgOptions` object across construction sites."""
    a = strider.lift.LifterOptions()
    b = strider.lift.LifterOptions()
    assert a.cfg is not b.cfg


def test_escape_analysis_defaults_false_and_round_trips():
    assert strider.lift.LifterOptions().assumptions.escape_analysis is False
    opts = strider.lift.LifterOptions(
        assumptions=strider.lift.AssumptionOptions(escape_analysis=True)
    )
    assert opts.assumptions.escape_analysis is True
    assert "escape_analysis=True" in repr(opts)


def test_assumptions_default_to_a_fresh_group_with_the_two_pipeline_claims_on():
    a = strider.lift.LifterOptions()
    b = strider.lift.LifterOptions()
    assert a.assumptions is not b.assumptions
    for name in ("stack_global_disjoint", "assume_incoming_args_survive_calls"):
        assert getattr(a.assumptions, name) is True
    for name in (
        "distinct_sp_bases_disjoint",
        "callee_preserves_stack_args",
        "escape_analysis",
    ):
        assert getattr(a.assumptions, name) is False
    assert a.assumptions.noalias_allocators == []


def test_incoming_args_survive_calls_defaults_true():
    assert strider.lift.AssumptionOptions().assume_incoming_args_survive_calls is True
    a = strider.lift.AssumptionOptions(assume_incoming_args_survive_calls=False)
    assert a.assume_incoming_args_survive_calls is False
    assert "assume_incoming_args_survive_calls=False" in repr(a)


def test_analyze_with_escape_analysis():
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))
    _cfg, g, _unresolved = lift.analyze(
        "add",
        opts=strider.lift.LifterOptions(
            assumptions=strider.lift.AssumptionOptions(escape_analysis=True)
        ),
    )
    assert g.node_count() > 0


def test_pipeline_override_runs_custom_pipeline():
    """`LifterOptions(pipeline=...)` overrides the pipeline for this call
    only: an empty pipeline leaves the graph less folded."""
    fixture = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(fixture))

    _cfg, default_fn, _unresolved = lift.analyze("add")

    _cfg, empty_fn, _unresolved2 = lift.analyze(
        "add",
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    )

    assert empty_fn.node_count() >= default_fn.node_count()
    assert empty_fn.node_count() > default_fn.node_count()


def test_with_cfg_carries_over_every_other_field():
    """`with_cfg` replaces only `cfg`.

    Fields are read-only, so overriding the nested `CfgOptions` once meant
    re-listing every field; anything forgotten silently reverted to its
    default.  Every non-cfg field here is set away from its default, so a
    dropped carry-over fails loudly.
    """
    pipeline = strider.opt.OptimizerPipeline.empty()
    assumptions = strider.lift.AssumptionOptions(
        stack_global_disjoint=False,
        assume_incoming_args_survive_calls=False,
        distinct_sp_bases_disjoint=True,
        callee_preserves_stack_args=True,
        noalias_allocators=[0x2000],
        escape_analysis=True,
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(function_max_size=64),
        compact=False,
        per_address_ccs={0x1000: strider.sleigh.CallingConvention.x86_64_systemv()},
        assumptions=assumptions,
        pipeline=pipeline,
    )

    out = opts.with_cfg(strider.cfg.CfgOptions(function_max_size=128))

    assert out.cfg.function_max_size == 128
    assert out.compact is False
    assert out.per_address_ccs is not None and 0x1000 in out.per_address_ccs
    assert out.assumptions is assumptions
    # Identity, not just presence: the carried-over pipeline must be the
    # SAME object, matching what passing `pipeline=opts.pipeline` did.
    assert out.pipeline is pipeline

    # `with_cfg` copies, it does not mutate the receiver.
    assert opts.cfg.function_max_size == 64
