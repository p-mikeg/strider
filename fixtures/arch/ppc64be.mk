# 64-bit PowerPC big-endian, ELFv2 ABI.  Clang's `--target=powerpc64-linux-gnu`
# defaults to ELFv1 (with `.opd` function descriptors that the cfg builder
# doesn't yet dereference); `-mabi=elfv2` forces ELFv2 — symbols point
# directly into `.text`, no descriptor indirection.
CC     := clang
# `-O1` (instead of `-O2`): see ppc32be.mk — preserves structural shape.
# `-ffp-contract=off`: prevent FMA fusion (fadd+fmul → fmadd).
CFLAGS := --target=powerpc64-linux-gnu -mabi=elfv2 -fuse-ld=lld -O0 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding \
          -ffp-contract=off
