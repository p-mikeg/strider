"""End-to-end Python smoke for `strider.run(per_address_ccs=...)`."""

import strider
from strider import CallingConvention, MemoryMap, SleighArch
from strider.pattern import call


def _x86_64_call_then_ret_bytes():
    # Layout at 0x1000:
    #   0x1000  e8 fb 0f 00 00     call 0x2000
    #   0x1005  c3                 ret
    return bytes([0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3])


def test_call_to_overridden_address_lifts_without_error():
    """End-to-end: passing per_address_ccs through strider.run lifts
    successfully and yields the expected single-Call shape."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_call_then_ret_bytes())

    fentry_addr = 0x2000
    overrides = {fentry_addr: CallingConvention.x86_64_all_preserving()}

    overridden = strider.run(
        arch,
        cc,
        mem,
        entry=0x1000,
        per_address_ccs=overrides,
    )
    # The override path must lift without error and find exactly one
    # Call in the resulting graph.  The Rust integration test
    # `crates/strider/tests/per_address_cc.rs` pins the exact clobber-
    # count shrink; the Python side here pins the kwarg plumbing.
    assert len(overridden.function.find_all(call())) == 1


def test_per_address_ccs_default_empty_does_not_break_normal_calls():
    """Smoke check the default-empty path matches today's behaviour."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_call_then_ret_bytes())
    result = strider.run(arch, cc, mem, entry=0x1000)
    matches = result.function.find_all(call())
    assert len(matches) == 1


def test_x86_64_all_preserving_classmethod_exists():
    cc = CallingConvention.x86_64_all_preserving()
    assert cc.name() == "x86_64_all_preserving"


import pytest

from strider import OptimizerPipeline, opt
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


def _build_default_equivalent_pipeline(sleigh, sl, cc, mem):
    """Mirrors `Strider::build_optimizer_pipeline` from the Rust side
    (the passes `strider.run(pipeline=None)` runs internally).  Used to
    pin the bug: this pipeline must produce the same matches as the
    None default once the per_address_ccs plumbing is fixed.

    `mem` is unused — `LoadReadOnly()` receives its rom via the
    orchestrator's `OptCtx` plumbing (the caller passes
    `strider.run(..., rom=mem)`)."""
    del mem
    pipe = OptimizerPipeline.empty()
    pipe.add(opt.ConstantFold())
    pipe.add(opt.KnownBits())
    pipe.add(opt.RedundantPhis())
    pipe.add(opt.DeadBranchElimination())
    pipe.add(opt.LoadReadOnly())
    pipe.add(opt.LoadForward(sl, cc, sleigh))
    pipe.add_post(opt.FunctionArgDetect(sl, cc))
    pipe.add_post(opt.CallStackArgCollect(sl, cc))
    return pipe


@pytest.mark.parametrize(
    "use_custom_pipeline,with_override,expected_hits",
    [
        # No override: the hook at 0x2000 uses the default SysV CC and
        # clobbers RDI, so the sink's arg0 is the hook's clobber output, not
        # the function's arg0 — function_arg(0) must NOT match.
        (False, False, 0),  # default pipeline, no override
        (True, False, 0),   # custom pipeline,  no override
        # With an all-preserving override at 0x2000 the hook preserves RDI,
        # so the sink's arg0 is still the function's InitialVar(rdi) =
        # function_arg(0) — the pattern matches exactly once.  (This was
        # previously masked by stale arg-carrier ids surviving graph
        # compaction; the side-table is now remapped through compact().)
        (False, True, 1),   # default pipeline, override
        (True, True, 1),    # custom pipeline,  override
    ],
)
def test_per_address_ccs_honoured_in_both_pipeline_paths(
    use_custom_pipeline, with_override, expected_hits
):
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_arg_thru_hook_to_sink_bytes())
    sl = strider.Sleigh(arch, mem)

    overrides = (
        {0x2000: CallingConvention.x86_64_all_preserving()} if with_override else {}
    )
    pipeline = (
        _build_default_equivalent_pipeline(arch, sl, cc, mem)
        if use_custom_pipeline
        else None
    )

    res = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=0x1000,
        per_address_ccs=overrides,
        pipeline=pipeline,
    )
    pat = call().at(0x3000).arg(0, function_arg(0))
    hits = res.function.find_all(pat)
    assert len(hits) == expected_hits, (
        f"use_custom_pipeline={use_custom_pipeline} "
        f"with_override={with_override}: got {len(hits)} hits, expected {expected_hits}"
    )
