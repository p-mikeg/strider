/*
 * memory.c — Load / Store at the default code space.
 */
#include <stddef.h>
#define NOINLINE __attribute__((noinline))

int NOINLINE array_sum(const int *arr, size_t len) {
    int s = 0;
    for (size_t i = 0; i < len; ++i) s += arr[i];
    return s;
}
void NOINLINE array_fill(int *arr, size_t len, int val) {
    for (size_t i = 0; i < len; ++i) arr[i] = val;
}
void NOINLINE array_copy(int *dst, const int *src, size_t len) {
    for (size_t i = 0; i < len; ++i) dst[i] = src[i];
}

int NOINLINE pointer_chase(int *const *p) {
    return **p;
}

struct point { int x; int y; };
int NOINLINE struct_field_load(const struct point *p)        { return p->x + p->y; }
void NOINLINE struct_field_store(struct point *p, int x, int y) { p->x = x; p->y = y; }

union tag { int as_int; unsigned char bytes[4]; };
int NOINLINE tagged_union_read(const union tag *t) { return t->as_int + (int)t->bytes[0]; }

int main(void) {
    int buf[4] = { 1, 2, 3, 4 };
    int dst[4];
    array_copy(dst, buf, 4);
    array_fill(dst, 4, 9);
    int s = array_sum(dst, 4);

    int x = 7;
    int *xp = &x;
    s ^= pointer_chase(&xp);

    struct point p = { 1, 2 };
    s ^= struct_field_load(&p);
    struct_field_store(&p, 3, 4);

    union tag t;
    t.as_int = 0x12345678;
    s ^= tagged_union_read(&t);
    return s;
}
