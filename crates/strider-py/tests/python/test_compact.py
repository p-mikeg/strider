import strider
from strider.reader import BufferReader
from strider.sleigh import CallingConvention, SleighArch


def _x86_64_strider():
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv()
    return arch, cc


def _trivial_function_bytes():
    # 48 c7 c0 2a 00 00 00     mov rax, 42
    # c3                        ret
    return bytes([0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3])


def _run_with(compact: bool):
    arch, cc = _x86_64_strider()
    mem = BufferReader(0x1000, _trivial_function_bytes())
    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(0x1000, cc, opts=strider.lift.LifterOptions(compact=compact))
    return function


def test_compact_default_true_does_not_grow_graph():
    compact_function = _run_with(True)
    noncompact_function = _run_with(False)
    assert compact_function.node_count() <= noncompact_function.node_count()


def test_compact_default_is_true():
    arch, cc = _x86_64_strider()
    mem = BufferReader(0x1000, _trivial_function_bytes())
    lift = strider.lift.lifter(arch, mem)
    _cfg, default_function, _unresolved = lift.analyze(0x1000, cc)
    explicit_function = _run_with(True)
    assert default_function.node_count() == explicit_function.node_count()


def test_elf_analyze_compact_override_does_not_leak_into_next_call():
    """`compact=False` is a per-call override on a handle whose state is
    reused across calls, so a later `.analyze()` without it must get the
    default back.  Observable because an uncompacted arena keeps culled
    nodes, giving a much larger node_count.
    """
    from .conftest import fixture_path

    elf_path = fixture_path("x64", "arithmetic")
    elf = strider.lift.load_elf(str(elf_path))

    _cfg, uncompacted, _unresolved = elf.analyze("add", opts=strider.lift.LifterOptions(compact=False))
    _cfg, after, _unresolved = elf.analyze("add")  # no override; compact again
    _cfg, fresh, _unresolved = strider.lift.load_elf(str(elf_path)).analyze("add")

    assert uncompacted.node_count() > after.node_count()
    # The follow-up default call matches a fresh handle's default exactly,
    # so compact=False did not leak.
    assert after.node_count() == fresh.node_count()
