CC     := $(shell command -v mips-linux-gnu-gcc 2>/dev/null || echo false)
# -EB forces big-endian; -mips32 selects the ISA level Sleigh's mipsbe32 spec expects.
CFLAGS := -EB -mips32 -O2 -g -fno-stack-protector -fno-pic
