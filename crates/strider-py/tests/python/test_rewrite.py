"""Pattern → pattern rewrite tests against a real graph."""

import strider
from strider.pattern import Capture, var, add, int_const

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
    return s.analyze_cfg(cfg).function


def test_rewrite_returns_fire_count(x86_memory_elf):
    g = _build_graph(x86_memory_elf)
    x = Capture()
    # Identity-ish rewrite that may or may not fire — just verify the
    # call returns an integer.
    n = g.rewrite(find=add(var(x), int_const(0)), replace=var(x))
    assert isinstance(n, int)
    assert n >= 0


def test_rewrite_all_returns_fire_count(x86_memory_elf):
    g = _build_graph(x86_memory_elf)
    x, y = Capture(), Capture()
    pairs = [
        (add(var(x), int_const(0)), var(x)),
        (add(int_const(0), var(y)), var(y)),
    ]
    n = g.rewrite_all(pairs)
    assert isinstance(n, int)


def test_rewrite_then_reoptimize(x86_memory_elf):
    g = _build_graph(x86_memory_elf)
    x = Capture()
    g.rewrite(find=add(var(x), int_const(0)), replace=var(x))
    g.reoptimize()
    assert g.node_count() > 0
