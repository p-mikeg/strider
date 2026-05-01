import strider


def test_sleigh_construct_with_memory_map():
    arch = strider.SleighArch.x86_64()
    mem = strider.MemoryMap()
    mem.add_region(0x1000, b"\x90\x90\x90\x90")  # 4 NOPs
    sleigh = strider.Sleigh(arch, mem)
    assert sleigh is not None
    assert sleigh.arch_name() == "x86_64"
    assert "Sleigh" in repr(sleigh)
