"""03 — Pattern → pattern rewrite + reoptimize.

Strider's optimizer has constant folding built in, so contrived
identities like `x + 0` rarely survive long enough to demonstrate
rewriting. Instead this example shows the *mechanism* — apply a
pattern-substitution rule, then re-run the destructive pipeline so
downstream passes (RedundantPhis, DeadBranchElim) collapse what the
rewrite exposed.

Read this example to understand:
  - How `find` and `replace` patterns share captures by name.
  - When to call `reoptimize(destructive=True)` vs the default stable.
  - That `rewrite_all` lets you stage multiple rules in one pass.

Run from the workspace root:
    python crates/strider-py/examples/python/03_pattern_rewrite.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import Capture, add, int_const, load, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

mem = strider.MemoryMap()
mem.add_region_from_elf(str(FIXTURE))
addr = mem.symbol("array_sum")
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem,
    rom=mem,
    entry=addr,
    allow_code_before_start_addr=True,
)

# Snapshot the load count before rewriting.
before = len(result.function.find_all(load()))
print(f"before rewrite: {before} loads")

# A no-op rewrite that demonstrates the API: replace `x + 0` with `x`
# wherever it appears. ConstantFold has likely already eaten any such
# expression in `array_sum`, so the rewrite is a no-op here — but the
# call shape is what matters as a template for real rewrites.
# `find` accepts string-shorthand captures, but `replace` needs a real Pat
# — strings on the replace side would be ambiguous (new wildcard? back-
# reference to a find-side capture?). Use a shared `Capture` object so
# both sides agree.
x = Capture()
rule_find = add(var(x), int_const(0))
rule_repl = var(x)
n = result.function.rewrite(find=rule_find, replace=rule_repl)
print(f"`x + 0 → x` substitution: {n} site(s) rewritten")

# After any structural change you should reoptimize. The default is the
# stable pipeline (ConstantFold + KnownBits — safe to re-run any time).
# Pass destructive=True at the end of analysis to also collapse phi/dead-
# branch noise the rewrite may have exposed.
result.function.reoptimize()
result.function.reoptimize(destructive=True)

after = len(result.function.find_all(load()))
print(f"after rewrite + reoptimize: {after} loads")

# A staged-rules example: apply two rules in order, first-match-wins per
# node. Useful for migration / canonicalization passes where the order
# matters.
y = Capture()
z = Capture()
result.function.rewrite_all([
    (add(var(y), int_const(0)), var(y)),
    (add(int_const(0), var(z)), var(z)),   # commutative-twin
])
print("rewrite_all with 2 rules complete")
