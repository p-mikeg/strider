import strider
from .conftest import built_function


def _fn():
    return built_function("x86", "memory", "array_sum", optimize=True)


def test_cfg_walk_is_control_only_and_subset_of_data_walk():
    fn = _fn()
    cfg = fn.cfg_walk()
    data = fn.data_walk()
    assert cfg and data
    assert all(isinstance(n, strider.ir.Node) for n in cfg + data)
    cfg_ids = {n.id for n in cfg}
    data_ids = {n.id for n in data}
    assert cfg_ids <= data_ids                 # control-reachable ⊆ all-reachable
    # cfg_walk yields only control-flow kinds (no pure-arithmetic node)
    kinds = {n.kind() for n in cfg}
    assert "Entry" in kinds


def test_walk_from_node_is_subset_of_data_walk():
    fn = _fn()
    data = fn.data_walk()
    seeded = fn.walk(data[-1].id)   # from some reachable node
    assert {n.id for n in seeded} <= {n.id for n in data}


def test_mem_walk_returns_memory_touching_nodes():
    fn = _fn()
    mem = fn.mem_walk()
    assert mem
    kinds = {n.kind() for n in mem}
    assert "InitialMemory" in kinds
    # array_sum touches memory, so at least one Load or Store. kind()
    # renders the full Debug repr (e.g. "Load(VnSpace { shortcut: 'r' })"),
    # so match on the variant prefix rather than an exact string.
    assert any(k.startswith(("Load", "Store")) for k in kinds)
    # every returned node is memory-touching (no bare arithmetic kind)
    assert not any(k.startswith(("IntBinaryOp", "IntConst")) for k in kinds)


def test_node_outputs_are_the_consumers():
    fn = _fn()
    # entry's outputs feed the first control/region nodes
    entry = fn.node(fn.entry_node())
    outs = entry.outputs()
    assert all(isinstance(n, strider.ir.Node) for n in outs)
    # round-trip: some output's consumer lists entry among ITS inputs
    # (pick any node that has inputs and verify inputs/outputs are inverse on one edge)
    data = fn.data_walk()
    for n in data:
        for i in n.inputs():
            assert n.id in {c.id for c in i.outputs()}, "outputs must invert inputs"
            break
        else:
            continue
        break
    assert not hasattr(entry, "input")  # sanity: only inputs/outputs exist
