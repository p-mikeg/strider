CC     := $(shell command -v gcc 2>/dev/null || echo false)
CFLAGS := -m64 -O2 -g -fno-stack-protector -fno-pic -no-pie