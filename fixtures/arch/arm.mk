# -marm forces ARM (non-Thumb) encoding, which SLA_SPEC_ARM8_LE expects.
CC     := $(shell command -v arm-linux-gnueabihf-gcc 2>/dev/null || echo false)
CFLAGS := -marm -O2 -g -fno-stack-protector
