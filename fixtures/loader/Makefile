# fixtures/loader/Makefile
#
# Images whose GOT slots the dynamic loader fills with values the file does
# not hold, and objects whose GOT references strider resolves, for the ELF
# reader's tests, plus a ppc64 ELFv2 image whose calls enter at local
# entries, GCC hot/cold splits (`foo` and `foo.cold`), a microMIPS image the
# loader refuses, a MIPS16 function in a MIPS32 image, an object whose
# R_X86_64_32 values must zero-extend, a MIPS library and an x86-64 PIE with
# a relro pointer, a ppc64 ELFv1 image and a ppc32 library whose table bases
# sit in `.got2`, and executables with an R_*_COPY object:
# out/<name>.{elf,o}, from src/.
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
MICROMIPS := mips-linux-gnu-gcc -EB -mips32r2 -mmicromips
MIPS64 := mips64el-linux-gnuabi64-gcc
A64    := aarch64-linux-gnu-gcc
ARM    := arm-linux-gnueabihf-gcc
PPC32   := powerpc-linux-gnu-gcc
PPC64   := powerpc64-linux-gnu-gcc
PPC64LE := powerpc64le-linux-gnu-gcc
PPC64LE_OBJCOPY := powerpc64le-linux-gnu-objcopy
LLD_A64 := clang --target=aarch64-linux-gnu -fuse-ld=lld
LLVM_OBJCOPY := llvm-objcopy-14

IMAGES := mips32be_got_lib.elf mips32be_got.elf \
          mips64le_got_lib.elf mips64le_got.elf \
          x64_ifunc_pie.elf x64_ifunc_lib.elf x86_ifunc_pie.elf x64_ifunc.o \
          x64_ifunc_nopic.o x64_ifunc_static.elf ppc64be_ifunc.o ppc64le_ifunc.o \
          x64_ifunc_extern.o \
          x86_got_call_nopic.o x86_got_call_pic.o aarch64_got_addend.o \
          ppc64le_local_entry.elf ppc64le_local_entry_stripped.elf \
          x64_nodynsym.elf mips32be_micromips.elf mips32be_mips16.elf x64_abs32.o \
          mips32be_relro.elf x64_relro_pie.elf ppc64be_elfv1.elf \
          ppc32be_got2.elf x64_copy_lib.elf x64_copy_pie.elf \
          aarch64_copy_lib.elf aarch64_copy.elf \
          aarch64_android_rela.elf aarch64_plain_rela.elf \
          x86_textrel.elf x86_textrel_noshdr.elf \
          x64_tls.elf x64_tlsdesc.elf aarch64_tls.elf aarch64_tls_trad.elf \
          ppc64le_tls.elf mips64le_tls.elf x86_tlsdesc.elf arm_tlsdesc.elf \
          arm_lazy.elf ppc32be_lazy.elf x64_cold.elf aarch64_cold.elf

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
# `bl f; nop`: an ELFv1 call names f's `.opd` symbol, an ELFv2 one its
# global entry.
$(OUTDIR)/ppc64be_ifunc.o: src/ifunc.c | $(OUTDIR)
	$(PPC64) $(filter-out -Wl% -nostdlib,$(CFLAGS)) -mabi=elfv1 -c $< -o $@
$(OUTDIR)/ppc64le_ifunc.o: src/ifunc.c | $(OUTDIR)
	$(PPC64LE) $(filter-out -Wl% -nostdlib,$(CFLAGS)) -c $< -o $@
$(OUTDIR)/x64_ifunc_extern.o: src/ifunc_extern.c | $(OUTDIR)
	$(X64) $(filter-out -Wl% -nostdlib -fPIC,$(CFLAGS)) -fno-pic -c $< -o $@

# The same calls through a GOT register and through the slot's own address.
$(OUTDIR)/x86_got_call_pic.o: src/got_call.c | $(OUTDIR)
	$(X86) $(filter-out -Wl% -nostdlib,$(CFLAGS)) -fno-plt -c $< -o $@
$(OUTDIR)/x86_got_call_nopic.o: src/got_call.c | $(OUTDIR)
	$(X86) $(filter-out -Wl% -nostdlib -fPIC,$(CFLAGS)) -fno-pic -fno-plt -c $< -o $@
$(OUTDIR)/aarch64_got_addend.o: src/aarch64_got_addend.s | $(OUTDIR)
	$(A64) -c $< -o $@

$(OUTDIR)/ppc64le_local_entry.elf: src/ppc64le_local_entry.s | $(OUTDIR)
	$(PPC64LE) $(CFLAGS) -no-pie -Wl,-e,noret_call $< -o $@
# The same image without its symbols, which `add_symbol_file` takes back.
$(OUTDIR)/ppc64le_local_entry_stripped.elf: $(OUTDIR)/ppc64le_local_entry.elf
	$(PPC64LE_OBJCOPY) --strip-all $< $@

# `.dynsym` discarded by the script; GNU ld then links `.rela.dyn` to
# `.symtab`; the test rewrites the link to 0, as the arm64 kernel has it.
$(OUTDIR)/x64_nodynsym.elf: src/nodynsym.s src/nodynsym.lds | $(OUTDIR)
	$(X64) -nostdlib -shared -Wl,-Bsymbolic,-z,notext,--build-id=none \
	  -Wl,-T,src/nodynsym.lds $< -o $@

$(OUTDIR)/mips32be_micromips.elf: src/micromips.c | $(OUTDIR)
	$(MICROMIPS) $(filter-out -fPIC,$(CFLAGS)) -fno-pic -mno-abicalls -no-pie \
	  -Wl,-e,min $< -o $@
$(OUTDIR)/mips32be_mips16.elf: src/mips16.c | $(OUTDIR)
	$(MIPS32) $(filter-out -fPIC,$(CFLAGS)) -fno-pic -mno-abicalls -no-pie \
	  -Wl,-e,min16 $< -o $@

$(OUTDIR)/x64_abs32.o: src/x64_abs32.s | $(OUTDIR)
	$(X64) -c $< -o $@

$(OUTDIR)/mips32be_relro.elf: src/relro.c | $(OUTDIR)
	$(MIPS32) $(CFLAGS) -shared -Wl,-z,relro,-z,now $< -o $@
$(OUTDIR)/x64_relro_pie.elf: src/relro.c | $(OUTDIR)
	$(X64) $(CFLAGS) -pie -Wl,-z,relro,-z,now -Wl,-e,get $< -o $@

# ELFv1, GCC's descriptor symbols beside an asm dot-symbol pair.
$(OUTDIR)/ppc64be_elfv1.elf: src/ppc64_elfv1.c src/ppc64_elfv1_dot.s | $(OUTDIR)
	$(PPC64) $(CFLAGS) -mabi=elfv1 -no-pie -Wl,-e,sw $^ -o $@

$(OUTDIR)/ppc32be_got2.elf: src/ppc32_got2.c | $(OUTDIR)
	$(PPC32) $(CFLAGS) -shared $< -o $@

# A full-RELRO PIE (and a non-PIC AArch64 executable) reading a library's
# const array directly: GNU ld copies it into `.data.rel.ro` under R_*_COPY.
$(OUTDIR)/x64_copy_lib.elf: src/copy_lib.c | $(OUTDIR)
	$(X64) $(CFLAGS) -shared -Wl,-soname,libcopy.so $< -o $@
$(OUTDIR)/x64_copy_pie.elf: src/copy.c $(OUTDIR)/x64_copy_lib.elf
	$(X64) $(filter-out -fPIC,$(CFLAGS)) -fPIE -pie -Wl,-z,relro,-z,now \
	  -Wl,-e,get3 $< $(OUTDIR)/x64_copy_lib.elf -o $@
$(OUTDIR)/aarch64_copy_lib.elf: src/copy_lib.c | $(OUTDIR)
	$(A64) $(CFLAGS) -shared -Wl,-soname,libcopy.so $< -o $@
$(OUTDIR)/aarch64_copy.elf: src/copy.c $(OUTDIR)/aarch64_copy_lib.elf
	$(A64) $(filter-out -fPIC,$(CFLAGS)) -fno-pic -no-pie -Wl,-z,relro,-z,now \
	  -Wl,-e,get3 $< $(OUTDIR)/aarch64_copy_lib.elf -o $@

# The same library with its dynamic relocations packed the Android way and
# not.
ANDROID_SRC := src/android_ptrs.c src/android_call.c
$(OUTDIR)/aarch64_android_rela.elf: $(ANDROID_SRC) | $(OUTDIR)
	$(LLD_A64) $(CFLAGS) -shared -Wl,-z,relro,-z,now \
	  -Wl,--pack-dyn-relocs=android $(ANDROID_SRC) -o $@
$(OUTDIR)/aarch64_plain_rela.elf: $(ANDROID_SRC) | $(OUTDIR)
	$(LLD_A64) $(CFLAGS) -shared -Wl,-z,relro,-z,now $(ANDROID_SRC) -o $@

# A TEXTREL library, and the same image with every section header dropped:
# `ld.so` reads its relocations off PT_DYNAMIC alone.
$(OUTDIR)/x86_textrel.elf: src/textrel.c | $(OUTDIR)
	$(X86) $(filter-out -fPIC,$(CFLAGS)) -fno-pic -shared -Wl,-z,notext $< -o $@
$(OUTDIR)/x86_textrel_noshdr.elf: $(OUTDIR)/x86_textrel.elf
	$(LLVM_OBJCOPY) --strip-sections $< $@

# TLS slots of every width: 64-bit TP offsets, module ids and offsets, and
# TLS descriptors (two words), AArch64's by default.
TLS_LIB := -shared -Wl,-z,relro,-z,now
$(OUTDIR)/x64_tls.elf: src/tls.c | $(OUTDIR)
	$(X64) $(CFLAGS) $(TLS_LIB) $< -o $@
$(OUTDIR)/x64_tlsdesc.elf: src/tls.c | $(OUTDIR)
	$(X64) $(CFLAGS) -mtls-dialect=gnu2 $(TLS_LIB) $< -o $@
$(OUTDIR)/aarch64_tls.elf: src/tls.c | $(OUTDIR)
	$(A64) $(CFLAGS) $(TLS_LIB) $< -o $@
$(OUTDIR)/aarch64_tls_trad.elf: src/tls.c | $(OUTDIR)
	$(A64) $(CFLAGS) -mtls-dialect=trad $(TLS_LIB) $< -o $@
$(OUTDIR)/ppc64le_tls.elf: src/tls.c | $(OUTDIR)
	$(PPC64LE) $(CFLAGS) $(TLS_LIB) $< -o $@
$(OUTDIR)/mips64le_tls.elf: src/tls.c | $(OUTDIR)
	$(MIPS64) $(CFLAGS) $(TLS_LIB) $< -o $@
$(OUTDIR)/x86_tlsdesc.elf: src/tls.c | $(OUTDIR)
	$(X86) $(CFLAGS) -mtls-dialect=gnu2 $(TLS_LIB) $< -o $@
$(OUTDIR)/arm_tlsdesc.elf: src/tls.c | $(OUTDIR)
	$(ARM) $(CFLAGS) -mtls-dialect=gnu2 $(TLS_LIB) $< -o $@

# Lazy binding: the loader writes ARM's GOT[1] / GOT[2] at DT_PLTGOT and the
# secure-PLT PPC32 words after DT_PPC_GOT, none under a relocation.
$(OUTDIR)/arm_lazy.elf: src/plt_call.c | $(OUTDIR)
	$(ARM) $(CFLAGS) -shared $< -o $@
$(OUTDIR)/ppc32be_lazy.elf: src/plt_call.c | $(OUTDIR)
	$(PPC32) $(CFLAGS) -shared $< -o $@

# A hot/cold split; AArch64 GCC only partitions when asked.
$(OUTDIR)/x64_cold.elf: src/cold.c | $(OUTDIR)
	$(X64) $(CFLAGS) -Wl,-e,foo $< -o $@
$(OUTDIR)/aarch64_cold.elf: src/cold.c | $(OUTDIR)
	$(A64) $(CFLAGS) -freorder-blocks-and-partition -Wl,-e,foo $< -o $@

clean:
	rm -rf $(OUTDIR)
