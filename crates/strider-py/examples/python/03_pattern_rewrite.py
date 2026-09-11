from __future__ import annotations

import pathlib

import strider
from strider import template as tpl
from strider.pattern import Capture, int_and, int_const, int_mul, int_shl, load, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

elf = strider.lift.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum").address
_cfg, function, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)

x = Capture()
before = len(function.find_all(int_mul(var(x), int_const(4))))
print(f"before rewrite: {before} `x * 4` site(s), {len(function.find_all(load()))} loads")

# `replace` takes a strider.template Template; a Pat works only for its
# build-valid subset. Share a Capture so both sides agree.
# The pipeline runs before the graph is handed over, so pick an idiom it
# leaves standing: `x + 0` is already folded by the time analyze() returns.
n = function.rewrite(
    find=int_mul(var(x), int_const(4)),
    replace=tpl.int_shl(tpl.var(x), tpl.int_const(2)),
)
print(f"`x * 4 -> x << 2` strength reduction: {n} site(s) rewritten")
assert n == before > 0

# Re-optimize after any structural change; no pipeline arg re-runs the full
# default pipeline.
elf.optimize(function)

print(f"after rewrite + reoptimize: {len(function.find_all(int_mul(var(x), int_const(4))))} `x * 4`, "
      f"{len(function.find_all(int_shl(var(x), int_const(2))))} `x << 2`")

# rewrite_all tries every rule at every reachable node and returns the total.
_cfg, fresh, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)
y, z = Capture(), Capture()
total = fresh.rewrite_all([
    (int_mul(var(y), int_const(4)), tpl.int_shl(tpl.var(y), tpl.int_const(2))),
    # Align-down-to-8 respelled as a shift pair.
    (int_and(var(z), int_const(0xFFFFFFF8)),
     tpl.int_shl(tpl.int_shr(tpl.var(z), tpl.int_const(3)), tpl.int_const(3))),
])
print(f"rewrite_all with 2 rules: {total} site(s) rewritten in one pass")
assert total > n, "both rules must have fired"
