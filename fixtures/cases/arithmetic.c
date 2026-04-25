/*
 * arithmetic.c — every IntBinaryOp / IntUnaryOp variant the analyzer must lower.
 *
 * Functions are annotated noinline so each becomes a distinct ELF symbol.
 * All operands are signed/unsigned 32-bit ints (the analyzer's most common
 * width); larger widths are exercised in abi.c.
 */
#define NOINLINE __attribute__((noinline))

int  NOINLINE add        (int a, int b)      { return a + b; }
int  NOINLINE sub        (int a, int b)      { return a - b; }
int  NOINLINE mul        (int a, int b)      { return a * b; }

unsigned NOINLINE udiv   (unsigned a, unsigned b) { return a / b; }
unsigned NOINLINE umod   (unsigned a, unsigned b) { return a % b; }
int      NOINLINE sdiv   (int a, int b)           { return a / b; }
int      NOINLINE smod   (int a, int b)           { return a % b; }

int  NOINLINE bit_and    (int a, int b)      { return a & b; }
int  NOINLINE bit_or     (int a, int b)      { return a | b; }
int  NOINLINE bit_xor    (int a, int b)      { return a ^ b; }
int  NOINLINE bit_not    (int a)             { return ~a; }

unsigned NOINLINE shl    (unsigned a, unsigned b) { return a << b; }
unsigned NOINLINE lshr   (unsigned a, unsigned b) { return a >> b; }
int      NOINLINE ashr   (int a, int b)           { return a >> b; }

int  NOINLINE negate     (int a)             { return -a; }

int main(void) {
    volatile int sink = 0;
    sink ^= add(1, 2);     sink ^= sub(3, 4);    sink ^= mul(5, 6);
    sink ^= (int)udiv(7, 1); sink ^= (int)umod(7, 3);
    sink ^= sdiv(7, 2);    sink ^= smod(7, 2);
    sink ^= bit_and(1,2);  sink ^= bit_or(1,2);  sink ^= bit_xor(1,2);  sink ^= bit_not(1);
    sink ^= (int)shl(1,2); sink ^= (int)lshr(8,1); sink ^= ashr(-8, 1);
    sink ^= negate(1);
    return sink;
}
