"""06 — Complex patterns: captures, back-references, predicates, commutative.

Patterns get interesting once you compose them. This example walks through
the four "you'll want this" features beyond simple node-shape matching:

  1. Multi-level capture chains — bind operands of nested operations and
     read their values back as Python ints.
  2. Back-references — re-use a capture name to require the same value
     in two positions (e.g. `xor(x, x)` → must be the same x).
  3. `.when` predicate guards — filter matches with arbitrary Python
     code that sees the partial Match.
  4. Commutative matching — `add(a, b)` automatically also matches
     `add(b, a)`; use `.ordered()` on the typed builder to disable.

Run from the workspace root:
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
    load,
    mul,
    xor,
)

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
function = result.function


# ---------------------------------------------------------------------------
# 1. Multi-level capture chain.
#
# Pattern: load(addr = base + (idx * 4))
# Capture both the base and idx, plus the matched stride constant.
# This is the canonical "indexed array load" shape — useful for finding
# bounds-check-free array accesses.
# ---------------------------------------------------------------------------
print("=== 1. multi-level capture chain ===")
# String-shorthand form: each unique string interns to a Capture for
# this pattern. `var(c)` only accepts Capture objects; bare strings go
# directly into the slot (and `var(name)` is implicit on a string).
pat1 = load(addr=add("base", mul("idx", "stride")))
hits = function.find_all(pat1, ignore_casts=True)
print(f"found {len(hits)} indexed-array-load shapes")
for h in hits[:3]:
    print(
        f"  base={h.uint('base')!r:>12}  idx={h.uint('idx')!r:>10}  "
        f"stride={h.uint('stride')!r:>4}"
    )


# ---------------------------------------------------------------------------
# 2. Back-reference: same name twice → must be same value.
#
# `xor("v","v")` matches only when both operands of the XOR are the same
# IR value. This is the textbook "zero a register" idiom (`xor eax, eax`)
# and is a strict subset of the general two-operand xor count.
# ---------------------------------------------------------------------------
print("\n=== 2. back-reference (xor x, x) ===")
all_xors = function.find_all(xor("a", "b"))
self_xors = function.find_all(xor("v", "v"))
print(
    f"all xors: {len(all_xors)}, "
    f"self-xors (back-ref): {len(self_xors)} ({'subset' if len(self_xors) <= len(all_xors) else 'WRONG'})"
)


# ---------------------------------------------------------------------------
# 3. `.when` predicate guard.
#
# Filter the indexed-load matches to only those whose stride is small
# (< 16 bytes). The lambda receives the partial Match and returns bool;
# return False to drop the match.
# ---------------------------------------------------------------------------
print("\n=== 3. .when predicate guard ===")
unfiltered = function.find_all(load(), ignore_casts=True)
small_addr = function.find_all(
    load(addr=add("b", "o")).when(
        lambda m: (m.uint("o") or 0) < 16
    ),
    ignore_casts=True,
)
print(
    f"all loads: {len(unfiltered)}, "
    f"loads with offset < 16: {len(small_addr)}"
)


# ---------------------------------------------------------------------------
# 4. Commutative matching.
#
# `add(const(8), function_arg(0))` and `add(function_arg(0), const(8))`
# describe the same IR shape because Add is commutative. Strider tries
# both orderings automatically. Use the typed builder with `.ordered()`
# to opt out (rarely needed).
# ---------------------------------------------------------------------------
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


# ---------------------------------------------------------------------------
# Bonus — `_any` variant-agnostic constructors. These bind the matched
# operator variant to a Capture so you can post-filter or report which
# specific op fired (Add vs Sub vs Mul, etc.).
# ---------------------------------------------------------------------------
print("\n=== bonus: int_bin_any binds the op variant ===")
from strider.pattern import Capture, int_bin_any

# `int_bin_any` binds the matched IntBinaryOp variant (Add/Sub/Mul/...)
# to a Capture so you can introspect which op fired per match. The first
# arg must be a Capture (not a string) since variant captures aren't
# interned the same way value captures are.
op = Capture()
pat = int_bin_any(op, "l", "r")
matches = function.find_all(pat)
print(f"matched {len(matches)} int-binary-op nodes (any variant)")
