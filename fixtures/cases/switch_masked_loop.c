/* A masked switch index carried around a loop through a phi.
 *
 * GCC proves `i < 6` from the loop guard and emits a SIX-entry jump table,
 * while the `& 7` alone admits 0..7.  Recovering the tighter bound needs the
 * back edge's guard, which does not dominate the loop header, so a classifier
 * working from the mask over-approximates and indexes past the table.  Such a
 * site must come back unresolved rather than failing the whole function. */

__attribute__((noinline)) static int masked_loop_sink(int v) {
    return v * 3;
}

int masked_loop_switch(int x) {
    int acc = 0;
    unsigned i = 0;
    while (i < 6u) {
        switch (i & 7u) {
        case 0: acc += x + 1; break;
        case 1: acc += x + 2; break;
        case 2: acc += x + 3; break;
        case 3: acc += x + 4; break;
        case 4: acc += x + 5; break;
        case 5: acc += masked_loop_sink(x); break;
        case 6: acc += x + 7; break;
        case 7: acc += x + 8; break;
        }
        i = (unsigned)(acc & 7);
    }
    return acc;
}

int main(void) {
    volatile int s = masked_loop_switch(1);
    return s;
}
