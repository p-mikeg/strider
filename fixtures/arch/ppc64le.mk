# 64-bit PowerPC little-endian (ELFv2).  Debian's `powerpc64le-linux-gnu-gcc`
# ships a full ELFv2 sysroot and links normally.  gcc's -O2 doesn't
# auto-vectorize as aggressively as clang, which matches the structural
# assertions our existing tests were tuned for.
CC     := $(shell command -v powerpc64le-linux-gnu-gcc 2>/dev/null || echo false)
# `-mcpu=power8`: Sleigh's PPC_64_LE spec doesn't decode some Power9 ISA
# 3.0 instructions (e.g. `xxspltib`), and gcc's default `-mcpu=power9`
# emits them.  Targeting Power8 keeps the codegen within Sleigh's
# decode capability.
# `-ffp-contract=off`: PPC's `-O2` fuses fadd+fmul into `fmadd`/`fmsub`
# (FMA), collapsing 4 conceptual float ops into 3 IR nodes — disable
# so each op stays a separate `FloatBinaryOp`.
CFLAGS := -mcpu=power8 -O2 -g -fno-stack-protector -fno-pic -no-pie \
          -ffp-contract=off
