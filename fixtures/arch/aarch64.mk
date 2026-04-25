CC     := $(shell command -v aarch64-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -O2 -g -fno-stack-protector
