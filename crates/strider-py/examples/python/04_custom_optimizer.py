"""Build an optimizer pipeline pass by pass.

`OptimizerPipeline.empty()` starts blank, `.default()` gives every built-in
pass; either is applied via `Lifter.optimize(function, pipeline)`.

`analyze()` always drives the default pipeline to a fixed point, so there is
no unoptimized IR to compare against. This example instead manufactures a
fresh redundancy by hand on clones of the optimized function, then measures
how much each pipeline collapses back out.

Run from the workspace root:
    python crates/strider-py/examples/python/04_custom_optimizer.py
"""

from __future__ import annotations

import pathlib

import strider
from strider import template as tpl
from strider.pattern import Capture, add, anything, load, store, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

elf = strider.lift.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum")

_cfg, baseline, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)


def shape(g: strider.ir.Function) -> dict[str, int]:
    """Cheap summary for comparing two graphs.

    `nodes` counts reachable-from-entry nodes via `anything()`, not
    `Function.node_count()`; the latter counts every allocated arena slot,
    reachable or not, and only shrinks after `compact()`.
    """
    return {
        "loads": len(g.find_all(load())),
        "stores": len(g.find_all(store())),
        "nodes": len(g.find_all(anything())),
    }


print(f"baseline (analyze()'s default-optimized graph) : {shape(baseline)}")

# Rewriting `x + y` into `x + (y + 0)` keeps the value but adds a real `Add`
# node, which survives until some pipeline folds the inner `y + 0` away.
x, y = Capture(), Capture()
rule_find = add(var(x), var(y))
rule_repl = tpl.add(tpl.var(x), tpl.add(tpl.var(y), tpl.int_const(0)))

no_opt = baseline.clone()
n = no_opt.rewrite(find=rule_find, replace=rule_repl)
print(f"\nreintroduced {n} redundant `y + 0` sub-expression(s)")
print(f"no further optimization applied                 : {shape(no_opt)}")

# Just enough pipeline to fold the identity back.
partial_pipe = strider.opt.OptimizerPipeline.empty()
partial_pipe.add(strider.opt.ConstantFold())
partial = no_opt.clone()
elf.optimize(partial, partial_pipe)
print(f"ConstantFold-only pipeline                      : {shape(partial)}")

# The default pipeline reaches at least the same fixed point, plus phi and
# branch cleanups this fixture doesn't happen to exercise.
full = no_opt.clone()
elf.optimize(full, strider.opt.OptimizerPipeline.default())
print(f"default pipeline                                : {shape(full)}")

print(
    f"\nnode counts — no reoptimize: {shape(no_opt)['nodes']}, "
    f"ConstantFold-only: {shape(partial)['nodes']}, "
    f"default: {shape(full)['nodes']}"
)
