/*
 * patterns.c — fixtures targeted at complex pattern-matching queries.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE mul_then_add(int a, int b, int c) { return a * b + c; }

int NOINLINE chained_xor_mask(unsigned x) {
    return (int)(((x ^ 0xdeadbeefu) & 0x00ff00ffu) ^ 0xcafebabeu);
}

int NOINLINE if_returns_const(int a) {
    if (a > 0) return 100;
    return -50;
}

int NOINLINE loop_with_invariant_load(const int *p, int n) {
    int s = 0;
    for (int i = 0; i < n; ++i) s += *p + i;
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
