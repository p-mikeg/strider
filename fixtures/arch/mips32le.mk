CC     := $(shell command -v mipsel-linux-gnu-gcc 2>/dev/null || echo false)
# -EL forces little-endian; -mips32 selects the ISA level Sleigh's mipsle32 spec expects.
CFLAGS := -EL -mips32 -O2 -g -fno-stack-protector -fno-pic
