/* Reads `tbl` from its copy-relocated slot: 4 at run time, 0 in the file. */
extern const int tbl[8];
int get3(void) { return tbl[3]; }
