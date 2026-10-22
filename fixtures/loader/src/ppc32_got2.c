/* Two jump tables whose bases a PPC32 -fPIC library loads out of `.got2`, its
 * per-object GOT, each slot one R_PPC_RELATIVE. */
extern int h0(int), h1(int), h2(int), h3(int), h4(int), h5(int), h6(int), h7(int);

int f(int x, int y)
{
    switch (x) {
    case 3: return h0(y);
    case 4: return h1(y) + 1;
    case 5: return h2(y) + 2;
    case 6: return h3(y) * 3;
    case 8: return h4(y) ^ 5;
    case 9: return h5(y) - 7;
    case 10: return h6(y) + 11;
    case 12: return h7(y) * 13;
    default: return y;
    }
}

int g(int x, int y)
{
    switch (x) {
    case -3: return h0(y);
    case -2: return h1(y) + 1;
    case -1: return h2(y) + 2;
    case 0: return h3(y) * 3;
    case 1: return h4(y) ^ 5;
    case 2: return h5(y) - 7;
    case 4: return h6(y) + 11;
    default: return y;
    }
}
