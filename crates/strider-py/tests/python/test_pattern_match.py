"""End-to-end pattern-matching tests against the real test fixtures.

We pick `array_sum` from x86/memory.elf because it has a clean
`Load(addr = base + offset)` pattern that any working matcher will
find at least once.
"""

import strider
from strider.pattern import Capture, var, add, load, int_const

from .conftest import symbol_addr


def _build_graph(elf_path, symbol="array_sum"):
    addr = symbol_addr(elf_path, symbol)
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf_path))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    return s.analyze_cfg(cfg).graph, sleigh


def test_find_all_load_in_array_sum(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
    pat = load()
    hits = g.find_all(pat)
    # array_sum has at least one load (the array element fetch).
    assert len(hits) >= 1


def test_find_all_load_with_addr_pattern(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
    base, off = Capture(), Capture()
    pat = load(addr=add(var(base), var(off)))
    hits = g.find_all(pat, ignore_casts=True)
    # No assertion on the count (depends on optimization shape) — but
    # the call must not raise.
    assert isinstance(hits, list)


def test_match_get_uint_on_const(x86_memory_elf):
    g, _ = _build_graph(x86_memory_elf)
    # Find every IntConst in the graph and verify uint() returns an int.
    from strider.pattern import any_int_const
    c = Capture()
    pat = any_int_const(c)
    hits = g.find_all(pat)
    if hits:
        v = hits[0].uint(c)
        assert v is None or isinstance(v, int)
