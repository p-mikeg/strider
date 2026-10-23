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


def test_to_text_names_a_register_byte_order_unlike_the_data_order():
    # bx lr, little-endian code on both
    code = bytes([0x1E, 0xFF, 0x2F, 0xE1])
    cc = CallingConvention.arm_aapcs()
    for arch, header in (
        (SleighArch.arm_be_kernel(), "endian big register=little\n"),
        (SleighArch.arm(), "endian little\n"),
    ):
        lift = strider.lift.lifter(arch, BufferReader(0x1000, code))
        _cfg, function, _unresolved = lift.analyze(0x1000, cc)
        assert function.to_text().startswith(header)


def test_to_text_is_the_same_for_two_analyses_a_clone_and_a_compacted_function():
    elf_path = str(fixture_path("x64", "arithmetic"))
    first = strider.lift.load_elf(elf_path).analyze("add").function
    second = strider.lift.load_elf(elf_path).analyze("add").function
    text = first.to_text(fingerprints=True)
    assert second.to_text(fingerprints=True) == text
    assert first.clone().to_text(fingerprints=True) == text
    second.compact()
    assert second.to_text(fingerprints=True) == text


def _stack_address_to_a_call(opts=None):
    # 48 8d 7c 24 08     lea rdi, [rsp+8]
    # e8 f6 0f 00 00     call 0x2000
    # c3                 ret
    code = bytes([0x48, 0x8D, 0x7C, 0x24, 0x08, 0xE8, 0xF6, 0x0F, 0x00, 0x00, 0xC3])
    lift = strider.lift.lifter(SleighArch.x86_64(), BufferReader(0x1000, code))
    return lift.analyze(0x1000, CallingConvention.x86_64_systemv(), opts).function


def test_to_text_names_a_stack_address_a_call_clobbers():
    lines = _stack_address_to_a_call().to_text().splitlines()
    assert lines[2].startswith("default_cc ")
    assert lines[3].startswith("assumptions ")
    assert lines[4] == "frame_address_in_call_register reach=8"  # lea rdi, [rsp+8]
    assert "frame_address_in_call_register" not in _mov_rax_42_ret().to_text()


def test_the_clobbered_stack_address_survives_a_clone_and_a_compaction():
    function = _stack_address_to_a_call(strider.lift.LifterOptions(compact=False))
    text = function.to_text()
    assert "\nframe_address_in_call_register reach=8\n" in text
    assert function.clone().to_text() == text
    function.compact()
    assert function.to_text() == text
