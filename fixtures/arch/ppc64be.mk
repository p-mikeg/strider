# 64-bit PowerPC big-endian.  Clang's `--target=powerpc64-linux-gnu`
# defaults to ELFv1, where symbols name an `.opd` function descriptor
# rather than `.text`; `-mabi=elfv2` forces ELFv2 so the fixtures need no
# descriptor indirection.  (The reader does dereference `.opd` via
# `OpdTable`; these fixtures simply are not the coverage for it.)
CC     := $(shell command -v clang 2>/dev/null || echo false)
# `-O0`: see ppc32be.mk, preserves structural shape.
# `-ffp-contract=off`: prevents FMA fusion (fadd+fmul -> fmadd).
CFLAGS := --target=powerpc64-linux-gnu -mabi=elfv2 -fuse-ld=lld -O0 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding \
          -ffp-contract=off
