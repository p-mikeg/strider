/*
 * control.c — branches, loops, and merge points.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE abs_val(int x)             { return x < 0 ? -x : x; }
int NOINLINE max_val(int a, int b)      { return a > b ? a : b; }
int NOINLINE clamp(int x, int lo, int hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}
int NOINLINE select_three(int a, int b, int c, int sel) {
    if (sel == 0) return a;
    if (sel == 1) return b;
    return c;
}

int NOINLINE sum_to_n(int n) {
    int s = 0;
    for (int i = 0; i <= n; ++i) s += i;
    return s;
}
int NOINLINE factorial(int n) {
    int r = 1;
    for (int i = 2; i <= n; ++i) r *= i;
    return r;
}
int NOINLINE count_bits(unsigned x) {
    int c = 0;
    while (x) { c += (int)(x & 1u); x >>= 1; }
    return c;
}
int NOINLINE nested_loops(int n, int m) {
    int s = 0;
    for (int i = 0; i < n; ++i)
        for (int j = 0; j < m; ++j)
            s += i * j;
    return s;
}
int NOINLINE early_return(int n) {
    for (int i = 0; i < n; ++i) {
        if (i == 7) return i;
    }
    return -1;
}

int main(void) {
    volatile int s = 0;
    s ^= abs_val(-3); s ^= max_val(3, 4); s ^= clamp(5, 0, 10);
    s ^= select_three(1, 2, 3, 1);
    s ^= sum_to_n(5); s ^= factorial(5); s ^= (int)count_bits(0xdeadbeefu);
    s ^= nested_loops(3, 4); s ^= early_return(9);
    return s;
}
