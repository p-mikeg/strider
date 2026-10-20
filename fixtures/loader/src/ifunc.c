/* An IFUNC `f` and a plain function `h`, each reached through a GOT slot
 * under -fno-plt, and `f_ptr`, a word holding `f`. The loader writes the
 * resolver's RESULT into f's slot and f_ptr: an R_*_IRELATIVE in a PIE, a
 * GLOB_DAT or an absolute word against the IFUNC symbol in a shared library.
 * Built -fno-pic, call_f and addr_f reach `f` from the instruction itself. */
typedef int fn_t(int);
static int impl_a(int x) { return x + 1; }
static int impl_b(int x) { return x + 2; }
int cpu_flag;
static fn_t *resolve_f(void) { return cpu_flag ? impl_a : impl_b; }
int f(int) __attribute__((ifunc("resolve_f")));
int h(int x) { return x + 3; }
int call_f(int x) { return f(x); }
int call_h(int x) { return h(x); }
fn_t *addr_f(void) { return f; }
fn_t *const f_ptr = f;
