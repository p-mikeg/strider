"""The single lift handle: `strider.lift.lifter(arch, mem, rom=None) -> Lifter`.

Collapses the four former entry points (`strider.strider`/`Strider`,
`strider.run`, and the old low-level `Lifter`/`AnalyzeOutcome`) into one
handle with `build_cfg(entry, ...)` and `analyze(entry, cc, ...)`.
"""

import strider

from .conftest import symbol_addr


def test_lifter_analyze_returns_graph_and_unresolved(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()

    lift = strider.lift.lifter(arch, mem)  # no cc at construction
    _cfg, graph, unresolved = lift.analyze(  # cc per call, tuple result
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)


def test_lifter_build_cfg_returns_cfg(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()

    lift = strider.lift.lifter(arch, mem)
    cfg = lift.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert isinstance(cfg, strider.cfg.Cfg)


def test_lifter_analyze_accepts_rom(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()

    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, graph, unresolved = lift.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    assert graph.node_count() > 0
    assert unresolved == []
