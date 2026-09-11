from __future__ import annotations

import pathlib

import strider
from strider.pattern import (
    Capture,
    int_add,
    function_arg,
    int_const,
    int_binary,
    any_int_binary,
    load,
    store,
    int_mul,
)

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
_cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)


# load(base + idx*stride) is the canonical indexed-array-load shape. uint_opt
# reads a capture, None when it did not bind a constant.
print("=== 1. multi-level capture chain ===")
base, idx, stride = Capture("base"), Capture("idx"), Capture("stride")
pat1 = load(addr=int_add(base, int_mul(idx, stride)))
hits = function.find_all(pat1, ignore_casts=True)
print(f"found {len(hits)} indexed-array-load shapes")
for h in hits[:3]:
    print(
        f"  base={h.uint_opt(base)!r:>12}  idx={h.uint_opt(idx)!r:>10}  "
        f"stride={h.uint_opt(stride)!r:>4}"
    )


# Reusing one Capture in both positions forces the two operands to be the same
# node. Add is the operator this function has most of, and none of its Adds
# doubles a value, so the back-reference filters every row out; the two stack
# stores off one base register are a back-reference that does bind.
print("\n=== 2. back-reference (one Capture in two positions) ===")
a, b, v = Capture("a"), Capture("b"), Capture("v")
print(
    f"int_add(a, b): {len(function.find_all(int_add(a, b)))}, "
    f"int_add(v, v) (same node both sides): {len(function.find_all(int_add(v, v)))}"
)
sbase, so1, so2 = Capture("sbase"), Capture("so1"), Capture("so2")
same_base = function.find_all(
    store(addr=int_add(sbase, int_const(so1))).mem(
        store(addr=int_add(sbase, int_const(so2)))
    ),
    ignore_casts=True,
)
print(f"store chains whose two addresses share one base: {len(same_base)}")
for m in same_base:
    print(f"  base{m.sint(so1):+}  and  base{m.sint(so2):+}")


# `.when` takes a lambda over the partial Match; return False to drop it.
# uint_opt is None for a symbolic offset, which the guard must reject rather
# than read as 0.
print("\n=== 3. .when predicate guard ===")
unfiltered = function.find_all(load(), ignore_casts=True)
addr_base, off = Capture("base"), Capture("off")
small_addr = function.find_all(
    load(addr=int_add(addr_base, off)).when(
        lambda m: (k := m.uint_opt(off)) is not None and k < 16
    ),
    ignore_casts=True,
)
print(
    f"all loads: {len(unfiltered)}, "
    f"`base + offset` loads at a constant offset < 16: {len(small_addr)}"
)


# Add is commutative: both orderings tried, so the two counts agree. .ordered()
# opts out, so only the spelling the graph actually holds still matches.
print("\n=== 4. commutative matching ===")
const_then_arg = function.find_all(int_add(int_const(8), function_arg(0)))
arg_then_const = function.find_all(int_add(function_arg(0), int_const(8)))
ordered_arg_first = function.find_all(
    int_binary("Add", function_arg(0), int_const(8)).ordered()
)
ordered_const_first = function.find_all(
    int_binary("Add", int_const(8), function_arg(0)).ordered()
)
print(
    f"int_add(const, arg): {len(const_then_arg)}, "
    f"int_add(arg, const): {len(arg_then_const)} "
    f"(equal because commutative)"
)
print(
    f".ordered() as (arg, const): {len(ordered_arg_first)}, "
    f"as (const, arg): {len(ordered_const_first)} "
    f"(only the order the graph stores)"
)
assert (len(ordered_arg_first) == 0) != (len(ordered_const_first) == 0)


# any_int_binary matches any int binary op, binding the node to its first Capture;
# Match.op reads back which variant fired.
print("\n=== bonus: any_int_binary binds the op variant ===")
op, l, r = Capture(), Capture("l"), Capture("r")
pat = any_int_binary(op, l, r)
matches = function.find_all(pat)
print(f"matched {len(matches)} int-binary-op nodes (any variant)")
