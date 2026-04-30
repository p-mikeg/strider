# crates/cfg review — round 6 (2026-04-30)

## Summary

- Files reviewed: 32 (all `.rs` under `crates/cfg/src`, `crates/cfg/tests`, `crates/cfg/examples`; no `crates/cfg/benches`)
- Findings total: 41
  - Correctness: 11
  - Dead code: 5
  - Duplication & unification: 7
  - Simplification: 6
  - Readability: 9
  - Performance: 3
- High-confidence findings: 26
- Low-risk findings: 33

The crate is structurally clean and well tested.  Three themes dominate:

- **Stale-comment drift after the anyhow + strider-restructure migrations.**  The `strider-error` machinery is gone but ~14 docstrings still cite `ErrorKind::Foo`, the `RegionIrCache` type was deleted in the strider restructure but `predecessor_count`'s docstring still explains how `RegionIrCache` uses it, and `Builder::new` claims a sibling `set_endianness` method that does not exist.  Internal codenames (R1.2, R1.3, R3.6, W3, W9, W11, F5, F7, BUG-25, Phase 4 / 5) appear in 30+ places.
- **`RegionTerminator::Switch` outside-fn-range tail-call rejection** (`region_builder.rs:438-442`) hard-`bail!`s when *any* target of a tier-2-resolved `Multiple` lies outside the function range, with the message "could not be statically resolved" — but the targets WERE resolved.  This silently turns a soft-recoverable tier-2 result into a hard CFG-build failure.  The `// (Future: refine to mixed Branch/TailCall.)` comment acknowledges the gap.
- **Tier-1 `decode_branch_target` for default-code-space absolute branches doesn't sign-extend** the way the CONST-space arm does.  Sleigh probably never produces a negative `off`, but the asymmetry is undocumented and would silently miscompile a 32-bit code space if one were ever supported.

A handful of smaller things — duplicate constructions of `PcodeInsnAddr { … insn_index: 0 }` (3 sites), the inlined `resolve_const_loads` that re-reads the rom's `Load(space)` filter logic but doesn't dedupe with `opt::LoadReadOnly::optimize`, and `predecessor_count` being publicly callable but with zero callers anywhere in the workspace.

## Table of contents

- F-001 `Switch` outside-fn-range hard-fails with misleading "could not be statically resolved" message (Correctness, `cfg/builder/region_builder.rs:413-443`)
- F-002 `decode_branch_target` default-code-space arm doesn't sign-extend `off` like the CONST arm does (Correctness, `cfg/builder/region_builder.rs:165-171`)
- F-003 Stale `ErrorKind::*` references in 14 docstrings post-anyhow migration (Correctness/Readability, multiple files)
- F-004 `predecessor_count` docstring still cites the deleted `RegionIrCache` type (Correctness, `cfg/query.rs:85-90`)
- F-005 `Cfg::sleigh` docstring cites the deleted `RegionIrCache` orchestrator and "harvest dance" terminology (Correctness/Readability, `cfg/mod.rs:36-58`)
- F-006 `Builder::new` docstring claims a sibling `set_endianness` method that does not exist (Correctness, `cfg/builder/mod.rs:79-80`)
- F-007 `region_fallthrough` docstring promises a `BUG-25-normalised` detection job that's actually only one of its callers' uses (Readability, `cfg/query.rs:50-58`)
- F-008 Tier-1 `Multiple` is **never** produced by the resolver — only by `known_targets` feedback — but the CFG builder treats it as a tier-1 outcome (Correctness, `cfg/builder/region_builder.rs:413-461`, `cfg/builder/indirect_resolve.rs:42-46`)
- F-009 Tier-1 `bail!` on out-of-range Multiple-switch targets is a *string* error rather than a typed kind (Correctness, `cfg/builder/region_builder.rs:439-441`)
- F-010 `resolve_indirect_target` swallows graph-construction-bug `find_unique_return` failures as `Ok(None)` (Correctness, `cfg/builder/indirect_resolve.rs:248-261`)
- F-011 `is_branch_tail_call` synthesises addresses with `insn_index = 0`, so the "invalid tail call (insn_index != 0)" check is unreachable from `Multiple`/`Single` paths (Correctness/Simplification, `cfg/builder/region_builder.rs:393-396, 434-437, 452-455`)
- F-012 `predecessor_count` has zero callers anywhere in the workspace (Dead code, `cfg/query.rs:90`)
- F-013 `_insn_addr` parameter on `resolve_indirect_target` is a documented "kept for future" — currently dead arg (Dead code, `cfg/builder/indirect_resolve.rs:114-121`)
- F-014 `RegionGraph` type alias is `pub` but never re-exported through `lib.rs`; tests reach it via the `Cfg::graph` field (Dead code, `cfg/types.rs:203`)
- F-015 `find_unique_return` walk-by-preorder helper unused outside `resolve_indirect_target` (Dead code, `cfg/builder/indirect_resolve.rs:348-363`)
- F-016 `OptionsBuilder::Default` impl manually calls `Self::new`, which itself just returns `Options::default` (Dead code/Simplification, `cfg/options.rs:109-122`)
- F-017 Repeated `PcodeInsnAddr { machine_addr: MachineInsnAddr { addr: target }, insn_index: 0 }` shape at 3 sites (Duplication, `cfg/builder/region_builder.rs:393-396, 434-437, 452-455`)
- F-018 `vn_to_name_with_regs` re-validates `space == REGISTER` after the dispatch, then forwards to `vn_to_name_non_register` for non-registers — same dispatch is in `Cfg::vn_to_name` (Duplication, `cfg/dot.rs:16-66`)
- F-019 Sort key `(space.shortcut_raw(), off, size)` is duplicated between `cfg/builder/indirect_resolve.rs:153` and `strider/src/strider/pipeline.rs:206` (Duplication, `cfg/builder/indirect_resolve.rs:149-153`)
- F-020 `resolve_const_loads` re-implements `opt::LoadReadOnly::optimize` line-for-line because of an `M: 'static` bound; the bound is sidestepped via inlining (Duplication, `cfg/builder/indirect_resolve.rs:287-330`)
- F-021 `Cfg::vn_to_name` and `cfg::dot::vn_to_name_*` overlap in dispatch, but `Cfg::vn_to_name` is only ever called via the `test_api` forwarder (Duplication, `cfg/dot.rs:16-24, 78-83`, `lib.rs:21`)
- F-022 The two `OptimizerPipeline` constructions in `resolve_indirect_target` (with-rom and without-rom) duplicate the `add(ConstantFold) + add(KnownBits) + add(RedundantPhis)` shape (Duplication, `cfg/builder/indirect_resolve.rs:205-230`)
- F-023 Three test-builder helpers (`make_builder`, `make_builder_opts`, `make_builder_with_bytes`) only differ in two scalar args — could collapse via a builder pattern (Duplication, `tests/common/synthetic.rs:48-65`)
- F-024 `Builder` field `sleigh` is `pub(super)` but the `pub` `Cfg::sleigh` is the user-facing harvest point — readers have to chase two layers (Simplification, `cfg/builder/mod.rs:52` + `cfg/mod.rs:58`)
- F-025 `decode_branch_target`'s sign-extension table reads as a `match` with three arms that reduce to "size in {1,2,4} → narrow signed" + "default → cast_signed" (Simplification, `cfg/builder/region_builder.rs:130-135`)
- F-026 `is_branch_tail_call_nocheck`'s `end_exclusive <= addr.addr` comparison reads as `end_exclusive` semantically, but is constructed via `saturating_add` which means `start + max_size`, not `end_exclusive` (Readability, `cfg/builder/region_builder.rs:201-204`)
- F-027 `Builder::build`'s `cfg failed accessing starting region` error is opaque and unrelated to the actual failure mode (Readability, `cfg/builder/mod.rs:217-220`)
- F-028 `failed spliting region` is a typo (should be "splitting") and the error string does not name the kind of failure (Readability, `cfg/builder/split.rs:43`)
- F-029 `Builder::with_known_targets` mutates `options.known_targets` in place; calling it twice silently replaces the prior set with no warning (Readability, `cfg/builder/mod.rs:194-201`)
- F-030 `RegionTerminator::Switch::target_value` is always `None` from cfg builder + no other constructor exists, but the field is `pub` and 99% of comments imply the orchestrator populates it (Readability/Dead code, `cfg/types.rs:114-138`)
- F-031 `resolve_indirect_target`'s 4-stage step-comment block (Step 1 … Step 6) is 100+ lines but the actual function body only spans 75 lines (Readability, `cfg/builder/indirect_resolve.rs:108-285`)
- F-032 `Options` `PartialEq` impl is implemented manually for ROM Arc-ptr-eq but not actually used anywhere in production code, only in two tests (Readability, `cfg/options.rs:78-90`)
- F-033 `RegionTerminator` doc comments for `Branch` / `Return` / `Fallthrough` paragraph-mismatched: `Fallthrough`'s comment talks about "first half of a split region" while `Branch` does not mention the split-second-half pairing (Readability, `cfg/types.rs:79-94`)
- F-034 `RegionBuilder::process_new_insn`'s 230-line dispatch is a long `match` over `insn.opcode` mixed with helper-style code blocks; could be split per-op (Readability, `cfg/builder/region_builder.rs:237-466`)
- F-035 `resolve_indirect_target` rebuilds the `OptimizerPipeline` from scratch for every `BranchIndirect` site in the region, even when there is no rom (Performance, `cfg/builder/indirect_resolve.rs:205-230`)
- F-036 `find_region_containing_addr` does a `BTreeMap::range(..=addr).next_back()` lookup, but the map's keys are `PcodeInsnAddr` so `last region whose start_addr <= addr` walks all map entries on a fresh BTreeMap (Performance, `cfg/builder/mod.rs:128-138`)
- F-037 `find_unique_return`'s `for node_id in fg.preorder()` iterates the entire reachable graph just to find the unique Return — a `match_first` with early break on the second hit would be O(1) for a graph with one Return (Performance, `cfg/builder/indirect_resolve.rs:348-363`)
- F-038 `region_fallthrough` is `pub` and 1 external caller (`strider`) gates `is_some()` on it; could expose `has_fallthrough_successor(_) -> bool` directly (Simplification, `cfg/query.rs:59-61`, `strider/src/strider/insn/control.rs:126`)
- F-039 `RegionTerminator::Switch` matches against `target_vn` to identify dispatch but the `target_vn` is the same as the `BranchIndirect`'s `inputs[0]` — which means strider must re-derive the same VN; could store it once (Readability/Simplification, `cfg/types.rs:127-138`)
- F-040 `cfg::dot::test_api::vn_to_name` is the only call site for `Cfg::vn_to_name`; the latter is otherwise pure-internal (Simplification, `cfg/dot.rs:16-24`, `lib.rs:21`)
- F-041 Test `region_terminator.rs` block-comment "Phase-5 update" / "R1.1 pin" still references in-flight phase numbers (Readability, `tests/region_terminator.rs:202-292`)

## Findings

### Correctness

### F-001 `Switch` outside-fn-range hard-fails with misleading "could not be statically resolved" message

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:413-443`
- **What:**
  ```rust
  super::indirect_resolve::ResolvedTargets::Multiple(targets) => {
      // …
      // If any target lies outside the function
      // range, surface as unresolved — `Multiple`
      // doesn't have a per-target tail-call escape.
      // (Future: refine to mixed Branch/TailCall.)
      for target in &targets {
          let target_addr = PcodeInsnAddr {
              machine_addr: MachineInsnAddr { addr: *target },
              insn_index: 0,
          };
          if self.is_branch_tail_call(target_addr)? {
              bail!(
                  "branch-indirect at {addr:?} could not be statically resolved"
              );
          }
      }
  ```
- **Why:** When tier 2 resolves a `BranchIndirect` to `Multiple([A, B, C])` where any of `A/B/C` is a tail-call address (outside `[start, start + fn_max_size)` or below `start_addr`), the cfg builder bails out of the entire CFG build with `"could not be statically resolved"`.  Two problems:
  - **The message lies.**  The targets WERE statically resolved by tier 2; the issue is purely that one is a tail call.
  - **The contract is asymmetric.**  `Single` with an out-of-range target produces `RegionTerminator::TailCall` (line 397-400) without complaint.  `Multiple` with any out-of-range target throws away ALL the tier-2 work.

  A real binary with a switch table that has one external-stub case would currently fail to lift.  The `// (Future: refine to mixed Branch/TailCall.)` comment acknowledges the gap.  The soft-contract policy that R1.2/R1.3 introduced for the `Single` / `LinkRegister` paths is not extended here.
- **Proposed change:** Either (a) emit `RegionTerminator::Switch` with the Multiple targets and treat out-of-range as a per-edge tail call (the future refinement the comment promises), or (b) fall back to `RegionTerminator::UnresolvedIndirectBranch` so the strider-level outer loop can retry — matching the soft contract.  At minimum, fix the error message to "branch-indirect at {addr:?} resolved to switch with target outside function range".
- **Confidence:** high
- **Risk if applied:** medium — affects the strider-level fixed-point convergence story.

### F-002 `decode_branch_target` default-code-space arm doesn't sign-extend `off` like the CONST arm does

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:130-171`
- **What:**
  ```rust
  rsleigh::VnSpace::CONST => {
      let raw = branch_target_var.addr.off;
      let off: i64 = match branch_target_var.size {
          1 => (raw as i8) as i64,
          2 => (raw as i16) as i64,
          4 => (raw as i32) as i64,
          _ => raw.cast_signed(),
      };
      // … (uses `off` after sign-extension)
  }
  // Absolute branch: the offset IS the target machine address
  space if space == default_code_space => Ok(PcodeInsnAddr {
      machine_addr: MachineInsnAddr {
          addr: branch_target_var.addr.off,
      },
      insn_index: 0,
  }),
  ```
- **Why:** The CONST arm explicitly sign-extends `off` from the varnode's declared `size` (1/2/4-byte) so a backward branch encoded as `(-4) as u32 = 0xFFFFFFFC` decodes to `i64 = -4` rather than `4_294_967_292`.  The default-code-space arm uses `branch_target_var.addr.off` directly as the absolute machine address, bypassing the `size`-aware sign extension entirely.

  In practice this is fine on every supported arch: Sleigh emits the absolute target as a full 64-bit `off` for `Vn`s in a 64-bit code space.  But:
  - The `branch_target_var.size` field is silently ignored — a mismatch between varnode width and target-address width never surfaces.
  - On a hypothetical 32-bit code space (e.g. 32-bit ARM mode where Sleigh might emit `size=4`), an absolute target encoding `(0xFFFFFFFC as u64)` would correctly stay at `0xFFFFFFFC` (which IS a valid 32-bit address, not a sign-extended -4).  So technically it's correct but only by accident — Sleigh's contract is undocumented at this layer.

  The asymmetry between the two arms is also a readability hazard: a future contributor seeing the CONST arm's careful sign-extend may "fix" the default-code-space arm to match, breaking 64-bit absolute branches.
- **Proposed change:** Add a `// SOUNDNESS:` comment to the default-code-space arm explaining why `branch_target_var.addr.off` is the absolute address regardless of `size`, and assert (or document) that Sleigh's contract guarantees this.
- **Confidence:** medium
- **Risk if applied:** low (comment only).

### F-003 Stale `ErrorKind::*` references in 14 docstrings post-anyhow migration

- **Category:** Correctness
- **Location:** Multiple files; see grep below
- **What:** After the workspace-wide anyhow migration, `cfg::Error` and `cfg::ErrorKind` no longer exist, but doc comments throughout the crate still cite specific kinds:
  ```rust
  /// # Errors
  /// Returns [`ErrorKind::EmptyRegion`] if `region.insns` is empty.
  pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
  ```
  References (line numbers shown):
  - `cfg/builder/mod.rs:111`: `ErrorKind::EmptyRegion`
  - `cfg/builder/region_builder.rs:20`: `ErrorKind::MachineAddrOverflow`
  - `cfg/builder/region_builder.rs:215`: `ErrorKind::InvalidTailCall`
  - `cfg/builder/region_builder.rs:335`: `ErrorKind::UnresolvedIndirectBranch`
  - `cfg/builder/region_builder.rs:381`: `ErrorKind::UnresolvedIndirectBranch(addr)`
  - `cfg/builder/region_builder.rs:472`: `ErrorKind::EmptyRegion`
  - `cfg/builder/region_builder.rs:572`: `crate::ErrorKind::MachineAddrOverflow`
  - `cfg/builder/region_builder.rs:674`: `crate::ErrorKind::EmptyRegion`
  - `cfg/builder/indirect_resolve.rs:28`: `crate::ErrorKind::UnresolvedIndirectBranch`
  - `cfg/builder/indirect_resolve.rs:98-99`: `crate::ErrorKind::IrError`, `OptError`, `PcodeLiftError`
  - `cfg/builder/indirect_resolve.rs:243`: `ErrorKind::UnresolvedIndirectBranch` (again)
  - `cfg/query.rs:25, 44, 57, 66`: `ErrorKind::DuplicateEdgeKind` (4 occurrences)
  - `cfg/types.rs:147`: `ErrorKind::UnresolvedIndirectBranch(addr)`
  - `cfg/types.rs:160`: `ErrorKind::UnresolvedIndirectBranch`
  - `tests/indirect_resolve.rs:456`: `crate::ErrorKind::MissingBranchTarget`
- **Why:** These all rendered as `rustdoc` broken-link warnings or were silently the wrong link.  They contradict the actual error type (`anyhow::Result<T>`), so a reader who finds and follows one of these mentally builds a wrong model of the error surface.

  Fixing each is a 5-character delete plus a sentence rewrite; no code change.
- **Proposed change:** Replace each `[\`ErrorKind::Foo\`]` link with a plain-prose description of the error condition, matching the message string.  Example: `"Returns an error when \`region.insns\` is empty."`.
- **Confidence:** high
- **Risk if applied:** low

### F-004 `predecessor_count` docstring still cites the deleted `RegionIrCache` type

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/query.rs:85-94`
- **What:**
  ```rust
  /// Returns the number of incoming edges (predecessors) for
  /// `region_id`.  Used by the strider fixed-point orchestrator's
  /// `RegionIrCache` to detect when a cached region has gained a
  /// new predecessor across iterations.
  #[must_use]
  pub fn predecessor_count(&self, region_id: RegionId) -> usize {
      self.graph
          .neighbors_directed(region_id, petgraph::Incoming)
          .count()
  }
  ```
- **Why:** `RegionIrCache` was deleted in the strider restructure (CLAUDE.md notes: "*RegionIrCache is gone*").  A workspace-wide grep confirms zero callers anywhere:
  ```
  $ grep -rn 'predecessor_count' --include='*.rs'
  crates/cfg/src/cfg/query.rs:90: …  // definition
  crates/cfg/src/cfg/query.rs:102: …  // self-link in another method's doc
  ```
  The docstring promises a use case that no longer exists.  The method itself is also dead (see F-012).
- **Proposed change:** Delete the method and its docstring.  If the orchestrator ever needs predecessor counts again, add it back at that time.
- **Confidence:** high
- **Risk if applied:** low — but combined with F-012 means a `pub` method goes away, which is technically a breaking API change for downstream users not in this workspace.  Doesn't apply here (no downstream users).

### F-005 `Cfg::sleigh` docstring cites the deleted `RegionIrCache` orchestrator and "harvest dance" terminology

- **Category:** Correctness/Readability
- **Location:** `crates/cfg/src/cfg/mod.rs:36-58`
- **What:**
  ```rust
  /// **Persistence contract** (W11 / Sleigh persistence work): the
  /// Sleigh handle is owned by the [`Cfg`] across the analysis
  /// lifetime and threaded through every iteration of the indirect-
  /// branch fixed-point orchestrator.  Each iteration: (1) builds a
  /// new [`Cfg`] via [`Builder`] (consuming the Sleigh by value);
  /// (2) harvests the Sleigh out of [`Cfg::sleigh`] before dropping
  /// the [`Cfg`]; (3) re-uses the same Sleigh in the next iteration
  /// build.  This avoids re-loading the SLA spec on every CFG
  /// rebuild — a measurable hot-path cost the orchestrator's
  /// fixed-point loop pays for every indirect-branch resolution.
  /// …
  pub sleigh: rsleigh::Sleigh<R>,
  ```
- **Why:** The "W11" code name and the "harvest the Sleigh out of `Cfg::sleigh`" pattern are still accurate for the current strider orchestrator.  But the surrounding terminology is in flux:
  - "indirect-branch fixed-point orchestrator" is now `strider::run` / `LoopState`.
  - "every CFG rebuild" — currently strider builds a new CFG per iteration, but the recent restructure may make that less of a hot path.
  - "W11" is an internal codename that doesn't appear in any doc the user can navigate to.

  The `tests/sleigh_reuse.rs` integration test exercises this contract correctly, so the *behaviour* is fine; the docstring just over-explains via internal codenames.
- **Proposed change:** Trim the codename, rewrite the contract in plain prose: "Owned by the Cfg across the analysis lifetime; the strider orchestrator harvests it via `cfg.sleigh` to avoid reloading the SLA spec across CFG rebuilds.  See `tests/sleigh_reuse.rs` for the round-trip test."
- **Confidence:** high
- **Risk if applied:** low

### F-006 `Builder::new` docstring claims a sibling `set_endianness` method that does not exist

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/mod.rs:79-80`
- **What:**
  ```rust
  /// The endianness defaults to [`target::Endianness::Little`].
  /// Callers that analyse big-endian binaries should use
  /// [`Self::with_endianness`] (or equivalently call
  /// [`Self::set_endianness`] before [`Self::build`]).
  ```
- **Why:** Workspace-wide grep:
  ```
  $ grep -rn 'fn set_endianness\|pub fn set_endianness' --include='*.rs'
  (no output)
  ```
  No such method exists.  Either it was planned and never landed, or it was removed and the doc comment got stale.  rustdoc will render this as a broken intra-doc link.
- **Proposed change:** Delete the `[\`Self::set_endianness\`]` half of the parenthetical.  The doc comment is otherwise correct.
- **Confidence:** high
- **Risk if applied:** low

### F-007 `region_fallthrough` docstring promises a `BUG-25-normalised` detection job that's actually only one of its callers' uses

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/query.rs:50-58`
- **What:**
  ```rust
  /// Returns the fallthrough successor of `region_id`, if any.
  ///
  /// Used by the analyzer to detect BUG-25-normalised unconditional
  /// branches: a CFG `Branch` p-code op whose target was reclassified
  /// as `Fallthrough` because it pointed at `pc + insn_len`.
  ```
- **Why:** The "analyzer" referenced here is the strider IR lifter, not a separate component.  And the docstring describes one specific use case (BUG-25 fallthrough detection in `strider::insn::control::handle_branch`).  But the method itself is a generic predecessor-by-edge-kind helper — `strider::pipeline.rs` also uses it indirectly via the standard fallthrough linker.  A fresh reader is confused about whether `region_fallthrough` is a special-case BUG-25 hook or a generic primitive.

  Note: BUG-25 normalisation lives in `cfg::region_builder::process_new_insn`'s `Branch` arm (line 273-281), which classifies the *edge kind* as `Fallthrough` rather than `Branch`.  The "BUG-25 detection" the docstring promises is actually the lifter calling `region_fallthrough` to handle the already-normalised case symmetrically.
- **Proposed change:** Rephrase to: "Returns the fallthrough successor of `region_id`, if any.  A region's fallthrough edge is its successor on the `Fallthrough` edge kind — emitted either by sequential decode reaching a known region OR by BUG-25 normalisation reclassifying a `Branch` whose target was the next instruction."
- **Confidence:** medium
- **Risk if applied:** low

### F-008 Tier-1 `Multiple` is **never** produced by the resolver — only by `known_targets` feedback — but the CFG builder treats it as a tier-1 outcome

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:413-461`, `crates/cfg/src/cfg/builder/indirect_resolve.rs:42-46`
- **What:**
  In `indirect_resolve.rs`:
  ```rust
  //! ## Multi-target / jump tables
  //!
  //! [`ResolvedTargets::Multiple`] is reserved for the future jump-table
  //! resolver and is not constructed by this round; the variant exists so
  //! adding jump-table support later is purely additive.
  ```
  And the actual classification arm in `resolve_indirect_target` (line 265-284) only returns `Single` / `LinkRegister` / `Ok(None)`; `Multiple` is never constructed.

  Yet `process_new_insn` has a full `Multiple(targets)` arm that:
  - Iterates targets to check for tail-call OOB (with the bail bug F-001),
  - Emits `RegionTerminator::Switch`,
  - Enqueues each target with a `Branch` edge.

  This arm is reached *only* when `known_targets` (from tier 2 feedback) injects a `Multiple` — but the comment block at line 414-425 talks about the "(or R4 jump-table classifier)" as if tier 1 could resolve `Multiple` itself.
- **Why:** A reader of the cfg builder reasonably assumes tier-1 hands off `Multiple`; the actual behaviour is "tier 2 hands off `Multiple` via `known_targets`".  Two things follow:
  - The extensive comment block at line 414-425 is misleading: it conflates "tier 1 resolved" and "tier 2 fed back via `known_targets`".
  - The behaviour for tier-2 feedback is not architecturally distinguishable from tier-1 — the same code path handles both, including the F-001 bail bug.
- **Proposed change:** Either (a) clarify the comment block to "Tier 2 feeds resolved switch tables back via `known_targets`; tier 1 never produces `Multiple` itself" or (b) document that the `Multiple` arm is the orchestrator's tier-2 input handler.  The actual behaviour is fine; the docstring/comment story is confusing.
- **Confidence:** high
- **Risk if applied:** low

### F-009 Tier-1 `bail!` on out-of-range Multiple-switch targets is a *string* error rather than a typed kind

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:439-441`
- **What:**
  ```rust
  if self.is_branch_tail_call(target_addr)? {
      bail!(
          "branch-indirect at {addr:?} could not be statically resolved"
      );
  }
  ```
- **Why:** Errors are bare strings.  No `anyhow!` builder, no `.context(...)`, no typed `kind`.  Future code that wants to recover from this specific failure mode (e.g. fall back to `UnresolvedIndirectBranch`) has to grep the string.

  Combined with F-001, this means the only way a higher layer can detect "Multiple resolved with OOB target" is by string match.  Anyhow `Error` provides `is::<T>` and `downcast_ref::<T>`, but neither is usable for bare `bail!("...")` produced errors.
- **Proposed change:** Add a small `enum CfgBuildError { … }` for the few typed errors the cfg crate produces and emit `anyhow::Error::new(CfgBuildError::SwitchTargetOutOfRange { addr, target })` when this case fires.  This is consistent with how other crates in the workspace handle "downstream code wants to react".
- **Confidence:** medium
- **Risk if applied:** low

### F-010 `resolve_indirect_target` swallows graph-construction-bug `find_unique_return` failures as `Ok(None)`

- **Category:** Correctness
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:248-261`
- **What:**
  ```rust
  let Some(return_node) = find_unique_return(&fg) else {
      return Ok(None);
  };
  let inputs = fg.graph.node_inputs(return_node);
  // Layout: [control, memory, value, ...ret_regs].  `build_return`
  // above passed `Some(value)` and `&[]`, so the value slot is at
  // index 2 and there are exactly 3 inputs.  A missing value input
  // signals a graph-construction bug in this module rather than a
  // runtime classification failure, but under the soft contract we
  // still surface it as "unresolved" — a later iteration won't
  // recover, so the strider-level loop will eventually error.
  let Some(&value_input) = inputs.get(2) else {
      return Ok(None);
  };
  ```
- **Why:** The comment explicitly states that a missing value input is a graph-construction bug *in this module*, but chooses to swallow it as `Ok(None)`.  The justification is "a later iteration won't recover, so the strider-level loop will eventually error" — but:
  - The strider-level fixed-point loop runs the resolver again, gets the same `Ok(None)`, and considers the branch unresolved.  Eventually it gives up and reports "unresolved indirect branch at <addr>" — which masks the original "graph-construction bug" diagnostic.
  - A real graph-construction bug can take the loop a lot longer to surface and gives the wrong error message, leading the dev down the wrong investigation path.
- **Proposed change:** Distinguish between "soft contract: producer doesn't classify" and "internal invariant violation: missing return input".  The latter should `bail!("internal: indirect_resolve emitted a Return with no value slot")` so the bug surfaces immediately.  Same for `find_unique_return` returning `None` due to multiple Returns (line 248).
- **Confidence:** high
- **Risk if applied:** low

### F-011 `is_branch_tail_call` synthesises addresses with `insn_index = 0`, so the "invalid tail call (insn_index != 0)" check is unreachable from `Multiple`/`Single` paths

- **Category:** Correctness/Simplification
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:393-396, 434-437, 452-455` and `216-228`
- **What:**
  Three call sites construct `target_addr` with `insn_index: 0`:
  ```rust
  let target_addr = PcodeInsnAddr {
      machine_addr: MachineInsnAddr { addr: target },
      insn_index: 0,
  };
  if self.is_branch_tail_call(target_addr)? { … }
  ```
  And the check itself:
  ```rust
  pub(super) fn is_branch_tail_call(&self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
      let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);
      if is_tail_call {
          if branch_target_addr.insn_index != 0 {
              bail!("invalid tail call at opcode {branch_target_addr:?}");
          }
      }
      Ok(is_tail_call)
  }
  ```
- **Why:** When called from the `Single`/`Multiple` resolver paths, the constructed `insn_index` is unconditionally `0`, so the inner `if branch_target_addr.insn_index != 0 { bail! }` is dead — it can only fire when called from `process_new_insn`'s `Branch` arm where `decode_branch_target` produces a real `insn_index`.  But the function is shared, so a future contributor reading the resolver paths could waste time understanding the `insn_index != 0` rejection.
- **Proposed change:** Either:
  - Inline `is_branch_tail_call_nocheck` in the resolver paths (they don't need the index check).
  - Or rename `is_branch_tail_call` to `is_branch_tail_call_validating_index` and document the `insn_index = 0` requirement at the call site for direct branches.
- **Confidence:** medium
- **Risk if applied:** low

### Dead code

### F-012 `predecessor_count` has zero callers anywhere in the workspace

- **Category:** Dead code
- **Location:** `crates/cfg/src/cfg/query.rs:85-94`
- **What:**
  ```rust
  pub fn predecessor_count(&self, region_id: RegionId) -> usize {
      self.graph
          .neighbors_directed(region_id, petgraph::Incoming)
          .count()
  }
  ```
  Workspace grep:
  ```
  $ grep -rn 'predecessor_count' --include='*.rs'
  crates/cfg/src/cfg/query.rs:90: …  // definition
  crates/cfg/src/cfg/query.rs:102: …  // self-link in another method's doc
  ```
- **Why:** Combined with F-004, the docstring's stated purpose (used by the deleted `RegionIrCache`) is now gone, and no caller remains.
- **Proposed change:** Delete the method.  If a future caller needs predecessor counts, `petgraph::Direction::Incoming` makes it a one-liner.
- **Confidence:** high
- **Risk if applied:** low

### F-013 `_insn_addr` parameter on `resolve_indirect_target` is a documented "kept for future" — currently dead arg

- **Category:** Dead code
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:114-121`
- **What:**
  ```rust
  pub(super) fn resolve_indirect_target<R: rsleigh::MemReader>(
      // …
      // R1.2: the address is no longer used as the payload of an
      // `UnresolvedIndirectBranch` error inside the resolver — the soft
      // contract returns `Ok(None)` for that path.  We retain the
      // argument so the call signature stays stable across the
      // softening (region_builder still passes it through unchanged) and
      // so a future strict-failure bypass — should one ever be needed —
      // has the address available without a signature change.
      _insn_addr: PcodeInsnAddr,
      endianness: target::Endianness,
  ) -> Result<Option<ResolvedTargets>> {
  ```
- **Why:** The `_insn_addr` parameter is dead — the function body uses neither the value nor any related context.  The forwarder `resolve_indirect_target_for_test` (line 389-407) faithfully threads it through.  YAGNI: keeping arguments around for "future strict-failure bypass" creates ambient noise.
- **Proposed change:** Delete the parameter from `resolve_indirect_target` and drop it from the test forwarder + the test file's call sites (they all just pass `br_addr()` — easy mechanical removal).  If a future strict-failure bypass needs the address, add it back at that time.
- **Confidence:** high
- **Risk if applied:** low

### F-014 `RegionGraph` type alias is `pub` but never re-exported through `lib.rs`; tests reach it via the `Cfg::graph` field

- **Category:** Dead code
- **Location:** `crates/cfg/src/cfg/types.rs:203`
- **What:**
  ```rust
  pub type RegionGraph = StableDiGraph<Region, RegionEdgeKind>;
  ```
  And in `lib.rs`:
  ```rust
  pub use cfg::{
      Builder, Cfg, IfRegionState, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, Region,
      RegionEdgeKind, RegionId, RegionInstruction, RegionTerminator, ResolvedTargets,
  };
  ```
  No `RegionGraph`.  External callers either name `&Cfg::graph` directly (e.g. `cfg.graph.edge_references()`) or use `petgraph::stable_graph::StableDiGraph<Region, RegionEdgeKind>` to construct a synthetic graph for tests.
- **Why:** The alias is technically "reachable" via the public `Cfg::graph` field (Rust's effective visibility rules), but a downstream user importing `cfg::*` does not get `RegionGraph` in scope.  This is mostly a cosmetic finding — the type alias is internally useful for the `Builder` struct's field type — but if it were `pub(crate)`, the `Builder` field type would still work and the public surface would shrink.
- **Proposed change:** Demote `RegionGraph` to `pub(crate)` (or `pub(super)`).  Add `RegionGraph` to `lib.rs::pub use` if external users actually want the alias.  Both options are acceptable; pick one.
- **Confidence:** medium
- **Risk if applied:** low

### F-015 `find_unique_return` walk-by-preorder helper unused outside `resolve_indirect_target`

- **Category:** Dead code (private helper, but only-one-caller indicator)
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:348-363`
- **What:**
  ```rust
  fn find_unique_return(fg: &ir::BuiltFunctionGraph) -> Option<ir::node::NodeId> {
      let mut found: Option<ir::node::NodeId> = None;
      for node_id in fg.preorder() {
          if matches!(fg.graph.node_kind(node_id), ir::node::NodeKind::Return) {
              if found.is_some() {
                  return None;
              }
              found = Some(node_id);
          }
      }
      found
  }
  ```
- **Why:** Single internal caller.  Could be inlined; alternatively, the comment block at lines 343-347 is longer than the function itself.
- **Proposed change:** Either inline at the single call site (line 248) or move the doc block down to make the function shorter than its preamble.
- **Confidence:** low
- **Risk if applied:** low

### F-016 `OptionsBuilder::Default` impl manually calls `Self::new`, which itself just returns `Options::default`

- **Category:** Dead code/Simplification
- **Location:** `crates/cfg/src/cfg/options.rs:109-122`
- **What:**
  ```rust
  impl Default for OptionsBuilder {
      fn default() -> Self {
          Self::new()
      }
  }

  impl OptionsBuilder {
      #[must_use]
      pub fn new() -> Self {
          OptionsBuilder {
              lifter_options: Options::default(),
          }
      }
  ```
- **Why:** `OptionsBuilder` has one field, defaulted via `Options::default()`.  `#[derive(Default)]` would replace 13 lines of code.  `Self::new()` is then just a documentation alias.
- **Proposed change:** Add `#[derive(Default)]` to the struct and either keep `new()` as a `#[doc(hidden)]` alias or remove it entirely.
- **Confidence:** medium
- **Risk if applied:** low

### Duplication & unification

### F-017 Repeated `PcodeInsnAddr { machine_addr: MachineInsnAddr { addr: target }, insn_index: 0 }` shape at 3 sites

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:393-396, 434-437, 452-455`
- **What:**
  ```rust
  let target_addr = PcodeInsnAddr {
      machine_addr: MachineInsnAddr { addr: target },
      insn_index: 0,
  };
  ```
  appears three times with identical shape (and a fourth time at line 41 inside `next_pcode_addr` where the shape is slightly different).
- **Why:** The pattern represents "the start of machine instruction at `target`".  A short helper `PcodeInsnAddr::at_machine_start(target: u64)` would deduplicate.  Could also be `From<u64> for PcodeInsnAddr`, mirroring the existing `From<u64> for MachineInsnAddr`.
- **Proposed change:** Add `impl PcodeInsnAddr { fn at_machine_start(addr: u64) -> Self { Self { machine_addr: MachineInsnAddr { addr }, insn_index: 0 } } }`.  Or `From<u64> for PcodeInsnAddr` since it's symmetric with the existing `MachineInsnAddr` impl.
- **Confidence:** high
- **Risk if applied:** low

### F-018 `vn_to_name_with_regs` re-validates `space == REGISTER` after the dispatch, then forwards to `vn_to_name_non_register` for non-registers — same dispatch is in `Cfg::vn_to_name`

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/dot.rs:16-66`
- **What:**
  ```rust
  pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
      match vn.addr.space {
          rsleigh::VnSpace::REGISTER => {
              let regs = self.sleigh.regs().map_err(anyhow::Error::from)?;
              vn_to_name_with_regs(&regs, vn)
          }
          _ => vn_to_name_non_register(vn),
      }
  }

  fn vn_to_name_with_regs(regs: &rsleigh::SleighRegs, vn: &rsleigh::Vn) -> Result<String> {
      if vn.addr.space == rsleigh::VnSpace::REGISTER {
          return Ok(regs.vn_to_name(*vn).ok_or_else(…)?.to_string());
      }
      vn_to_name_non_register(vn)
  }

  fn vn_to_name_non_register(vn: &rsleigh::Vn) -> Result<String> {
      // …
      match vn.addr.space {
          rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
          rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
          rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
          rsleigh::VnSpace::REGISTER => Err(anyhow!("invalid register vn {vn:?}")),
          s => Err(anyhow!("unsupported varnode space for display: {s:?}")),
      }
  }
  ```
- **Why:** Three layers, two separate REGISTER-space branches:
  - `Cfg::vn_to_name` dispatches REGISTER → with-regs, else → non-register.
  - `vn_to_name_with_regs` re-checks REGISTER (defensive), then forwards to non-register for *its* REGISTER-checks.
  - `vn_to_name_non_register` has its own REGISTER branch (returns `invalid register vn`) which is a "caller-routing bug" guard.
  
  The defensive REGISTER check in `vn_to_name_with_regs` is what makes the dot-dumper safe to call directly with any vn.  But the layering is muddy.  The `Cfg::vn_to_name` is a thin shim that exists only for the test forwarder (see F-040).
- **Proposed change:** Collapse to one function `vn_to_name(regs: Option<&SleighRegs>, vn: &Vn) -> Result<String>` that handles all spaces with explicit error messages.  Have `Cfg::vn_to_name` call it with `Some(regs)` only when space == REGISTER (or always with `Some`, since regs is always available).
- **Confidence:** medium
- **Risk if applied:** low

### F-019 Sort key `(space.shortcut_raw(), off, size)` is duplicated between `cfg/builder/indirect_resolve.rs:153` and `strider/src/strider/pipeline.rs:206`

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:149-153`
- **What:**
  ```rust
  // Determinism: sort by (space-shortcut, offset, size) like
  // strider's `find_all_unique_vns` so VarId numbering inside
  // FunctionBuilder is reproducible across runs (HashSet iteration
  // order would otherwise depend on the random hasher seed).
  all_vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));
  ```
  And in strider:
  ```rust
  vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));
  ```
- **Why:** Cross-crate duplication with explicit "must match strider's `find_all_unique_vns`" comment.  Cross-crate findings are out of scope, but flagged here so a future cleanup can hoist the sort key into `rsleigh::Vn` (e.g., `impl Ord for Vn`).
- **Proposed change:** Out of scope (cross-crate), but note the duplication.  An in-cfg-only mitigation: extract a private `fn sort_vns_for_determinism(vns: &mut Vec<Vn>)`.
- **Confidence:** medium
- **Risk if applied:** low

### F-020 `resolve_const_loads` re-implements `opt::LoadReadOnly::optimize` line-for-line because of an `M: 'static` bound; the bound is sidestepped via inlining

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:287-330`
- **What:**
  ```rust
  fn resolve_const_loads(
      fg: &mut ir::BuiltFunctionGraph,
      rom: &dyn ReadOnlyMemory,
  ) -> Result<()> {
      let nodes: Vec<_> = fg.preorder().collect();
      for node_id in nodes {
          let kind = *fg.graph.node_kind(node_id);
          let ir::node::NodeKind::Load(space) = kind else {
              continue;
          };
          // … 25 more lines …
      }
      Ok(())
  }
  ```
  Comment: "Mirrors the LoadReadOnly impl line-for-line; kept in sync via `crates/opt/src/load_readonly/mod.rs`'s test suite (the optimizer-side tests would catch any divergence in shared behaviour)."
- **Why:** The reason given for inlining is the `M: 'static` bound on `LoadReadOnly<M>`'s `Optimizer` impl.  But that's a function of how the pipeline stores boxed passes — not an inherent restriction on `LoadReadOnly`'s logic.  The inlined helper is a 30-line reimplementation of an already-tested optimizer pass.  Three risks:
  - Behaviour divergence is silent (the comment promises `opt`'s tests will catch it, but `cfg`'s mini-graph case isn't in `opt`'s test scope).
  - When `opt::LoadReadOnly` adds a feature (e.g. mask handling), the inlined copy doesn't auto-pickup.
  - `resolve_const_loads` doesn't re-run after each fold round: it does one pass, then re-runs the core fold pipeline, but if folding produces a NEW constant-address Load, that load isn't picked up.
- **Proposed change:** Either (a) lift the `'static` bound from `LoadReadOnly` (sidestep: store passes as `Box<dyn Optimizer + 'borrow>` with PhantomData), or (b) accept the duplication but call it `inline_load_readonly_for_borrowed_rom` and document that it must stay in lockstep.
- **Confidence:** high
- **Risk if applied:** medium (touches the `opt` pipeline contract).

### F-021 `Cfg::vn_to_name` and `cfg::dot::vn_to_name_*` overlap in dispatch, but `Cfg::vn_to_name` is only ever called via the `test_api` forwarder

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/dot.rs:16-24, 78-83`, `crates/cfg/src/lib.rs:21`
- **What:**
  ```rust
  // dot.rs
  impl<R: rsleigh::MemReader> Cfg<R> {
      pub(super) fn vn_to_name(&self, vn: &rsleigh::Vn) -> Result<String> {
          match vn.addr.space {
              rsleigh::VnSpace::REGISTER => {
                  let regs = self.sleigh.regs().map_err(anyhow::Error::from)?;
                  vn_to_name_with_regs(&regs, vn)
              }
              _ => vn_to_name_non_register(vn),
          }
      }
      // …
  }

  pub mod test_api {
      pub fn vn_to_name<R: rsleigh::MemReader>(
          cfg: &Cfg<R>,
          vn: &rsleigh::Vn,
      ) -> Result<String> {
          cfg.vn_to_name(vn)
      }
  }
  ```
  And in `lib.rs`:
  ```rust
  #[doc(hidden)]
  pub mod test_api;
  ```
  Workspace grep shows only the test forwarder uses `Cfg::vn_to_name`; the actual DOT dumper bypasses `Cfg::vn_to_name` and calls `vn_to_name_with_regs` directly.
- **Why:** `Cfg::vn_to_name` exists solely so the test_api forwarder can re-export it.  But `vn_to_name_with_regs` and `vn_to_name_non_register` are crate-private helpers that already do the work.  The shim adds a layer of indirection without value.
- **Proposed change:** Delete `Cfg::vn_to_name`.  Have the test_api forwarder dispatch directly to `vn_to_name_with_regs(regs, vn)` (with `regs` looked up internally).  This also reveals that `Cfg::vn_to_name` is the only `pub(super)` method in `Cfg`, which would let the rest of the impl block in `dot.rs` be plain free functions.
- **Confidence:** medium
- **Risk if applied:** low

### F-022 The two `OptimizerPipeline` constructions in `resolve_indirect_target` (with-rom and without-rom) duplicate the `add(ConstantFold) + add(KnownBits) + add(RedundantPhis)` shape

- **Category:** Duplication & unification
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:205-230`
- **What:**
  ```rust
  {
      let mut pipeline = opt::OptimizerPipeline::new();
      pipeline.add(opt::ConstantFold);
      pipeline.add(opt::KnownBits);
      pipeline.add(opt::RedundantPhis);
      pipeline.run_on_built(&mut fg)?;
  }

  if let Some(rom) = rom {
      resolve_const_loads(&mut fg, rom)?;
      let mut pipeline = opt::OptimizerPipeline::new();
      pipeline.add(opt::ConstantFold);
      pipeline.add(opt::KnownBits);
      pipeline.add(opt::RedundantPhis);
      pipeline.run_on_built(&mut fg)?;
  }
  ```
  Identical 4-line pipeline construction back-to-back.
- **Why:** Both pipelines are byte-for-byte identical; only the surrounding `resolve_const_loads` step differs.  Could factor as `let make_pipeline = || { let mut p = opt::OptimizerPipeline::new(); p.add(opt::ConstantFold); p.add(opt::KnownBits); p.add(opt::RedundantPhis); p };`.

  Better: lift to a top-level constant or pull out into a `fn build_resolver_pipeline()`.
- **Proposed change:** Add a small helper `fn make_resolver_pipeline() -> opt::OptimizerPipeline` and call it twice.
- **Confidence:** high
- **Risk if applied:** low

### F-023 Three test-builder helpers (`make_builder`, `make_builder_opts`, `make_builder_with_bytes`) only differ in two scalar args — could collapse via a builder pattern

- **Category:** Duplication & unification
- **Location:** `crates/cfg/tests/common/synthetic.rs:48-65`
- **What:**
  ```rust
  pub fn make_builder(start_addr: u64) -> Builder<TestReader> {
      Builder::new(make_sleigh(), start_addr, OptionsBuilder::new().build())
  }
  pub fn make_builder_opts(start_addr: u64, options: Options) -> Builder<TestReader> {
      Builder::new(make_sleigh(), start_addr, options)
  }
  pub fn make_builder_with_bytes(bytes: Vec<u8>, start_addr: u64) -> Builder<TestReader> {
      Builder::new(
          make_sleigh_with_bytes(bytes, start_addr),
          start_addr,
          OptionsBuilder::new().build(),
      )
  }
  ```
- **Why:** Three test helpers, each a thin Builder constructor with one variation.  Tests pick whichever fits.  A test-side `TestBuilderConfig` builder would unify but isn't strictly needed — these are short.  The duplication is mild.
- **Proposed change:** Optional.  If a fourth variant lands ("with options *and* bytes"), unify before adding it.
- **Confidence:** low
- **Risk if applied:** low

### Simplification

### F-024 `Builder` field `sleigh` is `pub(super)` but the `pub` `Cfg::sleigh` is the user-facing harvest point — readers have to chase two layers

- **Category:** Simplification
- **Location:** `crates/cfg/src/cfg/builder/mod.rs:52` + `crates/cfg/src/cfg/mod.rs:58`
- **What:**
  ```rust
  // builder/mod.rs
  pub struct Builder<R: rsleigh::MemReader> {
      pub(super) sleigh: rsleigh::Sleigh<R>,
      // …
  }

  // mod.rs
  pub struct Cfg<R: rsleigh::MemReader> {
      // …
      pub sleigh: rsleigh::Sleigh<R>,
      // …
  }
  ```
- **Why:** `Builder::sleigh` is `pub(super)` (mod-private, but exposed to test_api).  `Cfg::sleigh` is fully `pub`.  Both fields hold the same `rsleigh::Sleigh<R>` value (the builder's gets moved into the Cfg at `build()`).  The asymmetry is a minor friction: tests reading `Builder::sleigh` go through `test_api::sleigh()` while users reading `Cfg::sleigh` access the field directly.  Not a real problem, just reflects "tests reach inside the Builder; production sees only the Cfg".
- **Proposed change:** Cosmetic — make both fully `pub` or both `pub(crate)`.  Current state is fine.
- **Confidence:** low
- **Risk if applied:** low

### F-025 `decode_branch_target`'s sign-extension table reads as a `match` with three arms that reduce to "size in {1,2,4} → narrow signed" + "default → cast_signed"

- **Category:** Simplification
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:130-135`
- **What:**
  ```rust
  let off: i64 = match branch_target_var.size {
      1 => (raw as i8) as i64,
      2 => (raw as i16) as i64,
      4 => (raw as i32) as i64,
      _ => raw.cast_signed(),
  };
  ```
- **Why:** The `match` is fine but a `if size == 8 || size > 4 { raw.cast_signed() } else { sign_ext_lower(raw, size) }` would be more idiomatic.  Or a small helper `fn sign_extend_const_off(raw: u64, size: u32) -> i64`.

  Note: the catch-all `_` arm fires for size in {0, 3, 5, 6, 7} or size > 8.  Sleigh's CONST varnodes typically have size ∈ {1, 2, 4, 8}, so the catch-all is mostly the size=8 case.  But a malformed varnode with size=3 would silently take the cast_signed path, returning the unmasked u64 as i64 — likely wrong.
- **Proposed change:** Add an explicit `8 => raw.cast_signed()` arm and bail on the `_` arm: `_ => bail!("unsupported branch-target varnode size {size}")`.
- **Confidence:** medium
- **Risk if applied:** low

### F-026 `is_branch_tail_call_nocheck`'s `end_exclusive <= addr.addr` comparison reads as `end_exclusive` semantically, but is constructed via `saturating_add` which means `start + max_size`, not `end_exclusive`

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:201-204`
- **What:**
  ```rust
  let end_exclusive = self.builder.start_addr.addr.saturating_add(fn_max_size);
  if end_exclusive <= addr.addr {
      return true;
  }
  ```
- **Why:** The variable is named `end_exclusive` but the comparison `end_exclusive <= addr.addr` says "address >= end_exclusive is tail-call".  That's correct (saturating_add gives `start + max_size`, which IS the exclusive end of the function range).  But the naming + comparison is slightly off-key: a "less than or equal" against a name suggesting "exclusive boundary" reads odd.
- **Proposed change:** Either name it `end_inclusive_threshold` (the smallest address that triggers tail-call) or use the strict comparison: `if addr.addr >= end_exclusive { return true; }`.
- **Confidence:** low
- **Risk if applied:** low

### F-027 `Builder::build`'s `cfg failed accessing starting region` error is opaque and unrelated to the actual failure mode

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/mod.rs:217-220`
- **What:**
  ```rust
  let (starting_region, _) = self
      .find_region_containing_addr(self.start_pcode_addr())
      .ok_or_else(|| anyhow!("cfg failed accessing starting region"))?;
  ```
- **Why:** The error fires when, after the work-queue has drained, the function entry address is not in any region.  This can happen if:
  - The function never decoded any instructions (entry address was outside the readable range).
  - A bug in the work-queue caused the entry to be dropped.
  
  "cfg failed accessing starting region" is opaque.  A user sees this and has to read the code to understand.  Better: `"cfg build did not produce a region containing the entry address {addr}"` with the actual address.
- **Proposed change:** Include the entry address in the error and clarify what "accessing" means.  E.g. `"cfg build completed but no region contains the entry address {addr:?}; check that the entry is decodable"`.
- **Confidence:** medium
- **Risk if applied:** low

### F-028 `failed spliting region` is a typo (should be "splitting") and the error string does not name the kind of failure

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/split.rs:43`
- **What:**
  ```rust
  let split_index = second_region
      .insns
      .iter()
      .position(|insn| insn.addr == addr)
      .ok_or_else(|| anyhow!("failed spliting region {region_id:?} into 2 parts at {addr:?}"))?;
  ```
- **Why:** "spliting" is a typo for "splitting".  Also, the message could be more specific: "address {addr:?} not found in region {region_id:?}'s instruction list".
- **Proposed change:** `anyhow!("split address {addr:?} not found in region {region_id:?}'s instruction list")`.
- **Confidence:** high
- **Risk if applied:** low

### F-029 `Builder::with_known_targets` mutates `options.known_targets` in place; calling it twice silently replaces the prior set with no warning

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/mod.rs:194-201`
- **What:**
  ```rust
  /// Replaces any previous `known_targets` set on this builder.
  /// Pass an empty map to clear.
  #[must_use]
  pub fn with_known_targets(
      mut self,
      known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>,
  ) -> Self {
      self.options.known_targets = known_targets;
      self
  }
  ```
- **Why:** Replace-on-call is a fine semantic; the docstring documents it.  But "Replaces any previous" is buried in the docstring and the orchestrator may call this twice (once to populate from cache, once to update with fresh tier-2 results).  A helpful API would expose `extend_known_targets` for additive updates.
- **Proposed change:** Add a sibling `extend_known_targets(mut self, more: HashMap<…>) -> Self` method.  Optional; current API is sufficient.
- **Confidence:** low
- **Risk if applied:** low

### Readability

### F-030 `RegionTerminator::Switch::target_value` is always `None` from cfg builder + no other constructor exists, but the field is `pub` and 99% of comments imply the orchestrator populates it

- **Category:** Readability/Dead code
- **Location:** `crates/cfg/src/cfg/types.rs:114-138`
- **What:**
  ```rust
  /// `target_value` is an OPTIONAL pinned `NodeOutputId` for the
  /// dispatch value (W9).  When `Some`, strider's `handle_switch`
  /// uses this `NodeOutputId` directly instead of re-reading
  /// `target_vn` — pinning the soundness contract that the
  /// comparison value is the SAME value tier 2 classified.  The
  /// cfg builder always sets this to `None`; it is plumbing for
  /// the orchestrator's known-targets feedback path so a future
  /// incremental rebuild round (which preserves the previous
  /// iteration's IR across rebuilds) can wire the cached anchor
  /// directly through.
  Switch {
      target_vn: rsleigh::Vn,
      targets: Vec<u64>,
      target_value: Option<ir::Value>,
  },
  ```
- **Why:** The cfg builder always sets `target_value: None` (lines 446-449 of region_builder.rs).  The strider orchestrator currently *also* never produces `Some(_)` — workspace grep:
  ```
  $ grep -rn 'target_value' --include='*.rs'
  crates/cfg/src/cfg/types.rs:115: …  // doc
  crates/cfg/src/cfg/types.rs:132: …  // field
  crates/cfg/src/cfg/types.rs:137: …  // field
  crates/cfg/src/cfg/builder/region_builder.rs:448: target_value: None,
  crates/cfg/tests/region_terminator.rs: …  // tests
  crates/strider/src/strider/insn/control.rs:155: target_value: Option<ir::Value>,
  crates/strider/src/strider/insn/control.rs:181: let idx = match target_value {
  ```
  So `handle_switch` is wired to consume `Some(_)` but no production path produces it.
- **Proposed change:** Either:
  - Delete the field until the producer-side wiring lands (YAGNI).
  - Or document that the field is "future-use" and add a `#[allow(dead_code)]` / a TODO marker.
- **Confidence:** medium
- **Risk if applied:** medium (changes a `pub` enum variant shape)

### F-031 `resolve_indirect_target`'s 4-stage step-comment block (Step 1 … Step 6) is 100+ lines but the actual function body only spans 75 lines

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:108-285`
- **What:**
  ```rust
  pub(super) fn resolve_indirect_target<R: rsleigh::MemReader>(
      // …
  ) -> Result<Option<ResolvedTargets>> {
      // ── Step 1: collect every varnode the region touches so the IR
      //    builder can pre-declare them.  Includes target_vn so we can
      //    always read its value, even on regions that don't otherwise
      //    write through it (e.g. `bx lr` after no prior writes to lr).
      //    Includes cc_link_register_vn for the same reason: the LR-target
      //    classification needs a tracked InitialVar(lr) to show up.
      // … 25 lines …
      // ── Step 2: stand up a minimal FunctionBuilder.  No calling
      //    convention plumbing — `new_raw` with empty arg/callee/ret slices,
      //    no stack pointer, ret_stack_pop=0.  The mini-graph never emits
      //    Call or Store nodes, so the convention is irrelevant.
      // … 7 lines …
      // ── Step 3: lift every value-producing insn.  Stop at the first
      //    `Ok(false)` — that is the BranchIndirect (or any other
      //    control-flow / call / store opcode the lifter rejects).
      // … 9 lines …
      // ── Step 4: read target_vn's current value into a NodeOutputId and
      //    emit a Return so the value is reachable from the function entry.
      //    `read_vn` uses pcode-lift's register-aliasing logic, so a
      //    sub-register target (`jmp *eax` on x86_64) folds correctly via
      //    KnownBits even though we tracked `rax`.
      // … 7 lines …
      // ── Step 5: build the graph and run the resolver pipeline.
      // … 30 lines …
      // ── Step 6: classify.
      // … 35 lines …
  }
  ```
- **Why:** The step-comments are documentation, not narration.  Each "step" is implemented in 5-10 lines of code preceded by 5-10 lines of comment.  The comments are accurate but their density obscures the linear flow.  Splitting into private helper functions (`build_minigraph`, `lift_region`, `run_minigraph_pipeline`, `classify_minigraph_target`) would let each helper carry its own short doc and the entry point would be a 20-line read-and-write linear flow.
- **Proposed change:** Extract per-step helper functions.  This also addresses F-022 (the duplicate pipeline construction) by hoisting it into one helper.
- **Confidence:** medium
- **Risk if applied:** low

### F-032 `Options` `PartialEq` impl is implemented manually for ROM Arc-ptr-eq but not actually used anywhere in production code, only in two tests

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/options.rs:78-90`
- **What:**
  ```rust
  impl PartialEq for Options {
      fn eq(&self, other: &Self) -> bool {
          self.fn_max_size == other.fn_max_size
              && self.allow_code_before_start_addr == other.allow_code_before_start_addr
              && self.link_register_vn == other.link_register_vn
              && self.known_targets == other.known_targets
              && match (&self.read_only_memory, &other.read_only_memory) {
                  (None, None) => true,
                  (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                  _ => false,
              }
      }
  }
  ```
- **Why:** The `PartialEq` impl is invoked only in `tests/options.rs` (4 tests).  `Arc::ptr_eq` is a strong-but-not-value-equality.  The behaviour pin is fine, but the impl exists solely to support `assert_ne!` / `assert_eq!` in `tests/options.rs`.

  In production, `Options` is constructed from an `OptionsBuilder` and consumed by a `Builder`; equality is never checked.  This is technically dead code in the sense that no production caller relies on it — but it's small and harmless.
- **Proposed change:** Optional.  Could move the impl + comment into `tests/options.rs` (cfg-test only), but that requires `#[cfg(test)]` gating.  Current state is acceptable.
- **Confidence:** low
- **Risk if applied:** low

### F-033 `RegionTerminator` doc comments for `Branch` / `Return` / `Fallthrough` paragraph-mismatched: `Fallthrough`'s comment talks about "first half of a split region" while `Branch` does not mention the split-second-half pairing

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/types.rs:79-94`
- **What:**
  ```rust
  /// No terminator opcode; control falls into the next region.  This
  /// covers the case where decoding hits the start of an
  /// already-discovered region and the current region is closed out
  /// with a [`RegionEdgeKind::Fallthrough`] edge, as well as the
  /// first half of a split region.
  Fallthrough,
  /// Direct unconditional branch, intra-function.  Successor lives on
  /// the [`RegionEdgeKind::Branch`] edge.
  Branch,
  ```
- **Why:** `Fallthrough` mentions "first half of a split region" but `Branch` does not document its symmetric role: the second half of a split region inherits the original region's terminator.  Asymmetric documentation; not a bug but reader-hostile.
- **Proposed change:** Add a sentence to `Branch` / `CondBranch` / `Return` etc.: "When a region is split, the *second* half retains the original terminator (the first half always becomes `Fallthrough`)."
- **Confidence:** medium
- **Risk if applied:** low

### F-034 `RegionBuilder::process_new_insn`'s 230-line dispatch is a long `match` over `insn.opcode` mixed with helper-style code blocks; could be split per-op

- **Category:** Readability
- **Location:** `crates/cfg/src/cfg/builder/region_builder.rs:237-466`
- **What:**
  A 230-line `fn process_new_insn` whose body is one big `match insn.opcode { Branch => { 50 lines }, CondBranch => { 20 lines }, Return => { 4 lines }, BranchIndirect => { 140 lines }, _ => { 1 line } }`.  The `BranchIndirect` arm dominates and contains 100+ lines of comments.
- **Why:** The dispatch is structured but vertically dense.  Per-arm helper methods (`process_branch`, `process_cond_branch`, `process_return`, `process_branch_indirect`) would make `process_new_insn` an 8-line dispatcher and let each helper carry its own focused doc.  The `BranchIndirect` arm in particular would benefit since its comment-to-code ratio is high.
- **Proposed change:** Extract one helper per opcode arm.
- **Confidence:** high
- **Risk if applied:** low

### F-035 `resolve_indirect_target` rebuilds the `OptimizerPipeline` from scratch for every `BranchIndirect` site in the region

- **Category:** Performance
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:205-230`
- **What:**
  ```rust
  // TODO(perf): the pipeline is rebuilt per resolver invocation.  The
  // user explicitly deferred caching (plan Q4) until measured — most
  // binaries have only a handful of indirect branches, so the
  // construction cost is in the noise.  Revisit if profiling shows
  // otherwise.
  let mut fg = builder.build()?;

  {
      let mut pipeline = opt::OptimizerPipeline::new();
      pipeline.add(opt::ConstantFold);
      pipeline.add(opt::KnownBits);
      pipeline.add(opt::RedundantPhis);
      pipeline.run_on_built(&mut fg)?;
  }
  ```
- **Why:** The TODO acknowledges this and defers measurement.  Note for the record: a binary with 100+ jump tables / virtual-function dispatches would re-construct the pipeline 100+ times.  The pipeline construction is `Box::new(opt) × 3` and `Vec::push × 3`, so each invocation is ~6 small allocations.  Likely O(microseconds) per call, dwarfed by the actual fold work.
- **Proposed change:** Defer until measured.  A pre-built pipeline could be passed in via `Strider`'s state (it already owns one for the IR-level work) — but that requires plumbing through cfg.
- **Confidence:** low
- **Risk if applied:** medium (cross-crate plumbing).

### F-036 `find_region_containing_addr` does a `BTreeMap::range(..=addr).next_back()` lookup, which is O(log n) per call

- **Category:** Performance
- **Location:** `crates/cfg/src/cfg/builder/mod.rs:128-138`
- **What:**
  ```rust
  pub(super) fn find_region_containing_addr(&self, addr: PcodeInsnAddr) -> Option<(NodeIndex, &Region)> {
      // Find the last region whose start_addr <= addr
      let (_, &region_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;

      let region = self.graph.node_weight(region_id)?;
      if region.contains_addr(addr) {
          Some((region_id, region))
      } else {
          None
      }
  }
  ```
- **Why:** `BTreeMap::range(..=addr).next_back()` is O(log n) plus the cost of `next_back` (constant after the range is set up, since BTreeMap::range returns a typed cursor).  This is fine.  No actual perf problem.

  This is just a note that the code is correct and well-suited to the data structure.
- **Proposed change:** None.  Flagged for the record; the implementation is good.
- **Confidence:** high
- **Risk if applied:** low

### F-037 `find_unique_return`'s `for node_id in fg.preorder()` iterates the entire reachable graph just to find the unique Return — a `match_first` with early break on the second hit would be O(reachable) but with no early-out

- **Category:** Performance
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:348-363`
- **What:**
  ```rust
  fn find_unique_return(fg: &ir::BuiltFunctionGraph) -> Option<ir::node::NodeId> {
      let mut found: Option<ir::node::NodeId> = None;
      for node_id in fg.preorder() {
          if matches!(fg.graph.node_kind(node_id), ir::node::NodeKind::Return) {
              if found.is_some() {
                  return None;
              }
              found = Some(node_id);
          }
      }
      found
  }
  ```
- **Why:** Walks the full reachable graph to confirm uniqueness.  Could break after finding two Returns (already does) but doesn't break after finding *one* — it has to confirm uniqueness.  In a mini-graph with one Return (the resolver always emits exactly one), this is mostly the cost of `fg.preorder()` materializing.

  For real-world performance, the mini-graph is small (single basic block, no calls, no stores) so this is irrelevant.  Flagged for the record.
- **Proposed change:** None — the function is correct and fast enough.
- **Confidence:** low
- **Risk if applied:** low

### F-038 `region_fallthrough` is `pub` and 1 external caller (`strider`) gates `is_some()` on it; could expose `has_fallthrough_successor(_) -> bool` directly

- **Category:** Simplification
- **Location:** `crates/cfg/src/cfg/query.rs:59-61`, `crates/strider/src/strider/insn/control.rs:126`
- **What:**
  ```rust
  // cfg
  pub fn region_fallthrough(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
      self.unique_outgoing(region_id, RegionEdgeKind::Fallthrough)
  }

  // strider
  if self.cfg.region_fallthrough(region_id)?.is_some() {
      return Ok(());
  }
  ```
- **Why:** The strider caller doesn't actually need the successor `NodeIndex`; it only checks for presence.  But the duplicate-edge-detection (the `Result`-bearing path) is precisely what `region_fallthrough` does.  A `has_fallthrough_successor(...) -> Result<bool>` would be a cleaner API.
- **Proposed change:** Optional — add `has_fallthrough_successor`, retain `region_fallthrough` for users that want the successor index.  Or leave as-is and accept the `is_some()` idiom.
- **Confidence:** low
- **Risk if applied:** low

### F-039 `RegionTerminator::Switch` matches against `target_vn` to identify dispatch but the `target_vn` is the same as the `BranchIndirect`'s `inputs[0]` — strider must re-derive the same VN; could store it once

- **Category:** Readability/Simplification
- **Location:** `crates/cfg/src/cfg/types.rs:127-138`
- **What:**
  ```rust
  Switch {
      target_vn: rsleigh::Vn,
      targets: Vec<u64>,
      target_value: Option<ir::Value>,
  },
  ```
- **Why:** `target_vn` is the dispatch varnode.  Strider uses it to call `read_vn` to get the comparison value.  This information is also derivable from the original `BranchIndirect` insn via `region.insns.last().unwrap().insn.inputs[0]` — but storing it in the terminator avoids the lookup.

  Comment at line 137 says "When `Some`, strider uses it directly *instead of* re-reading `target_vn`" — emphasis: the orchestrator's known-targets feedback path may pin a `NodeOutputId` for soundness, but the storage is otherwise a hint.

  This is mostly a readability note: a future contributor wondering "why is `target_vn` stored if it's just the BranchIndirect's input?" gets the answer from the comment, but the storage is convenient (avoids the `region.insns.last().unwrap()` chain).
- **Proposed change:** None.  Flagged for clarity.
- **Confidence:** low
- **Risk if applied:** low

### F-040 `cfg::dot::test_api::vn_to_name` is the only call site for `Cfg::vn_to_name`; the latter is otherwise pure-internal

- **Category:** Simplification
- **Location:** `crates/cfg/src/cfg/dot.rs:16-24, 78-83`, `crates/cfg/src/lib.rs:21`
- **What:**
  See F-021.
- **Why:** Combined with F-021, this means the public surface includes a method (`Cfg::vn_to_name`) that exists only so the test_api forwarder can re-export it.
- **Proposed change:** See F-021 (delete `Cfg::vn_to_name`, dispatch directly from the test forwarder).
- **Confidence:** medium
- **Risk if applied:** low

### F-041 Test `region_terminator.rs` block-comment "Phase-5 update" / "R1.1 pin" still references in-flight phase numbers

- **Category:** Readability
- **Location:** `crates/cfg/tests/region_terminator.rs:202-292`
- **What:**
  ```rust
  // Phase-5 update: the legacy `BranchIndirect -> Return` mapping is
  // gone.  The Phase-5 resolver classifies the target and produces
  // `Branch` / `TailCall` / `Return` based on the resolved value, or
  // errors with `UnresolvedIndirectBranch` when the target can't be
  // proven.  All of those paths are covered in `indirect_dispatch.rs`,
  // …

  // R1.1: pin the new `UnresolvedIndirectBranch` variant.  Tier 1 will
  // fall through to this terminator when the cfg-time mini-graph
  // resolver cannot prove a `BranchIndirect`'s target.
  ```
- **Why:** "Phase-5", "R1.1", "W9", "F7", "BUG-25" are internal codenames used during a feature roadmap.  The work is now landed.  Comments referencing in-flight code names age poorly: a fresh reader has no way to look these up.  This is a workspace-wide pattern noted in the strider review (F-037 there).
- **Proposed change:** Strip codename references.  Keep the *behaviour* descriptions, drop the "Phase-5", "R1.1" prefixes.
- **Confidence:** high
- **Risk if applied:** low

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| `crates/cfg/Cargo.toml` | reviewed | Clean, no findings |
| `crates/cfg/src/lib.rs` | reviewed | Clean |
| `crates/cfg/src/error.rs` | reviewed | 3 lines — Result alias only |
| `crates/cfg/src/test_api.rs` | reviewed | Forwarder module |
| `crates/cfg/src/cfg/mod.rs` | reviewed | F-005 (stale RegionIrCache) |
| `crates/cfg/src/cfg/types.rs` | reviewed | F-030 (Switch::target_value), F-033 (terminator doc asymmetry), several stale codename refs |
| `crates/cfg/src/cfg/options.rs` | reviewed | F-016 (Default impl), F-032 (PartialEq impl) |
| `crates/cfg/src/cfg/query.rs` | reviewed | F-004, F-007, F-012, F-038 |
| `crates/cfg/src/cfg/dot.rs` | reviewed | F-018, F-021, F-040 |
| `crates/cfg/src/cfg/builder/mod.rs` | reviewed | F-006, F-027, F-029 |
| `crates/cfg/src/cfg/builder/split.rs` | reviewed | F-028 |
| `crates/cfg/src/cfg/builder/region_builder.rs` | reviewed | F-001, F-002, F-008, F-009, F-011, F-017, F-025, F-034 |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs` | reviewed | F-010, F-013, F-015, F-019, F-020, F-022, F-031, F-035, F-037 |
| `crates/cfg/examples/cfg_creator.rs` | reviewed | Demo example, clean |
| `crates/cfg/tests/common/mod.rs` | reviewed | Test-helper aggregator |
| `crates/cfg/tests/common/synthetic.rs` | reviewed | F-023 |
| `crates/cfg/tests/common/assertions.rs` | reviewed | Clean |
| `crates/cfg/tests/common/real_binary.rs` | reviewed | Clean |
| `crates/cfg/tests/addr_types.rs` | reviewed | Ordering tests, clean |
| `crates/cfg/tests/build_end_to_end.rs` | reviewed | Synthetic-bytes E2E |
| `crates/cfg/tests/builder_add_region.rs` | reviewed | Clean |
| `crates/cfg/tests/builder_find_region.rs` | reviewed | Clean |
| `crates/cfg/tests/builder_split_region.rs` | reviewed | Clean |
| `crates/cfg/tests/cfg_integration.rs` | reviewed | 14-arch macro generator |
| `crates/cfg/tests/cfg_query.rs` | reviewed | Tests for region_branch / region_if / region_id_at_start |
| `crates/cfg/tests/dot_dumper.rs` | reviewed | Clean smoke tests |
| `crates/cfg/tests/indirect_dispatch.rs` | reviewed | Phase-5 dispatch tests, has codename refs |
| `crates/cfg/tests/indirect_resolve.rs` | reviewed | Mini-graph resolver tests |
| `crates/cfg/tests/known_targets.rs` | reviewed | with_known_targets feedback path |
| `crates/cfg/tests/options.rs` | reviewed | Clean |
| `crates/cfg/tests/region_builder_decode.rs` | reviewed | decode_branch_target paths |
| `crates/cfg/tests/region_builder_process.rs` | reviewed | process_new_insn / process_insn |
| `crates/cfg/tests/region_builder_tail_call.rs` | reviewed | is_branch_tail_call paths |
| `crates/cfg/tests/region_edge_kind.rs` | reviewed | 1 trivial test |
| `crates/cfg/tests/region.rs` | reviewed | contains_addr |
| `crates/cfg/tests/region_terminator.rs` | reviewed | F-041 |
| `crates/cfg/tests/sleigh_reuse.rs` | reviewed | Sleigh-harvest pin |
| `crates/cfg/tests/vn_to_name.rs` | reviewed | Clean |

## Out-of-scope items observed

- **Cross-crate `vn_to_name`** duplication between `cfg::dot` and `ir::dot::label`: both implement near-identical varnode-to-string formatting with slightly different error paths.  Worth hoisting into rsleigh or a shared dot helper module.  Not flagged in cfg findings.
- **Cross-crate `find_all_unique_vns` sort key** (F-019): same `(space.shortcut_raw(), off, size)` tuple used in cfg's mini-graph builder and strider's main lifter.  An `impl Ord for rsleigh::Vn` would let both call sites use the standard sort.
- **`opt::LoadReadOnly::optimize` line-for-line duplication** (F-020): the `M: 'static` bound on the pass forces cfg to inline a copy.  A redesign to allow borrowed ROMs would let cfg dogfood the pass directly.

## Stopped here marker

Review complete.  Every `.rs` file under `crates/cfg/` was read front to back.

## Outcomes (review/cfg-crate-r6)

Commits in apply order:

- `0e3a3be` — Bucket 1: anyhow / restructure drift
- `324bf4b` — Bucket 2: correctness (Multiple OOB defer, decode-branch-target docs, find_unique_return propagation, helper extraction)
- `afa8a84` — Bucket 3: dead code (`_insn_addr` parameter, RegionGraph visibility, OptionsBuilder Default, vn_to_name dispatch)
- `ec8a34f` — Bucket 4: duplication (resolver pipeline helper, Arc/Box ReadOnlyMemory blanket impls, sort-key cross-ref)
- `0debd4c` — Bucket 5: simplification + readability + codename strip

| Finding | Outcome | Commit | Notes |
| --- | --- | --- | --- |
| F-001 | Applied | 324bf4b | OOB Multiple defers via UnresolvedIndirectBranch; regression test added |
| F-002 | Applied | 324bf4b | Documented why size is ignored on the absolute-branch arm |
| F-003 | Applied | 0e3a3be | 14 ErrorKind:: docstrings rewritten in plain prose |
| F-004 | Applied | 0e3a3be | predecessor_count + its docstring deleted |
| F-005 | Applied | 0e3a3be | Cfg::sleigh docstring rewritten without RegionIrCache / W11 |
| F-006 | Applied | 0e3a3be | Stray `Self::set_endianness` reference removed |
| F-007 | Applied | 0e3a3be | region_fallthrough docstring rephrased generic-first |
| F-008 | Applied | 324bf4b | Multiple-arm comment clarified as tier-2-only feedback shape |
| F-009 | Obviated | 324bf4b | bail! call site removed by F-001's redesign |
| F-010 | Applied | 324bf4b | find_unique_return now propagates real bugs as errors |
| F-011 | Applied | 324bf4b | Resolver paths use is_branch_tail_call_nocheck (insn_index always 0) |
| F-012 | Applied | 0e3a3be | Folded into F-004's deletion |
| F-013 | Applied | afa8a84 | Dead `_insn_addr` parameter removed from resolver, test forwarder, callers |
| F-014 | Applied | afa8a84 | RegionGraph demoted to pub(crate) |
| F-015 | Skipped | — | Rejected: function carries useful documentation; one-caller is fine |
| F-016 | Applied | afa8a84 | derive(Default) on OptionsBuilder, new() inlined |
| F-017 | Applied | 324bf4b | PcodeInsnAddr::at_machine_start helper added in types.rs |
| F-018 | Applied | afa8a84 | Collapsed into one vn_to_name(Option<&SleighRegs>, ...) |
| F-019 | Applied (doc) | ec8a34f | Comment now points at strider's twin sort key explicitly |
| F-020 | Applied (option b) | ec8a34f | Inlined helper kept; rationale rewritten as a clear "why a copy" pointer.  Also added blanket Arc/Box ReadOnlyMemory impls in reader for any future relaxation |
| F-021 | Applied | afa8a84 | Cfg::vn_to_name shim deleted; test_api forwards directly |
| F-022 | Applied | ec8a34f | make_resolver_pipeline helper used at both sites |
| F-023 | Skipped | — | Marked optional in review; the three test helpers are short and the duplication is mild |
| F-024 | Skipped | — | Cosmetic / low confidence per review |
| F-025 | Applied | 0debd4c | Explicit 8-byte arm + bail on unsupported size |
| F-026 | Applied | 0debd4c | Renamed comment to "half-open"; comparison flipped to addr >= end_exclusive |
| F-027 | Applied | 0debd4c | Error names the entry address |
| F-028 | Applied | 0debd4c | Typo fixed; message names the missing address |
| F-029 | Skipped | — | Marked optional / low confidence; doc already says "Replaces any previous" |
| F-030 | Skipped | — | Risk medium; orchestrator already supports the field's None case.  Reworded the docstring instead |
| F-031 | Applied | 0debd4c | Step-comment block trimmed |
| F-032 | Skipped | — | Optional / low confidence; impl is small and harmless |
| F-033 | Skipped | — | Cosmetic; addressing requires repeating split-region context across 4 variants |
| F-034 | Applied | 0debd4c | BranchIndirect arm extracted to process_branch_indirect helper |
| F-035 | Skipped | — | Low confidence + cross-crate plumbing; comment rewritten to remove the TODO |
| F-036 | Skipped | — | Review confirms the implementation is correct (no fix proposed) |
| F-037 | Skipped | — | Review confirms early-out is fine (no fix proposed) |
| F-038 | Skipped | — | Optional / low confidence per review |
| F-039 | Skipped | — | Review marks "None.  Flagged for clarity." |
| F-040 | Applied | afa8a84 | Folded into F-021's dispatch collapse |
| F-041 | Applied | 0debd4c | Codename refs stripped from tests |

Tally: 28 applied, 1 obviated, 12 skipped.

Final cargo test summary: all 39 test binaries pass, 0 failures.
