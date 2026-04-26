# 32-bit PowerPC little-endian.  Clang+lld supports `powerpcle-linux-gnu`
# directly and inlines libgcc helpers (`__popcountsi2` etc.) as bit-
# twiddling code, so the BE-only-libgcc problem that the gcc cross
# toolchain has on this target doesn't apply here.
CC     := clang
# `--unresolved-symbols=ignore-all`: 32-bit clang emits `memset` calls for
# the large stack-array fixture; without a libc the link needs stub
# relocations.
# `-ffp-contract=off`: prevent FMA fusion at -O2 so float-arithmetic
# tests see all 4 separate FloatBinaryOps.
CFLAGS := --target=powerpcle-linux-gnu -fuse-ld=lld -O2 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding \
          -ffp-contract=off \
          -Wl,--unresolved-symbols=ignore-all
