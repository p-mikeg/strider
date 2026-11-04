# strider-orchestrator

Drives one function from bytes to optimised IR: decode, lift, optimise,
resolve indirect branches, and rebuild until the CFG stops changing.

## API

- `Strider::new(arch, reader, rom)`: the per-binary handle. `rom` is the
  read-only image `LoadReadOnly` and the jump-table classifier read; `None`
  disables both.
- `Strider::analyze(entry, cc, opts, pipeline)` -> `Result<AnalyzeResult>`,
  the loop below; `pipeline` defaults to `strider_opt::default_pipeline`.
- `Strider::build_cfg(entry, cfg_opts)`: a structural decode only.
- Accessors for running your own pipeline: `arch`, `lifter`, `sleigh`,
  `sleigh_regs`, `user_op_names`, `rom`.
- Re-exports: `strider_opt` as `opt`, `Lifter` / `LiftOutcome` / `LiftSites`,
  and `Incompleteness` / `Reason`.

## The loop

Each round builds the CFG from the caller's `known_targets` and the loop's
own seats (`CfgOptions::derived_targets`), lifts and optimises it, and
classifies every dispatch site: each `IndirectBranch` placeholder and each
seated `Switch`, which can still widen. The answers fold into the seats, so
one round seats one level of discovery.

It converges when no site is left, or a round changes no seat and refuses
none. 256 folds is the cap; sites still growing there are reported. Two
verdicts freeze a site:

- A site that narrows twice is withdrawn.
- A seat naming a target that does not decode, or that reaches code that does
  not, is withdrawn whole. A target two edges reach in different ISA modes
  loses only that arm.

## Incompleteness

`AnalyzeResult::incomplete()` is one `Incompleteness` set of
`(PcodeInsnAddr, Reason)`; check it before trusting an absence.

- `UnresolvedBranch`: a live placeholder, a seated `Switch` whose selector no
  longer derives, a site still growing at the cap, a site that lost a proved
  successor or narrowed, a withdrawn site, and an unproven return the caller
  did not seed.
- `UnverifiedSeed`: a `Switch` holding only the caller's seed, a site the CFG
  consumed as a `TailCall` or as a caller-seeded `Return`, an unproven return
  the caller seeded `LinkRegister`, and an arm seated in a mode nothing
  proved for it.
- `IsaModeConflict`, `InteriorTarget`, `UnmappedTarget`, `UndecodableTarget`,
  `PastFunctionEnd`: from every round, since an inexact edge fed the
  classifier whether or not the final CFG keeps it.

A seed suppresses only the unclassified-`Switch` report, and only until the
site outgrows it.
