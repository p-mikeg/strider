from __future__ import annotations

import pathlib

import strider
from strider.pattern import Capture, int_add, int_const, load, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

elf = strider.lift.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum").address
_cfg, function, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)

before = len(function.find_all(load()))
print(f"before rewrite: {before} loads")

# replace takes a strider.template Template; a Pat works only for its
# build-valid subset. Share a Capture so both sides agree.
x = Capture()
rule_find = int_add(var(x), int_const(0))
rule_repl = var(x)
n = function.rewrite(find=rule_find, replace=rule_repl)
print(f"`x + 0 → x` substitution: {n} site(s) rewritten")

# Re-optimize after any structural change; no pipeline arg re-runs the full
# default pipeline.
elf.optimize(function)

after = len(function.find_all(load()))
print(f"after rewrite + reoptimize: {after} loads")

# rewrite_all tries every rule at every reachable node.
y = Capture()
z = Capture()
function.rewrite_all([
    (int_add(var(y), int_const(0)), var(y)),
    (int_add(int_const(0), var(z)), var(z)),   # already covered: int_add matches both orders
])
print("rewrite_all with 2 rules complete")
