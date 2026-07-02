"""End-to-end Python smoke for `Lifter.analyze(per_address_ccs=...)`."""

import strider
from strider import BufferReader, CallingConvention, SleighArch
from strider.pattern import call


def _x86_64_call_then_ret_bytes():
    # Layout at 0x1000:
    #   0x1000  e8 fb 0f 00 00     call 0x2000
    #   0x1005  c3                 ret
    return bytes([0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3])


def test_call_to_overridden_address_lifts_without_error():
    """End-to-end: passing per_address_ccs through `Lifter.analyze`
    lifts successfully and yields the expected single-Call shape."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = BufferReader(0x1000, _x86_64_call_then_ret_bytes())

    fentry_addr = 0x2000
    overrides = {fentry_addr: CallingConvention.x86_64_all_preserving()}

    function, _unresolved = strider.lifter(arch, mem).analyze(
        0x1000, cc, opts=strider.LifterOptions(per_address_ccs=overrides)
    )
    # The override path must lift without error and find exactly one
    # Call in the resulting graph.  The Rust integration test
    # `crates/strider/tests/per_address_cc.rs` pins the exact clobber-
    # count shrink; the Python side here pins the kwarg plumbing.
    assert len(function.find_all(call())) == 1


def test_per_address_ccs_default_empty_does_not_break_normal_calls():
    """Smoke check the default-empty path matches today's behaviour."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = BufferReader(0x1000, _x86_64_call_then_ret_bytes())
    function, _unresolved = strider.lifter(arch, mem).analyze(0x1000, cc)
    matches = function.find_all(call())
    assert len(matches) == 1


def test_x86_64_all_preserving_classmethod_exists():
    cc = CallingConvention.x86_64_all_preserving()
    assert cc.name() == "x86_64_all_preserving"


import pytest

from strider.pattern import call, function_arg


def _x86_64_arg_thru_hook_to_sink_bytes():
    """Layout at 0x1000:
        0x1000  48 89 ff           mov rdi, rdi  ; force RDI tracked
        0x1003  e8 f8 0f 00 00     call 0x2000   ; "hook" (clobbers rdi by default)
        0x1008  e8 f3 1f 00 00     call 0x3000   ; "sink" — we match its arg0
        0x100d  c3                 ret
    """
    return bytes(
        [
            0x48, 0x89, 0xFF,                    # mov rdi, rdi
            0xE8, 0xF8, 0x0F, 0x00, 0x00,        # call 0x2000
            0xE8, 0xF3, 0x1F, 0x00, 0x00,        # call 0x3000
            0xC3,                                 # ret
        ]
    )


# `use_custom_pipeline` used to parametrize a second axis exercising a
# hand-built pipeline passed via the old `strider.run(pipeline=...)`
# custom-pipeline entry point.  The single-`Lifter` collapse removed
# that knob — `Lifter.analyze` always drives the canonical default
# pipeline — so only the `with_override` axis remains meaningful.
@pytest.mark.parametrize(
    "with_override,expected_hits",
    [
        # No override: the hook at 0x2000 uses the default SysV CC and
        # clobbers RDI, so the sink's arg0 is the hook's clobber output, not
        # the function's arg0 — function_arg(0) must NOT match.
        (False, 0),
        # With an all-preserving override at 0x2000 the hook preserves RDI,
        # so the sink's arg0 is still the function's InitialVar(rdi) =
        # function_arg(0) — the pattern matches exactly once.  (This was
        # previously masked by stale arg-carrier ids surviving graph
        # compaction; the side-table is now remapped through compact().)
        (True, 1),
    ],
)
def test_per_address_ccs_honoured_in_both_pipeline_paths(with_override, expected_hits):
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = BufferReader(0x1000, _x86_64_arg_thru_hook_to_sink_bytes())

    overrides = (
        {0x2000: CallingConvention.x86_64_all_preserving()} if with_override else {}
    )

    function, _unresolved = strider.lifter(arch, mem, rom=mem).analyze(
        0x1000, cc, opts=strider.LifterOptions(per_address_ccs=overrides)
    )
    pat = call().at(0x3000).arg(0, function_arg(0))
    hits = function.find_all(pat)
    assert len(hits) == expected_hits, (
        f"with_override={with_override}: got {len(hits)} hits, expected {expected_hits}"
    )
