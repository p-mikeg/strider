import strider

from .conftest import symbol_addr


def test_build_cfg_for_array_sum(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    assert cfg is not None


def test_cfg_to_html_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)

    out_html = tmp_path / "cfg.html"
    cfg.to_html(str(out_html))
    assert out_html.exists()
    assert out_html.stat().st_size > 0


def test_cfg_to_dot_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)

    out_dot = tmp_path / "cfg.dot"
    cfg.to_dot(str(out_dot))
    assert out_dot.exists()
    assert out_dot.stat().st_size > 0


def test_cfg_html_str_returns_html(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    html = cfg.html_str()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_build_cfg_consumes_sleigh(x86_memory_elf):
    """Once build_cfg runs, the same PySleigh cannot be reused."""
    import pytest as _pytest
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    _ = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    with _pytest.raises(strider.errors.LiftError):
        strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
