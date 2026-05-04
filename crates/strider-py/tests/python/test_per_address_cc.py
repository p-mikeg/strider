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
    cc = CallingConvention.x86_64_systemv_abi()
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
    assert len(overridden.graph.find_all(call())) == 1


def test_per_address_ccs_default_empty_does_not_break_normal_calls():
    """Smoke check the default-empty path matches today's behaviour."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_call_then_ret_bytes())
    result = strider.run(arch, cc, mem, entry=0x1000)
    matches = result.graph.find_all(call())
    assert len(matches) == 1


def test_x86_64_all_preserving_classmethod_exists():
    cc = CallingConvention.x86_64_all_preserving()
    assert cc.name() == "x86_64_all_preserving"
