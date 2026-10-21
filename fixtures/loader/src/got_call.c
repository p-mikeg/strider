/* Calls through an imported function's GOT slot under -fno-plt: `call` and
 * a tail `jmp`, each an R_386_GOT32X. Built -fno-pic the instruction names
 * the slot's own address (ModRM disp32, no base register); built -fPIC it
 * names the slot's offset from the GOT, which a register holds. */
extern void ext_fn(int);
void f(int x) { ext_fn(x); ext_fn(x + 1); }
