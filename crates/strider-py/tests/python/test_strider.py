import strider

from .conftest import symbol_addr


def test_analyze_cfg_returns_graph(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)

    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    outcome = s.analyze_cfg(cfg)
    assert isinstance(outcome.function, strider.Function)


def test_analyze_outcome_has_unresolved_branches_attr(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)

    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    outcome = s.analyze_cfg(cfg)
    # array_sum has no indirect branches.
    assert outcome.unresolved_branch_count == 0
