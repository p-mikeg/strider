# 64-bit MIPS Linux, big-endian, N64 ABI.
CC     := $(shell command -v mips64-linux-gnuabi64-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie
