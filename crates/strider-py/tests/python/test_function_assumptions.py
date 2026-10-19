"""The claims in force accumulate per `Function`: `analyze` starts a fresh one
under the claims it is given, every `optimize` merges in the set its options
name, and none withdraws one."""

from __future__ import annotations

import re

import pytest

import strider
from strider.lift import AssumptionOptions, LifterOptions
from strider.pattern import load

_BASE = 0x1000
_IGNORE = strider.lift.OnIncomplete.IGNORE  # under `none()` the return is unverified

# `call 0x1100; mov qword [rax], 42; ret`: with 0x1100 a listed allocator the
# store is heap-rooted.
_HEAP_CODE = bytes.fromhex("e8fb000000" "48c7002a000000" "c3")
_ALLOCATOR = 0x1100

# `mov [rsp-8], rdi; mov qword [0x2000], 0; mov rax, [rsp-8]; ret`: the reload
# forwards past the constant-address store only under `stack_global_disjoint`.
_SPILL_CODE = bytes.fromhex(
    "48897c24f8" "48c704250020000000000000" "488b4424f8" "c3"
)

_FIELDS = (
    "stack_global_disjoint",
    "assume_incoming_args_survive_calls",
    "distinct_sp_bases_disjoint",
    "callee_preserves_stack_args",
    "noalias_allocators",
    "escape_analysis",
    "return_slots_survive_untracked_writes",
)


def _claims(a):
    return {name: getattr(a, name) for name in _FIELDS}


def _lifter(code):
    mem = strider.reader.BufferReader(_BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def _analyzed(code, assumptions, lift=None):
    lift = lift or _lifter(code)
    opts = LifterOptions(assumptions=assumptions, on_incomplete=_IGNORE)
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    return lift, lift.analyze(_BASE, cc, opts).function


def _heap(allocators=(_ALLOCATOR,)):
    return _analyzed(_HEAP_CODE, AssumptionOptions(noalias_allocators=list(allocators)))


def _spill_at_floor():
    lift, g = _analyzed(_SPILL_CODE, AssumptionOptions.none())
    assert len(g.find_all(load())) == 1, "the reload stays without the claim"
    return lift, g


def test_analyze_records_the_claims_it_ran_under():
    _lift, g = _heap()
    assert _claims(g.assumptions) == _claims(
        AssumptionOptions(noalias_allocators=[_ALLOCATOR])
    )


def test_each_analyze_starts_from_its_own_claims():
    """Per `Function`, not per `Lifter`: a later analysis on the same handle
    inherits nothing from an earlier one."""
    lift, first = _heap()
    _lift, second = _analyzed(_HEAP_CODE, AssumptionOptions.none(), lift=lift)
    assert _claims(second.assumptions) == _claims(AssumptionOptions.none())
    assert first.assumptions.noalias_allocators == [_ALLOCATOR]


def test_optimize_without_opts_adds_no_claim():
    lift, g = _spill_at_floor()
    lift.optimize(g)
    assert _claims(g.assumptions) == _claims(AssumptionOptions.none())
    assert len(g.find_all(load())) == 1, "optimize turned stack_global_disjoint on"


def test_options_that_name_no_claims_add_none():
    lift, g = _spill_at_floor()
    lift.optimize(g, opts=LifterOptions())
    assert len(g.find_all(load())) == 1, "LifterOptions() added the default claims"


def test_a_copy_keeps_whether_claims_were_named():
    lift, g = _spill_at_floor()
    lift.optimize(g, opts=LifterOptions()._with_cfg(strider.cfg.CfgOptions()))
    assert len(g.find_all(load())) == 1, "_with_cfg turned the defaults into a named set"


def test_optimize_keeps_the_allocator_in_force():
    lift, g = _heap()
    lift.optimize(g)
    assert g.assumptions.noalias_allocators == [_ALLOCATOR]


def test_a_named_set_that_omits_a_claim_in_force_raises():
    lift, g = _heap()
    narrower = LifterOptions(assumptions=AssumptionOptions(stack_global_disjoint=False))
    with pytest.raises(
        strider.StriderError,
        match=re.escape(
            "in force on this Function: stack_global_disjoint, noalias_allocators (0x1100). "
        ),
    ) as raised:
        lift.optimize(g, opts=narrower)
    assert "function.assumptions | <your set>" in str(raised.value)


def test_a_refused_optimize_runs_nothing():
    """Had it run, the defaults it names would forward the reload."""
    lift, g = _analyzed(_SPILL_CODE, AssumptionOptions.none(noalias_allocators=[_ALLOCATOR]))
    assert len(g.find_all(load())) == 1, "the reload stays without the claim"
    before = _claims(g.assumptions)
    with pytest.raises(strider.StriderError, match="noalias_allocators"):
        lift.optimize(g, opts=LifterOptions(assumptions=AssumptionOptions()))
    assert len(g.find_all(load())) == 1, "a refused optimize ran"
    assert _claims(g.assumptions) == before


def test_the_omission_names_only_the_missing_allocators():
    lift, g = _heap(allocators=(0x1300, _ALLOCATOR, 0x1200))
    partial = LifterOptions(assumptions=AssumptionOptions(noalias_allocators=[0x1200]))
    with pytest.raises(
        strider.StriderError,
        match=re.escape("in force on this Function: noalias_allocators (0x1100, 0x1300). "),
    ):
        lift.optimize(g, opts=partial)


def test_the_union_of_the_claims_in_force_adds_one():
    lift, g = _heap()
    wider = g.assumptions | AssumptionOptions.none(escape_analysis=True)
    lift.optimize(g, opts=LifterOptions(assumptions=wider))
    assert g.assumptions.escape_analysis is True
    assert g.assumptions.noalias_allocators == [_ALLOCATOR]
    assert g.assumptions.stack_global_disjoint is True


def test_none_with_a_keyword_adds_exactly_that_claim():
    lift, g = _spill_at_floor()
    wider = g.assumptions | AssumptionOptions.none(escape_analysis=True)
    lift.optimize(g, opts=LifterOptions(assumptions=wider))
    want = _claims(AssumptionOptions.none())
    want["escape_analysis"] = True
    assert _claims(g.assumptions) == want


def test_none_takes_each_claim_as_a_keyword():
    a = AssumptionOptions.none(distinct_sp_bases_disjoint=True, noalias_allocators=[5])
    want: dict[str, object] = {name: False for name in _FIELDS}
    want["distinct_sp_bases_disjoint"] = True
    want["noalias_allocators"] = [5]
    assert _claims(a) == want
    assert _claims(AssumptionOptions.none(escape_analysis=None)) == _claims(
        AssumptionOptions.none()
    )


def test_an_unknown_claim_raises_in_none_as_in_the_constructor():
    with pytest.raises(TypeError, match="escape_analyses"):
        AssumptionOptions(escape_analyses=True)  # type: ignore[call-arg]
    with pytest.raises(TypeError, match="escape_analyses"):
        AssumptionOptions.none(escape_analyses=True)  # type: ignore[call-arg]


def test_a_keyword_set_carries_its_defaults_into_the_union():
    """`AssumptionOptions(x=True)` fills every other claim with its default,
    three of them on, so on a `none()` function it adds four. Pinned so that
    changing it is a decision."""
    want = _claims(AssumptionOptions())
    want["escape_analysis"] = True
    assert _claims(AssumptionOptions(escape_analysis=True)) == want
    lift, g = _spill_at_floor()
    wider = g.assumptions | AssumptionOptions(escape_analysis=True)
    lift.optimize(g, opts=LifterOptions(assumptions=wider))
    assert _claims(g.assumptions) == want
    assert len(g.find_all(load())) == 0, "stack_global_disjoint did not forward"


def test_union_holds_every_claim_of_both():
    a = AssumptionOptions(
        stack_global_disjoint=False,
        assume_incoming_args_survive_calls=False,
        return_slots_survive_untracked_writes=False,
        escape_analysis=True,
        noalias_allocators=[3],
    )
    b = AssumptionOptions(
        stack_global_disjoint=False,
        assume_incoming_args_survive_calls=False,
        distinct_sp_bases_disjoint=True,
        noalias_allocators=[1, 3],
    )
    both = a.union(b)
    assert _claims(both) == {
        "stack_global_disjoint": False,
        "assume_incoming_args_survive_calls": False,
        "distinct_sp_bases_disjoint": True,
        "callee_preserves_stack_args": False,
        "noalias_allocators": [1, 3],
        "escape_analysis": True,
        "return_slots_survive_untracked_writes": True,
    }
    assert repr(a | b) == repr(both)
    assert a.noalias_allocators == [3], "union copies; the operands stay"


def test_clone_carries_the_claims():
    _lift, g = _heap()
    assert repr(g.clone().assumptions) == repr(g.assumptions)
