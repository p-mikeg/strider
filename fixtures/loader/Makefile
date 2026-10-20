# fixtures/loader/Makefile
#
# Images whose GOT slots the dynamic loader fills with values the file does
# not hold, for the ELF reader's tests: out/<name>.elf, from src/.
#
# Not per-arch like ../Makefile: each image needs a link of its own (a shared
# library, a PIE, an executable against that library), and the golden suite
# analyses none of them.
#
# Usage:
#   make         build every image
#   make clean   remove out/
#
# A missing cross compiler is a hard error.

OUTDIR := out

# No libc: none of these runs, and the crt files would tie the bytes to the
# build machine's. `--build-id=none` keeps a rebuild identical.
CFLAGS := -O2 -fPIC -fno-stack-protector -nostdlib \
          -Wall -Wextra -Werror -Wno-unused-parameter \
          -Wl,--build-id=none -Wl,-z,noseparate-code

MIPS32 := mips-linux-gnu-gcc -EB -mips32
MIPS64 := mips64el-linux-gnuabi64-gcc

IMAGES := mips32be_got_lib.elf mips32be_got.elf \
          mips64le_got_lib.elf mips64le_got.elf

.PHONY: all clean
all: $(addprefix $(OUTDIR)/,$(IMAGES))

$(OUTDIR):
	mkdir -p $@

# The library first, then the executable against it.
$(OUTDIR)/mips32be_got_lib.elf: src/mips_got_lib.c | $(OUTDIR)
	$(MIPS32) $(CFLAGS) -shared -Wl,-soname,libext.so $< -o $@
$(OUTDIR)/mips32be_got.elf: src/mips_got.c $(OUTDIR)/mips32be_got_lib.elf
	$(MIPS32) $(CFLAGS) $< $(OUTDIR)/mips32be_got_lib.elf -o $@
$(OUTDIR)/mips64le_got_lib.elf: src/mips_got_lib.c | $(OUTDIR)
	$(MIPS64) $(CFLAGS) -shared -Wl,-soname,libext.so $< -o $@
$(OUTDIR)/mips64le_got.elf: src/mips_got.c $(OUTDIR)/mips64le_got_lib.elf
	$(MIPS64) $(CFLAGS) $< $(OUTDIR)/mips64le_got_lib.elf -o $@

clean:
	rm -rf $(OUTDIR)
