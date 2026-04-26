# 64-bit PowerPC big-endian, ELFv2 ABI.  Clang's `--target=powerpc64-linux-gnu`
# defaults to ELFv1 (with `.opd` function descriptors that the cfg builder
# doesn't yet dereference); `-mabi=elfv2` forces ELFv2 — symbols point
# directly into `.text`, no descriptor indirection.
CC     := clang
# `-ffp-contract=off`: prevent FMA fusion (clang fuses fadd+fmul → fmadd
# at -O2, collapsing 4 float ops into 3 IR nodes).
CFLAGS := --target=powerpc64-linux-gnu -mabi=elfv2 -fuse-ld=lld -O2 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding \
          -ffp-contract=off
