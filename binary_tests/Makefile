# ── compiler selection ─────────────────────────────────────────────────────────
# Prefer an explicitly versioned cross-compiler; fall back to the generic one.
# Override any compiler on the command line, e.g.:  make CC32=i686-linux-gnu-gcc-12

# ── x86 (32-bit) ──────────────────────────────────────────────────────────────
CC32 ?= $(shell command -v i686-linux-gnu-gcc-11 2>/dev/null \
	       || command -v i686-linux-gnu-gcc 2>/dev/null \
	       || echo gcc)
CFLAGS32 := -m32 -O2 -g

# ── x86-64 ────────────────────────────────────────────────────────────────────
CC64    ?= gcc
CFLAGS64 := -m64 -O2 -g

# ── AArch64 (64-bit ARM) ──────────────────────────────────────────────────────
CCAARCH64 ?= $(shell command -v aarch64-linux-gnu-gcc 2>/dev/null || echo false)
CFLAGS_AARCH64 := -O2 -g

# ── ARM (32-bit, hard-float ABI, force ARM encoding — no Thumb) ───────────────
# -marm forces ARM instruction encoding so the Sleigh ARM spec can decode it.
# arm-linux-gnueabihf targets ARMv7-A with hardware floating point.
CCARM ?= $(shell command -v arm-linux-gnueabihf-gcc 2>/dev/null || echo false)
CFLAGS_ARM := -marm -O2 -g

.PHONY: all clean

all: binary_test binary_test_x64 binary_test_aarch64 binary_test_arm

binary_test: test.c
	$(CC32) $(CFLAGS32) $< -o $@

binary_test_x64: test.c
	$(CC64) $(CFLAGS64) $< -o $@

binary_test_aarch64: test.c
	@if [ "$(CCAARCH64)" = "false" ]; then \
		echo "WARNING: aarch64-linux-gnu-gcc not found; skipping $@"; \
	else \
		$(CCAARCH64) $(CFLAGS_AARCH64) $< -o $@; \
	fi

binary_test_arm: test.c
	@if [ "$(CCARM)" = "false" ]; then \
		echo "WARNING: arm-linux-gnueabihf-gcc not found; skipping $@"; \
	else \
		$(CCARM) $(CFLAGS_ARM) $< -o $@; \
	fi

clean:
	rm -f binary_test binary_test_x64 binary_test_aarch64 binary_test_arm
