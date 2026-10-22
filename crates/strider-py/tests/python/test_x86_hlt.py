"""x86 `hlt` resumes at the next instruction once an interrupt is serviced,
as kernel code relies on. In user mode it is privileged and faults, which a
caller restores by reclassifying the `halt` user op as no-return."""

import strider
from strider.sleigh import CallingConvention, CallOtherAbi, SleighArch

_BASE = 0x1000
#: sti ; hlt ; cli ; ret
_IDLE = bytes([0xFB, 0xF4, 0xFA, 0xC3])


def _analyze(cfg_opts=None):
    lift = strider.lift.lifter(
        SleighArch.x86_64(), strider.reader.BufferReader(_BASE, _IDLE)
    )
    opts = strider.lift.LifterOptions(cfg=cfg_opts) if cfg_opts else None
    return lift.analyze(_BASE, CallingConvention.x86_64_systemv(), opts)


def _ends(result):
    return [
        line.split()[0]
        for line in result.function.to_text().splitlines()
        if line.split()[:1] in (["return"], ["unreachable"])
    ]


def test_hlt_falls_through_to_the_return():
    result = _analyze()
    assert result.cfg.incomplete == []
    assert _ends(result) == ["return"]


def test_a_no_return_halt_ends_the_function_at_the_hlt():
    """The userland reading: `_start`'s `hlt` after `__libc_start_main`."""
    opts = strider.cfg.CfgOptions(call_other_abis={"halt": CallOtherAbi.no_return()})
    result = _analyze(opts)
    assert result.cfg.incomplete == []
    assert _ends(result) == ["unreachable"]


def test_halt_is_the_last_user_op_so_no_other_id_moved():
    for arch in (SleighArch.x86_64(), SleighArch.x86()):
        names = strider.lift.lifter(
            arch, strider.reader.BufferReader(_BASE, _IDLE)
        ).user_op_names()
        assert names[-1] == "halt", arch.name()
        assert names.index("swi") == 16, arch.name()
