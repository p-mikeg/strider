# fixtures/Makefile
#
# Builds one ELF per (arch, case) combination:  out/<arch>/<case>.elf
#
# Each architecture is described by arch/<arch>.mk, which sets CC and CFLAGS.
# Each test case is a single .c file in cases/.  New cases require no Makefile
# edits — drop a file in cases/ and `make` picks it up.
#
# Usage:
#   make               — build every case for every arch
#   make ARCH=x64      — build every case for x64 only
#   make x64           — same via phony shortcut
#   make CASE=memory   — build the memory case for every arch
#   make ARCH=x64 CASE=memory
#                      — build cases/memory.c for x64 only
#   make clean         — remove out/

OUTDIR := out
ARCHES := x86 x86_kernel x64 aarch64 aarch64be arm arm_be arm_thumb \
          mips32le mips32be mips64le mips64be \
          ppc32be ppc32le ppc64be ppc64le
CASES  := $(notdir $(basename $(wildcard cases/*.c)))

# Common warning-flag set: every fixture must compile clean.
# -fno-optimize-sibling-calls: keep recursive / wrapper calls visible as
#   real `call` instructions in the lifted p-code.  Without this GCC turns
#   them into branches and the analyzer's Call-counting pattern queries
#   fail for reasons that are compiler-specific rather than analyzer bugs
#   (BUG-6 in analyzer-known-issues).
COMMON_CFLAGS := -Wall -Wextra -Werror -Wno-unused-parameter \
                 -fno-optimize-sibling-calls

.PHONY: all clean $(ARCHES)

ifdef ARCH
# ── single-arch mode ──────────────────────────────────────────────────────────
include arch/$(ARCH).mk

# Filter to a single case if CASE is set; otherwise build all cases.
ifdef CASE
SELECTED_CASES := $(CASE)
else
SELECTED_CASES := $(CASES)
endif

TARGETS := $(addprefix $(OUTDIR)/$(ARCH)/,$(addsuffix .elf,$(SELECTED_CASES)))

all: $(TARGETS)

$(OUTDIR)/$(ARCH):
	mkdir -p $@

$(OUTDIR)/$(ARCH)/%.elf: cases/%.c | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	else \
		$(CC) $(CFLAGS) $(COMMON_CFLAGS) $< -o $@ && echo "  Built $@"; \
	fi

else
# ── all-arch mode ─────────────────────────────────────────────────────────────
all: $(ARCHES)

$(ARCHES):
	@$(MAKE) --no-print-directory ARCH=$@ $(if $(CASE),CASE=$(CASE))

endif

clean:
	rm -rf $(OUTDIR)
