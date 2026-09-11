# BE8 ARM 32-bit: little-endian instruction encoding, big-endian data,
# flagged `EF_ARM_BE8`, which is what `SleighArch::arm_be_kernel` decodes.
# `arm_be.mk` builds the legacy BE32 form instead (big-endian instructions
# AND data); only the flag separates the two, `EI_DATA` cannot.
#
# The assembler emits BE32 objects.  GNU `ld --be8` byte-reverses the code
# sections against their `$a` / `$d` mapping symbols, leaving data words in
# place, and sets the flag.
#
# `-mbig-endian` over the LE `arm-linux-gnueabihf-gcc`: Debian ships no
# `armeb-` cross package, and the arm sysroot's libgcc_s is LE-only, so
# `-static -nostdlib -ffreestanding` sidesteps the C library as
# `aarch64be.mk` and `arm_be.mk` do, with `--unresolved-symbols=ignore-all`
# for the odd libgcc helper call.
#
# `-marm` forces 4-byte ARM encoding (see `arm.mk`); `-O0` keeps the
# sub-register shapes the VFP slicing test reads.
#
# `-mword-relocations` puts address constants in `.text` literal pools
# instead of `movw` / `movt` pairs.  A pool word keeps the data order while
# `ld --be8` reverses the instructions around it, so one `ldr [pc, #n]`
# reads the two orders apart; an immediate encoded in the instruction cannot.
CC := $(shell command -v arm-linux-gnueabihf-gcc 2>/dev/null || echo false)
LINK_ONLY_CFLAGS := -Wl,--be8
CFLAGS := -mbig-endian -marm -mword-relocations $(LINK_ONLY_CFLAGS) \
          -static -nostdlib -ffreestanding \
          -O0 -g -fno-stack-protector -fno-pic \
          -Wl,--unresolved-symbols=ignore-all
