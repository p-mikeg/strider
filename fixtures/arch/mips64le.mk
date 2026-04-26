# 64-bit MIPS Linux, little-endian, N64 ABI.
CC     := $(shell command -v mips64el-linux-gnuabi64-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector -fno-pic -no-pie
