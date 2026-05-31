import strider

from .conftest import symbol_addr


def test_build_cfg_for_array_sum(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    assert cfg is not None


def test_cfg_to_html_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)

    out_html = tmp_path / "cfg.html"
    cfg.to_html(str(out_html))
    assert out_html.exists()
    assert out_html.stat().st_size > 0


def test_cfg_to_dot_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)

    out_dot = tmp_path / "cfg.dot"
    cfg.to_dot(str(out_dot))
    assert out_dot.exists()
    assert out_dot.stat().st_size > 0


def test_cfg_html_str_returns_html(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    html = cfg.html_str()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_build_cfg_leaves_sleigh_reusable(x86_memory_elf):
    """build_cfg borrows the inner Sleigh mutably for the duration of the
    build; the same PySleigh wrapper stays usable for the next build,
    and the first Cfg can still render via the shared handle.

    Regression guard: pins the borrow contract — re-introducing an
    ownership transfer that consumes the wrapper would fail here."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    mem = strider.load_elf(str(x86_memory_elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)

    cfg1 = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)

    # Reusing the same Sleigh wrapper for a second build must succeed.
    cfg2 = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    assert cfg2 is not None

    # The first Cfg must still render even after the Sleigh was reused
    # (both Cfgs borrow the same shared handle).
    html1 = cfg1.html_str()
    assert isinstance(html1, str)
    assert len(html1) > 0
