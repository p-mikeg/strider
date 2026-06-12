// fixtures/cases/switch_value_range.c
//
// Jump-table fixtures whose index bound comes from the compiler's own
// range-check `If`, NOT from an explicit mask — the complement of
// `switch.c` (which masks `kind & 0x7` so the bound falls out of
// `KnownBits`).  These exercise the classifier's `value_range` arm: to
// bound the table index it must walk the dominating `cmp; ja` guard the
// compiler emits ahead of the indexed jump.
//
// At -O2 gcc lowers both to a real `.rodata` jump table on x86/x64:
//
//   dispatch_unmasked:  cmp $7, k;       ja default; jmp *tbl(,k,8)
//   dispatch_offset:    sub $10, k; cmp $7, k; ja default; jmp *tbl(,k,8)
//
// `dispatch_offset`'s cases start at 10, so the compiler subtracts the
// base before indexing — the table index is `k - 10`, and `value_range`
// must propagate the bound back through that subtraction.  Each arm body
// is a distinct `x + N` so a post-resolution pattern query can pin it.

int dispatch_unmasked(int k, int x) {
    switch (k) {
    case 0: return x + 1;
    case 1: return x + 2;
    case 2: return x + 3;
    case 3: return x + 4;
    case 4: return x + 5;
    case 5: return x + 6;
    case 6: return x + 7;
    case 7: return x + 8;
    default: return -1;
    }
}

int dispatch_offset(int k, int x) {
    switch (k) {
    case 10: return x + 1;
    case 11: return x + 2;
    case 12: return x + 3;
    case 13: return x + 4;
    case 14: return x + 5;
    case 15: return x + 6;
    case 16: return x + 7;
    case 17: return x + 8;
    default: return -1;
    }
}

int main(void) {
    volatile int s = 0;
    for (int i = 0; i < 9; i++) {
        s += dispatch_unmasked(i, i);
        s += dispatch_offset(i + 10, i);
    }
    return s;
}
