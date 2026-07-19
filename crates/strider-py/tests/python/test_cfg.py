import strider

from .conftest import symbol_addr


def test_build_cfg_for_array_sum(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert cfg is not None


def test_cfg_to_html_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    out_html = tmp_path / "cfg.html"
    assert cfg.to_html(str(out_html)) is None
    assert out_html.exists()
    assert out_html.stat().st_size > 0


def test_cfg_to_dot_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    out_dot = tmp_path / "cfg.dot"
    assert cfg.to_dot(str(out_dot)) is None
    assert out_dot.exists()
    assert out_dot.stat().st_size > 0


def test_cfg_to_html_returns_html_str(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    html = cfg.to_html()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_cfg_to_dot_str_and_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    dot_str = cfg.to_dot()
    assert isinstance(dot_str, str) and "digraph" in dot_str.lower()

    out = tmp_path / "c.dot"
    assert cfg.to_dot(str(out)) is None
    assert out.read_text()

    assert isinstance(cfg.to_html(), str)
    assert not hasattr(cfg, "html_str")
    assert not hasattr(cfg, "raw_neighborhood_dot")


def test_cfg_region_texts_renamed_private(x86_memory_elf):
    """`region_texts` is private (`_region_texts`); it backs explore.py's
    search bar and is not a stable public API."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert not hasattr(cfg, "region_texts")
    texts = cfg._region_texts()
    assert isinstance(texts, dict) and len(texts) > 0


def test_cfg_region_at(x86_memory_elf):
    """The CFG unit is a Region, so the accessor is `region_at`; there is
    no `block_at`."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    region_idx = cfg.region_at(addr)
    assert isinstance(region_idx, int)
    assert not hasattr(cfg, "block_at")


def test_build_cfg_leaves_lifter_reusable(x86_memory_elf):
    """build_cfg must not consume the Lifter: it stays usable for a second
    build, and earlier Cfgs still render (they render through the Lifter,
    which owns the Sleigh)."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)

    cfg1 = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    cfg2 = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert cfg2 is not None

    # Still renders after the Lifter was reused for cfg2.
    html1 = cfg1.to_html()
    assert isinstance(html1, str)
    assert len(html1) > 0
