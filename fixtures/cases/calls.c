/*
 * calls.c — direct, indirect, mutual, and nested calls.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE fib_recursive(int n) {
    if (n <= 1) return n;
    return fib_recursive(n - 1) + fib_recursive(n - 2);
}

int NOINLINE mutual_a(int n);
int NOINLINE mutual_b(int n);
int NOINLINE mutual_a(int n) { return n <= 0 ? 0 : 1 + mutual_b(n - 1); }
int NOINLINE mutual_b(int n) { return n <= 0 ? 0 : 1 + mutual_a(n - 1); }

int NOINLINE leaf(int x)     { return x + 1; }
int NOINLINE mid(int x)      { return leaf(x) * 2; }
int NOINLINE nested_3deep(int x) { return mid(x) - 1; }

int NOINLINE pair_a(int x, int y) { return x + y; }
int NOINLINE repeat_call_pair(int a, int b) {
    return pair_a(a, b) + pair_a(b, a);
}

int NOINLINE pass_through(int x) { return leaf(x); }

typedef int (*fnptr)(int);
int NOINLINE apply_indirect(fnptr f, int x) { return f(x); }

int main(void) {
    volatile int s = 0;
    s ^= fib_recursive(6);
    s ^= mutual_a(5);
    s ^= nested_3deep(3);
    s ^= repeat_call_pair(1, 2);
    s ^= pass_through(4);
    s ^= apply_indirect(&leaf, 5);
    return s;
}
