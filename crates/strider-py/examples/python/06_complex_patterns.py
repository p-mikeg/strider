"""Composing patterns: capture chains, back-references, guards, commutativity.

The four things you reach for beyond plain node-shape matching. Run from the
workspace root:

    python crates/strider-py/examples/python/06_complex_patterns.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import (
    add,
    function_arg,
    int_const,
    int_binary,
    int_xor,
    load,
    mul,
)

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

prog = strider.lift.load_elf(str(FIXTURE))
_cfg, function, unresolved = prog.analyze(
    "array_sum", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
)


# Capture chains bind operands of nested operations. `load(base + idx*stride)`
# is the canonical indexed-array-load shape.
#
# Each unique string interns to a Capture scoped to this pattern. `var(c)`
# only accepts Capture objects; a bare string goes straight into the slot,
# with `var(name)` implied.
print("=== 1. multi-level capture chain ===")
pat1 = load(addr=add("base", mul("idx", "stride")))
hits = function.find_all(pat1, ignore_casts=True)
print(f"found {len(hits)} indexed-array-load shapes")
for h in hits[:3]:
    print(
        f"  base={h.const_uint('base')!r:>12}  idx={h.const_uint('idx')!r:>10}  "
        f"stride={h.const_uint('stride')!r:>4}"
    )


# Reusing a capture name forces both positions to bind the same IR value, so
# `int_xor("v", "v")` matches only the zero-a-register idiom (`xor eax, eax`)
# and is a strict subset of the general two-operand xor count.
print("\n=== 2. back-reference (xor x, x) ===")
all_xors = function.find_all(int_xor("a", "b"))
self_xors = function.find_all(int_xor("v", "v"))
print(
    f"all xors: {len(all_xors)}, "
    f"self-xors (back-ref): {len(self_xors)} ({'subset' if len(self_xors) <= len(all_xors) else 'WRONG'})"
)


# `.when` takes a lambda over the partial Match; return False to drop it.
print("\n=== 3. .when predicate guard ===")
unfiltered = function.find_all(load(), ignore_casts=True)
small_addr = function.find_all(
    load(addr=add("b", "o")).when(
        lambda m: (m.const_uint("o") or 0) < 16
    ),
    ignore_casts=True,
)
print(
    f"all loads: {len(unfiltered)}, "
    f"loads with offset < 16: {len(small_addr)}"
)


# Add is commutative, so both operand orderings are tried automatically and
# the two counts below agree. `.ordered()` on the typed builder opts out.
print("\n=== 4. commutative matching ===")
const_then_arg = function.find_all(add(int_const(8), function_arg(0)))
arg_then_const = function.find_all(add(function_arg(0), int_const(8)))
ordered_only = function.find_all(int_binary("Add", int_const(8), function_arg(0)).ordered())
print(
    f"add(const, arg): {len(const_then_arg)}, "
    f"add(arg, const): {len(arg_then_const)} "
    f"(equal because commutative — should match)"
)
print(f"int_binary(Add, ...).ordered() (left-to-right only): {len(ordered_only)}")


# `int_bin_any` matches any integer binary op and binds the variant (Add, Mul,
# ...) to a Capture, so you can post-filter or report which op fired. The
# first argument must be a Capture, not a string; variant captures are not
# interned the way value captures are.
print("\n=== bonus: int_bin_any binds the op variant ===")
from strider.pattern import Capture, int_bin_any

op = Capture()
pat = int_bin_any(op, "l", "r")
matches = function.find_all(pat)
print(f"matched {len(matches)} int-binary-op nodes (any variant)")
