# Round 11 — Per-crate README diffs (R7 consolidation)

Two per-crate README changes required, sourced from doc-verification report 3A.

## Change 1 — `crates/target/README.md:10-12`

**Source:** 3A finding 22 (REFUTED — wrong `ArchPreset` casing).
**Severity:** MED (would mislead a contributor copying variant names into Rust source).

### Current

```
- `ArchPreset` — `X8664`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`,
  `ArmThumb`, `Mipsbe32`, `Mipsle32`, `Mipsbe64`, `Mipsle64`,
  `Ppc32Be`, `Ppc32Le`, `Ppc64Be`, `Ppc64Le`.
```

### Proposed

```
- `ArchPreset` — `X86_64`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`,
  `ArmThumb`, `MipsBe32`, `MipsLe32`, `MipsBe64`, `MipsLe64`,
  `Ppc32Be`, `Ppc32Le`, `Ppc64Be`, `Ppc64Le`.
```

### Verification

`crates/target/src/arch.rs:91-107` declares:
- `X86_64` (underscore between 86 and 64; README wrote `X8664`)
- `MipsBe32` (mixed-case `B`; README wrote `Mipsbe32`)
- `MipsLe32` (mixed-case `L`; README wrote `Mipsle32`)
- `MipsBe64` (README wrote `Mipsbe64`)
- `MipsLe64` (README wrote `Mipsle64`)
- `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`, `ArmThumb`, `X86`, `Ppc32Be`, `Ppc32Le`, `Ppc64Be`, `Ppc64Le` — all correctly spelled

The five wrong names would produce `use target::ArchPreset::X8664;` (compile error) or `use target::ArchPreset::Mipsbe32;` (compile error) if a contributor copies them literally.

## Change 2 — `crates/pcode-lift/README.md:31-33`

**Source:** 3A finding 24 (PARTIALLY CONFIRMED — `require_output_vn` listed as public, is `pub(crate)`).
**Severity:** LOW (surface mismatch; misleads external contributors about the crate's public API).

### Current

```
- `first_input_or_err(insn)` / `decode_space_id(insn)` /
  `require_output_vn(insn)` — small shared helpers used by the per-opcode
  handlers.
```

(under `## Public surface` heading)

### Proposed

```
- `first_input_or_err(insn)` / `decode_space_id(insn)` — small shared
  helpers used by the per-opcode handlers (public API).
- `require_output_vn(insn)` — crate-private helper (`pub(crate)`) used
  internally by the per-opcode handlers; not part of the public API.
```

### Verification

`crates/pcode-lift/src/lib.rs`:
- Line 117: `pub fn first_input_or_err` — `pub`
- Line 138: `pub fn decode_space_id` — `pub`
- Line 93: `pub(crate) fn require_output_vn` — `pub(crate)` only

No external caller of `require_output_vn` exists in any crate other than `pcode-lift` itself.  Option (a) from the 3A report is adopted: move `require_output_vn` out of the public-surface bullet.  Option (b) — promoting to `pub` — is not recommended since no external user has been identified.
