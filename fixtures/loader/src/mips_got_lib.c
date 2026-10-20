/* The library mips_got.c imports from. Its own primary GOT holds a global
 * entry for the symbol it defines and takes the address of. */
int ext_var = 3;
int ext_fn(int x) { return x + 1; }
int ext_call(int x) { return x + 2; }
int *lib_addr(void) { return &ext_var; }
