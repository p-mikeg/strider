# 32-bit PowerPC big-endian.  Debian's `powerpc-linux-gnu-gcc` ships a
# full BE sysroot and links normally.
#
# `-O1` (instead of `-O2`): `-O2` aggressively reshapes loops via
# `isel`-flattening, closed-form rewrites (`sum_to_n → n*(n+1)/2`),
# branchless `abs`, etc.  Those are valid optimizations but they erase
# the structural shapes (ControlPhi loop headers, If nodes) the analyzer
# tests assert on.  `-O1` keeps the analyzer paths exercised (constant
# folding, register allocation, basic CSE) without the structural
# rewrites — so the same assertions match across all 14 arches and
# differences signal real analyzer bugs rather than compiler drift.
#
# `-ffp-contract=off`: prevents fadd+fmul fusion into fmadd/fmsub —
# keeps each op as a separate FloatBinaryOp node.
CC     := $(shell command -v powerpc-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O0 -g -fno-stack-protector -fno-pic -no-pie -ffp-contract=off
