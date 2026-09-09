from __future__ import annotations

import pathlib

import strider
from strider.pattern import (
    Capture,
    anything,
    first_of,
    any_int_binary,
    int_add,
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
# Disjoint arms behave the same either way.
print("=== one_of / first_of ===")
mem_ops = fn.find_all(one_of([load(), store()]))
print(f"memory ops (load OR store): {len(mem_ops)}")

# Overlapping arms are where they part. one_of is a union: `base + K` fires
# both arms, so the row where `off` bound survives alongside the bare-base one.
# first_of cuts at the first arm that matches, so a permissive arm in front
# means the specific one is never tried and `off` never binds.
b, off = Capture("b"), Capture("off")
specific = int_add(var(b), int_const(off))
for label, addr_pat in (
    ("one_of([var, add])", one_of([var(b), specific])),
    ("first_of([var, add])", first_of([var(b), specific])),
    ("first_of([add, var])", first_of([specific, var(b)])),
):
    rows = fn.find_all(load(addr=addr_pat), ignore_casts=True)
    bound = sum(1 for m in rows if off in m)
    print(f"{label:22} {len(rows):3d} rows, {bound} with `off` bound")


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
# The guard leaves only int constants, so uint reads them without the _opt.
sample = sorted({m.uint(c) for m in big})[:8]
print(f"  distinct values (first 8): {sample}")


# --- 4. find_unique: raises unless the pattern matches exactly once ---
print("\n=== find_unique ===")
the_ret = fn.find_unique(ret())
print(f"exactly one Return node (root node id {the_ret.root})")


# --- 5. any_int_binary: bind the operator variant ---
# The first Capture binds the node; Match.op reads back which variant fired.
# Both operands are wildcards, so a commutative op answers twice per node;
# collecting roots into a set counts nodes.
print("\n=== any_int_binary: which operators appear ===")
op, lhs, rhs = Capture("op"), Capture("l"), Capture("r")
by_op: dict[str, set[int]] = {}
for m in fn.find_all(any_int_binary(op, lhs, rhs)):
    by_op.setdefault(m.op(op), set()).add(m.root)
for name, nodes in sorted(by_op.items(), key=lambda kv: -len(kv[1])):
    print(f"  {name:12} x{len(nodes)}")
