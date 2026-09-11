"""Raw ids must not outlive the arena they name.

`compact()` renumbers every node and bumps the graph generation. The
capture readers check that generation; `Match.root`, `Match.roots` and
`Node.id` did not, so they kept handing out pre-compaction integers that
now name a different node. Every producer of a raw id checks it now; an
int the caller kept is beyond reach and is documented as such.
"""

import pytest

import strider
from strider.pattern import Capture, load

from .conftest import built_function, built_lifter_and_function


def _graph_and_match():
    g = built_function("x86", "memory", "array_sum", optimize=False)
    c = Capture()
    matches = g.find_all(load().capture(c))
    assert matches, "fixture has no Load to match"
    return g, matches[0], c


def test_match_root_is_stale_after_compact():
    g, m, _c = _graph_and_match()
    assert isinstance(m.root, int)
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        m.root


def test_match_roots_is_stale_after_compact():
    g, m, _c = _graph_and_match()
    assert m.roots
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        m.roots


def test_node_id_is_stale_after_compact():
    g, m, c = _graph_and_match()
    node = m.node(c)
    assert isinstance(node.id, int)
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        node.id


def test_bare_int_id_is_not_generation_checked():
    """The residual, and by design: an int carries no generation, so
    `Function.node` resolves a kept id against whatever arena is current."""
    g, _m, _c = _graph_and_match()
    raw = min(g.node_ids())
    g.compact()
    assert g.node(raw).id == raw


def test_match_getitem_is_stale_after_compact():
    """`m[c]` is a capture accessor like the rest: it must raise here, not
    hand back a `BoundCapture` whose first read raises instead."""
    g, m, c = _graph_and_match()
    assert m[c] is not None
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        m[c]


def test_match_has_is_stale_after_compact():
    """`has` / `in` are capture accessors too: the natural
    `if c in m: m.node(c)` guard must not turn a raise into a clean skip."""
    g, m, c = _graph_and_match()
    assert m.has(c)
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        m.has(c)


def test_match_contains_is_stale_after_compact():
    g, m, c = _graph_and_match()
    assert c in m
    g.compact()
    with pytest.raises(strider.StriderError, match="stale"):
        _ = c in m


# `optimize` bumps the handle generation BEFORE running the pipeline, so a
# mid-run failure still invalidates outstanding handles.  Bumping first means
# even a successful optimize invalidates them; that is the contract pinned here.
def test_optimize_invalidates_outstanding_handles():
    lift, fn = built_lifter_and_function("x86", "memory", "array_sum")

    c = Capture()
    add_hits = fn.find_all(load().capture(c))
    if not add_hits:
        pytest.skip("no load node to hold a stale handle against")
    handle = add_hits[0]
    assert handle.node(c) is not None

    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    lift.optimize(fn, pipe)

    # Stale handle: dereferencing must raise, not read the mutated arena.
    with pytest.raises(strider.StriderError):
        handle.node(c)
