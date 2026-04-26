# 32-bit PowerPC big-endian (System V ABI).
CC     := $(shell command -v powerpc-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie
