/*
 * stack.c — exercises stack-frame allocation and StackStoreDetect.
 */
#include <stddef.h>
#define NOINLINE __attribute__((noinline))

void NOINLINE volatile_three_writes(volatile int *p, int v) {
    *p = v;
    *p = v + 1;
    *p = v + 2;
}

extern void external_take_ptr(int *);
int NOINLINE escape_via_ptr(int seed) {
    int local = seed * 3;
    external_take_ptr(&local);
    return local;
}

int NOINLINE large_local_array(int n) {
    int buf[16] = { 0 };
    for (int i = 0; i < n && i < 16; ++i) buf[i] = i * i;
    int s = 0;
    for (int i = 0; i < 16; ++i) s += buf[i];
    return s;
}

void NOINLINE inplace_swap(int *a, int *b) {
    int t = *a;
    *a = *b;
    *b = t;
}

int NOINLINE recursive_stack_growth(int n) {
    int buf[8] = { n, n+1, n+2, n+3, n+4, n+5, n+6, n+7 };
    if (n <= 0) {
        int s = 0;
        for (int i = 0; i < 8; ++i) s += buf[i];
        return s;
    }
    return recursive_stack_growth(n - 1) + buf[0];
}

/* Make the body opaque so the optimiser can't inline / fold the call away —
 * without this GCC sees the empty body and elides the call site in
 * escape_via_ptr (which is the bug BUG-12 was tracking). */
void __attribute__((noinline)) external_take_ptr(int *p) {
    __asm__ volatile ("" :: "r"(p) : "memory");
}

int main(void) {
    volatile int sink = 0;
    volatile_three_writes((volatile int*)&sink, 1);
    int x = 1, y = 2;
    inplace_swap(&x, &y);
    sink ^= x;
    sink ^= escape_via_ptr(7);
    sink ^= large_local_array(8);
    sink ^= recursive_stack_growth(3);
    return sink;
}
