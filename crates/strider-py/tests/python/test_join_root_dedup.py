"""A lone capture-bearing pattern keeps every root under a join."""

import strider
from strider.pattern import Capture, anything, int_add, load
from strider.pattern.constraints import all_of


def test_always_true_constraint_drops_no_row(x86_memory_elf):
    prog = strider.lift.load_elf(str(x86_memory_elf))
    _cfg, f, _ = prog.analyze("array_sum")
    base = Capture("base")
    pat = load(addr=int_add(base, anything()).ordered())

    def rows(hits):
        return sorted((h.root, h.node(base)) for h in hits)

    plain = rows(f.find_all(pat, ignore_casts=True))
    joined = rows(f.find_all(pat, ignore_casts=True, constraints=[all_of([])]))
    assert len({b for _, b in plain}) < len(plain), "needs a base shared by two loads"
    assert joined == plain
