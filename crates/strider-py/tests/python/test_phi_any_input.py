"""`phi().any_input(p)` — match a Phi one of whose data inputs matches `p`,
without knowing which predecessor slot carries it.
"""

import strider
from strider import pattern as p


def _diamond_phi():
    # if (edi != 0) { eax = 1 } else { eax = 2 }; return eax
    #   31 ff              xor edi,edi        (edi = 0)  -- keep it simple:
    # Instead lift a real branch producing a two-input phi over eax:
    #   85 ff              test edi,edi
    #   75 05              jne  +5
    #   b8 01 00 00 00     mov  eax,1
    #   eb 03              jmp  +3   (over the else)   -> actually encode a clean diamond
    #   b8 02 00 00 00     mov  eax,2
    #   c3                 ret
    code = bytes([
        0x85, 0xff,                    # test edi, edi
        0x75, 0x07,                    # jne  +7  -> mov eax,2
        0xb8, 0x01, 0x00, 0x00, 0x00,  # mov eax, 1
        0xeb, 0x05,                    # jmp  +5  -> ret
        0xb8, 0x02, 0x00, 0x00, 0x00,  # mov eax, 2
        0xc3,                          # ret
    ])
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())
    return fn


def test_any_input_matches_either_branch_value_regardless_of_slot():
    fn = _diamond_phi()
    # A phi over the two branch values exists; each constant sits on one slot,
    # and any_input must find it without us naming the predecessor.
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)))) >= 1
    assert len(fn.find_all(p.phi().any_input(p.int_const(2)))) >= 1
    # A value present on no phi input matches nothing.
    assert fn.find_all(p.phi().any_input(p.int_const(99))) == []


def test_multiple_any_input_bind_distinct_slots():
    fn = _diamond_phi()  # phi over the constants 1 and 2
    # 1 and 2 sit on different slots -> distinct match.
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.int_const(2)))) == 1
    # two any_input(1) need two DIFFERENT inputs equal to 1; only one exists.
    assert fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.int_const(1))) == []
    # 1 on one slot, any const on the other -> distinct match.
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.any_int_const()))) == 1


def test_any_input_binds_captures_out():
    fn = _diamond_phi()
    c = p.Capture()
    hits = fn.find_all(p.phi().any_input(p.any_int_const().capture(c)))
    assert len(hits) >= 1
    assert hits[0].uint(c) in (1, 2)
