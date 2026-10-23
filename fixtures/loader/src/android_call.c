/* A separate unit, so the call goes through the table. */
extern int (*const ptrs[4])(void) __attribute__((visibility("hidden")));
int call1(void) { return ptrs[1](); }
