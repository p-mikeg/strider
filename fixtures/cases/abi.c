/*
 * abi.c — exercises calling-convention argument materialisation.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE eight_int_args(int a, int b, int c, int d,
                            int e, int f, int g, int h) {
    return a + b + c + d + e + f + g + h;
}

int NOINLINE mixed_args(int a, int b, int c, int d, const int *p, const int *q) {
    return a + b + c + d + *p + *q;
}

struct point { int x; int y; };
int  NOINLINE point_sum(struct point p) { return p.x + p.y; }

struct pair { int lo; int hi; };
struct pair NOINLINE make_pair(int a, int b) {
    struct pair r = { a, b };
    return r;
}

int NOINLINE tail_caller(int a, int b) {
    return eight_int_args(a, b, a, b, a, b, a, b);
}

int main(void) {
    volatile int s = 0;
    s ^= eight_int_args(1, 2, 3, 4, 5, 6, 7, 8);
    int p = 9, q = 10;
    s ^= mixed_args(1, 2, 3, 4, &p, &q);
    struct point pt = { 1, 2 };
    s ^= point_sum(pt);
    struct pair pr = make_pair(11, 22);
    s ^= pr.lo + pr.hi;
    s ^= tail_caller(3, 4);
    return s;
}
