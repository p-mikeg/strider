/*
 * calling_convention.c — fixtures for calling-convention coverage tests.
 *
 * Pins commit f179c7f's sub-register fallback fix in `detect_register_args`
 * AND systematically exercises every parameter-count regime
 * (1, 2, 4, 8, 16) plus narrow-width and pointer parameter shapes,
 * so future regressions in the calling-convention machinery
 * (`opt::FunctionArgDetect`, `target::CallingConvention`) are caught
 * across all 14 supported arches.
 *
 * Each callee is a locally-bodied opaque (asm-volatile barrier) so
 * freestanding builds link cleanly without libc.  Each forwarder
 * passes its parameters straight through to a sinkN call so the
 * test can match `call(sinkN).arg(I, function_arg(I))` for every I.
 */
#define NOINLINE __attribute__((noinline))

/* Local typedef so we don't need <stdint.h> in freestanding builds. */
typedef unsigned long uintptr_t;

/* ── Sinks ──────────────────────────────────────────────────────────────────
 * Locally-bodied so freestanding builds link cleanly; the asm-volatile
 * "memory" clobber forces every parameter to be live at the call site,
 * preventing DCE from dropping any positional arg. */

void NOINLINE sink1(int a) {
    __asm__ volatile ("" :: "r"(a) : "memory");
}
void NOINLINE sink2(int a, int b) {
    __asm__ volatile ("" :: "r"(a), "r"(b) : "memory");
}
void NOINLINE sink4(int a, int b, int c, int d) {
    __asm__ volatile ("" :: "r"(a), "r"(b), "r"(c), "r"(d) : "memory");
}
/* sink8: 8 register-bound asm operands exceeds x86's 8-GPR allocation
 * limit (gcc reserves at least one for sp/bp on -fno-omit-frame-pointer
 * targets).  Use the same volatile-store-load idiom as sink16. */
void NOINLINE sink8(int a, int b, int c, int d,
                    int e, int f, int g, int h) {
    volatile int s = 0;
    s = a + b + c + d + e + f + g + h;
    /* Read s back so clang doesn't warn set-but-not-used. */
    __asm__ volatile ("" :: "r"((int)s) : "memory");
}
/* sink16: clobbering 16 registers in inline asm exceeds the constraint
 * limit on some arches (notably x86 and arm).  Force the params to be
 * live by routing them through a volatile local store-load. */
void NOINLINE sink16(int a, int b, int c, int d,
                     int e, int f, int g, int h,
                     int i, int j, int k, int l,
                     int m, int n, int o, int p) {
    volatile int s = 0;
    s = a + b + c + d + e + f + g + h
      + i + j + k + l + m + n + o + p;
    /* Read s back so clang doesn't warn set-but-not-used. */
    __asm__ volatile ("" :: "r"((int)s) : "memory");
}

/* Sink for narrow-width forwarder. */
void NOINLINE sink_narrow(int a, int b, int c, int d) {
    __asm__ volatile ("" :: "r"(a), "r"(b), "r"(c), "r"(d) : "memory");
}

/* Sink that takes one value plus two pointers (mixed-type forwarder). */
void NOINLINE sink_mixed(int a, int *p, int c, int *q) {
    __asm__ volatile ("" :: "r"(a), "r"(p), "r"(c), "r"(q) : "memory");
}

/* ── Forwarders (identity-style) ────────────────────────────────────────────
 * Every forwarder takes N parameters and passes them straight through
 * to sinkN in the SAME order so the test can match
 *   call(sinkN).arg(I, function_arg(I))
 * for every I in 0..N. */

/* Each forwarder writes its parameters into a `volatile` global
 * accumulator BEFORE the sinkN call, then passes the same parameters
 * through the call.  The volatile write forces the compiler to:
 *  (a) materialise every parameter as an opaque value before the call
 *      (no constant-fold / dead-code shortcuts),
 *  (b) keep each parameter live in its arg-passing container register
 *      at the call site (sub-register-only reads can't satisfy a
 *      volatile-int store of the original value).
 * Both properties are necessary for the analyzer's
 * `detect_register_args` to either exact-match the arg register's
 * `InitialVar` or, failing that, fall back to a sub-register match. */

extern volatile int g_sink_int;
extern volatile int g_sink_p_int;
volatile int g_sink_int = 0;
volatile int g_sink_p_int = 0;

int NOINLINE forward_1(int a) {
    g_sink_int = a;
    sink1(a);
    return a;
}
int NOINLINE forward_2(int a, int b) {
    g_sink_int = a; g_sink_int = b;
    sink2(a, b);
    return a + b;
}
int NOINLINE forward_4(int a, int b, int c, int d) {
    g_sink_int = a; g_sink_int = b;
    g_sink_int = c; g_sink_int = d;
    sink4(a, b, c, d);
    return a + b + c + d;
}
int NOINLINE forward_8(int a, int b, int c, int d,
                       int e, int f, int g, int h) {
    g_sink_int = a; g_sink_int = b;
    g_sink_int = c; g_sink_int = d;
    g_sink_int = e; g_sink_int = f;
    g_sink_int = g; g_sink_int = h;
    sink8(a, b, c, d, e, f, g, h);
    return a + b + c + d + e + f + g + h;
}
int NOINLINE forward_16(int a, int b, int c, int d,
                        int e, int f, int g, int h,
                        int i, int j, int k, int l,
                        int m, int n, int o, int p) {
    g_sink_int = a; g_sink_int = b;
    g_sink_int = c; g_sink_int = d;
    g_sink_int = e; g_sink_int = f;
    g_sink_int = g; g_sink_int = h;
    g_sink_int = i; g_sink_int = j;
    g_sink_int = k; g_sink_int = l;
    g_sink_int = m; g_sink_int = n;
    g_sink_int = o; g_sink_int = p;
    sink16(a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p);
    return a + b + c + d + e + f + g + h
         + i + j + k + l + m + n + o + p;
}

/* ── Narrow widths ──────────────────────────────────────────────────────────
 * Cover sub-register handling.  Each of these C-source widths
 * (signed/unsigned char, short, unsigned short) maps to a
 * narrower-than-container Vn that the IR may surface either as a
 * narrow `InitialVar` (which `detect_register_args`'s sub-register
 * fallback must promote to a `FunctionArg`) or as a `Truncate` /
 * `Extract` of the full container `FunctionArg`. */

int NOINLINE narrow_widths(signed char a, unsigned char b,
                           short c, unsigned short d) {
    /* Force each narrow parameter to be live in its (sub-)register
     * before the call; without this the optimiser routes the values
     * through scratch regs and erases the InitialVar entirely. */
    g_sink_int = (int)a; g_sink_int = (int)b;
    g_sink_int = (int)c; g_sink_int = (int)d;
    sink_narrow((int)a, (int)b, (int)c, (int)d);
    return (int)a + (int)b + (int)c + (int)d;
}

/* ── Mixed types ────────────────────────────────────────────────────────────
 * Two pointers interleaved with two ints — exercises pointer-typed
 * arg slots that on some arches share register classes with int
 * parameters and on others don't. */

int NOINLINE mixed_4(int a, int *p, int c, int *q) {
    g_sink_int = a; g_sink_p_int = (int)(uintptr_t)p;
    g_sink_int = c; g_sink_p_int = (int)(uintptr_t)q;
    sink_mixed(a, p, c, q);
    *p = a + c;
    *q = a - c;
    return *p ^ *q;
}

/* ── Return-value chaining ──────────────────────────────────────────────────
 * `returns_int` produces a return value; `uses_return` consumes it
 * (twice, so it can't be DCE'd) and the test verifies the outer
 * Call's relevant input slot traces back to the inner Call. */

int NOINLINE returns_int(int x) {
    g_sink_int = x;
    return x * 3 + 1;
}
int NOINLINE uses_return(int x) {
    g_sink_int = x;
    int r = returns_int(x);
    /* Barrier so the optimiser cannot fuse the call-uses-call edge
     * into a single arithmetic op. */
    g_sink_int = r;
    return r + r;
}

int main(void) {
    /* Reference every forwarder so DCE can't drop them.  Use a
     * volatile sink to keep the side-effect chain live. */
    volatile int sink = 0;
    sink ^= forward_1(1);
    sink ^= forward_2(1, 2);
    sink ^= forward_4(1, 2, 3, 4);
    sink ^= forward_8(1, 2, 3, 4, 5, 6, 7, 8);
    sink ^= forward_16(1, 2, 3, 4, 5, 6, 7, 8,
                       9, 10, 11, 12, 13, 14, 15, 16);
    sink ^= narrow_widths((signed char)1, (unsigned char)2,
                          (short)3, (unsigned short)4);
    int x = 5, y = 6;
    sink ^= mixed_4(7, &x, 8, &y);
    sink ^= uses_return(9);
    return sink;
}
