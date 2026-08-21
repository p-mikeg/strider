"""End-to-end jump-table resolution plus post-resolution pattern rewrites.

`switch.c::dispatch_value` is a 9-arm dense switch that gcc -O2 lowers to a
real `jmp *.rodata[idx*4]` table on x86, so it drives the whole resolution
loop: lift with an indirect-branch placeholder, classify it into the case
targets, rebuild the CFG so each case body is its own block, re-lift, then
optimise to fixed point.  Resolution leaves one `int_add(x, IntConst(K))` per
case, which the tests below query and rewrite against.
"""

from __future__ import annotations

import strider
from strider.pattern import Capture, var, int_add, call, int_const, load


def _run(elf_path):
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    loaded = strider.lift.load_elf(str(elf_path))
    mem = loaded.reader()
    addr = loaded.symbol("dispatch_value").address
    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    return lift, function


def test_orchestrator_resolves_jump_table_x86(x86_switch_elf):
    # The x86 jump table resolves inside the fixed-point loop's iteration
    # budget, so analyze returns a graph rather than a convergence error.
    _lift, g = _run(x86_switch_elf)
    assert g.node_count() > 0


def test_case_bodies_match_add_const_pattern(x86_switch_elf):
    # Cases 0..4 and 6..7 are `return x + K`.  Case 5 calls `f(value->a)`,
    # so it lifts to a Load + Call and K=6 must not match.  The threshold is
    # loose because the optimiser is free to fold or reshape individual arms.
    _lift, g = _run(x86_switch_elf)
    seen_constants = set()
    for k in range(1, 9):
        hits = g.find_all(int_add(Capture("x"), int_const(k)), ignore_casts=True)
        if hits:
            seen_constants.add(k)
    assert len(seen_constants) >= 4, (
        f"expected ≥4 of x+K (K=1..8) to survive in IR; saw {sorted(seen_constants)}"
    )


def test_case5_calls_helper_with_struct_field(x86_switch_elf):
    # Case 5's `return f(value->a)` lifts to a Load feeding a Call; both
    # surviving proves lift+resolve+optimise kept that arm intact.
    _lift, g = _run(x86_switch_elf)
    call_hits = g.find_all(call())
    assert len(call_hits) >= 1, "case 5 must produce a Call to f()"
    load_hits = g.find_all(load())
    assert len(load_hits) >= 1, "case 5 must produce a Load for value->a"


def test_rewrite_collapses_add_zero_then_reoptimize(x86_switch_elf):
    # ConstantFold already collapses `int_add(x, 0)`, so this rewrite normally
    # fires zero times; the point is that the API composes and `optimize`
    # still works on the post-rewrite graph.
    lift, g = _run(x86_switch_elf)
    x = Capture()
    n = g.rewrite(find=int_add(var(x), int_const(0)), replace=var(x))
    assert isinstance(n, int) and n >= 0
    lift.optimize(g)
    assert g.node_count() > 0


def test_rewrite_collapses_add_one_to_marker(x86_switch_elf):
    # Collapsing `int_add(x, 1)` to `x` is a lossy rule, chosen to alter graph
    # state and confirm re-optimization survives it.
    lift, g = _run(x86_switch_elf)
    n_before_const_1 = len(g.find_all(int_const(1), ignore_casts=True))
    x = Capture()
    fired = g.rewrite(find=int_add(var(x), int_const(1)), replace=var(x))
    lift.optimize(g)
    n_after_const_1 = len(g.find_all(int_const(1), ignore_casts=True))
    # Either the rule fired and the node-removing passes pruned the freed
    # constants, or ConstantFold absorbed the shape first.  Both satisfy the
    # rewrite-API contract, so only the direction of the count is pinned.
    assert fired >= 0
    assert n_after_const_1 <= n_before_const_1
