CC     := $(shell command -v i686-linux-gnu-gcc-11 2>/dev/null \
               || command -v i686-linux-gnu-gcc 2>/dev/null \
               || echo gcc)
# Same flags as x86 plus `-mregparm=3` — the x86 32-bit Linux kernel
# CC: first three integer args go in EAX, EDX, ECX rather than on the
# stack.  Used by `crates/target::CallingConvention::x86_linux_kernel`.
CFLAGS := -m32 -O2 -g -fno-stack-protector -fno-pic -no-pie -mregparm=3
