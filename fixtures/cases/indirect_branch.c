// fixtures/cases/indirect_branch.c
//
// A computed goto through a local array of two labels, lowered to a load
// from a stack copy of the array or from a label table in a data section.
// The resolved IR must reach L0 and L1. It does not resolve on aarch64be,
// arm_be8, mips64, ppc32le and ppc64: see `GAPS` in
// crates/strider-orchestrator/tests/golden.rs.
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
