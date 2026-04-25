/*
 * builtins.c — GCC builtins lowered to dedicated p-code opcodes.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE popcount32(unsigned x)         { return __builtin_popcount(x); }
int NOINLINE clz32(unsigned x)              { return __builtin_clz(x); }
int NOINLINE ctz32(unsigned x)              { return __builtin_ctz(x); }
int NOINLINE popcount64(unsigned long long x) { return __builtin_popcountll(x); }
int NOINLINE clz64(unsigned long long x)      { return __builtin_clzll(x); }

int NOINLINE expect_branch(int x) {
    if (__builtin_expect(x > 100, 0)) return -1;
    return x + 1;
}

int main(void) {
    volatile int s = 0;
    s ^= popcount32(0xdeadbeefu);
    s ^= clz32(0x00010000u);
    s ^= ctz32(0x00010000u);
    s ^= popcount64(0xdeadbeefcafebabeULL);
    s ^= clz64(0x0000000000010000ULL);
    s ^= expect_branch(50);
    return s;
}
