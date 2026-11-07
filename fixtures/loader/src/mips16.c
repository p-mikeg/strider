/* A MIPS16 function in a MIPS32 image: its `st_value` is even and
 * STO_MIPS16 in `st_other` marks the ISA. */
int __attribute__((mips16)) min16(int a, int b) { return a < b ? a : b; }
