/*
 * patterns.c — fixtures targeted at complex pattern-matching queries.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE mul_then_add(int a, int b, int c) { return a * b + c; }

/* chained_xor_mask: a three-deep (x ^ k1) & m1 ^ k2 chain.  ConstantFold
 * may collapse the constants into a single fused mask, so pattern queries
 * against this fixture should match on structural shape (xor/and/xor)
 * with `any()` operands rather than insisting on three distinct IntConst
 * captures. */
int NOINLINE chained_xor_mask(unsigned x) {
    return (int)(((x ^ 0xdeadbeefu) & 0x00ff00ffu) ^ 0xcafebabeu);
}

int NOINLINE if_returns_const(int a) {
    if (a > 0) return 100;
    return -50;
}

int NOINLINE loop_with_invariant_load(const int *p, int n) {
    /* The load `*p` reads the same address every iteration, but the
     * `volatile` cast tells GCC it MAY change between reads — so the
     * load is NOT loop-invariant from the optimizer's perspective.
     *
     * This is load-bearing for the test: without `volatile`, GCC at
     * -O2 (especially `-mregparm=3` profile for the kernel build)
     * recognises that `*p + i` summed over `0..n` is a closed-form
     * arithmetic-progression formula and lowers the entire function
     * to straight-line `imul/mul/shld/add` with no back-edge.  Strider's
     * `count_loops` would then see zero loop headers and the test
     * would fail on the kernel arch even though the source describes
     * a real loop.
     *
     * The `volatile` qualifier on the load address forces GCC to keep
     * the per-iteration load AND the per-iteration arithmetic in the
     * binary, so the strider lift sees a real loop on every arch. */
    int s = 0;
    const volatile int *vp = (const volatile int *)p;
    for (int i = 0; i < n; ++i) s += *vp + i;
    return s;
}

int NOINLINE recursive_with_accumulator(int n, int acc) {
    if (n <= 0) return acc;
    return recursive_with_accumulator(n - 1, acc + n);
}

int main(void) {
    volatile int s = 0;
    s ^= mul_then_add(2, 3, 4);
    s ^= chained_xor_mask(0xdeadbeefu);
    s ^= if_returns_const(-1);
    int p = 7;
    s ^= loop_with_invariant_load(&p, 5);
    s ^= recursive_with_accumulator(5, 0);
    return s;
}
