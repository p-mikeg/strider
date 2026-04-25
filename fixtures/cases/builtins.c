/*
 * builtins.c — GCC builtins lowered to dedicated p-code opcodes.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE popcount32(unsigned x)         { return __builtin_popcount(x); }
int NOINLINE clz32(unsigned x)              { return __builtin_clz(x); }
int NOINLINE ctz32(unsigned x)              { return __builtin_ctz(x); }
int NOINLINE popcount64(unsigned long long x) { return __builtin_popcountll(x); }
int NOINLINE clz64(unsigned long long x)      { return __builtin_clzll(x); }

/* expect_branch: a plain branch annotated with __builtin_expect.  Use
 * memory-clobber asm barriers to force the compiler to emit a real
 * conditional branch (instead of a conditional-move / select pattern,
 * which doesn't lift to an `If` p-code node). */
int NOINLINE expect_branch(int x) {
    __asm__ volatile ("" :: "r"(x) : "memory");
    if (__builtin_expect(x > 100, 0)) {
        __asm__ volatile ("" :: "r"(x) : "memory");
        return -1;
    }
    __asm__ volatile ("" :: "r"(x) : "memory");
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
