/* `p`, a relro word holding `x`'s address: one R_MIPS_REL32 against `x` in a
 * MIPS library's `.rel.dyn`, one R_X86_64_RELATIVE in an x86-64 PIE's
 * `.rela.dyn`. */
int x;
int *const p = &x;
int get(void) { return *p; }
