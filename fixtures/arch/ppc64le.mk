# 64-bit PowerPC little-endian (ELFv2).  Debian's `powerpc64le-linux-gnu-gcc`
# ships a full ELFv2 sysroot and links normally.
CC     := $(shell command -v powerpc64le-linux-gnu-gcc 2>/dev/null || echo false)
# `-O0`: keeps source-level structural shape (real loops, real branches,
# real Loads, real conversions) — see ppc32be.mk for the full rationale.
# `-mcpu=power8`: needed for the ELFv2/POWER8 ABI; Sleigh's
# `PPC_64_ISA_ALTIVEC_LE` spec decodes Power7+ scalar ops (popcntw,
# cntlzd, …) and Altivec vector ops.
# `-mno-vsx`: gcc with VSX lowers `(int)f` to `xscvdpsxws`, which the
# Sleigh VSX spec emits as a `xscvdpsxwsOp` user p-code op (lifts to
# `CallOther` in our IR — opaque, no `FloatToInt` node).  Disabling VSX
# routes `(int)f` through plain `fctiwz` which lifts as `float2int`.
# `-ffp-contract=off`: prevents fadd+fmul fusion into fmadd/fmsub.
CFLAGS := -mcpu=power8 -mno-vsx -O0 -g -fno-stack-protector -fno-pic -no-pie \
          -ffp-contract=off
