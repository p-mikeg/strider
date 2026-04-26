# 64-bit PowerPC big-endian (ELFv1 ABI with function descriptors).
CC     := $(shell command -v powerpc64-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie
