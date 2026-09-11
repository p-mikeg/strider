from __future__ import annotations

import pathlib

import strider
from strider.pattern import Capture, int_add, int_const, int_mul, load
from strider.pattern import constraints as k

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]


def analyze(elf: str, name: str):
    prog = strider.lift.load_elf(
        str(WORKSPACE / "fixtures" / "out" / "x86" / f"{elf}.elf")
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    )
    return prog.analyze(name, opts=opts)[1]


# --- 1. Producer -> consumer: one value, two patterns, one shared capture ---
# The capture prod is the multiply's result AND the add's operand, so the join
# pairs each a*b with the + c that consumes it.
print("=== 1. producer -> consumer (shared capture) ===")
fn = analyze("patterns", "mul_then_add")
a, b, prod, c = Capture("a"), Capture("b"), Capture("prod"), Capture("c")
joined = fn.find_all([int_mul(a, b).capture(prod), int_add(prod, c)])
nested = fn.find_all(int_add(int_mul(a, b), c))  # the same shape, written inline
print(f"join [mul.capture(prod), add(prod, c)]: {len(joined)} (mul, add) pair(s)")
print(f"equivalent nested add(mul(a, b), c): {len(nested)}")
print("  a shared capture equals the nested form; the two halves need not nest")


# --- 2. JoinPredicate: correlate two INDEPENDENT matches ---
# Two loads off the same base aren't one nested pattern; a join relates them.
# captures() declares the captures it correlates; constraint() decides.
print("\n=== 2. JoinPredicate over two matches ===")
fn2 = analyze("memory", "array_sum")
base, o1, o2 = Capture("base"), Capture("o1"), Capture("o2")
read1 = load(addr=int_add(base, int_const(o1)))
read2 = load(addr=int_add(base, int_const(o2)))


class DifferentOffsets(k.JoinPredicate):
    def captures(self):
        return [o1, o2]

    def constraint(self, m):
        x, y = m.uint_opt(o1), m.uint_opt(o2)
        return x is not None and y is not None and x != y


pairs = fn2.find_all([read1, read2], constraints=[DifferentOffsets()], ignore_casts=True)
print(f"two same-base loads at different offsets: {len(pairs)}")
if pairs:
    r = pairs[0]
    print(f"  e.g. base + {r.uint_opt(o1)} and base + {r.uint_opt(o2)}")


# --- 3. Constraint algebra: negate (and any_of / all_of) ---
# Constraints compose like booleans. negate(SameOffset) recovers section 2;
# any_of / all_of OR / AND several conditions over one join.
print("\n=== 3. composing constraints ===")


class SameOffset(k.JoinPredicate):
    def captures(self):
        return [o1, o2]

    def constraint(self, m):
        x, y = m.uint_opt(o1), m.uint_opt(o2)
        return x is not None and y is not None and x == y


negated = fn2.find_all(
    [read1, read2], constraints=[k.negate(SameOffset())], ignore_casts=True
)
print(f"negate(SameOffset) -> {len(negated)} rows (matches section 2)")
