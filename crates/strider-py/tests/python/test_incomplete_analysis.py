"""`OnIncomplete` decides whether `analyze` and `build_cfg` return a result
that `cfg.incomplete` names something in, or raise `IncompleteAnalysis`."""

import pytest
import strider

BASE = 0x1000
JMP_RAX = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])  # mov rax, [rdi] ; jmp rax
MOV_RET = bytes([0x48, 0xC7, 0xC0, 0x2A, 0, 0, 0, 0xC3])  # mov rax, 42 ; ret
ERROR = strider.lift.OnIncomplete.ERROR
IGNORE = strider.lift.OnIncomplete.IGNORE


def _lifter(code: bytes) -> strider.lift.Lifter:
    mem = strider.reader.BufferReader(BASE, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def _cc() -> strider.sleigh.CallingConvention:
    return strider.sleigh.CallingConvention.x86_64_systemv()


def _opts(on_incomplete: strider.lift.OnIncomplete) -> strider.lift.LifterOptions:
    return strider.lift.LifterOptions(on_incomplete=on_incomplete)


def test_error_raises_what_ignore_returns() -> None:
    ignored = _lifter(JMP_RAX).analyze(BASE, _cc(), _opts(IGNORE))
    with pytest.raises(strider.lift.IncompleteAnalysis) as raised:
        _lifter(JMP_RAX).analyze(BASE, _cc(), _opts(ERROR))
    assert isinstance(raised.value, strider.StriderError)
    assert raised.value.sites == ignored.cfg.incomplete == [(0x1003, "unresolved_branch")]
    assert str(raised.value) == "incomplete analysis: 0x1003 unresolved_branch"


def test_build_cfg_raises_under_error_only() -> None:
    lift = _lifter(JMP_RAX)
    cfg = lift.build_cfg(BASE, on_incomplete=IGNORE)
    with pytest.raises(strider.lift.IncompleteAnalysis) as raised:
        lift.build_cfg(BASE, on_incomplete=ERROR)
    assert raised.value.sites == cfg.incomplete


def test_a_complete_function_is_returned_under_error() -> None:
    lift = _lifter(MOV_RET)
    assert lift.analyze(BASE, _cc(), _opts(ERROR)).cfg.is_complete()
    assert lift.build_cfg(BASE, on_incomplete=ERROR).is_complete()


def test_on_incomplete_compares_by_value() -> None:
    assert strider.lift.OnIncomplete.ERROR == ERROR != IGNORE
    assert repr(ERROR) == "OnIncomplete.ERROR"
    assert _opts(IGNORE).on_incomplete == IGNORE


def test_on_incomplete_takes_only_its_type() -> None:
    """A string is refused, not read as a policy name."""
    with pytest.raises(TypeError):
        strider.lift.LifterOptions(on_incomplete="ignore")  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        _lifter(MOV_RET).build_cfg(BASE, on_incomplete="ignore")  # type: ignore[arg-type]


def test_a_seeded_retry_still_raises_on_its_seat() -> None:
    """Every reason raises for now, so seeding the reported site moves it
    from `unresolved_branch` to `unverified_seed` rather than completing it."""
    code = JMP_RAX + bytes([0xC3])  # ... ; ret at 0x1005
    with pytest.raises(strider.lift.IncompleteAnalysis) as first:
        _lifter(code).analyze(BASE, _cc(), _opts(ERROR))
    assert first.value.sites == [(0x1003, "unresolved_branch")]
    seeded = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(known_targets={0x1003: [0x1005]}),
        on_incomplete=ERROR,
    )
    with pytest.raises(strider.lift.IncompleteAnalysis) as retry:
        _lifter(code).analyze(BASE, _cc(), seeded)
    assert retry.value.sites == [(0x1003, "unverified_seed")]


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


def test_an_unmapped_target_raises_under_error() -> None:
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), _Reader(None))
    with pytest.raises(strider.lift.IncompleteAnalysis) as raised:
        lift.analyze(BASE, _cc(), _opts(ERROR))
    assert raised.value.sites == [(0x2000, "unmapped_target")]


def test_a_stashed_keyboard_interrupt_beats_the_raise() -> None:
    lift = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), _Reader(KeyboardInterrupt("read"))
    )
    with pytest.raises(KeyboardInterrupt):
        lift.analyze(BASE, _cc(), _opts(ERROR))
