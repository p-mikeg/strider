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
CASES  := $(notdir $(basename $(wildcard cases/*.c) $(wildcard cases/*.S)))

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

# Shared-library fixtures: cases that need ET_DYN linkage so they
# carry actual relocations (R_*_RELATIVE on function-pointer tables,
# PLT entries for unresolved externals).  Cases listed here are
# built with `-shared -fPIC` and skip arch CFLAGS that conflict with
# `-shared` (`-no-pie`, `-fno-pic`).  Used by reader::elf::
# apply_elf_relocations tests; see cases/elf_relocs.c.
SHARED_CASES := elf_relocs

# CFLAGS_BASE: drop `-no-pie` / `-fno-pic` from the arch's CFLAGS for
# the SHARED_CASES rule, since `-shared` requires PIC.  Other arch
# flags (-m32, -O2, -g, …) are still needed.
CFLAGS_BASE := $(filter-out -no-pie -fno-pic,$(CFLAGS))

$(OUTDIR)/$(ARCH)/%.elf: cases/%.c | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	elif echo "$(SHARED_CASES)" | tr ' ' '\n' | grep -qx "$(basename $(notdir $@))"; then \
		$(CC) $(CFLAGS_BASE) $(COMMON_CFLAGS) -shared -fPIC -nostdlib $< -o $@ && echo "  Built $@ (shared)"; \
	else \
		$(CC) $(CFLAGS) $(COMMON_CFLAGS) $< -o $@ && echo "  Built $@"; \
	fi

# Hand-written assembly cases (`cases/*.S`).  An `.S` source is built
# with `-nostdlib -static` so it needs no libc and produces a
# freestanding ELF; the arch's `CFLAGS` are still applied for `-m32`
# / `-march=...` style targeting.  An `.S` file is typically arch-
# specific (instructions only assemble for one ISA); for non-matching
# arches the assembler errors out, the build fails for that single
# case, and tests skip cleanly because the fixture won't exist.
$(OUTDIR)/$(ARCH)/%.elf: cases/%.S | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	else \
		$(CC) $(CFLAGS) -nostdlib -static $< -o $@ 2>/dev/null && echo "  Built $@ (asm)" \
			|| echo "  Skipped $@ (asm doesn't build for $(ARCH))"; \
	fi

else
# ── all-arch mode ─────────────────────────────────────────────────────────────
all: $(ARCHES)

$(ARCHES):
	@$(MAKE) --no-print-directory ARCH=$@ $(if $(CASE),CASE=$(CASE))

endif

clean:
	rm -rf $(OUTDIR)
