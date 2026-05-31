/* tzcount.c — count trailing zeros of a small mask.
 *
 * The body is a tight loop with an iteration cap, used as the smallest
 * function shape that exercises the bounded-lift / overflow path in the
 * cfg builder.  A bigger purpose for this fixture: a `.o` (ET_REL) form
 * of it pins that the loader's section-walker (used for relocatable
 * objects, which have no PT_LOAD program headers) actually surfaces
 * the `.text` bytes — without that, `strider.load("tzcount.o")`
 * silently produces an empty memory map and any analysis is a no-op.
 */

#define NOINLINE __attribute__((noinline))

unsigned NOINLINE tzcount(unsigned x) {
    unsigned n = 0;
    while ((x & 1) == 0 && n < 32) {
        x >>= 1;
        n += 1;
    }
    return n;
}

int main(void) {
    volatile unsigned sink = tzcount(0x10);
    return (int)sink;
}
