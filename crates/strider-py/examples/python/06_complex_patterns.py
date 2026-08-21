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
    int_xor,
    load,
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
# node. On an analysed graph int_xor(v, v) is always 0 hits: the default
# pipeline folds xor(x, x) to int_const(0) before any query runs, so the
# zero-a-register idiom (xor eax, eax) is found by its folded result.
print("\n=== 2. back-reference (xor x, x) ===")
a, b, v = Capture("a"), Capture("b"), Capture("v")
all_xors = function.find_all(int_xor(a, b))
self_xors = function.find_all(int_xor(v, v))
print(
    f"all xors: {len(all_xors)}, "
    f"self-xors (back-ref, folded away): {len(self_xors)}, "
    f"folded zeros: {len(function.find_all(int_const(0)))}"
)


# `.when` takes a lambda over the partial Match; return False to drop it.
print("\n=== 3. .when predicate guard ===")
unfiltered = function.find_all(load(), ignore_casts=True)
addr_base, off = Capture("base"), Capture("off")
small_addr = function.find_all(
    load(addr=int_add(addr_base, off)).when(
        lambda m: (m.uint_opt(off) or 0) < 16
    ),
    ignore_casts=True,
)
print(
    f"all loads: {len(unfiltered)}, "
    f"`base + offset` rows with offset < 16: {len(small_addr)} "
    f"(commutative int_add counts each such load under both operand orders)"
)


# Add is commutative: both orderings tried, so the two counts agree. .ordered() opts out.
print("\n=== 4. commutative matching ===")
const_then_arg = function.find_all(int_add(int_const(8), function_arg(0)))
arg_then_const = function.find_all(int_add(function_arg(0), int_const(8)))
ordered_only = function.find_all(int_binary("Add", int_const(8), function_arg(0)).ordered())
print(
    f"int_add(const, arg): {len(const_then_arg)}, "
    f"int_add(arg, const): {len(arg_then_const)} "
    f"(equal because commutative)"
)
print(f"int_binary(Add, ...).ordered() (left-to-right only): {len(ordered_only)}")


# any_int_binary matches any int binary op, binding the node to its first Capture;
# Match.op reads back which variant fired.
print("\n=== bonus: any_int_binary binds the op variant ===")
op, l, r = Capture(), Capture("l"), Capture("r")
pat = any_int_binary(op, l, r)
matches = function.find_all(pat)
print(f"matched {len(matches)} int-binary-op nodes (any variant)")
