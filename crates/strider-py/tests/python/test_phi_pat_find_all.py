"""Regression: `pattern.mem_phi()` was missing from the union the Python
boundary coerces Pat-like inputs through, so `find_all(mem_phi())` raised
`TypeError`.  Match count is irrelevant here; the boundary is the target.
"""

import strider
import strider.pattern as pat


def test_find_all_mem_phi_pat_does_not_raise():
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    # add: leaq (%rdi,%rsi), %rax; retq
    bytes_ = bytes([0x48, 0x8d, 0x04, 0x37, 0xc3])
    mem = strider.reader.BufferReader(0x1000, bytes_)
    _cfg, g, _unresolved = strider.lift.lifter(arch, mem).analyze(0x1000, cc)
    # An empty list is fine; not raising is the point.
    matches = g.find_all(pat.mem_phi())
    assert isinstance(matches, list)


def test_phi_nests_as_a_value_operand():
    """Regression: `phi()` produces a value output, so it must nest as a value
    operand (`store(data=phi())`, `truncate(phi())`, `add(x, phi())`).  It was
    node-rooted only and raised "PhiPat cannot be nested as a value operand".
    `mem_phi()` is a memory token and stays correctly value-rejected.
    """
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    # Diamond: test edi,edi; je; mov eax,1 / mov eax,2; mov [rbp-4],eax; ret
    # Gives a value Phi at the merge, truncated then stored to the slot.
    code = bytes(
        [0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0, 0xEB, 0x05,
         0xB8, 0x02, 0, 0, 0, 0x89, 0x45, 0xFC, 0xC3]
    )
    mem = strider.reader.BufferReader(0x1000, code)
    _cfg, g, _u = strider.lift.lifter(arch, mem).analyze(0x1000, cc)

    assert len(g.find_all(pat.phi())) == 1
    # Nests as a value operand, reaching the stored value through the
    # intervening width cast.
    assert len(g.find_all(pat.store(data=pat.phi()), ignore_casts=True)) == 1
    assert len(g.find_all(pat.truncate(pat.phi()))) == 1

    # A MemPhi is a memory token, not a value, so `data=` still rejects it.
    import pytest

    with pytest.raises(strider.StriderError):
        g.find_all(pat.store(data=pat.mem_phi()))
