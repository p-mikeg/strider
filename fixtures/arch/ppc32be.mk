# 32-bit PowerPC big-endian.  Debian's `powerpc-linux-gnu-gcc` ships a
# full BE sysroot and links normally — gcc's -O2 produces less-vectorized
# code than clang and matches the structural assertions our existing tests
# were tuned for.
CC     := $(shell command -v powerpc-linux-gnu-gcc 2>/dev/null || echo false)
# `-ffp-contract=off`: PPC's `-O2` fuses fadd+fmul into `fmadds`/`fmsubs`
# (FMA), which collapses what's conceptually 4 float ops into 3 IR
# `FloatBinaryOp` nodes.  Disable contraction so each op remains a
# separate node — matches the structural assertion `≥4 FloatBinaryOp`.
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie -ffp-contract=off
