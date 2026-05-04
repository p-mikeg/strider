"""End-to-end Python smoke for `strider.run(compact=...)`."""

import strider
from strider import CallingConvention, MemoryMap, SleighArch


def _x86_64_strider():
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    return arch, cc


def _trivial_function_bytes():
    # 48 c7 c0 2a 00 00 00     mov rax, 42
    # c3                        ret
    return bytes([0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3])


def _run_with(compact: bool):
    arch, cc = _x86_64_strider()
    mem = MemoryMap()
    mem.add_region(0x1000, _trivial_function_bytes())
    return strider.run(arch, cc, mem, entry=0x1000, compact=compact)


def test_compact_default_true_does_not_grow_graph():
    """compact=True (default) must not produce more node ids than compact=False."""
    compact_result = _run_with(True)
    noncompact_result = _run_with(False)
    assert compact_result.graph.node_count() <= noncompact_result.graph.node_count()


def test_compact_default_is_true():
    """Calling strider.run without an explicit compact= keyword applies compaction."""
    arch, cc = _x86_64_strider()
    mem = MemoryMap()
    mem.add_region(0x1000, _trivial_function_bytes())
    default_result = strider.run(arch, cc, mem, entry=0x1000)
    explicit_result = _run_with(True)
    assert default_result.graph.node_count() == explicit_result.graph.node_count()
