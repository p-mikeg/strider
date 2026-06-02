"""Regression: `pattern.mem_phi()` and `pattern.value_phi()` are
accepted by `Function.find_all` without raising `TypeError`.

These pat builders were added but missing from `PatLike` (the union
the Python boundary uses to coerce Pat-like inputs), so calling
`g.find_all(pat.mem_phi())` raised before the fix.  The test runs
each find_all on a freshly-lifted small function and asserts only
that no exception is raised — match count is irrelevant, the
boundary check is the regression target.
"""

import strider
import strider.pattern as pat


def test_find_all_mem_phi_pat_does_not_raise():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    # add: leaq (%rdi,%rsi), %rax; retq
    bytes_ = bytes([0x48, 0x8d, 0x04, 0x37, 0xc3])
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    res = strider.run(arch=arch, cc=cc, mem=mem, entry=0x1000)
    g = res.function
    # Should not raise — empty list is fine, the test verifies the
    # Python boundary accepts MemPhiPat as a Pat-like input.
    matches = g.find_all(pat.mem_phi())
    assert isinstance(matches, list)


def test_find_all_value_phi_pat_does_not_raise():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    bytes_ = bytes([0x48, 0x8d, 0x04, 0x37, 0xc3])
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    res = strider.run(arch=arch, cc=cc, mem=mem, entry=0x1000)
    g = res.function
    matches = g.find_all(pat.value_phi())
    assert isinstance(matches, list)


# A function that stores a different register into the same stack slot on
# each arm of a branch and loads it back after the merge.  `LoadForward`
# forwards the two stores across the `MemPhi`, synthesizing an anonymous
# `Phi` (value-phi) at the merge — the exact shape `value_phi()` matches.
#
#   sub  $0x10,%rsp
#   test %edi,%edi
#   je   L1
#   mov  %edi,(%rsp)      ; arm A store
#   jmp  L2
# L1: mov %esi,(%rsp)     ; arm B store
# L2: mov  (%rsp),%eax    ; load at merge -> synthesized value phi
#   add  $0x10,%rsp
#   ret
_VALUE_PHI_CODE = bytes(
    [
        0x48, 0x83, 0xEC, 0x10,  # sub  $0x10,%rsp
        0x85, 0xFF,              # test %edi,%edi
        0x74, 0x05,              # je   L1 (0xd)
        0x89, 0x3C, 0x24,        # mov  %edi,(%rsp)
        0xEB, 0x03,              # jmp  L2 (0x10)
        0x89, 0x34, 0x24,        # L1: mov %esi,(%rsp)
        0x8B, 0x04, 0x24,        # L2: mov (%rsp),%eax
        0x48, 0x83, 0xC4, 0x10,  # add  $0x10,%rsp
        0xC3,                    # ret
    ]
)


def _value_phi_function():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    mem = strider.MemoryMap()
    mem.add_region(0x1000, _VALUE_PHI_CODE)
    return strider.run(arch=arch, cc=cc, mem=mem, entry=0x1000).function


def test_value_phi_when_predicate_is_honored():
    """Regression: `value_phi(...).when(pred)` must apply `pred`.

    The Python `value_phi` builder previously routed through a macro
    root flavor whose `build_pattern_py` never read `common.when`, so
    `.when()` was silently dropped.  This asserts the predicate is now
    honored: an always-False guard finds 0, an always-True guard
    preserves the unguarded count (which is >= 1 for this function).
    """
    g = _value_phi_function()

    unguarded = len(g.find_all(pat.value_phi()))
    assert unguarded >= 1, "fixture must lift at least one anonymous value phi"

    guarded_false = len(g.find_all(pat.value_phi().when(lambda m: False)))
    assert guarded_false == 0, ".when(False) must drop every value-phi match"

    guarded_true = len(g.find_all(pat.value_phi().when(lambda m: True)))
    assert guarded_true == unguarded, ".when(True) must preserve the count"
