import strider

from .conftest import symbol_addr


def _build_graph(elf_path):
    addr = symbol_addr(elf_path, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf_path))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    return s.analyze_cfg(cfg).graph


def test_graph_to_html_writes_file(x86_memory_elf, tmp_path):
    g = _build_graph(x86_memory_elf)
    out = tmp_path / "graph.html"
    g.to_html(str(out))
    assert out.exists() and out.stat().st_size > 0


def test_graph_to_dot_writes_file(x86_memory_elf, tmp_path):
    g = _build_graph(x86_memory_elf)
    out = tmp_path / "graph.dot"
    g.to_dot(str(out))
    assert out.exists() and out.stat().st_size > 0


def test_graph_html_str_returns_html(x86_memory_elf):
    g = _build_graph(x86_memory_elf)
    html = g.html_str()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_graph_node_count_positive(x86_memory_elf):
    g = _build_graph(x86_memory_elf)
    assert g.node_count() > 0
