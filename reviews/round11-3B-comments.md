# Round 11 — 3B: stale comment sweep

## Summary

| Category | Count |
|----------|-------|
| Deleted-symbol references | 0 |
| Closed-TODO references | 0 |
| Behaviour-drift descriptions | 4 |
| Migration breadcrumbs | 8 |
| Broken paths / intra-doc links | 35 |
| Multi-round-old placeholders | 0 |

The codebase is in good shape regarding deleted-symbol references and stale TODOs (the `CallOtherElide` / `NO_OP_USER_OPS` deletion was thorough). The dominant problems are:

1. **Broken intra-doc links** — `cargo doc` (with `-W rustdoc::broken-intra-doc-links`) reports 89 warnings across 7 crates. Many resolve to private items, several reference moved/renamed entities, several were corrupted by a string-merge that accidentally concatenated two doc lines.
2. **Malformed merged-line doc comments** — six doc comments where a bug in some prior automated edit merged two `///` lines into one, producing source like `/// Foo.  /// (R9-2D M3): Bar`. These render as a single line with the literal `///` mid-string.
3. **Round-9 migration breadcrumbs** — eight comments still talk about "round 9 ..." migration work as if it were ongoing, even though the migrations have shipped (the canonical accessors exist; the commented "old API" comparisons are now history).

## Findings

### 1. Malformed merged doc-comment ("R9-2D M3" breadcrumb spliced into a one-liner)
- **Severity:** HIGH (renders broken; confusing-looking source; survives reformat)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/options.rs:9`
- **Comment:** `/// Function-extent boundary for tail-call classification.  /// (R9-2D M3): the previous \`(Option<u64>, bool)\` pair carried the`
- **Why stale:** Two distinct doc-comment lines were merged into one. The literal `/// (R9-2D M3):` shows up as text in the rendered docs. Also, the "round 9" breadcrumb has outlived its context — `FunctionBoundary` is now the canonical type; nobody needs the historical justification at the top of its docstring.
- **Proposed rewrite:** Split into two lines and drop the migration tag:
  ```
  /// Function-extent boundary for tail-call classification.
  ///
  /// Replaces the previous `(Option<u64>, bool)` pair which silently
  /// ignored `allow_code_before_start_addr` in the bounded case.
  /// This sum type makes the rule unrepresentable by construction:
  ```

### 2. Malformed merged doc-comment on `Options::function_boundary`
- **Severity:** HIGH (renders broken)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/options.rs:99`
- **Comment:** `/// \`(fn_max_size, allow_code_before_start_addr)\`.      /// (R9-2D M3): canonical accessor that resolves the documented`
- **Why stale:** Same string-merge bug. Two `///` lines run together into one; the second `///` literal is rendered. The "(R9-2D M3)" tag also names a closed task.
- **Proposed rewrite:** Split into two `///` lines and drop the breadcrumb. The "canonical accessor" wording is fine on its own.

### 3. Malformed merged doc-comment on `PcodeInsnAddr::machine_addr`
- **Severity:** HIGH (renders broken)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/types.rs:79`
- **Comment:** `/// Read the parent machine instruction's address.      /// (R9-2D H2): canonical accessor for the migration path that`
- **Why stale:** Same string-merge bug; round-9 breadcrumb. Also the `/// canonical accessor.` one-liner pattern at lines 87 and 95 (and at `arch.rs:145, 150`) is a leftover migration tag — now that the accessors have shipped this language is just noise.
- **Proposed rewrite:** Split into proper paragraphs; drop "(R9-2D H2)" and the dangling `/// canonical accessor.` lines.

### 4. Malformed merged doc-comment on `lift_at`
- **Severity:** HIGH (renders broken)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/mod.rs:410`
- **Comment:** `/// restore via the inner guard's \`Drop\` impl.      /// (R9-1A I3) closed the prior leak path where a panic would leave`
- **Why stale:** Same string-merge bug; "R9-1A I3" is closed work. The current code is correct (the `Guard` implements `Drop`); explaining what was wrong in the *previous* version of `lift_at` is round-9 archaeology.
- **Proposed rewrite:** Split the lines, delete the historical tag. The relevant fact is the `Drop` invariant in the current code — that should be stated positively, not via "closed the prior leak path".

### 5. Malformed merged doc-comment on `SleighArch::preset`
- **Severity:** HIGH (renders broken)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/arch.rs:150`
- **Comment:** `/// Read the arch preset discriminator.      /// canonical accessor.`
- **Why stale:** Same string-merge bug. The dangling "canonical accessor" tag is migration noise (also appears at line 145 for `endianness()`).
- **Proposed rewrite:** Just `/// Read the arch preset discriminator.` — drop the trailing tag entirely.

### 6. Malformed merged doc-comment on `link_register_vn_resolves_to_callee_saved_lr`
- **Severity:** HIGH (renders broken)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/tests.rs:682`
- **Comment:** `/// list) agree for every link-register preset.  /// previously only ARM was pinned; AArch64 / MIPS / PPC could drop`
- **Why stale:** Same string-merge bug. The rationale clause about "only ARM was pinned" is round-9-vintage and tests now cover all archs.
- **Proposed rewrite:** Split the lines and drop the historical clause.

### 7. "(round 9 P5 / R9-2D M6)" breadcrumb in `ResolvedTargets::multiple` doc
- **Severity:** MED (cosmetic noise; tag named in docs that ship)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:102-103`
- **Comment:** `/// Validating constructor for [\`Self::Multiple\`] (round 9 P5 /\n    /// R9-2D M6).  Rejects empty \`targets\` so a future arm cannot`
- **Why stale:** "round 9 P5 / R9-2D M6" identifies the audit task that introduced the validating constructor. The constructor is now the canonical API — the audit-tag is meaningless to anyone reading docs in 2026-05+.
- **Proposed rewrite:** `/// Validating constructor for [\`Self::Multiple\`].  Rejects empty \`targets\` so a future arm cannot`

### 8. "(round 9 V4 / R9-2D H3)" breadcrumb on `try_from_parts`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:192`
- **Comment:** `/// Validating constructor (round 9 V4 / R9-2D H3).  Builds a`
- **Why stale:** Same pattern. The constructor name "validating constructor" is descriptive enough on its own.
- **Proposed rewrite:** `/// Validating constructor.  Builds a \`BuiltCallingConvention\` from explicit parts...`

### 9. "round 9 wave 24 added LR per CLAUDE.md" test-data breadcrumb
- **Severity:** LOW (test comment; cosmetic)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/tests.rs:154`
- **Comment:** `// r2 + r14..r31 (18) + LR — round 9 wave 24 added LR per\n            // CLAUDE.md deliberate-tradeoff (consistent with PPC32).`
- **Why stale:** "round 9 wave 24 added LR" is a historical note about when the change was made — the rationale (CLAUDE.md deliberate tradeoff) stands on its own.
- **Proposed rewrite:** `// r2 + r14..r31 (18) + LR — see CLAUDE.md "Note (link-register handling)".`

### 10. "round 9 Ask-8 R2 F7" breadcrumb in stall-guard doc
- **Severity:** MED (history-as-current-event; "Pre-fix" framing)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:215-216`
- **Comment:** `/// Pre-fix (round 9 Ask-8 R2 F7) the comparison was \`>=\`, which\n/// incorrectly consumed budget on every count-stable iteration.`
- **Why stale:** "Pre-fix" tags the current code with a delta against a pre-shipping state. New maintainers don't have access to the buggy version. The substantive doc above this paragraph already states the invariant — this is back-story.
- **Proposed rewrite:** Delete the two lines.

### 11. "round 9 wave 30 (D3+D4)" breadcrumb in test
- **Severity:** LOW (test code)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:680-684`
- **Comment:** `// Tests for \`Truncate(IntConst)\` / \`Extend(IntConst)\` classifier\n    // arms were deleted in round 9 wave 30 (D3+D4): ConstantFold rules\n    // 4-6 and the builder's \`truncate_if_needed\` / \`extend_if_needed\`\n    // helpers fold those shapes to \`IntConst\` before the classifier\n    // ever runs, so the dedicated arms were dead-in-production.  The\n    // surviving \`IntConst\` arm covers the live path.`
- **Why stale:** Explains why a test is *missing*. Either the comment lives forever or it gets deleted; useful only at the moment of deletion. The "ConstantFold rules 4-6" nomenclature is also fragile — rule numbering doesn't appear in `ConstantFold` source.
- **Proposed rewrite:** `// Truncate/Extend arms aren't tested here: ConstantFold and the\n    // builder helpers fold those shapes to IntConst before the\n    // classifier sees them, so the IntConst arm covers the live path.`

### 12. "wrap_when fix from round 9 wave 31 (H-8)" breadcrumb
- **Severity:** MED (history-as-doc; mirrors a fix that's already shipped)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs:577-578`
- **Comment:** `// silently absorbed.  Mirrors the wrap_when fix from round 9\n            // wave 31 (H-8).`
- **Why stale:** "wrap_when fix" no longer exists as a discoverable name. Mirroring a previous fix-in-the-codebase doesn't help — what helps is naming what the code does.
- **Proposed rewrite:** Drop the trailing two lines; the preceding paragraph already explains why `KeyboardInterrupt` / `SystemExit` are re-raised.

### 13. "round 9's reachability gate (Ask-8 R2 F2)" breadcrumb
- **Severity:** LOW (test code)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/tests.rs:308`
- **Comment:** `// round 9's reachability gate (Ask-8 R2 F2 fix in \`check_layer_c_control_state\`)`
- **Why stale:** "round 9 Ask-8 R2 F2" is closed audit metadata. The rest of the comment about *why* the bad ControlState must be reachable is the substantive part and should stand on its own.
- **Proposed rewrite:** `// The bad ControlState must be **reachable** from entry — otherwise\n    // the Layer-C reachability gate in \`check_layer_c_control_state\`\n    // correctly skips it as an unreachable zombie.`

### 14. Broken intra-doc link `[Self::from_parts]`
- **Severity:** HIGH (load-bearing — the link claims a constructor exists that doesn't)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:99`
- **Comment:** `/// Produced by [\`CallingConvention::build\`] (canonical path) or\n/// [\`Self::from_parts\`] (test/override construction).`
- **Why stale:** No method named `from_parts` exists on `BuiltCallingConvention`. The actual ctors are `from_parts_unchecked` and `try_from_parts`. The doc misdirects readers.
- **Proposed rewrite:** `/// Produced by [\`CallingConvention::build\`] (canonical path) or\n/// [\`Self::try_from_parts\`] (validating override) / [\`Self::from_parts_unchecked\`] (test-only override).`

### 15. Broken intra-doc link `[node::Graph]` (twice in lib doc)
- **Severity:** MED (top-of-crate doc; misdirects newcomers)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/lib.rs:18, 31`
- **Comment:** `//! cached inside [\`node::Graph\`].` and `//! - [\`node::Graph\`] — raw node/edge store`
- **Why stale:** `Graph` lives in the `graph` module, not `node`. Top of `lib.rs` re-exports it as `crate::Graph`. The link is unresolvable.
- **Proposed rewrite:** `[\`Graph\`]` (uses the crate-level re-export at line 46).

### 16. Broken intra-doc link `[BuiltFunctionGraph]` in `FunctionBuilder::build`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/mod.rs:552`
- **Comment:** `/// Finalises and returns the completed [\`BuiltFunctionGraph\`], after running`
- **Why stale:** No `use` of `BuiltFunctionGraph` in this module. The link target doesn't resolve from this scope.
- **Proposed rewrite:** `[\`crate::function::BuiltFunctionGraph\`]` or `[\`crate::BuiltFunctionGraph\`]` (the latter via the lib.rs re-export).

### 17. Broken intra-doc link `[FunctionBuilder]` in `BuiltFunctionGraph` doc
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:42`
- **Comment:** `/// Produced by consuming a [\`FunctionBuilder\`] after all regions have been`
- **Why stale:** `FunctionBuilder` not in scope; the link doesn't resolve.
- **Proposed rewrite:** `[\`crate::FunctionBuilder\`]` (or `crate::builder::FunctionBuilder`).

### 18. Broken intra-doc link `[NodeKind]` in `NodeOutputType::get_unsigned_int`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs:200`
- **Comment:** `/// category in [\`NodeKind\`], so callers reading a \`Bool\` constant`
- **Why stale:** `NodeKind` is in the same `node` module but not imported in `output_type.rs`. The link doesn't resolve from this scope.
- **Proposed rewrite:** `[\`crate::node::NodeKind\`]`.

### 19. Broken intra-doc link `[crate::validate::layer_c]`
- **Severity:** MED (this crate's docs name a private module)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/kind.rs:43`
- **Comment:** `/// per \`index\` (enforced by [\`crate::validate::layer_c\`]).`
- **Why stale:** `validate::layer_c` is a private module (`mod layer_c;` in `validate/mod.rs:26`). Not visible from public docs. Same pattern at `validate/mod.rs:5,7,9` (`[layer_a]`, `[layer_b]`, `[layer_c]`).
- **Proposed rewrite:** Drop the link; describe the enforcement in prose: `// enforced by validate's Layer C (graph-level invariants)`.

### 20. Markdown link to opt source path from inside the ir crate
- **Severity:** MED (link is a relative file path that won't resolve in published rustdoc)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/kind.rs:38`
- **Comment:** `[\`opt::FunctionArgDetect\`](../../../opt/src/function_args/mod.rs)`
- **Why stale:** Markdown link with relative path crosses crate boundaries; rustdoc's link resolver can't follow it. Even if the file is checked in, navigating to a `.rs` source from a docs page is not what readers expect (they expect the rendered API doc).
- **Proposed rewrite:** Drop the link and use prose: `Introduced by the \`FunctionArgDetect\` opt pass.`

### 21. Broken intra-doc link `[Capture]` in `find_all_requirements` doc
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs:365`
- **Comment:** `/// matches where every [\`Capture\`] appearing in more than one`
- **Why stale:** `Capture` not imported in `matcher/mod.rs`. The link doesn't resolve.
- **Proposed rewrite:** `[\`crate::Capture\`]` (a top-level `pub use` exists in lib.rs).

### 22. Broken intra-doc link `[find_all]` in `match_at`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs:492`
- **Comment:** `/// Unlike [\`find_all\`] which iterates every candidate root, this checks a`
- **Why stale:** `find_all` is a method on `Matcher`; rustdoc resolves `[Self::find_all]` but bare `[find_all]` doesn't see the method.
- **Proposed rewrite:** `[\`Self::find_all\`]`.

### 23. Broken intra-doc link `[opt::IfCondInversion]` (from pattern crate)
- **Severity:** HIGH (cross-crate link to a non-dependency)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/builders/branch.rs:40` and `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/ctor/control.rs:142`
- **Comment:** `/// the [\`opt::IfCondInversion\`] pass guarantees`
- **Why stale:** The `pattern` crate doesn't depend on `opt` (only as a dev-dependency — `pattern/Cargo.toml:21`). Public rustdoc can't link to symbols in a dev-dep.
- **Proposed rewrite:** Drop the backticks-as-link form; describe in prose: `the \`IfCondInversion\` opt pass guarantees ...`.

### 24. Pseudo-link `[0]` / `[1]` / `[2]` rendered as broken intra-doc links
- **Severity:** MED (multiple sites, each a published-docs warning; output looks broken)
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/builders/call.rs:193, 202`
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/builders/memory.rs:21, 42, 47, 54, 132, 137, 142, 149, 155`
- **Comment example:** `/// output (outputs[0]) — forward walk via`
- **Why stale:** rustdoc parses `outputs[0]` as a link reference where the target is `0`. Without an intervening backtick around the whole `outputs[0]`, rustdoc emits an "unresolved link" warning.
- **Proposed rewrite:** Wrap the whole expression in backticks: `\`outputs[0]\`` — already the convention in surrounding lines (e.g. `inputs[1]` is correctly backticked at memory.rs:132 *outside* the `(` parens but the inner usages aren't).

### 25. Broken intra-doc link `[CapturePat]` and `[VarPat]`/`[AnyPat]`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/ctor/wildcards.rs:22`
- **Comment:** `/// [\`VarPat\`] rather than wrapping [\`AnyPat\`] in a [\`CapturePat\`] — one`
- **Why stale:** `VarPat`/`AnyPat` are private; `CapturePat` doesn't exist as a name (the matching mechanism is now the `WithCapture` Pat wrapper or the bare `var(c)` ctor).
- **Proposed rewrite:** Drop the link form: `... rather than wrapping the matched node — one fewer vtable hop and no backtracking snapshot per match.`

### 26. Broken intra-doc link `[int_const(-50)]`
- **Severity:** LOW (cosmetic)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/ctor/wildcards.rs:144`
- **Comment:** `/// [\`int_const(-50)\`] does an exact-bit-pattern match`
- **Why stale:** rustdoc parses the parens-with-arg as part of a path, can't resolve `int_const(-50)` as a function call. A docs link can name a function but not an invocation.
- **Proposed rewrite:** `[\`int_const\`]\`(-50)\`` (link + literal) or wrap the whole thing in plain backticks.

### 27. Broken intra-doc link `[crate::Error]` in opt
- **Severity:** HIGH (the type doesn't exist)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/pipeline.rs:118, 251`
- **Comment:** `/// validation failure or a pattern-rewrite error propagated up through\n/// [\`crate::Error\`].` and `/// Returns the first [\`crate::Error\`] reported by any pass.`
- **Why stale:** opt's `error.rs` only defines `Result<T> = anyhow::Result<T>` — there's no `Error` type at all. Errors are bare `anyhow::Error`s.
- **Proposed rewrite:** `[\`anyhow::Error\`]` (or just drop the link form entirely — opt errors are dynamic).

### 28. Broken intra-doc link `[Graph::add_node_input]` etc. in `inplace.rs`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:41-42`
- **Comment:** `/// ([\`Graph::add_node_input\`] / [\`Graph::remove_node_input\`] /\n/// [\`Graph::set_node_kind\`]) fail.`
- **Why stale:** `Graph` is not imported in this file (the code uses `pattern::RewriteCtx`, not `Graph` directly). The link doesn't resolve.
- **Proposed rewrite:** `[\`ir::Graph::add_node_input\`]` / `[\`ir::Graph::remove_node_input\`]` / `[\`ir::Graph::set_node_kind\`]`.

### 29. Broken intra-doc link `[KnownBits]` in jump-table module doc
- **Severity:** LOW (link target exists; just not in scope)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table.rs:17`
- **Comment:** `//! 1. **Bounded index.**  Either [\`KnownBits\`] proves \`idx\`'s upper`
- **Why stale:** `KnownBits` is at `crate::known_bits::KnownBits`; not imported in this file's module doc.
- **Proposed rewrite:** `[\`crate::KnownBits\`]` (assuming a top-level re-export — confirm in `lib.rs`) or `[\`crate::known_bits::KnownBits\`]`.

### 30. Broken intra-doc link `[super::IndirectBranchResolve::optimize]`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table.rs:290`
- **Comment:** `/// callers (typically [\`super::IndirectBranchResolve::optimize\`]) compute`
- **Why stale:** `optimize` is a trait method; the path needs the trait. rustdoc can't resolve `IndirectBranchResolve::optimize` because `optimize` isn't an inherent method.
- **Proposed rewrite:** Either link to the impl block or drop the parameterised form: `[\`super::IndirectBranchResolve\`]'s \`optimize\` method`.

### 31. Broken intra-doc link `[cfg::Builder::with_known_targets]` (cross-crate, opt → cfg)
- **Severity:** HIGH (cross-crate link to non-dependency)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:66`
- **Comment:** `/// Re-exported from\n/// \`cfg\` so callers that build \`known_targets\` maps for\n/// [\`cfg::Builder::with_known_targets\`] use the same type the\n/// classifier returns.`
- **Why stale:** `opt` does not depend on `cfg` (`opt/Cargo.toml` lists no cfg dep). The link is unresolvable from opt's published docs. The dependency direction is `cfg → opt`, not the reverse.
- **Proposed rewrite:** Drop the rustdoc-link form: `\`cfg\`'s \`Builder::with_known_targets\` consumes the same type the classifier returns.`

### 32. Broken intra-doc link `[Strider::analyze_cfg(cfg)]`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:86`
- **Comment:** `/// defaults match the [\`Strider::analyze_cfg(cfg)\`] convenience`
- **Why stale:** rustdoc cannot put a parenthesised argument list inside an intra-doc link target. The argument list `(cfg)` makes the link target unparseable.
- **Proposed rewrite:** `[\`Strider::analyze_cfg\`]` and put `(cfg)` outside the link.

### 33. Broken intra-doc link `[mem::take]`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/rewrite.rs:89`
- **Comment:** `/// per call (via [\`mem::take\`]) so the closure has the input shape`
- **Why stale:** `mem` (i.e. `std::mem`) is not imported here. The link doesn't resolve from this scope.
- **Proposed rewrite:** `[\`std::mem::take\`]`.

### 34. Broken intra-doc link `[crate::Builder::is_branch_tail_call_nocheck]`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/query.rs:13`
- **Comment:** `/// Shared by [\`crate::Builder::is_branch_tail_call_nocheck\`] (cfg-time`
- **Why stale:** `is_branch_tail_call_nocheck` is `pub(super)` on `RegionBuilder`, not on `crate::Builder`. The path resolves to nothing.
- **Proposed rewrite:** Drop the rustdoc-link form: `Shared by the cfg-time region-builder's classifier and \`strider\`'s ...`.

### 35. Behaviour drift in `build_optimizer_pipeline` doc
- **Severity:** HIGH (doc undercount of pipeline contents)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:182-183`
- **Comment:** `/// 1. All passes from [\`opt::default_pipeline\`] (constant folding,\n///    known-bits, redundant-phi, dead-branch).`
- **Why stale:** `opt::default_pipeline()` runs **six** passes (`ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, `RedundantPhis`, `DeadBranchElimination` — see `opt/src/lib.rs:193-202`). The doc lists only four, missing `FlagCmpCanonicalize` and `IfCondInversion`.
- **Proposed rewrite:** `1. All passes from [\`opt::default_pipeline\`] (constant folding,\n///    known-bits, flag-cmp canonicalisation, if-cond inversion,\n///    redundant-phi, dead-branch).`

### 36. Behaviour drift in `LoopState::sleigh` doc — names a deprecated ctor
- **Severity:** MED (current code uses a different ctor)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:261`
- **Comment:** `/// from \`RunConfig::sleigh\` at construction; consumed by\n/// \`Builder::with_endianness\` per iteration and harvested back from`
- **Why stale:** `Builder::with_endianness` is `#[deprecated]` (cfg/builder/mod.rs:117). The actual call site at `orchestrator.rs:940` uses `Builder::for_arch`. The doc names the wrong ctor.
- **Proposed rewrite:** `consumed by \`Builder::for_arch\` per iteration and harvested back from the resulting \`Cfg::sleigh\`.`

### 37. Broken intra-doc link `[super::region_builder]`
- **Severity:** LOW (private module name; only visible to crate-internal docs)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/mod.rs:169`
- **Comment:** `/// Sets the [\`target::ArchPreset\`] used by the \`Opcode::CallOther\`\n    /// arm in [\`super::region_builder\`] when consulting`
- **Why stale:** `region_builder` is `mod region_builder;` (private). Public docs can't link to private modules.
- **Proposed rewrite:** Drop the link form: `... arm in this builder's region-builder when consulting ...`.

### 38. Broken intra-doc link `[super::builder::Builder]`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/mod.rs:62`
- **Comment:** `/// scan.  Promoted from [\`super::builder::Builder\`]'s field of the`
- **Why stale:** `cfg/mod.rs:1` declares `mod builder;` (private). The path `super::builder::Builder` traverses a private module from a sibling, which rustdoc rejects.
- **Proposed rewrite:** `[\`crate::Builder\`]` (the re-export at line 8 of cfg/mod.rs).

### 39. Broken intra-doc link `[crate::Error]` in cfg builder
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/mod.rs:306`
- **Comment:** `/// Returns a [\`crate::Error\`] if disassembly fails, if the start region`
- **Why stale:** Same as opt: cfg's error module doesn't expose an `Error` type at the crate root. Need to check if cfg crate has one.
- **Proposed rewrite:** Verify the cfg `Error` type exists; if not, change to `anyhow::Error` description.

### 40. Broken intra-doc link `[RegionGraph]` in three sites
- **Severity:** MED (private alias surfacing in pub docs)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/types.rs:5, 115, 213`
- **Comment example:** `/// Every edge in the [\`RegionGraph\`] carries one of these four labels.`
- **Why stale:** `RegionGraph` is `pub(crate) type RegionGraph = StableDiGraph<...>;` (types.rs:253). It's not part of the public API; pub docs can't link to it.
- **Proposed rewrite:** Use the actual public path `crate::Cfg::graph` field, or drop the link form: "Every edge in the region graph carries...".

### 41. Private intra-doc link to `crate::Graph::call_clobbered_overrides`
- **Severity:** LOW (private item; rustdoc warns)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/call.rs:33`, `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:95`
- **Comment:** `[\`crate::Graph::call_clobbered_overrides\`]`
- **Why stale:** `call_clobbered_overrides` is private. Linking it from public-API docs creates a `private_intra_doc_links` warning and dead-end navigation.
- **Proposed rewrite:** Drop the link form; describe the field in prose without naming the private storage.

### 42. Private-item intra-doc links in builder/vars.rs and builder/mod.rs
- **Severity:** LOW (multiple sites, all rustdoc private-item warnings)
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/vars.rs:68, 69, 97` — `Self::link_control_regions`, `Self::link_memory_regions`, `Self::link_region_variables`, `Self::build_control_phi`
  - `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/mod.rs:150, 156` — `FunctionGraph` (declared in private module)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/region.rs:239` — `Self::link_region`
- **Why stale:** All link to `pub(crate)` / private items from public-doc surfaces.
- **Proposed rewrite:** Drop the link form on each — describe in prose. Or document `pub(crate)` items behind `#[cfg(doc)]` if they really need to be visible.

### 43. Private-item intra-doc links in pattern crate
- **Severity:** LOW
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/error.rs:7, 19` — `is_skip` (pub(crate))
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:38, 39` — `Self::mark`, `Self::restore` (both pub(crate))
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs:66, 226` — `crate::pat::node_pat::KindSpec`, `crate::pat::traits::Pattern::kind_spec` (private modules)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs:506` — `FunctionArgHandle` (private?)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/traits.rs:113` — `Pattern::try_build`
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/rewrite.rs:17, 33` — `try_build`, `is_skip`
- **Why stale:** Each is a private-item link in public doc, generating rustdoc warnings.
- **Proposed rewrite:** Drop the link forms; describe in prose. Or — if these items genuinely belong in the public API — re-export them.

### 44. Private-item intra-doc links in pcode-lift
- **Severity:** LOW
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/value/mod.rs:5` — `lift` (private fn)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/vn_io.rs:55, 60, 94, 97` — `Self::read_reg_vn`, `Self::write_reg_vn` (both pub(crate))
- **Why stale:** Linked from `pub fn read_vn` / `pub fn write_vn` doc but the link target is `pub(crate)`. From outside the crate the name doesn't resolve.
- **Proposed rewrite:** Drop the link form — `[\`Self::read_reg_vn\`]` becomes `the register-aliasing helper` in prose.

### 45. Private-item intra-doc links in opt
- **Severity:** LOW
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/stack_array.rs:23` — `opt::stack_load_forward::find_stack_stored_value_at_offset` (private)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs:42` — `Kb::merge`, `Kb::from_const` (both private)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_load_forward/mod.rs:488` — `find_stack_stored_value_at_offset` (private)
- **Why stale:** Links to private items.
- **Proposed rewrite:** Drop the link form on each.

### 46. Private-item intra-doc link in cfg
- **Severity:** LOW
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/indirect_resolve.rs:363` — `super::resolve_indirect_target` (private fn)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/mod.rs:29` — `RegionBuilder` (private struct)
  - `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/options.rs:54` — `Self::read_only_memory` (private field)
- **Why stale:** Each is a private-item link from a public doc surface.
- **Proposed rewrite:** Drop link forms.

### 47. Private-item intra-doc link in strider pipeline
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:91`
- **Comment:** `/// [\`Strider::find_all_unique_vns\`] itself.`
- **Why stale:** `find_all_unique_vns` is `pub(crate)`.
- **Proposed rewrite:** Describe it in prose: `/// the strider's internal varnode-collection helper.`

### 48. Stale parameter description for `target` in `get_unsigned_int` / IntConst doc
- **Severity:** LOW (informational drift)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs:194-196`
- **Comment:** `/// For widths >= 128 returns \`val\` unchanged (the carrier is \`u128\`, so\n/// \`U128\` returns its full mask and \`U256\` returns \`val\` as-is - callers\n/// that need to distinguish the two must check the type explicitly).`
- **Why stale:** With `IntConst(u128)` rejecting U256/U512 (per CLAUDE.md: "Wide types (U256 / U512) are stored via IntConstWide ... IntConst(u128) rejects them"), the comment about "U256 returns val as-is" is misleading — readers may think they can call `get_unsigned_int` on a `U256` value-typed const node and get the value back, when in reality wide consts go through `IntConstWide` + `wide_const`.
- **Proposed rewrite:** Clarify that this helper is for the integer-typed widths actually carried by `IntConst`; widths > 128 belong on a separate (wide-const) lookup path.

### 49. Stale module-doc reference to `[`field`]` link target
- **Severity:** LOW (cosmetic — link target is a literal English word)
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:371`
- **Comment:** `/// docs.  See the [\`Self::no_memory_clobber\`](field) docs.`
- **Why stale:** rustdoc parses `(field)` as a link target. It can't resolve "field" because there's nothing of that name in scope. Same construct appears in the rendered output as a literal "field" hyperlink-to-nowhere.
- **Proposed rewrite:** `/// See the docs on the \`no_memory_clobber\` field above.` (drop both the link and the parenthesised hint).


## Notable correct comments (positive findings)

The codebase has *many* comments that are clearly active and well-tied to their referent. A few examples:

- The two public ctor docs in `cfg/src/cfg/builder/mod.rs:90, 112` correctly explain why `new` and `with_endianness` are `#[deprecated]`, naming the silent miscompile (LE+X86_64 default) — load-bearing for non-x86 callers.
- `crates/target/src/calling_convention/tests.rs:154-155` (PowerPC ELFv1) survives migration breadcrumb stripping cleanly (factual content remains useful even after dropping the round/wave tag).
- Pattern `LoadPat::bit_width` doc (memory.rs:54-56) correctly states the int+float same-width matching behaviour.
- `Strider::build_stable_optimizer_pipeline` (strider/pipeline.rs:209-219) correctly enumerates inherited passes from `opt::stable_default_pipeline()` — contrast with item 35 above where the corresponding `build_optimizer_pipeline` has drifted.
