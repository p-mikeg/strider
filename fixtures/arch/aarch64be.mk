# Big-endian AArch64.  Use clang+lld so we get a real BE executable
# (gcc has no `aarch64_be-linux-gnu` cross package on Debian; clang's
# `aarch64_be-linux-gnu` target produces MSB ELF directly).
CC     := clang
CFLAGS := --target=aarch64_be-linux-gnu -fuse-ld=lld -O2 -g \
          -fno-stack-protector -fno-pic -static -nostdlib -ffreestanding
