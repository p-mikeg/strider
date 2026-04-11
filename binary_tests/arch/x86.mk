CC     := $(shell command -v i686-linux-gnu-gcc-11 2>/dev/null \
               || command -v i686-linux-gnu-gcc 2>/dev/null \
               || echo gcc)
CFLAGS := -m32 -O2 -g
