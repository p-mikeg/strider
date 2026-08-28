import strider
from strider import pattern as p


def _diamond_phi():
    # A clean diamond, so eax merges at a two-input phi:
    # if (edi != 0) { eax = 2 } else { eax = 1 }; return eax
    code = bytes([
        0x85, 0xff,                    # test edi, edi
        0x75, 0x07,                    # jne  +7  -> mov eax,2
        0xb8, 0x01, 0x00, 0x00, 0x00,  # mov eax, 1
        0xeb, 0x05,                    # jmp  +5  -> ret
        0xb8, 0x02, 0x00, 0x00, 0x00,  # mov eax, 2
        0xc3,                          # ret
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def test_any_input_matches_either_branch_value_regardless_of_slot():
    fn = _diamond_phi()
    # Each constant sits on one slot, found without naming the predecessor.
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)))) >= 1
    assert len(fn.find_all(p.phi().any_input(p.int_const(2)))) >= 1
    assert fn.find_all(p.phi().any_input(p.int_const(99))) == []


def test_multiple_any_input_bind_distinct_slots():
    fn = _diamond_phi()  # phi over the constants 1 and 2
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.int_const(2)))) == 1
    # Two any_input(1) need two DIFFERENT inputs equal to 1; only one exists.
    assert fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.int_const(1))) == []
    assert len(fn.find_all(p.phi().any_input(p.int_const(1)).any_input(p.int_const()))) == 1


def test_any_input_binds_captures_out():
    fn = _diamond_phi()
    c = p.Capture()
    hits = fn.find_all(p.phi().any_input(p.int_const().capture(c)))
    assert len(hits) >= 1
    assert hits[0].uint(c) in (1, 2)
