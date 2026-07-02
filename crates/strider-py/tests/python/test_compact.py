"""End-to-end Python smoke for `Lifter.analyze(compact=...)`."""

import strider
from strider import BufferReader, CallingConvention, SleighArch


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
    lift = strider.lifter(arch, mem)
    function, _unresolved = lift.analyze(0x1000, cc, compact=compact)
    return function


def test_compact_default_true_does_not_grow_graph():
    """compact=True (default) must not produce more node ids than compact=False."""
    compact_function = _run_with(True)
    noncompact_function = _run_with(False)
    assert compact_function.node_count() <= noncompact_function.node_count()


def test_compact_default_is_true():
    """Calling `Lifter.analyze` without an explicit compact= keyword
    applies compaction."""
    arch, cc = _x86_64_strider()
    mem = BufferReader(0x1000, _trivial_function_bytes())
    lift = strider.lifter(arch, mem)
    default_function, _unresolved = lift.analyze(0x1000, cc)
    explicit_function = _run_with(True)
    assert default_function.node_count() == explicit_function.node_count()


# ── per-call override isolation on the persistent ElfStrider handle ──────


def test_elf_analyze_compact_override_does_not_leak_into_next_call():
    """`ElfStrider.analyze(target, compact=False)` is a per-call
    override on a persistent handle (the inner `Strider` is reused
    across calls).  A subsequent `.analyze()` WITHOUT the override must
    get the default (`compact=True`) — observable because the
    uncompacted arena keeps culled nodes (much larger node_count).
    """
    from .conftest import fixture_path

    elf_path = fixture_path("x64", "arithmetic")
    elf = strider.load_elf(str(elf_path))

    uncompacted = elf.analyze("add", compact=False)
    after = elf.analyze("add")  # no override — must be compact again
    fresh = strider.load_elf(str(elf_path)).analyze("add")

    # The override call itself observably differs from the default…
    assert uncompacted.function.node_count() > after.function.node_count()
    # …and the follow-up default call matches a fresh handle's default
    # exactly (no leak of compact=False into later calls).
    assert after.function.node_count() == fresh.function.node_count()
