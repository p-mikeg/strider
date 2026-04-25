/*
 * globals.c — exercises the LoadReadOnly fold against .rodata constants.
 */
#define NOINLINE __attribute__((noinline))

static const unsigned char k_byte = 'a';
static const int          k_int  = 0x12345678;
static const char *const  k_str  = "yes";

int NOINLINE read_const_byte(void)             { return (int)k_byte; }
int NOINLINE read_const_int (void)             { return k_int; }
int NOINLINE branch_on_const_string(int a, int b) {
    if (k_str[0] == 'y') return a + b;
    return a - b;
}
int NOINLINE runtime_const_idx(int idx) {
    static const int table[4] = { 10, 20, 30, 40 };
    if (idx < 0 || idx >= 4) return -1;
    return table[idx];
}

int main(void) {
    volatile int s = 0;
    s ^= read_const_byte();
    s ^= read_const_int();
    s ^= branch_on_const_string(1, 2);
    s ^= runtime_const_idx(2);
    return s;
}
