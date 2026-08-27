from __future__ import annotations

import pathlib

import strider
from strider import template as tpl
from strider.pattern import Capture, int_add, anything, load, store, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

elf = strider.lift.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum").address

_cfg, baseline, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)


def shape(g: strider.ir.Function) -> dict[str, int]:
    """`nodes` counts reachable-from-entry via anything(); node_count() counts
    every arena slot and only shrinks after compact()."""
    return {
        "loads": len(g.find_all(load())),
        "stores": len(g.find_all(store())),
        "nodes": len(g.find_all(anything())),
    }


print(f"baseline (analyze()'s default-optimized graph) : {shape(baseline)}")

# x + y -> x + (y + 0) keeps the value but adds an Add node that survives until
# a pipeline folds the inner y + 0 away.
x, y = Capture(), Capture()
rule_find = int_add(var(x), var(y))
rule_repl = tpl.int_add(tpl.var(x), tpl.int_add(tpl.var(y), tpl.int_const(0)))

no_opt = baseline.clone()
n = no_opt.rewrite(find=rule_find, replace=rule_repl)
print(f"\nreintroduced {n} redundant `y + 0` sub-expression(s)")
print(f"no further optimization applied                 : {shape(no_opt)}")

# The smallest pipeline that folds the identity back.
partial_pipe = strider.opt.OptimizerPipeline.empty()
partial_pipe.add(strider.opt.ConstantFold())
partial = no_opt.clone()
elf.optimize(partial, partial_pipe)
print(f"ConstantFold-only pipeline                      : {shape(partial)}")

# The default pipeline reaches at least the same fixed point.
full = no_opt.clone()
elf.optimize(full, strider.opt.OptimizerPipeline.default())
print(f"default pipeline                                : {shape(full)}")

print(
    f"\nnode counts, no reoptimize: {shape(no_opt)['nodes']}, "
    f"ConstantFold-only: {shape(partial)['nodes']}, "
    f"default: {shape(full)['nodes']}"
)
