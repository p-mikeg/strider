from .conftest import built_function


def _build_graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def test_graph_to_html_writes_file(tmp_path):
    g = _build_graph()
    out = tmp_path / "graph.html"
    g.to_html(str(out))
    assert out.exists() and out.stat().st_size > 0


def test_graph_to_dot_writes_file(tmp_path):
    g = _build_graph()
    out = tmp_path / "graph.dot"
    g.to_dot(str(out))
    assert out.exists() and out.stat().st_size > 0


def test_graph_html_str_returns_html():
    g = _build_graph()
    html = g.html_str()
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
    dot = g.raw_dot_str()
    assert isinstance(dot, str) and "digraph" in dot.lower()
    node_decls = [ln for ln in dot.splitlines() if "[label=" in ln and "->" not in ln]
    assert 0 < len(node_decls) <= len(g.node_ids()), (
        f"raw dot must have one node per reachable node (<= arena): "
        f"{len(node_decls)} decls vs {len(g.node_ids())} arena node ids"
    )


def test_raw_html_str_returns_html():
    g = _build_graph()
    html = g.raw_html_str()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_to_raw_dot_and_to_raw_html_write_files(tmp_path):
    g = _build_graph()
    dot_out = tmp_path / "raw.dot"
    html_out = tmp_path / "raw.html"
    g.to_raw_dot(str(dot_out))
    g.to_raw_html(str(html_out))
    assert dot_out.exists() and dot_out.stat().st_size > 0
    assert html_out.exists() and html_out.stat().st_size > 0
