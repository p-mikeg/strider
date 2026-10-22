/* A microMIPS image: EF_MIPS_ARCH_ASE_MICROMIPS in e_flags, and every
 * function's address has the ISA bit set. */
int min(int a, int b) { return a < b ? a : b; }
