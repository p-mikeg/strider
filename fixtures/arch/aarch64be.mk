# Big-endian AArch64.  Use clang+lld so we get a real BE executable
# (gcc has no `aarch64_be-linux-gnu` cross package on Debian; clang's
# `aarch64_be-linux-gnu` target produces MSB ELF directly).
CC     := $(shell command -v clang 2>/dev/null || echo false)
# `-O0`: preserves loop structure the analyzer tests assert on (clang at
# -O2 closed-form-rewrites sum_to_n into n*(n+1)/2, erasing the loop).
CFLAGS := --target=aarch64_be-linux-gnu -fuse-ld=lld -O0 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding
