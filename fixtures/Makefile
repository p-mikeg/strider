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
#   fail for reasons that are compiler-specific rather than analyzer bugs.
# -ffile-prefix-map: without it every committed fixture embeds the build
#   machine's absolute path in DWARF's DW_AT_comp_dir.
COMMON_CFLAGS := -Wall -Wextra -Werror -Wno-unused-parameter \
                 -fno-optimize-sibling-calls \
                 -ffile-prefix-map=$(CURDIR)=/strider/fixtures

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

TARGETS    := $(addprefix $(OUTDIR)/$(ARCH)/,$(addsuffix .elf,$(SELECTED_CASES)))
# Every C case also gets compiled to an unlinked ET_REL object file at
# `out/<arch>/<case>.o`.  Used by reader tests that need a .o for the
# section-walking loader path (ET_REL has no PT_LOAD program headers).
# Hand-written `.S` cases are excluded because they're already small
# freestanding things; only `cases/*.c` get the .o twin.
C_CASES    := $(notdir $(basename $(wildcard cases/*.c)))
OBJ_CASES  := $(filter $(C_CASES),$(SELECTED_CASES))
OBJ_TARGETS := $(addprefix $(OUTDIR)/$(ARCH)/,$(addsuffix .o,$(OBJ_CASES)))

all: $(TARGETS) $(OBJ_TARGETS)

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

# `,` is a Make-meta character inside `filter-out`'s arg list; wrap
# one in `$(comma)` so we can match a literal comma inside `-Wl,...`.
comma := ,

# CFLAGS_OBJ: as `CFLAGS_BASE` but additionally drops link-only flags
# (`-fuse-ld=...`, `-static`, `-nostdlib`, `-Wl,...`) — `gcc -c` /
# `clang -c` emits an unused-argument warning for those, and we
# compile with `-Werror`.  The .o build never links, so these flags
# have no effect and should be silently dropped.
#
# Manually drop the known link-only flags one at a time — Make's `%`
# pattern in `filter-out` matches text containing `=`, so a pattern
# like `-Wl,%` would also nibble at `--target=...`.  Spelling each
# flag out keeps the filter conservative.  Add new tokens here when
# an arch's CFLAGS picks up a fresh linker-only option; flags that
# only affect link-time but trip `-Werror,-Wunused-command-line-argument`
# during `clang -c` need to be in this list.
CFLAGS_OBJ := $(filter-out -fuse-ld=lld -static -nostdlib --ld-path=/usr/arm-linux-gnueabihf/bin/ld -Wl$(comma)--unresolved-symbols=ignore-all -shared -fPIC,$(CFLAGS_BASE))

$(OUTDIR)/$(ARCH)/%.elf: cases/%.c | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	elif echo "$(SHARED_CASES)" | tr ' ' '\n' | grep -qx "$(basename $(notdir $@))"; then \
		$(CC) $(CFLAGS_BASE) $(COMMON_CFLAGS) -shared -fPIC -nostdlib $< -o $@ && echo "  Built $@ (shared)"; \
	else \
		$(CC) $(CFLAGS) $(COMMON_CFLAGS) $< -o $@ && echo "  Built $@"; \
	fi

# Unlinked object files (`out/<arch>/<case>.o`) for the same `.c`
# sources.  Built with `-c` so the result is ET_REL (no link step):
# section addresses are 0 pre-link, and there are no PT_LOAD program
# headers.  Strider's loader treats ET_REL specially (section-walker
# with first-wins VMA dedup) — these fixtures exercise that path.
# `CFLAGS_OBJ` drops link-only flags so `-c` doesn't emit unused-
# argument warnings (`-Werror` would otherwise fail the build).
$(OUTDIR)/$(ARCH)/%.o: cases/%.c | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	else \
		$(CC) $(CFLAGS_OBJ) $(COMMON_CFLAGS) -c $< -o $@ && echo "  Built $@ (object)"; \
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
