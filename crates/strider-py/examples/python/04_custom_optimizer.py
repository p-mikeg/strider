"""04 — Custom optimizer pipeline: build it pass by pass.

`OptimizerPipeline.default()` builds the full pipeline (every built-in
pass); `OptimizerPipeline.empty()` starts blank so you add exactly the
passes your workflow needs, then apply the result via
`Lifter.optimize(function, pipeline)`.

`Lifter.analyze()` (and `ElfLifter.analyze()`) always drive the
canonical default pipeline to a fixed point — there is no
"unoptimized IR" entry point in this API. To still compare pipelines
side by side, this example manufactures a fresh redundancy with a
manual `.rewrite()` on independent clones of the already-optimized
function, then shows how much of it each pipeline collapses back out.

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

elf = strider.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum")

# `ElfLifter.analyze` drives the full default pipeline already — this
# is the baseline every further clone below starts from.
_cfg, baseline, _unresolved = elf.analyze(
    addr, opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True))
)


def shape(g: strider.Function) -> dict[str, int]:
    """A small grab-bag summary so we can compare graphs cheaply.

    `nodes` counts reachable-from-entry nodes via `anything()` rather
    than `Function.node_count()` — the latter counts every allocated
    arena slot (reachable or not) and only shrinks after `compact()`."""
    return {
        "loads": len(g.find_all(load())),
        "stores": len(g.find_all(store())),
        "nodes": len(g.find_all(anything())),
    }


print(f"baseline (analyze()'s default-optimized graph) : {shape(baseline)}")

# Reintroduce a foldable redundancy on a clone: rewrite every `x + y`
# into `x + (y + 0)` — same value, but a real extra `Add` node until
# something folds the inner `y + 0` back down to `y`.
x, y = Capture(), Capture()
rule_find = add(var(x), var(y))
rule_repl = tpl.add(tpl.var(x), tpl.add(tpl.var(y), tpl.int_const(0)))

no_opt = baseline.clone()
n = no_opt.rewrite(find=rule_find, replace=rule_repl)
print(f"\nreintroduced {n} redundant `y + 0` sub-expression(s)")
print(f"no further optimization applied                 : {shape(no_opt)}")

# A hand-built PARTIAL pipeline: just enough to fold the identity back.
partial_pipe = strider.OptimizerPipeline.empty()
partial_pipe.add(strider.opt.ConstantFold())
partial = no_opt.clone()
elf.optimize(partial, partial_pipe)
print(f"ConstantFold-only pipeline                      : {shape(partial)}")

# The full default pipeline reaches at least the same fixed point (and
# would additionally clean up phi/branch redundancy this fixture
# doesn't happen to exercise).
full = no_opt.clone()
elf.optimize(full, strider.OptimizerPipeline.default())
print(f"default pipeline                                : {shape(full)}")

print(
    f"\nnode counts — no reoptimize: {shape(no_opt)['nodes']}, "
    f"ConstantFold-only: {shape(partial)['nodes']}, "
    f"default: {shape(full)['nodes']}"
)
