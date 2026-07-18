from .conftest import built_function, built_lifter_and_function


def _build_graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def _build_lifter_and_graph():
    return built_lifter_and_function("x86", "memory", "array_sum", optimize=False)


def test_pretty_dump_on_lifter():
    """Pretty renders need a `Sleigh` (register-name resolution, constant
    inlining, virtual nodes) which only the `Lifter` owns — a bare
    `Function` has none, so the Sleigh-needing `html_str` accessor stays
    off it entirely.  `Function.to_dot`/`to_html` DO exist, but they render
    the raw (as-stored, no-Sleigh) graph — pretty-vs-raw is decided by
    which object you call, not by the method name."""
    lift, graph = _build_lifter_and_graph()
    html = lift.html_str(graph)
    assert isinstance(html, str) and len(html) > 0
    assert not hasattr(graph, "html_str")


def test_lifter_dump_html_writes_file(tmp_path):
    lift, g = _build_lifter_and_graph()
    out = tmp_path / "graph.html"
    lift.dump_html(g, str(out))
    assert out.exists() and out.stat().st_size > 0


def test_lifter_dump_dot_writes_file(tmp_path):
    lift, g = _build_lifter_and_graph()
    out = tmp_path / "graph.dot"
    lift.dump_dot(g, str(out))
    assert out.exists() and out.stat().st_size > 0


def test_lifter_html_str_returns_html():
    lift, g = _build_lifter_and_graph()
    html = lift.html_str(g)
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


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
