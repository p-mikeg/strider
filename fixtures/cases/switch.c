// fixtures/cases/switch.c
//
// 8-arm dense switch with an explicit `kind & 0x7` bit-mask before the
// dispatch.  At -O2 gcc lowers this to a real `jmp *.rodata[idx*4]`
// jump-table dispatch on x86; the AND-mask gives KnownBits a direct
// upper-bound proof (`idx ≤ 7`) — without it, the orchestrator would
// have to discover the bound by walking the predecessor-If, which on
// -O2 x86 is wrapped in several `CastToBool(CastToInt(...))` chains
// and a `Not(Or(Less, Equal))` shape the canonical predecessor-If
// walker doesn't yet decompose.
//
// Each case body has a distinct `add(x, IntConst(K))` so the
// post-resolution IR carries 8 recognisable expressions for pattern
// matching tests.
int dispatch_value(int kind, int x) {
    unsigned i = (unsigned)kind & 0x7u;
    switch (i) {
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

int main(void) {
    volatile int s = 0;
    for (int i = 0; i < 9; i++) {
        s += dispatch_value(i, i);
    }
    return s;
}
