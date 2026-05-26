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
    g = res.graph
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
    g = res.graph
    matches = g.find_all(pat.value_phi())
    assert isinstance(matches, list)
