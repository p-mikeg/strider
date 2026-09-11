import strider

from .conftest import built_function, built_lifter_and_function, fixture_path


def _build_graph():
    return built_function("x86", "memory", "array_sum", optimize=False)


def _build_lifter_and_graph():
    return built_lifter_and_function("x86", "memory", "array_sum", optimize=False)


def test_pretty_render_is_a_flag_not_a_receiver():
    """Pretty renders need a `Sleigh` (register names, constant inlining,
    virtual nodes), reached through the `Function`'s parent `Cfg`.  Even
    so, pretty-vs-raw is a FLAG on one method, not two identical verbs on
    different receivers."""
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


def test_pretty_carries_the_theme():
    """`pretty` is the whole render selector: `False` raw, `True` pretty in the
    default theme, a theme name pretty in that theme.  Every value means
    something, so there is no combination to reject and none to ignore."""
    import pytest

    g = _build_graph()
    assert g.to_html(pretty="dark") == g.to_html(pretty=True)
    assert g.to_html(pretty="dark") != g.to_html()
    assert g.to_html(pretty="empty") != g.to_html(pretty="dark")
    with pytest.raises(strider.StriderError, match="unknown dot style"):
        # Deliberate: an unknown theme name is a runtime error.
        g.to_html(pretty="not_a_theme")  # type: ignore[arg-type]


def test_style_keyword_is_gone():
    import pytest

    g = _build_graph()
    with pytest.raises(TypeError):
        # Deliberate: `style` is the removed keyword this test pins.
        g.to_html(style="dark")  # type: ignore[call-arg]
    with pytest.raises(TypeError):
        g.to_dot(style="dark")  # type: ignore[call-arg]


def test_lifter_render_methods_removed():
    """The pretty renders live on `Function` behind `pretty=True`; the
    `Lifter`-side duplicates are gone, so one verb has one home."""
    lift, g = _build_lifter_and_graph()
    del g
    for gone in ("to_dot", "to_html", "dump_html", "dump_dot", "html_str"):
        assert not hasattr(lift, gone)


def test_elf_lifter_has_no_render_methods_either():
    """`ElfLifter` subclasses `Lifter`, so it inherits the removal too;
    rendering lives on the `Function` it returns."""
    prog = strider.lift.load_elf(str(fixture_path("x86", "memory")))
    for gone in ("to_dot", "to_html", "dump_html", "dump_dot", "html_str"):
        assert not hasattr(prog, gone)


def test_graph_node_count_positive():
    g = _build_graph()
    assert g.node_count() > 0


def test_raw_dot_is_one_node_per_reachable_node():
    # The raw renderer reflects the graph as stored: one DOT node per node
    # reachable from entry, none of the pretty renderer's constant inlining
    # or virtual nodes.  Reachable nodes are a subset of the arena, so only
    # the bound is checked here; the Rust unit test pins strict 1:1.
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
    # `pretty` selects the renderer, as on `to_dot` / `to_html`.
    pretty = fn.neighborhood_dot(fn.entry_node(), pretty=True)
    assert pretty != fn.neighborhood_dot(fn.entry_node())
    assert not hasattr(strider.lift.Lifter, "neighborhood_dot")
    for gone in (
        "raw_dot_str",
        "raw_html_str",
        "to_raw_dot",
        "to_raw_html",
        "raw_neighborhood_dot",
    ):
        assert not hasattr(fn, gone)
