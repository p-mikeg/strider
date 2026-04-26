# Big-endian AArch64.  No dedicated `aarch64_be-linux-gnu-gcc` package on
# Debian; the standard `aarch64-linux-gnu-gcc` accepts `-mbig-endian` and
# emits MSB ELF.  No matching libc in the toolchain — `-nostartfiles` skips
# _start but keeps libgcc available for compiler-emitted helpers.
CC     := $(shell command -v aarch64-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -mbig-endian -O2 -g -fno-stack-protector -fno-pic -no-pie \
          -static -nostartfiles -ffreestanding
