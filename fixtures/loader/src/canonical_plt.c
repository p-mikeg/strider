/* A non-PIC executable taking an imported function's address: `ext`'s
 * undefined .dynsym symbol holds its canonical PLT entry, which the loader
 * gives every other reference, while its JUMP_SLOT gets the library's `ext`. */
extern int ext(void);
int (*addr(void))(void) { return ext; }
int call_ext(void) { return ext() + 1; }
