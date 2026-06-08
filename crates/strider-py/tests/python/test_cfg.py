import strider

from .conftest import symbol_addr


def test_build_cfg_for_array_sum(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    assert cfg is not None


def test_cfg_to_html_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)

    out_html = tmp_path / "cfg.html"
    cfg.to_html(str(out_html))
    assert out_html.exists()
    assert out_html.stat().st_size > 0


def test_cfg_to_dot_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)

    out_dot = tmp_path / "cfg.dot"
    cfg.to_dot(str(out_dot))
    assert out_dot.exists()
    assert out_dot.stat().st_size > 0


def test_cfg_html_str_returns_html(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    html = cfg.html_str()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_build_cfg_leaves_lifter_reusable(x86_memory_elf):
    """Lifter.build_cfg borrows the Lifter's owned Sleigh mutably for the
    duration of the build; the same Lifter stays usable for the next
    build, and the first Cfg can still render via its back-reference to
    the Lifter (which owns the Sleigh).

    Regression guard: pins the borrow contract — re-introducing an
    ownership transfer that consumes the Lifter would fail here."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)

    cfg1 = s.build_cfg(addr, allow_code_before_start_addr=True)

    # Reusing the same Lifter for a second build must succeed.
    cfg2 = s.build_cfg(addr, allow_code_before_start_addr=True)
    assert cfg2 is not None

    # The first Cfg must still render even after the Lifter was reused
    # (both Cfgs borrow the same owned Sleigh through the Lifter).
    html1 = cfg1.html_str()
    assert isinstance(html1, str)
    assert len(html1) > 0
