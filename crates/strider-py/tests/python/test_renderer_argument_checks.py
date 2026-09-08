"""A renderer argument that cannot serve the render must say so.

Both halves used to return a graph instead: `Function.neighborhood_dot` looked
at `lifter=` only on the `pretty=True` path, and `Cfg.neighborhood_dot` built a
`RegionId` straight from its argument, which is a `debug_assert` away from
petgraph's end sentinel and otherwise renders an empty graph.
"""

from __future__ import annotations

import threading

import pytest

import strider

from .conftest import fixture_path


@pytest.fixture(scope="module")
def analyzed():
    lift = strider.lift.load_elf(str(fixture_path("x86", "memory")))
    return lift, lift.analyze("array_sum")


def _foreign_arch_lifter(lift):
    return strider.lift.lifter(strider.sleigh.SleighArch.aarch64(), lift.reader())


@pytest.mark.parametrize("pretty", [False, True])
def test_neighborhood_dot_checks_its_lifter_whether_or_not_it_renders_with_it(
    analyzed, pretty
):
    """`pretty=False` is pure IR and needs no decoder, but accepting a handle
    it never looks at reports a mismatched arch as a successful render."""
    lift, result = analyzed
    with pytest.raises(strider.StriderError, match="lifter is for aarch64"):
        result.function.neighborhood_dot(
            result.function.entry_node(),
            pretty=pretty,
            lifter=_foreign_arch_lifter(lift),
        )


@pytest.mark.parametrize("pretty", [False, True])
def test_neighborhood_dot_rejects_a_lifter_owned_by_another_thread(analyzed, pretty):
    """Decoding is pinned to the thread that built the handle; every other
    `lifter=` renderer raises off-thread rather than rendering."""
    lift, result = analyzed
    same_arch = strider.lift.lifter(lift.arch, lift.reader())
    outcome = []

    def render():
        try:
            result.function.neighborhood_dot(
                result.function.entry_node(), pretty=pretty, lifter=same_arch
            )
            outcome.append(None)
        except strider.StriderError as e:
            outcome.append(str(e))

    t = threading.Thread(target=render)
    t.start()
    t.join()
    assert outcome[0] is not None, "an off-thread decoder rendered anyway"


@pytest.mark.parametrize("center", [4294967295, 99999])
def test_cfg_neighborhood_dot_rejects_a_region_id_it_has_no_region_for(
    analyzed, center
):
    """`4294967295` is petgraph's end sentinel; the other is simply past the
    end. `Function.neighborhood_dot` already raised on the same mistake."""
    _lift, result = analyzed
    with pytest.raises(strider.StriderError, match=f"invalid region id {center}"):
        result.cfg.neighborhood_dot(center)


def test_a_real_region_id_still_renders(analyzed):
    _lift, result = analyzed
    assert result.cfg.neighborhood_dot(result.cfg.entry()).startswith("digraph")
