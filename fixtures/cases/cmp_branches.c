/* cmp_branches: one conditional branch per comparison flavour, used to
 * exercise every FlagCmpCanonicalize rule on real lifted code across the
 * flag-register architectures (AArch64 / ARM / Thumb).
 *
 * Each function wraps its `if` in `memory`-clobbering asm barriers so the
 * compiler emits a real conditional branch (an `If` p-code node) instead of
 * a conditional-select / cmov, which would not exercise the flag tree.
 *
 * Condition-code mapping (AArch64 `cmp` + B.cond):
 *   br_eq  ==  -> EQ   (ZR)
 *   br_ne  !=  -> NE   (!ZR)
 *   br_slt  <  -> LT   (signed)
 *   br_sge >=  -> GE   (signed)
 *   br_sgt  >  -> GT   (signed)
 *   br_sle <=  -> LE   (signed)
 *   br_ugt  >  -> HI   (unsigned)
 *   br_ule <=  -> LS   (unsigned)
 *   br_ult  <  -> CC/LO (unsigned)
 *   br_uge >=  -> CS/HS (unsigned)
 *   br_neg  <0 -> MI   (sign bit)
 */

#define NOINLINE __attribute__((noinline))
#define BAR(v) __asm__ volatile("" ::"r"(v) : "memory")

int NOINLINE br_eq(int a, int b)  { BAR(a); if (a == b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_ne(int a, int b)  { BAR(a); if (a != b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_slt(int a, int b) { BAR(a); if (a <  b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_sge(int a, int b) { BAR(a); if (a >= b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_sgt(int a, int b) { BAR(a); if (a >  b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_sle(int a, int b) { BAR(a); if (a <= b) { BAR(a); return 1; } BAR(a); return 0; }

int NOINLINE br_ugt(unsigned a, unsigned b) { BAR(a); if (a >  b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_ule(unsigned a, unsigned b) { BAR(a); if (a <= b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_ult(unsigned a, unsigned b) { BAR(a); if (a <  b) { BAR(a); return 1; } BAR(a); return 0; }
int NOINLINE br_uge(unsigned a, unsigned b) { BAR(a); if (a >= b) { BAR(a); return 1; } BAR(a); return 0; }

int NOINLINE br_neg(int x) { BAR(x); if (x < 0) { BAR(x); return 1; } BAR(x); return 0; }

int main(void) {
    return br_eq(1, 2) + br_ne(1, 2) + br_slt(1, 2) + br_sge(1, 2) + br_sgt(1, 2) +
           br_sle(1, 2) + br_ugt(1u, 2u) + br_ule(1u, 2u) + br_ult(1u, 2u) +
           br_uge(1u, 2u) + br_neg(-1);
}
