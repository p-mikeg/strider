// fixtures/cases/switch_phi_index.c
//
// A computed goto over a six-entry label table through an index that is
// one of three constants, `s` in {0, 3, 5}.  Built at -O0 on every arch
// (`O0_CASES` in the Makefile): gcc keeps `s` in a stack slot, which the
// optimiser forwards into a phi of the three constants, and -O1 and up
// fold the dispatch away.  Only `t[0]`, `t[3]` and `t[5]` are reachable;
// the range 0..5 the three span also names `t[1]`, `t[2]` and `t[4]`.

int h0(int x) { return x; }
int h1(int x) { return x + 1; }
int h2(int x) { return x + 2; }
int h3(int x) { return x + 3; }
int h4(int x) { return x + 4; }
int h5(int x) { return x + 5; }

int phi_index_dispatch(int x, int v) {
    static void *const t[] = { &&a, &&b, &&c, &&d, &&e, &&f };
    int s = x > 10 ? 3 : (x > 5 ? 5 : 0);
    goto *t[s];
a:  return h0(v);
b:  return h1(v) + 1;
c:  return h2(v) * 3;
d:  return h3(v) ^ 4;
e:  return h4(v) - 5;
f:  return h5(v) | 6;
}

int main(void) {
    volatile int x = 7;
    return phi_index_dispatch(x, 1);
}
