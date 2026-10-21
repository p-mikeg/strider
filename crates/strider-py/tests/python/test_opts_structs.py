from __future__ import annotations

import inspect
import sys

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


def _below_and_past_entry():
    # 0x1000: c3     ret           (below the entry)
    # 0x1010: 74 ee  je 0x1000
    # 0x1012: eb 0c  jmp 0x1020
    # 0x1020: c3     ret           (16 bytes past the entry)
    buf = bytearray([0xCC] * 0x21)
    buf[0x00] = 0xC3
    buf[0x10:0x14] = bytes([0x74, 0xEE, 0xEB, 0x0C])
    buf[0x20] = 0xC3
    mem = strider.reader.BufferReader(0x1000, bytes(buf))
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


@pytest.mark.parametrize(
    ("size", "allow", "expected"),
    [
        # expected: (function_max_size, allow_code_before_start_addr,
        #            decodes below the entry, decodes past 16 bytes), or the
        #            ValueError message.
        (None, False, (None, False, False, True)),
        (None, True, (None, True, True, True)),
        (0, False, "> 0"),
        (0, True, "> 0"),
        (0x10, False, (0x10, False, False, False)),
        (0x10, True, "allow_code_before_start_addr"),
    ],
)
def test_cfg_options_bound_kwargs(size, allow, expected):
    """Every `function_max_size` / `allow_code_before_start_addr` pair: a size
    fixes the extent, so the flag beside it is refused rather than ignored."""
    if isinstance(expected, str):
        with pytest.raises(ValueError, match=expected):
            strider.cfg.CfgOptions(function_max_size=size, allow_code_before_start_addr=allow)
        return
    opts = strider.cfg.CfgOptions(function_max_size=size, allow_code_before_start_addr=allow)
    cfg = _below_and_past_entry().build_cfg(0x1010, opts)
    assert (
        opts.function_max_size,
        opts.allow_code_before_start_addr,
        cfg.pcode_at(0x1000) is not None,
        cfg.pcode_at(0x1020) is not None,
    ) == expected


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


def test_assumptions_default_to_a_fresh_group_with_the_pipeline_claims_on():
    a = strider.lift.LifterOptions()
    b = strider.lift.LifterOptions()
    assert a.assumptions is not b.assumptions
    for name in (
        "stack_global_disjoint",
        "assume_incoming_args_survive_calls",
        "return_slots_survive_untracked_writes",
        "msr_register_keeps_mode",
    ):
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


def test_return_slots_survive_untracked_writes_defaults_true():
    assert (
        strider.lift.AssumptionOptions().return_slots_survive_untracked_writes is True
    )
    a = strider.lift.AssumptionOptions(return_slots_survive_untracked_writes=False)
    assert a.return_slots_survive_untracked_writes is False
    assert "return_slots_survive_untracked_writes=False" in repr(a)


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
    """`_with_cfg` replaces only `cfg`.

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
        return_slots_survive_untracked_writes=False,
        msr_register_keeps_mode=False,
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(function_max_size=64),
        compact=False,
        per_address_ccs={0x1000: strider.sleigh.CallingConvention.x86_64_systemv()},
        assumptions=assumptions,
        pipeline=pipeline,
    )

    out = opts._with_cfg(strider.cfg.CfgOptions(function_max_size=128))

    assert out.cfg.function_max_size == 128
    assert out.compact is False
    assert out.per_address_ccs is not None and 0x1000 in out.per_address_ccs
    assert out.assumptions is assumptions
    # Identity, not just presence: the carried-over pipeline must be the
    # SAME object, matching what passing `pipeline=opts.pipeline` did.
    assert out.pipeline is pipeline

    # `_with_cfg` copies, it does not mutate the receiver.
    assert opts.cfg.function_max_size == 64


def test_none_clears_every_claim():
    n = strider.lift.AssumptionOptions.none()
    # Enumerated, not listed: a claim added default-on has to trip this, which
    # is the whole reason `none` exists beside the constructor.
    claims = [
        f for f in dir(n) if not f.startswith("_") and not callable(getattr(n, f))
    ]
    assert claims, "no claims discovered"
    for name in claims:
        value = getattr(n, name)
        assert value is False or value == [], f"{name} is {value!r}, not cleared"


def test_none_differs_from_the_default_group():
    assert strider.lift.AssumptionOptions().stack_global_disjoint is True
    assert strider.lift.AssumptionOptions.none().stack_global_disjoint is False


# call 0x1080 ; jmp rax, with a `ret` at the callee
CALL_THEN_JMP_RAX = b"\xe8\x7b\x00\x00\x00\xff\xe0" + b"\xcc" * 0x79 + b"\xc3"


def test_a_no_return_target_ends_the_call_in_build_cfg_and_analyze():
    """`build_cfg` ignored the no-return mark `analyze` honoured, and decoded
    the `jmp rax` after the call."""
    mem = strider.reader.BufferReader(0x1000, CALL_THEN_JMP_RAX)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    cfg_opts = strider.cfg.CfgOptions(function_max_size=0x40, no_return_targets=[0x1080])
    built = lift.build_cfg(0x1000, cfg_opts)
    assert built.region_at(0x1005) is None
    assert built.incomplete == []
    result = lift.analyze(
        0x1000,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(cfg=cfg_opts),
    )
    assert result.cfg.region_at(0x1005) is None
    assert result.unresolved == []


def test_no_return_targets_read_back_as_a_frozenset():
    opts = strider.cfg.CfgOptions(no_return_targets=[0x2000, 0x2000, 0x1000])
    assert opts.no_return_targets == frozenset({0x1000, 0x2000})
    assert opts.no_return_targets is opts.no_return_targets


def test_cfg_options_repr():
    assert repr(strider.cfg.CfgOptions()) == (
        "CfgOptions(function_max_size=None, allow_code_before_start_addr=False, "
        "known_targets={0 entries}, call_other_abis={0 entries}, "
        "data_ranges=[0 ranges], no_return_targets={0 entries})"
    )
    assert repr(
        strider.cfg.CfgOptions(
            function_max_size=16, no_return_targets=[0x2000], data_ranges=[(0x10, 0x18)]
        )
    ) == (
        "CfgOptions(function_max_size=16, allow_code_before_start_addr=False, "
        "known_targets={0 entries}, call_other_abis={0 entries}, "
        "data_ranges=[1 ranges], no_return_targets={1 entries})"
    )


_ASSUMPTION_FIELDS = (
    "stack_global_disjoint",
    "assume_incoming_args_survive_calls",
    "distinct_sp_bases_disjoint",
    "callee_preserves_stack_args",
    "noalias_allocators",
    "escape_analysis",
    "return_slots_survive_untracked_writes",
    "msr_register_keeps_mode",
)


def test_assumption_options_defaults_are_the_lifter_options_defaults():
    """One default per field, Rust's, however the group is built."""
    built = strider.lift.AssumptionOptions()
    nested = strider.lift.LifterOptions().assumptions
    for name in _ASSUMPTION_FIELDS:
        assert getattr(built, name) == getattr(nested, name), name
    assert repr(built) == repr(nested)


def test_assumption_options_none_is_the_default_for_every_field():
    default = repr(strider.lift.AssumptionOptions())
    for name in _ASSUMPTION_FIELDS:
        assert repr(strider.lift.AssumptionOptions(**{name: None})) == default, name
    if sys.version_info >= (3, 10):  # abi3 exposes a class signature from 3.10
        params = inspect.signature(strider.lift.AssumptionOptions).parameters
        assert list(params) == list(_ASSUMPTION_FIELDS)
        assert all(p.default is None for p in params.values())


def test_assumption_options_repr():
    assert repr(strider.lift.AssumptionOptions()) == (
        "AssumptionOptions(stack_global_disjoint=True, "
        "assume_incoming_args_survive_calls=True, distinct_sp_bases_disjoint=False, "
        "callee_preserves_stack_args=False, noalias_allocators=[], "
        "escape_analysis=False, return_slots_survive_untracked_writes=True, "
        "msr_register_keeps_mode=True)"
    )


def test_noalias_allocators_read_back_sorted_and_deduplicated():
    top = 2**64 - 1
    a = strider.lift.AssumptionOptions(noalias_allocators=[top, 3, 3, 1])
    assert a.noalias_allocators == [1, 3, top]
    assert f"noalias_allocators=[1, 3, {top}]" in repr(a)


def test_msr_register_keeps_mode_defaults_true_and_round_trips():
    A = strider.lift.AssumptionOptions
    assert A().msr_register_keeps_mode is True
    assert A.none().msr_register_keeps_mode is False
    off = A(msr_register_keeps_mode=False)
    assert off.msr_register_keeps_mode is False
    assert "msr_register_keeps_mode=False" in repr(off)
    assert eval(repr(off), {"AssumptionOptions": A}) == off
    on = A.none(msr_register_keeps_mode=True)
    assert on.msr_register_keeps_mode is True
    assert (A.none() | on).msr_register_keeps_mode is True
    assert A.none() != on


#: dump_stack_lvl (arm 6.6-clang): mrs r5, cpsr; cpsid i around a retry loop
#: that saves again on its back edge, then msr cpsr_c, r5; pop {.., pc}.
_ARM_IRQ_RESTORE_LOOP = b"".join(
    w.to_bytes(4, "little")
    for w in (
        0xE92D4830, 0xE1A04000, 0xE10F5000, 0xF10C0080,
        0xEB00003A, 0xE3500000, 0x1A000004, 0xE121F005,
        0xEB000036, 0xE10F5000, 0xF10C0080, 0xEAFFFFF7,
        0xE1A00004, 0xEB000031, 0xE121F005, 0xE8BD8830,
    )
) + bytes(0x140)


@pytest.mark.parametrize("keeps", [True, False])
def test_msr_register_keeps_mode_reaches_the_lift(keeps):
    arch = strider.sleigh.SleighArch.arm()
    lift = strider.lift.lifter(
        arch, strider.reader.BufferReader(0x1000, _ARM_IRQ_RESTORE_LOOP)
    )
    claims = strider.lift.AssumptionOptions(msr_register_keeps_mode=keeps)
    cfg, function, unresolved = lift.analyze(
        0x1000,
        strider.sleigh.CallingConvention.arm_aapcs(),
        strider.lift.LifterOptions(assumptions=claims),
    )
    assert function.assumptions.msr_register_keeps_mode is keeps
    assert unresolved == ([] if keeps else [0x103C])
