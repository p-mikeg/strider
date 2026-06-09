/*
 * known_bits.c — exercises the KnownBits bit-level lattice pass.
 *
 * `kb_or_then_mask` ORs bit 0 to one, then masks with `& 1`.  A
 * `__asm__ volatile("" : "+r"(x))` barrier sits BETWEEN the `|= 1` and the
 * `& 1`, so the C compiler at -O2 cannot collapse the pair: it emits a real
 * `or $1` followed by a real `and $1` (verified on x64 and mips32be — the
 * compiler does NOT pre-fold to `mov $1`).  The lifted IR is therefore
 * `And(Or(x, 1), 1)`.
 *
 * strider's bit-level KnownBits pass knows bit 0 of `Or(x, 1)` is one and
 * the higher bits don't matter under `& 1`, so it folds the whole thing to
 * the constant 1 and the `And` node disappears.  Plain ConstantFold CANNOT
 * do this — it has no bit-lattice and the `or`'s operand `x` is opaque — so
 * this fixture pins the KnownBits pass specifically: the e2e assertion fails
 * if KnownBits is removed from the pipeline.
 */
#ifndef NOINLINE
#define NOINLINE __attribute__((noinline))
#endif

int NOINLINE kb_or_then_mask(int x) {
    x |= 1;                          /* bit 0 now provably one */
    __asm__ volatile("" : "+r"(x));  /* barrier: keep the `or` and `and` distinct */
    return x & 1;                    /* KnownBits folds this to constant 1 */
}

int main(void) {
    volatile int s = 0;
    s ^= kb_or_then_mask(s);
    return s;
}
