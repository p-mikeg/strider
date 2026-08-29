import pytest
import strider

BASE = 0x1000
# 0x1000: ff e0                 jmp rax
# 0x1002: 48 c7 c0 01 00 00 00  mov rax, 1
# 0x1009: c3                    ret
CODE = bytes([0xFF, 0xE0, 0x48, 0xC7, 0xC0, 0x01, 0, 0, 0, 0xC3])


def _lifter():
    return strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), strider.reader.BufferReader(BASE, CODE)
    )


def _decoded(cfg):
    return [a for a in (0x1000, 0x1002, 0x1009) if cfg.region_at(a) is not None]


def test_build_cfg_seats_known_targets():
    """`build_cfg` read three of `CfgOptions`' four fields and filled the rest
    from `default()`, so a seeded arm was silently never decoded."""
    opts = strider.cfg.CfgOptions(known_targets={0x1000: [0x1002]})
    assert _decoded(_lifter().build_cfg(BASE, opts)) == [0x1000, 0x1002, 0x1009]


def test_build_cfg_without_a_seed_leaves_the_arm_undecoded():
    assert _decoded(_lifter().build_cfg(BASE)) == [0x1000]


def test_a_seeded_build_cfg_does_not_claim_to_be_complete():
    """No classifier runs here, so nothing checked the caller's answer."""
    opts = strider.cfg.CfgOptions(known_targets={0x1000: [0x1002]})
    cfg = _lifter().build_cfg(BASE, opts)
    assert cfg.unverified_seeded_sites() == [0x1000]
    assert not cfg.is_complete()


def test_an_unseeded_build_cfg_reports_nothing_unverified():
    cfg = _lifter().build_cfg(BASE)
    assert cfg.unverified_seeded_sites() == []


def test_out_of_range_int_entry_reports_the_real_cause():
    """It used to say an `ElfLifter` was needed, to someone holding one."""
    with pytest.raises(strider.StriderError, match="out of range"):
        _lifter().analyze(2**64, strider.sleigh.CallingConvention.x86_64_systemv())
