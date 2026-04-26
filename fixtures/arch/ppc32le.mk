# 32-bit PowerPC little-endian.  Debian's `powerpc-linux-gnu-gcc` cross
# toolchain ships only big-endian libgcc — linking LE objects against it
# fails ("compiled for a big endian system and target is little endian").
# Workaround: compile-only (`-c`), producing relocatable ELF objects that
# the analyzer's reader handles transparently.  Sleigh decodes section-
# relative symbol addresses just like absolute ones.
CC     := $(shell command -v powerpc-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS := -mlittle-endian -O2 -g -fno-stack-protector -fno-pic -no-pie \
          -ffreestanding -c
