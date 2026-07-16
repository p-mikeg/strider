"""Regression: `pattern.mem_phi()` is accepted by `Function.find_all`
without raising `TypeError`.

`mem_phi()` was added but initially missing from `PatLike` (the union
the Python boundary uses to coerce Pat-like inputs), so calling
`g.find_all(pat.mem_phi())` raised before the fix.  The test runs
find_all on a freshly-lifted small function and asserts only that no
exception is raised — match count is irrelevant, the boundary check is
the regression target.
"""

import strider
import strider.pattern as pat


def test_find_all_mem_phi_pat_does_not_raise():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    # add: leaq (%rdi,%rsi), %rax; retq
    bytes_ = bytes([0x48, 0x8d, 0x04, 0x37, 0xc3])
    mem = strider.BufferReader(0x1000, bytes_)
    _cfg, g, _unresolved = strider.lifter(arch, mem).analyze(0x1000, cc)
    # Should not raise — empty list is fine, the test verifies the
    # Python boundary accepts MemPhiPat as a Pat-like input.
    matches = g.find_all(pat.mem_phi())
    assert isinstance(matches, list)


def test_phi_nests_as_a_value_operand():
    """Regression: `phi()` produces a value output, so it must nest as a value
    operand (`store(data=phi())`, `truncate(phi())`, `add(x, phi())`).  It used
    to be node-rooted only and raised "PhiPat cannot be nested as a value
    operand".  `mem_phi()` (a memory token) stays correctly value-rejected.
    """
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    # Diamond: test edi,edi; je; mov eax,1 / mov eax,2; mov [rbp-4],eax; ret
    # → a value Phi at the merge, truncated then stored to the stack slot.
    code = bytes(
        [0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0, 0xEB, 0x05,
         0xB8, 0x02, 0, 0, 0, 0x89, 0x45, 0xFC, 0xC3]
    )
    mem = strider.BufferReader(0x1000, code)
    _cfg, g, _u = strider.lifter(arch, mem).analyze(0x1000, cc)

    assert len(g.find_all(pat.phi())) == 1
    # phi nests as a value operand (no error), and reaches the stored value
    # through the intervening width cast.
    assert len(g.find_all(pat.store(data=pat.phi()), ignore_casts=True)) == 1
    assert len(g.find_all(pat.truncate(pat.phi()))) == 1

    # A MemPhi is a memory token, not a value — still rejected as `data=`.
    import pytest

    with pytest.raises(strider.errors.StriderError):
        g.find_all(pat.store(data=pat.mem_phi()))
