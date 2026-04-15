/*
 * test.c — exercise binary to stress-test the analyzer pipeline.
 *
 * Covers: arithmetic, conditionals, loops, memory accesses, bitwise ops,
 * multi-return paths, recursion, nested function calls, and jump tables.
 *
 * Compile:  i686-linux-gnu-gcc-11 -m32 -O0 -g test.c -o binary_test
 *
 * Notes:
 *   - The jump-table candidate functions below are marked optimize("O2")
 *     so GCC is much more likely to lower the dense switches into actual
 *     jump tables even if the file is otherwise built with -O0.
 */

/* ── simple arithmetic ─────────────────────────────────────────────── */

int __attribute__((noinline)) add(int a, int b) {
    return a + b;
}

int __attribute__((noinline)) sub(int a, int b) {
    return a - b;
}

int __attribute__((noinline)) mul(int a, int b) {
    return a * b;
}

/* ── bitwise operations ────────────────────────────────────────────── */

int __attribute__((noinline)) bitwise_ops(int a, int b) {
    int r = a & b;
    r = r | (a ^ b);
    r = r << 2;
    r = r >> 1;
    return ~r;
}

/* ── conditional branch (if / else) ───────────────────────────────── */

int __attribute__((noinline)) abs_val(int x) {
    if (x < 0)
        return -x;
    return x;
}

int __attribute__((noinline)) max_val(int a, int b) {
    if (a > b)
        return a;
    else
        return b;
}

int __attribute__((noinline)) clamp(int x, int lo, int hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}

/* ── loops ─────────────────────────────────────────────────────────── */

int __attribute__((noinline)) sum_to_n(int n) {
    int s = 0;
    int i = 0;
    while (i <= n) {
        s += i;
        i++;
    }
    return s;
}

int __attribute__((noinline)) factorial(int n) {
    int r = 1;
    for (int i = 2; i <= n; i++)
        r *= i;
    return r;
}

int __attribute__((noinline)) count_bits(unsigned int x) {
    int count = 0;
    while (x) {
        count += x & 1;
        x >>= 1;
    }
    return count;
}

/* ── memory / pointer operations ───────────────────────────────────── */

int __attribute__((noinline)) array_sum(int *arr, int len) {
    int total = 0;
    for (int i = 0; i < len; i++)
        total += arr[i];
    return total;
}

void __attribute__((noinline)) array_fill(int *arr, int len, int val) {
    for (int i = 0; i < len; i++)
        arr[i] = val;
}

/* ── recursion ─────────────────────────────────────────────────────── */

int __attribute__((noinline)) fib(int n) {
    if (n <= 1)
        return n;
    return fib(n - 1) + fib(n - 2);
}

/* ── nested calls (exercises call IR nodes) ───────────────────────── */

int __attribute__((noinline)) g(int x, int y) {
    return add(x + 2, mul(2 * x, sub(x, y)));
}

/* ── recursion ─────────────────────────────────────────────────────── */

int __attribute__((noinline)) hard_func(int a, int b, int c) {
    char buf[16] = { 0 };
    if (c > 5) {
        array_fill(&buf, 1, a);
    } else {
        b += array_sum(&buf, 1);
    }
    array_fill(&a, 1, b);
    return g(a, b);
}

/* ── jump-table candidates ─────────────────────────────────────────── */

/*
 * Dense switch over 0..9.
 * This is a classic candidate for jump-table lowering.
 */
int __attribute__((noinline, optimize("O2")))
jump_table_dense(int x, int sel) {
    switch (sel) {
        case 0: return x + 11;
        case 1: return x - 7;
        case 2: return x * 3;
        case 3: return x ^ 0x55;
        case 4: return x | 0x1234;
        case 5: return x & 0x7f;
        case 6: return (x << 2) + 1;
        case 7: return (x >> 1) - 9;
        case 8: return ~x;
        case 9: return abs_val(x - 20);
        default: return -1;
    }
}

/*
 * Another dense switch, but each arm does slightly different work and some
 * nested calls, to make the control flow more interesting.
 */
int __attribute__((noinline, optimize("O2")))
jump_table_calls(int a, int b, int sel) {
    switch (sel) {
        case 0: return add(a, b);
        case 1: return sub(a, b);
        case 2: return mul(a, b);
        case 3: return bitwise_ops(a, b);
        case 4: return max_val(a, b);
        case 5: return clamp(a, -5, 5);
        case 6: return sum_to_n(a & 7);
        case 7: return count_bits((unsigned int)(a ^ b));
        default: return 0x5a5a;
    }
}

/*
 * Small state-machine style dispatcher to produce repeated indexed jumps.
 */
int __attribute__((noinline, optimize("O2")))
jump_table_loop(int seed, int rounds) {
    int state = seed & 7;
    int acc = 0;

    for (int i = 0; i < rounds; i++) {
        switch (state) {
            case 0:
                acc += i;
                state = 3;
                break;
            case 1:
                acc ^= (i << 1);
                state = 6;
                break;
            case 2:
                acc -= seed;
                state = 5;
                break;
            case 3:
                acc += mul(i, 2);
                state = 1;
                break;
            case 4:
                acc += sub(seed, i);
                state = 7;
                break;
            case 5:
                acc += bitwise_ops(acc, i);
                state = 0;
                break;
            case 6:
                acc += add(acc, seed & 3);
                state = 4;
                break;
            case 7:
                acc += abs_val(seed - i);
                state = 2;
                break;
            default:
                acc ^= 0xdead;
                state = 0;
                break;
        }
    }

    return acc;
}

int test_call(int a, int b) {
    return bitwise_ops(a, b) + bitwise_ops(b,a);
}

/* ── entry point ────────────────────────────────────────────────────── */

int main(int argc, char **argv) {
    int a = argc;
    int b = argc * 2;

    volatile int r = 0;
    r += add(a, b);
    r += sub(a, b);
    r += mul(a, b);
    r += bitwise_ops(a, b);
    r += abs_val(-a);
    r += max_val(a, b);
    r += clamp(a, 0, 10);
    r += sum_to_n(a);
    r += factorial(5);
    r += count_bits((unsigned int)a);
    r += g(a, b);
    r += fib(8);

    int arr[8];
    array_fill(arr, 8, a);
    r += array_sum(arr, 8);

    // /* Exercise jump-table candidates with non-constant selectors. */
    // r += jump_table_dense(a, (a + b) % 10);
    // r += jump_table_dense(b, (a * 3 + b) % 10);

    // r += jump_table_calls(a, b, (a ^ b) & 7);
    // r += jump_table_calls(b, a, ((a + 1) ^ (b + 3)) & 7);

    // r += jump_table_loop(a ^ b, 12);

    return r;
}