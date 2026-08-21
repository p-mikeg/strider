from __future__ import annotations

import pathlib

import strider
from strider.pattern import (
    Capture,
    anything,
    first_of,
    any_int_binary,
    int_const,
    load,
    one_of,
    ret,
    store,
    var,
)

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
_cfg, fn, _unresolved = prog.analyze(
    "array_sum",
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    ),
)
print(f"array_sum: {len(fn.find_all(anything()))} reachable IR nodes\n")


# --- 1. one_of / first_of: alternation ---
# first_of commits to the first alternative per node; differs from one_of only
# when the alternatives overlap.
print("=== one_of / first_of ===")
mem_ops = fn.find_all(one_of([load(), store()]))
print(f"memory ops (load OR store): {len(mem_ops)}")
catch_all = fn.find_all(first_of([load(), anything()]))
print(f"first_of([load, anything]): {len(catch_all)}")


# --- 2. int_const of a set: a constant drawn from several ---
print("\n=== int_const([...]) ===")
powers = fn.find_all(int_const([1, 2, 4, 8, 16, 32]))
print(f"power-of-two constants in {{1..32}}: {len(powers)}")


# --- 3. var(c).when(...): a Python guard on a captured value ---
# .when is on Pat, so wrap the capture in var() first. Return False to drop.
print("\n=== var(c).when(predicate) ===")
c = Capture("c")
big = fn.find_all(var(c).when(lambda m: (m.uint_opt(c) or 0) >= 8))
print(f"constant values >= 8: {len(big)}")
sample = sorted({m.uint_opt(c) for m in big if m.uint_opt(c) is not None})[:8]
print(f"  distinct values (first 8): {sample}")


# --- 4. find_unique: raises unless the pattern matches exactly once ---
print("\n=== find_unique ===")
the_ret = fn.find_unique(ret())
print(f"exactly one Return node (root node id {the_ret.root})")


# --- 5. any_int_binary: bind the operator variant ---
# The first Capture binds the node; Match.op reads back which variant fired.
print("\n=== any_int_binary: which operators appear ===")
op, lhs, rhs = Capture("op"), Capture("l"), Capture("r")
by_op: dict[str, int] = {}
for m in fn.find_all(any_int_binary(op, lhs, rhs)):
    by_op[m.op(op)] = by_op.get(m.op(op), 0) + 1
for name, count in sorted(by_op.items(), key=lambda kv: -kv[1]):
    print(f"  {name:12} x{count}")
