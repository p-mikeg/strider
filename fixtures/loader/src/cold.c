/* GCC -O2 moves the unlikely branch of `foo` into `foo.cold`, entered by a
   conditional branch from the body and jumping back into it. */
__attribute__((cold, noinline)) void log_err(int x) { __asm__ volatile("" ::"r"(x)); }
int g;
int foo(int *p, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (p[i] < 0) {
            log_err(i);
            g += 3;
        }
        s += p[i];
    }
    return s;
}
