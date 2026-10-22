/* `p`, a relro word holding `x`'s address: one R_MIPS_REL32 in `.rel.dyn`. */
int x;
int *const p = &x;
int get(void) { return *p; }
