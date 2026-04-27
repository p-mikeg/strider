/*
 * complex.c — fixtures targeted at compound pattern queries:
 * struct field offsets, bit-test branches, call args under
 * a control-flow path, plus a scale smoke-test function.
 *
 * The "extern" callees are defined locally with an asm-volatile
 * memory barrier so the analyser sees real Call sites (no DCE),
 * and so freestanding builds (PPC, aarch64be, arm_thumb) link
 * cleanly without libc.
 */
#define NOINLINE __attribute__((noinline))

struct S      { int a, b, c; int flags; int *handler; };
struct Inner  { int x, y; };
struct Outer  { int padding; struct Inner inner; };

/* opaque-body helpers — see stack.c::external_take_ptr for the same
 * idiom.  The asm-volatile clobber prevents the optimiser from
 * inlining the bodies away. */
void NOINLINE cb_zero(int *p) {
    __asm__ volatile ("" :: "r"(p) : "memory");
}
void NOINLINE cb_set(int *p) {
    __asm__ volatile ("" :: "r"(p) : "memory");
}
void NOINLINE invoke(int *h) {
    __asm__ volatile ("" :: "r"(h) : "memory");
}
int NOINLINE ext_three(int a, int b, int c) {
    __asm__ volatile ("" :: "r"(a), "r"(b), "r"(c) : "memory");
    return a;
}
int NOINLINE produce(int x) {
    __asm__ volatile ("" : "+r"(x));
    return x + 1;
}
int NOINLINE consume(int x) {
    __asm__ volatile ("" : "+r"(x) : : "memory");
    return x;
}

/* 1. Field-load offsets: distinct Load(base + K) for K in {0, 4, 8} */
int NOINLINE read_struct_fields(struct S *s) { return s->a + s->b + s->c; }

/* 2. Field-store offsets: distinct Store(base + K, v) */
void NOINLINE write_struct_fields(struct S *s, int v) {
    s->a = v; s->b = v; s->c = v;
}

/* 3. Nested-struct field: combined offset.  May fold to one IntConst
 * (= padding + offsetof(Inner, x)) or stay split — assertion below
 * matches either shape. */
int NOINLINE nested_struct_field(struct Outer *o) { return o->inner.x; }

/* 4. Bit-test against a single-bit constant — value of the bit is NOT
 * something the test should hardcode; use .when() to capture it.
 *
 * Two asm-volatile barriers (one on `mask`, one on the masked
 * product `t`) plus side-effect calls on each leg keep the literal
 * `And` + `IntCmp(Equal, _, 0)` shape in the IR on every arch we
 * build for.  Without the second barrier gcc collapses `(m & K) ==
 * 0` into a bit-extraction sequence that erases both nodes. */
int NOINLINE bit_test_zero(unsigned mask) {
    int p = 0;
    unsigned m = mask;
    __asm__ volatile ("" : "+r"(m));
    unsigned t = m & 0x4u;
    __asm__ volatile ("" : "+r"(t));
    if (t == 0) { cb_zero(&p); return 17; }
    else        { cb_set(&p);  return 42; }
}

/* 5. Call inside the True branch of a bit-test if. */
void NOINLINE if_bit_clear_call(unsigned mask, int *p) {
    unsigned m = mask;
    __asm__ volatile ("" : "+r"(m));
    unsigned t = m & 1u;
    __asm__ volatile ("" : "+r"(t));
    if (t == 0) cb_zero(p);
}

/* 6. Call's argument is a struct field load. */
void NOINLINE call_with_field_arg(struct S *s) { invoke(s->handler); }

/* 7. Composition: bit-test → if → call → field-load.
 *
 * Both `f` and the masked product `t` go through asm-volatile
 * barriers.  Without the second barrier on `t`, gcc on Thumb-2
 * folds `(f & 4) == 0` into a single `lsls` (shift bit 2 into the
 * sign bit) + branch-on-PL, erasing the `And` and `IntCmp(Equal)`
 * IR nodes the test wants to match. */
void NOINLINE dispatch_on_flag(struct S *s) {
    unsigned f = (unsigned)s->flags;
    __asm__ volatile ("" : "+r"(f));
    unsigned t = f & 4u;
    __asm__ volatile ("" : "+r"(t));
    if (t == 0) invoke(s->handler);
}

/* 8. Two distinct Call sites distinguishable by arg ordering.
 *
 * GCC on ARM (32-bit) at -O2 will rewrite both legs into a single bl
 * preceded by `moveq` / `movne` register shuffling — collapsing the
 * two source-level calls into a single IR `Call` site.  Adding a
 * post-call asm-volatile barrier on the return value of each leg
 * stops the compiler from sharing the call instruction (the barriers'
 * outputs are different in the two legs, so the calls cannot be
 * merged). */
int NOINLINE multi_arg_call_in_branch(int cond, int a, int b, int c) {
    int k = cond;
    __asm__ volatile ("" : "+r"(k));
    if (k) {
        int r = ext_three(a, b, c);
        __asm__ volatile ("" : "+r"(r));
        return r + 1;
    } else {
        int r = ext_three(c, b, a);
        __asm__ volatile ("" : "+r"(r));
        return r + 2;
    }
}

/* 9. Scale smoke test.  Many local variables of varying types, several
 * stack-allocated structs/arrays passed by reference to calls, ≥10
 * branches, 3 loops, ≥7 calls, struct field reads/writes throughout,
 * mixed-width compute (int / short / char), and a final write-back via
 * `*out`.  The point is to exercise the full pipeline (cfg → IR →
 * StackStoreDetect → StackLoadForward → ConstantFold / KnownBits) on
 * a single function and confirm we don't time out / OOM. */
struct Big { int items[8]; int header; struct Inner inner; };

int NOINLINE complex_dispatch(struct S *s, unsigned flags, int n, int *out) {
    int            acc = 0;
    int            locals[8] = { 0, 1, 2, 3, 4, 5, 6, 7 };
    char           tag = 0;
    short          scratch = 0;
    struct Big     big;
    struct Outer   local_outer = { 0xfeed, { 100, 200 } };
    int            *handler_local = &locals[0];

    if ((flags & 1) == 0) {
        cb_zero(s->handler);
        acc += s->a;
        tag = 1;
    }
    if ((flags & 2) != 0) {
        cb_set(s->handler);
        acc += s->b;
        scratch = (short)acc;
    }
    if ((flags & 4) == 0) {
        invoke(s->handler);
        acc -= s->c;
    } else {
        acc += s->flags;
    }

    /* Loop 1 — stride-1 field accumulation, also writing into a local
     * stack array. */
    for (int i = 0; i < n; ++i) {
        acc += s->a + i;
        locals[i & 7] = acc;
    }
    if (acc < 0) acc = -acc;
    if ((flags & 8) != 0) {
        acc ^= s->b;
    }

    /* Loop 2 — inner branch, calls inside the loop pass pointers to
     * stack locals. */
    for (int i = 0; i < n; ++i) {
        if ((flags & 16) == 0) {
            acc += s->c;
            cb_zero(&locals[i & 7]);
        } else {
            acc -= s->c;
            invoke(handler_local);
        }
    }

    /* Stack-allocated `big` populated then passed (by inner-field
     * pointer) to opaque calls.  Stresses StackStoreDetect across
     * a sizeof(Big) ≥ 44-byte frame slot. */
    big.header = acc;
    big.inner.x = s->a;
    big.inner.y = s->b;
    for (int i = 0; i < 8; ++i) big.items[i] = locals[i];
    if ((flags & 32) != 0) {
        invoke(&big.inner.x);
        scratch = (short)ext_three(big.header, big.inner.x, big.inner.y);
    }
    if ((flags & 64) != 0) {
        cb_set(&big.items[3]);
    }

    /* Loop 3 — short stride that uses tag/scratch arithmetic. */
    for (unsigned i = 0; i < (unsigned)(n & 7); ++i) {
        acc ^= (int)(scratch + (short)i);
        if ((flags & ((unsigned)1 << (i & 3))) != 0) {
            tag = (char)(tag + 1);
        }
    }

    if (acc > 100) {
        invoke(s->handler);
        ext_three(acc, (int)scratch, (int)tag);
    }

    /* Mix in the local Outer struct. */
    if (local_outer.padding != 0) {
        acc ^= local_outer.inner.x;
        acc ^= local_outer.inner.y;
    }

    *out = acc + (int)tag + (int)scratch;
    return acc;
}

/* 10. Call's argument is the return value of another Call.
 *     `consume(produce(x))` — exercises Call→PostCallVarState→Call
 *     dataflow that pattern queries about call chains hit in real code. */
int NOINLINE call_uses_call_return(int x) {
    int p = produce(x);
    /* Barrier: stop the optimiser from inlining the call-uses-call edge
     * into a single fused operation on some arches. */
    __asm__ volatile ("" : "+r"(p));
    return consume(p);
}

int main(void) {
    volatile int sink = 0;
    struct S s = { 1, 2, 3, 0xff, (int*)0 };
    struct Outer o = { 7, { 11, 22 } };
    int p = 0;
    int out = 0;
    sink ^= read_struct_fields(&s);
    write_struct_fields(&s, 5);
    sink ^= nested_struct_field(&o);
    sink ^= bit_test_zero((unsigned)sink);
    if_bit_clear_call((unsigned)sink, &p);
    call_with_field_arg(&s);
    dispatch_on_flag(&s);
    sink ^= multi_arg_call_in_branch(sink, 1, 2, 3);
    sink ^= complex_dispatch(&s, (unsigned)sink, 4, &out);
    sink ^= call_uses_call_return(sink);
    return sink ^ out;
}
