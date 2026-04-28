// fixtures/cases/indirect_branch.c
//
// Computed-goto fixture for the tier-2 indirect-branch resolver.
// At -O0 GCC and clang lower the indirect goto to either:
//   - a simple `mov reg, K; jmp *reg` (single target each time;
//     resolves at tier 1 once the array element is folded), or
//   - a load+jmp from a local array (tier 2 resolves via
//     LoadReadOnly + StackLoadForward), or
//   - a true jump table in .rodata (tier 2 resolves via the
//     R4 jump-table arm).
//
// Whichever shape lifters produce, the resolved IR must reach
// L0 / L1 cleanly with `Branch` / `TailCall` edges.
int indirect_branch_resolved(int x) {
    void *targets[] = {&&L0, &&L1};
    goto *targets[(unsigned)x & 1];
L0: return 0;
L1: return 1;
}

int main(void) {
    volatile int s = 0;
    s ^= indirect_branch_resolved(0);
    s ^= indirect_branch_resolved(1);
    return s;
}
