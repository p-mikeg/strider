import strider
from strider.reader import BufferReader
from strider.sleigh import CallingConvention, SleighArch

from .conftest import fixture_path


def _mov_rax_42_ret():
    # 48 c7 c0 2a 00 00 00     mov rax, 42
    # c3                        ret
    lift = strider.lift.lifter(
        SleighArch.x86_64(),
        BufferReader(0x1000, bytes([0x48, 0xC7, 0xC0, 0x2A, 0x00, 0x00, 0x00, 0xC3])),
    )
    _cfg, function, _unresolved = lift.analyze(0x1000, CallingConvention.x86_64_systemv())
    return function


def test_to_text_prints_the_header_and_the_returned_constant():
    function = _mov_rax_42_ret()
    text = function.to_text()
    assert text.startswith("endian little\ntracked [")
    assert "= entry\n" in text
    assert "iconst[0x2a]\n" in text
    assert "\nreturn " in text
    assert "iconst[0x2a]  @{0x1000}" in function.to_text(fingerprints=True)


def test_to_text_is_the_same_for_two_analyses_a_clone_and_a_compacted_function():
    elf_path = str(fixture_path("x64", "arithmetic"))
    first = strider.lift.load_elf(elf_path).analyze("add").function
    second = strider.lift.load_elf(elf_path).analyze("add").function
    text = first.to_text(fingerprints=True)
    assert second.to_text(fingerprints=True) == text
    assert first.clone().to_text(fingerprints=True) == text
    second.compact()
    assert second.to_text(fingerprints=True) == text
