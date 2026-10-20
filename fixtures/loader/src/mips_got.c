/* A MIPS PIC executable whose global GOT entries name imported symbols: no
 * relocation record covers them, and the loader fills each from .dynsym.
 *   ext_var   an object, file word 0
 *   ext_fn    an address-taken function, file word 0
 *   ext_call  a called function, file word its lazy-binding stub */
extern int ext_var;
extern int ext_fn(int);
extern int ext_call(int);
int get(void) { return ext_var; }
int *addr(void) { return &ext_var; }
int (*getfp(void))(int) { return ext_fn; }
int call_ext(int x) { return ext_call(x) + 1; }
void __start(void) {}
