# Big-endian ARM (ARMv8 A-profile, non-Thumb).  Debian has no
# `armeb-linux-gnueabi[hf]-gcc` cross package, and the LE
# `arm-linux-gnueabihf-gcc -mbig-endian` cannot link because Debian's
# arm sysroot ships an LE-only libgcc_s.  ld.lld 14 also lacks the
# `armelfb_linux_eabi` emulation, so `-fuse-ld=lld` fails.
#
# Workaround: clang assembles BE ARM, and the GNU `ld` from the
# `arm-linux-gnueabihf` cross package links it via the `elf32-bigarm`
# BFD target (`-EB`).  Mirror aarch64be.mk's `-static -nostdlib
# -ffreestanding`: we don't need the C library to test analyzer behavior
# on per-function fixtures, and avoiding it sidesteps the BE-sysroot
# gap entirely.
#
# `--target=armeb-linux-gnueabihf` (the *hf* variant) selects the AAPCS
# hard-float ABI so float ops compile to VFP `vadd.f32` etc. rather than
# `bl __aeabi_fadd` calls into libgcc.  The soft-float `armeb-linux-gnueabi`
# target erases the FloatBinaryOp shapes the analyzer's float tests
# assert on (the calls fold into Call nodes, not Float* ops).
#
# `-mno-thumb` forces ARM (4-byte) instruction encoding, which the
# `SLA_SPEC_ARM8_BE` Sleigh spec expects (mirroring the `-marm` flag in
# `arm.mk`).
#
# `-O0`: preserves loop / branch shapes the analyzer tests assert on
# (see ppc32be.mk for the rationale).  `-ffp-contract=off`: prevents
# fadd+fmul fusion into fmadd/fmsub — keeps each op as a separate
# FloatBinaryOp node.
#
# `-Wl,--unresolved-symbols=ignore-all`: clang at -O0 still emits the
# odd libgcc helper call (e.g. integer-division `__aeabi_uidiv`); without
# a libc we let those resolve to stub addresses (matching ppc32le.mk).
#
# `-march=armv5te -mfpu=vfp2`: matches the `PSPEC_ARM_V45` Sleigh spec.
# Without this, clang's default `armeb-linux-gnueabihf` baseline is
# ARMv7-A with movw/movt — which the v4/v5 SLA cannot decode
# ("Unable to resolve constructor"). VFPv2 supplies the float ops the
# hard-float ABI promises while staying within the v45 instruction set.
CC     := clang
CFLAGS := --target=armeb-linux-gnueabihf -march=armv5te -mfpu=vfp2 -mno-thumb \
          --ld-path=/usr/arm-linux-gnueabihf/bin/ld \
          -static -nostdlib -ffreestanding \
          -O0 -g -fno-stack-protector -fno-pic \
          -ffp-contract=off \
          -Wl,--unresolved-symbols=ignore-all
