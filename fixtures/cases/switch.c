// fixtures/cases/switch.c
//
// 8-arm dense switch with a mix of cheap arithmetic case bodies and
// one "interesting" case that loads a struct field and calls a helper
// — case 5 returns `f(value->a)`.  Every case body has a distinct,
// recognisable IR shape so post-resolution pattern queries can pin
// each arm independently.
//
// At -O2, gcc lowers this to a real `.rodata` jump table on x86 (and
// the analogous shape on the other supported arches).  The explicit
// `kind & 0x7` mask gives `KnownBits` a direct upper-bound proof so
// the orchestrator's classifier resolves the table on the rodata-load
// arm without having to walk the predecessor `If`.

struct dispatch_value_arg {
    int a;
    int b;
};

// `f` is defined later in this file with `__attribute__((noinline))`
// so gcc -O2 doesn't inline its body into the case-5 arm — without
// the attribute the lifter would see `Load(value->a) + 100` and the
// Call shape would disappear.  `-fno-optimize-sibling-calls` (set in
// fixtures/Makefile) additionally prevents the call site from being
// turned into a tail-jump.
int f(int v) __attribute__((noinline));

int dispatch_value(int kind, int x, struct dispatch_value_arg *value) {
    unsigned i = (unsigned)kind & 0x7u;
    switch (i) {
    case 0: return x + 1;
    case 1: return x + 2;
    case 2: return x + 3;
    case 3: return x + 4;
    case 4: return x + 5;
    case 5: return f(value->a);   // load + call — distinct shape
    case 6: return x + 7;
    case 7: return x + 8;
    default: return -1;
    }
}

__attribute__((noinline))
int f(int v) {
    return v + 100;
}

int main(void) {
    struct dispatch_value_arg sample = {.a = 10, .b = 20};
    volatile int s = 0;
    for (int i = 0; i < 9; i++) {
        s += dispatch_value(i, i, &sample);
    }
    return s;
}
