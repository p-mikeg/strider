/* An IFUNC `f` and an undefined array `tab`, each at an extern address in
 * the unlinked object, `tab`'s the slot past `f`'s. */
typedef int fn_t(int);
static int impl_a(int x) { return x + 1; }
static int impl_b(int x) { return x + 2; }
int cpu_flag;
static fn_t *resolve_f(void) { return cpu_flag ? impl_a : impl_b; }
int f(int) __attribute__((ifunc("resolve_f")));
extern int tab[];
int read_tab(void) { return tab[2] + f(1); }
