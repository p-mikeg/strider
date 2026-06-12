// fixtures/cases/switch_sparse.c
//
// A switch whose case labels are sparse and far apart, so the compiler
// declines to build a jump table (a dense table would be mostly empty)
// and lowers it to a balanced comparison chain instead.  At -O2 gcc emits
// `cmp; je / jg` branches and `cmove`, with NO indexed `jmp *table` — so
// the lifted IR contains no `IndirectBranch` placeholder at all.
//
// This is the contrast fixture to the jump-table cases: it pins that a
// non-table switch flows through the pipeline as ordinary `If` control
// flow (the resolver is never invoked) and never leaves an unresolved
// indirect branch behind.

int sparse_dispatch(int k, int x) {
    switch (k) {
    case 3:    return x + 1;
    case 17:   return x + 2;
    case 42:   return x + 3;
    case 1000: return x + 4;
    default:   return -1;
    }
}

int main(void) {
    volatile int s = 0;
    int keys[] = {3, 17, 42, 1000, 5};
    for (int i = 0; i < 5; i++) {
        s += sparse_dispatch(keys[i], i);
    }
    return s;
}
