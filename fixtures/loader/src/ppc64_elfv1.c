/* GCC's ELFv1 output has no dot symbols: `sw`'s descriptor symbol in `.opd`
 * carries the code's size. */
int sw(int x)
{
    switch (x) {
    case 0: return 11;
    case 1: return 22;
    case 2: return 33;
    case 3: return 44;
    default: return x * 3 + 1;
    }
}
