/* Non-PIC code in a shared library: `getl` and `geti` load `loc` and `imp`
 * through absolute operands, each under an R_386_32 in `.text`, which makes
 * the library DT_TEXTREL. */
int loc = 7;
extern int imp;
int getl(void) { return loc; }
int geti(void) { return imp; }
