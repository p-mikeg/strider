"""A renderer argument that cannot serve the render must say so.

Returning a graph instead is the failure: `Cfg.neighborhood_dot` has to
range-check its centre rather than building a `RegionId` straight from it,
which is a `debug_assert` away from petgraph's end sentinel and otherwise
renders an empty graph.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


@pytest.fixture(scope="module")
def analyzed():
    lift = strider.lift.load_elf(str(fixture_path("x86", "memory")))
    return lift, lift.analyze("array_sum")


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
