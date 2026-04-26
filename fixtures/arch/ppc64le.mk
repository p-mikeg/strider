# 64-bit PowerPC little-endian (ELFv2 ABI — no function descriptors).
CC     := $(shell command -v powerpc64le-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie
