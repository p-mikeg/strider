import strider

from .conftest import symbol_addr


def test_run_returns_run_result(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    result = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        entry=addr,
        rom=mem,
        allow_code_before_start_addr=True,
    )
    assert isinstance(result.cfg, strider.Cfg)
    assert isinstance(result.function, strider.Function)
    assert isinstance(result.sleigh, strider.Sleigh)
    assert result.function.node_count() > 0


def test_run_with_custom_pipeline(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    result = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        entry=addr,
        pipeline=pipe,
        allow_code_before_start_addr=True,
    )
    assert result.function.node_count() > 0
