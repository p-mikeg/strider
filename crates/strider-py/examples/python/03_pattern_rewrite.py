"""Pattern-to-pattern rewriting, then re-optimizing.

The rules here are deliberately trivial; constant folding has already eaten
any real `x + 0` in this fixture. What matters is the call shape: how `find`
and `replace` share captures, when to re-run the optimizer, and how
`rewrite_all` stages several rules at once.

Run from the workspace root:
    python crates/strider-py/examples/python/03_pattern_rewrite.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import Capture, add, int_const, load, var

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

elf = strider.lift.load_elf(str(FIXTURE))
addr = elf.symbol("array_sum")
_cfg, function, _unresolved = elf.analyze(
    addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)

before = len(function.find_all(load()))
print(f"before rewrite: {before} loads")

# `find` accepts string-shorthand captures, but `replace` needs a real Pat: a
# bare string on the replace side would be ambiguous (a new wildcard, or a
# back-reference to a find-side capture?). Share a `Capture` object instead so
# both sides agree.
x = Capture()
rule_find = add(var(x), int_const(0))
rule_repl = var(x)
n = function.rewrite(find=rule_find, replace=rule_repl)
print(f"`x + 0 → x` substitution: {n} site(s) rewritten")

# Re-optimize after any structural change. With no pipeline argument this
# re-runs the full default pipeline, including the node-removing passes that
# collapse phi and dead-branch noise the rewrite may have exposed.
elf.optimize(function)

after = len(function.find_all(load()))
print(f"after rewrite + reoptimize: {after} loads")

# `rewrite_all` applies rules in order, first match wins per node. That
# ordering is the point when you are writing a canonicalization pass.
y = Capture()
z = Capture()
function.rewrite_all([
    (add(var(y), int_const(0)), var(y)),
    (add(int_const(0), var(z)), var(z)),   # commutative-twin
])
print("rewrite_all with 2 rules complete")
