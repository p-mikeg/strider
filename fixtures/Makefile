# binary_tests/Makefile
#
# Builds test binaries into out/<arch>/test.elf.
#
# Each architecture is described by arch/<arch>.mk, which sets CC and CFLAGS.
#
# Usage:
#   make               — build all architectures
#   make ARCH=x64      — build only the x64 binary
#   make x64           — same via phony shortcut
#   make clean         — remove the out/ directory
#
# Override the compiler on the command line, e.g.:
#   make ARCH=x86 CC=i686-linux-gnu-gcc-12

OUTDIR := out
ARCHES := x86 x64 aarch64 arm
SRC    := test.c

.PHONY: all clean $(ARCHES)

ifdef ARCH
# ── single-arch mode: include arch/<ARCH>.mk, build out/<ARCH>/test.elf ───────

include arch/$(ARCH).mk

TARGET := $(OUTDIR)/$(ARCH)/test.elf

all: $(TARGET)

$(OUTDIR)/$(ARCH):
	mkdir -p $@

$(TARGET): $(SRC) | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	else \
		$(CC) $(CFLAGS) $< -o $@ && echo "  Built $@"; \
	fi

else
# ── all-arch mode: delegate each arch to a single-arch recursive invocation ───

all: $(ARCHES)

$(ARCHES):
	@$(MAKE) --no-print-directory ARCH=$@

endif

# Phony shortcuts always available: `make x86`, `make aarch64`, etc.
# In all-arch mode the rule above handles them; in single-arch mode Make
# finds no rule for other arch names, which is harmless.

clean:
	rm -rf $(OUTDIR)
