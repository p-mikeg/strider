"""`analyze` and `build_cfg` return their result whatever `cfg.incomplete`
names."""

import pytest
import strider

BASE = 0x1000
JMP_RAX = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])  # mov rax, [rdi] ; jmp rax
MOV_RET = bytes([0x48, 0xC7, 0xC0, 0x2A, 0, 0, 0, 0xC3])  # mov rax, 42 ; ret


def _lifter(code: bytes) -> strider.lift.Lifter:
    mem = strider.reader.BufferReader(BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def _cc() -> strider.sleigh.CallingConvention:
    return strider.sleigh.CallingConvention.x86_64_systemv()


def test_analyze_returns_an_incomplete_result() -> None:
    result = _lifter(JMP_RAX).analyze(BASE, _cc())
    assert result.cfg.incomplete == [(0x1003, "unresolved_branch")]
    assert result.unresolved == [0x1003]


def test_build_cfg_returns_an_incomplete_cfg() -> None:
    assert _lifter(JMP_RAX).build_cfg(BASE).incomplete == [(0x1003, "unresolved_branch")]


def test_a_complete_function_names_nothing() -> None:
    lift = _lifter(MOV_RET)
    assert lift.analyze(BASE, _cc()).cfg.incomplete == []
    assert lift.build_cfg(BASE).incomplete == []


def test_a_seeded_retry_moves_the_site_to_its_seat() -> None:
    """Seeding the reported site moves it from `unresolved_branch` to
    `unverified_seed` rather than completing it."""
    code = JMP_RAX + bytes([0xC3])  # ... ; ret at 0x1005
    first = _lifter(code).analyze(BASE, _cc())
    assert first.cfg.incomplete == [(0x1003, "unresolved_branch")]
    seeded = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(known_targets={0x1003: [0x1005]}),
    )
    retry = _lifter(code).analyze(BASE, _cc(), seeded)
    assert retry.cfg.incomplete == [(0x1003, "unverified_seed")]
    assert retry.unresolved == []


class _Reader(strider.reader.MemReader):
    """`jmp 0x2000` at `BASE`; a read at 0x2000 raises `exc`, or misses."""

    CODE = bytes.fromhex("e9fb0f0000")

    def __init__(self, exc):
        super().__init__()
        self.exc = exc

    def read(self, addr, size):
        off = addr - BASE
        if 0 <= off < len(self.CODE):
            return self.CODE[off : off + size]
        if self.exc is not None and addr >= 0x2000:
            raise self.exc
        return None


def test_an_unmapped_target_is_returned() -> None:
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), _Reader(None))
    assert lift.analyze(BASE, _cc()).cfg.incomplete == [(0x2000, "unmapped_target")]


def test_a_stashed_keyboard_interrupt_still_surfaces() -> None:
    """The result is not raised, but the reader's interrupt is."""
    lift = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), _Reader(KeyboardInterrupt("read"))
    )
    with pytest.raises(KeyboardInterrupt):
        lift.analyze(BASE, _cc())


def test_elf_lifter_returns_by_name_too(x86_switch_elf) -> None:
    prog = strider.lift.load_elf(str(x86_switch_elf))
    result = prog.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    )
    assert result.unresolved == [
        0x40114B,
        0x401152,
        0x401159,
        0x401160,
        0x401167,
        0x401179,
        0x401180,
        0x401187,
    ]
    assert [(a, "unresolved_branch") for a in result.unresolved] == [
        e for e in result.cfg.incomplete if e[1] == "unresolved_branch"
    ]
