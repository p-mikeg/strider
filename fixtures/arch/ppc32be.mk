# 32-bit PowerPC big-endian.  Debian's `powerpc-linux-gnu-gcc` ships a
# full BE sysroot and links normally.
#
# `-O0`: at -O2 gcc reshapes loops on this target via `isel`-flattening,
# closed-form rewrites (`sum_to_n -> n*(n+1)/2`) and branchless `abs`,
# erasing the structural shapes (loop-header Regions, If nodes) the
# analyzer tests assert on.  Arches whose toolchain leaves those shapes
# alone stay at -O2.
#
# `-ffp-contract=off`: prevents fadd+fmul fusion into fmadd/fmsub,
# keeping each op a separate FloatBinaryOp node.
CC     := $(shell command -v powerpc-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O0 -g -fno-stack-protector -fno-pic -no-pie -ffp-contract=off
