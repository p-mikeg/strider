import strider

from .conftest import built_function, built_lifter_and_function, fixture_path


def _build_graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def _build_lifter_and_graph():
    return built_lifter_and_function("x86", "memory", "array_sum", optimize=False)


def test_pretty_render_is_a_flag_not_a_receiver():
    """Pretty renders need a `Sleigh` (register-name resolution, constant
    inlining, virtual nodes), which a `Function` reaches through its
    parent `Cfg`'s `Lifter`.  So pretty-vs-raw is chosen by the `pretty`
    FLAG on one method, not by which object you happen to call — the two
    renders no longer hide behind the same verb on different receivers."""
    graph = _build_graph()
    html = graph.to_html(pretty=True)
    assert isinstance(html, str) and len(html) > 0
    assert graph.to_html() != html, "pretty and raw must differ"


def test_pretty_to_html_writes_file(tmp_path):
    g = _build_graph()
    out = tmp_path / "graph.html"
    assert g.to_html(str(out), pretty=True) is None
    assert out.exists() and out.stat().st_size > 0


def test_pretty_to_dot_writes_file(tmp_path):
    g = _build_graph()
    out = tmp_path / "graph.dot"
    assert g.to_dot(str(out), pretty=True) is None
    assert out.exists() and out.stat().st_size > 0


def test_pretty_to_dot_returns_dot_str():
    g = _build_graph()
    dot = g.to_dot(pretty=True)
    assert isinstance(dot, str) and "digraph" in dot.lower()


def test_pretty_to_html_returns_html_str():
    g = _build_graph()
    html = g.to_html(pretty=True)
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_style_requires_pretty():
    """`style` themes the pretty render only.  Accepting-and-ignoring it
    on the raw path would be the same silent-success defect as the old
    unknown-style fallback."""
    import pytest

    g = _build_graph()
    with pytest.raises(strider.StriderError, match="pretty"):
        g.to_html(style="dark")
    with pytest.raises(strider.StriderError, match="unknown dot style"):
        g.to_html(pretty=True, style="not_a_theme")


def test_lifter_render_methods_removed():
    """The pretty renders moved onto `Function` behind `pretty=True`;
    the `Lifter`-side duplicates are gone, so one verb has one home."""
    lift, g = _build_lifter_and_graph()
    del g
    for gone in ("to_dot", "to_html", "dump_html", "dump_dot", "html_str"):
        assert not hasattr(lift, gone)


def test_elf_lifter_has_no_render_methods_either():
    """`ElfLifter` is a pure-Python `Lifter` subclass, so it inherits the
    removal too — rendering lives on the `Function` it returns."""
    prog = strider.lift.load_elf(str(fixture_path("x86", "memory")))
    for gone in ("to_dot", "to_html", "dump_html", "dump_dot", "html_str"):
        assert not hasattr(prog, gone)


def test_graph_node_count_positive():
    g = _build_graph()
    assert g.node_count() > 0


def test_raw_dot_is_one_node_per_reachable_node():
    # The raw renderer reflects the graph as stored: one DOT node per node
    # reachable from entry, no constant inlining or synthetic virtual nodes
    # (which the pretty renderer adds).  Reachable nodes are a subset of the
    # arena (g.node_ids()); the strict 1:1 + detached-exclusion contract is
    # pinned by the Rust unit test.
    g = _build_graph()
    dot = g.to_dot()
    assert isinstance(dot, str) and "digraph" in dot.lower()
    node_decls = [ln for ln in dot.splitlines() if "[label=" in ln and "->" not in ln]
    assert 0 < len(node_decls) <= len(g.node_ids()), (
        f"raw dot must have one node per reachable node (<= arena): "
        f"{len(node_decls)} decls vs {len(g.node_ids())} arena node ids"
    )


def test_raw_html_str_returns_html():
    g = _build_graph()
    html = g.to_html()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_to_raw_dot_and_to_raw_html_write_files(tmp_path):
    g = _build_graph()
    dot_out = tmp_path / "raw.dot"
    html_out = tmp_path / "raw.html"
    assert g.to_dot(str(dot_out)) is None
    assert g.to_html(str(html_out)) is None
    assert dot_out.exists() and dot_out.stat().st_size > 0
    assert html_out.exists() and html_out.stat().st_size > 0


def test_function_to_dot_str_and_file(tmp_path):
    fn = _build_graph()
    assert isinstance(fn.to_dot(), str)
    out = tmp_path / "f.dot"
    assert fn.to_dot(str(out)) is None and out.read_text()
    assert isinstance(fn.to_html(), str)
    assert isinstance(fn.neighborhood_dot(fn.entry_node()), str)
    for gone in (
        "raw_dot_str",
        "raw_html_str",
        "to_raw_dot",
        "to_raw_html",
        "raw_neighborhood_dot",
    ):
        assert not hasattr(fn, gone)
