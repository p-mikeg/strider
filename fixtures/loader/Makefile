# fixtures/loader/Makefile
#
# Images whose GOT slots the dynamic loader fills with values the file does
# not hold, and objects whose GOT references strider resolves, for the ELF
# reader's tests: out/<name>.{elf,o}, from src/.
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
# build machine's. No sibling-call flag either: the IFUNC calls must stay
# tail jumps through the slot. `--build-id=none` keeps a rebuild identical.
CFLAGS := -O2 -fPIC -fno-stack-protector -nostdlib \
          -Wall -Wextra -Werror -Wno-unused-parameter \
          -Wl,--build-id=none -Wl,-z,noseparate-code

X64    := gcc
X86    := i686-linux-gnu-gcc
MIPS32 := mips-linux-gnu-gcc -EB -mips32
MIPS64 := mips64el-linux-gnuabi64-gcc

IMAGES := mips32be_got_lib.elf mips32be_got.elf \
          mips64le_got_lib.elf mips64le_got.elf \
          x64_ifunc_pie.elf x64_ifunc_lib.elf x86_ifunc_pie.elf x64_ifunc.o \
          x64_ifunc_nopic.o x64_ifunc_static.elf \
          x86_got_call_nopic.o x86_got_call_pic.o

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

# `-z now` with -fno-plt puts f's slot in `.got`, not `.got.plt`.
$(OUTDIR)/x64_ifunc_pie.elf: src/ifunc.c | $(OUTDIR)
	$(X64) $(CFLAGS) -fno-plt -pie -Wl,-z,now -Wl,-e,call_f $< -o $@
$(OUTDIR)/x64_ifunc_lib.elf: src/ifunc.c | $(OUTDIR)
	$(X64) $(CFLAGS) -fno-plt -shared -Wl,-z,now $< -o $@
$(OUTDIR)/x86_ifunc_pie.elf: src/ifunc.c | $(OUTDIR)
	$(X86) $(CFLAGS) -fno-plt -pie -Wl,-z,now -Wl,-e,call_f $< -o $@
$(OUTDIR)/x64_ifunc.o: src/ifunc.c | $(OUTDIR)
	$(X64) $(filter-out -Wl% -nostdlib,$(CFLAGS)) -fno-plt -c $< -o $@
# Static and unstripped: `.rela.plt` holds the IRELATIVE slots and links to
# `.symtab`, there being no `.dynsym`.
$(OUTDIR)/x64_ifunc_static.elf: src/ifunc.c | $(OUTDIR)
	$(X64) $(CFLAGS) -fno-plt -static -no-pie -Wl,-z,now -Wl,-e,call_f $< -o $@
# Non-PIC, the call and the address-of carry their relocations in .text,
# as a kernel's do.
$(OUTDIR)/x64_ifunc_nopic.o: src/ifunc.c | $(OUTDIR)
	$(X64) $(filter-out -Wl% -nostdlib -fPIC,$(CFLAGS)) -fno-pic -c $< -o $@

# The same calls through a GOT register and through the slot's own address.
$(OUTDIR)/x86_got_call_pic.o: src/got_call.c | $(OUTDIR)
	$(X86) $(filter-out -Wl% -nostdlib,$(CFLAGS)) -fno-plt -c $< -o $@
$(OUTDIR)/x86_got_call_nopic.o: src/got_call.c | $(OUTDIR)
	$(X86) $(filter-out -Wl% -nostdlib -fPIC,$(CFLAGS)) -fno-pic -fno-plt -c $< -o $@

clean:
	rm -rf $(OUTDIR)
