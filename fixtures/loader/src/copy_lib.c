/* `tbl`, a read-only object a PIE reads directly: the PIE's link copies it
 * into its own `.data.rel.ro` under an R_*_COPY, and `ld.so` fills it from
 * here. */
const int tbl[8] = {1, 2, 3, 4, 5, 6, 7, 8};
