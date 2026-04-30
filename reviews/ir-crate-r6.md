# crates/ir review — round 6 (2026-04-30)

## Summary

- Files reviewed: 46 (`crates/ir/src` + `crates/ir/tests` + `crates/ir/examples`; no `benches`)
- Findings total: 53
  - Correctness: 18
  - Dead code: 9
  - Duplication & unification: 6
  - Simplification: 7
  - Readability: 8
  - Performance: 5
- High-confidence findings: 39
- Low-risk findings: 41

The crate is solid at its core — the validator's three-layer split is well-tested, the dedup cache eviction is correctly tied to the only two graph-mutation paths (`update_input`, `detach_node_inputs`), and `replace_all_uses`'s cursor semantics are sound under self-redirect, fan-out, and identical old/new inputs.  The bulk of the findings are documentation drift: nearly every public method that documents its error variants still cites a `crate::ErrorKind::Foo` link that has been gone since the workspace anyhow migration (97 grep hits across the crate), and two `pub` accessors still document themselves as feeding `crate::ir_cache::RegionIrCache` (which the strider restructure deleted).

The most consequential findings beyond doc drift:

- **`build_int_const` on `FunctionBuilder` panics** via `assert!` for non-integer types and for `U256` (`builder/nodes.rs:94-101`).  Two of its in-crate callers (`coerce.rs::truncate_if_needed` and `coerce.rs::extend_if_needed`'s SignExtend/ZeroExtend folds) take `output_type` from arbitrary callers and forward it directly.  `Strider` allocates U256 entries when a Sleigh varnode is 32 bytes wide, so the panic is reachable from real IR construction, not just synthetic input.  Project memory pins `feedback_no_panic_unwrap` ("never panic/unwrap/expect in normal error paths").
- **Two `make_int_const` definitions disagree on validation** — `Graph::make_int_const` (`ops/consts.rs:63`) accepts any `NodeOutputType` (no integer check, would happily emit `IntConst(_): F32` and let validate complain later) while `FunctionBuilder::build_int_const` (`builder/nodes.rs:89`) panics on the same shape.  Same name, different contracts, both `pub`.
- **`Graph::int_const_val` silently narrows U128/U256 IntConst payloads** to `Option<u64>` via `u64::try_from(w).ok()` (`ops/consts.rs:24`), returning `None` for in-range values that don't fit.  Callers that branch on `int_const_val(...) == Some(0)` will mis-classify a U128 zero (returns `Some(0)`) vs a U128 1<<64 (returns `None`).  No `int_const_val_u128` exists.
- **`region_entry_control` / `region_entry_memory` synthesise a fake `NodeOutputId::from_u32(0)`** as a fallback to look up an output kind for the error message (`region.rs:301, 318`), which can poke into a live unrelated output.  Cosmetic but the resulting "got {kind:?}" text is whatever happens to live at index 0 — typically Entry's Control output — for what is supposed to be an "outputs is empty" error.
- **`expected_signature` rejects no node — by construction**, but the `head_len()` for `ControlState` is `0` while `Layer A` validates against `head_len()` for arity, so a *reachable* zero-predecessor `ControlState` slips Layer A entirely (variadic-min-len = 0 trivially holds).  Layer C does catch it (validate/layer_c.rs:64), but the asymmetry is undocumented and Layer A's `>= head_len()` rule has no test for the head_len=0 + variadic case.

A handful of smaller things are noted: the ~40 `BUG-N` / `F2` codename references in tests and prod comments, the typo `cloberred` (4 fields/locals + 5 use sites in builder/), `walk::cfg_succs` walks consumers via `output_uses` (use-list walk) which is deterministic only by insertion order (test-pinned but undocumented), and the dedup cache hash key allocates two `Vec`s per `create_node` call even on a cache hit.

## Table of contents

- F-001 `build_int_const` panics on non-integer or U256 type (Correctness, `src/builder/nodes.rs:89-104`)
- F-002 `Graph::make_int_const` accepts any `NodeOutputType` without validation (Correctness, `src/ops/consts.rs:63-70`)
- F-003 `Graph::int_const_val` silently narrows U128/U256 to `Option<u64>` (Correctness, `src/ops/consts.rs:17-27`)
- F-004 `region_entry_control` / `region_entry_memory` fall back to a fake `NodeOutputId::from_u32(0)` to format the error (Correctness, `src/region.rs:293-328`)
- F-005 Layer A's `>= head_len()` rule trivially passes for `ControlState` (head_len = 0 + variadic CTRL); zero-predecessor reachability is checked only in Layer C (Correctness, `src/validate/layer_a.rs:28-45`, `src/validate/layer_c.rs:49-84`)
- F-006 `Inputs::index` and `Outputs::index` panic on out-of-bounds; only `Inputs::index` notes this (Correctness, `src/iterators.rs:34-40, 99-106`)
- F-007 `as_value_or_err` / `as_integer_or_err` / `as_float_or_err` are `#[track_caller]` but error messages don't include the caller (Correctness, `src/node/output_kind.rs:45-90`)
- F-008 97 doc references to `ErrorKind::*` link types that don't exist after the anyhow migration (Correctness, multiple files)
- F-009 `region.rs:177, 250` and `builder/mod.rs:372` doc references to the deleted `crate::ir_cache::RegionIrCache` / `crate::ErrorKind::*` (Correctness, `src/region.rs:177, 250-251`, `src/builder/mod.rs:372`)
- F-010 `NodeKind::CastToInt` doc comment describes converting to **Bool**, not Int (Correctness, `src/node/kind.rs:121`)
- F-011 `lib.rs` lists `NodeOutputType` with `U8/U16/U32/U64/U128/U256, F32/F64` — silently omitting U80/F80 (Correctness, `src/lib.rs:36-37`)
- F-012 `node_signature.rs::ExpectedOutputKind::AnyInt` doc says "U8, U16, U32, U64, U128, U256" — misses U80 (Correctness, `src/node_signature.rs:33-34`)
- F-013 `bit_mask_u128` returns `u128::MAX` for U256 instead of `None`/`Result`; documented "best-effort sentinel" but downstream `get_unsigned_int(_, U256)` quietly returns `Some(val)` (Correctness, `src/node/output_type.rs:144-162`)
- F-014 `coerce.rs:188 SignExtend => build_int_const(signed_val as u128, output_type)` works correctly only because `i64 as u128` sign-extends — but the comment says "reinterpret bits as u128 (sign-extended to i128 then cast)" which is misleading (Correctness/Readability, `src/builder/coerce.rs:185-189`)
- F-015 `extend_if_needed` returns `Ok(value_id)` early when "wider input → narrower target", silently producing a wrong-typed result (Correctness, `src/builder/coerce.rs:206-209`)
- F-016 Validate `Layer C uniqueness` checks `Entry`/`InitialMemory` over **all** nodes (including zombies); zombie Entries left by an unsound rewrite would trigger a false positive (Correctness, `src/validate/layer_c.rs:16-45`)
- F-017 `node_signature::tail` for `Return`'s ret-val tail is `RET = AnyValue` but `ARG`/`RET`/`CALL_OUT` aren't deduped despite identical kind/role differences — three near-identical slot constants (Duplication, `src/node_signature.rs:216-237`)
- F-018 `link_region_variables` (`region.rs:136-149`) and `link_region`'s embedded loop (`region.rs:222-229`) duplicate the per-var phi-input append loop (Duplication, `src/region.rs:136-149, 213-231`)
- F-019 `BuiltFunctionGraph::replace_all_uses` and `Graph::replace_all_uses` are byte-identical except for the `self.graph.` prefix; documented as "back-compat after F2" (Duplication, `src/ops/rewrite.rs:22-55`)
- F-020 The five `BuiltFunctionGraph` back-compat wrappers in `ops/builder.rs` and `ops/consts.rs` are 1-line forwarders that all postdate F2 (Duplication, `src/ops/builder.rs:58-118`, `src/ops/consts.rs:103-148`)
- F-021 `nodes.rs::build_return`/`build_branch`/`build_if` each repeat the same `if !ctrl_kind.is_control() { ... } if !mem_kind.is_memory() { ... }` block (Duplication, `src/builder/nodes.rs:451-559`)
- F-022 `region.rs::require_control_kind` / `require_memory_kind` (`region.rs:53-73`) and the inline checks in `nodes.rs::build_return`/`build_branch`/`build_if` are equivalent — but the inline versions don't call the helpers (Duplication, `src/region.rs:53-73` vs `src/builder/nodes.rs:466-548`)
- F-023 `pub use ExpectedOutputKind` from `lib.rs` has zero callers outside `ir` (Dead code, `src/lib.rs:57`)
- F-024 `pub use Node, NodeInput, NodeOutput` from `node/mod.rs` have zero callers outside `ir`; their fields are `pub(crate)` so the type names alone are unusable downstream (Dead code, `src/node/mod.rs:15`)
- F-025 `NodeOutputKind::is_phi` only used in its own test (Dead code, `src/node/kind.rs:259-267`)
- F-026 `NodeOutputKind::is_control_phi` only used in `builder/nodes.rs:727`; could be `pub(crate)` (Dead code, `src/node/output_kind.rs:101-105`)
- F-027 `NodeOutputKind::as_float_or_err` is `pub` but only its own test calls it (Dead code, `src/node/output_kind.rs:84-91`)
- F-028 `Graph::node_count` is `pub` with zero callers anywhere in the workspace (Dead code, `src/graph/mod.rs:114-118`)
- F-029 `walk::GraphWalkSuccs::new` is `pub` with zero external callers (Dead code, `src/walk.rs:50-55`)
- F-030 `OutputIter::DoubleEndedIterator` / `ExactSizeIterator` and `InputIter::DoubleEndedIterator` / `ExactSizeIterator` impls are unused (Dead code, `src/iterators.rs:57-63, 126-132`)
- F-031 `ops/consts.rs::tests` has 3 tests already covered by `builder/tests.rs` (Dead code/Duplication, `src/ops/consts.rs:150-186`)
- F-032 `node/data.rs::Node::new` and `NodeInput::new` / `NodeOutput::new` are `#[must_use]`-less `pub` constructors only used by `graph/store.rs` (Dead code, `src/node/data.rs:25-67, 81-90`)
- F-033 `region.rs::TerminatedRegion`'s `region_id` field is `pub(crate)` and never read by any `crate::ir` consumer; only the constructor uses it (Simplification, `src/region.rs:42-48`)
- F-034 The `cache_key` clone path in `create_node` allocates two `Vec`s on every cacheable hit (Performance, `src/graph/store.rs:78-86`)
- F-035 `evict_cache_entry_if_cacheable` collects two new `Vec`s on every detach/update; HashMap key shape locks this in (Performance, `src/graph/store.rs:128-150`)
- F-036 `cfg_reachable` allocates a `HashSet<NodeId>` from scratch on every call; the existing `DenseEntitySet` in `walk::PreOrder` is faster but only used for general walks (Performance, `src/walk.rs:22-34`)
- F-037 Three back-compat layers — `BuiltFunctionGraph` wrapper methods on `ops/{rewrite,builder,consts}.rs` — re-borrow the inner Graph and forward; the inlined `self.graph.foo()` style is the convention everywhere else (Simplification, `src/ops/rewrite.rs:38-55`, `src/ops/builder.rs:58-118`, `src/ops/consts.rs:103-148`)
- F-038 `region.rs::require_cur_region`'s "no current region is set" error doesn't include the field's history (e.g. last-set value if any) (Readability, `src/region.rs:77-86`)
- F-039 `FunctionGraph::new_invalid` allocates an empty Graph and four reserved-id sentinels — but is never observed as "still invalid" by any consumer (Readability, `src/function.rs:25-38`)
- F-040 `BuiltFunctionGraph::from_graph_and_entry`'s docstring still cites "F2" by name, the temporary's "dummy fields are safe" claim is buried mid-paragraph (Readability, `src/function.rs:68-90`)
- F-041 `cloberred` typo in `call_cloberred_variables` field, 4 use sites in `builder/mod.rs`, 4 use sites in `builder/call.rs`, plus the local `cloberred_kinds` (Readability, `src/builder/mod.rs:106, 274-279, 335`, `src/builder/call.rs:32-44, 66`)
- F-042 `node_signature.rs::SlotRole::Sp` is the only role used by exactly one slot constant (`SP`); `R::Sp` carries no information beyond `R::Lhs` would (Readability, `src/node_signature.rs:54, 206-210`)
- F-043 `node_signature.rs::expected_signature` is a 90-line `match` with one arm per kind; works fine but the per-kind doc comments live on `node/kind.rs` (200 lines away) (Readability, `src/node_signature.rs:269-389`)
- F-044 `lib.rs:65 pub type Value = node::NodeOutputId; pub type ValueType = node::NodeOutputType;` — two type aliases with two callers each in opt; the long forms are still used in 100+ sites (Readability, `src/lib.rs:65-66`)
- F-045 `BuiltFunctionGraph` carries `call_clobbered: Box<[Vn]>` and `ret_val_regs: Box<[Vn]>` but the variants are read separately by dot rendering and pattern-matching; `ret_val_regs` is the prefix of `call_clobbered` (`builder/mod.rs:283-292`) (Simplification, `src/function.rs:45-66`)
- F-046 `node_signature.rs::SlotList::head_len` returns `head.len()` — an `inline_const` getter that the validator could just call as `sig.inputs.head.len()` directly (Simplification, `src/node_signature.rs:103-105`)
- F-047 `expected_signature_covers_every_node_kind` test enumerates 41 kinds by hand; would silently miss a new variant added without a test update (Correctness, `src/node_signature.rs:660-735`)
- F-048 `bit_width_is_eight_times_byte_size` and `is_integer_for_all_integer_output_types` tests omit U80 / F80 from their iteration lists (Correctness, `src/node/tests.rs:71-89, 110-130`)
- F-049 `type_info_table_matches_variants` test omits U80 / F80 from its `cases` array (Correctness, `src/node/tests.rs:330-353`)
- F-050 Tests reference `BUG-1`, `BUG-3`, `BUG-8`, `BUG-10`, `BUG-28`, `F2` codenames in 30+ comments without explanation (Readability, `src/builder/tests.rs`, `src/function.rs:72`, `src/graph/mod.rs:94`, `src/ops/builder.rs:2-4`, `src/ops/consts.rs:4`, `src/ops/rewrite.rs:2-4, 41-42`, `src/builder/mod.rs:147, 253, 310`)
- F-051 `pretty_label` for `BoolBinaryOp(op)` and `BoolUnaryOp(op)` produces `format!("{op:?}:bool")` — `op:?` is the Rust `Debug` impl of the enum, which is implementation-detail formatting (Readability, `src/dot/label.rs:223-224`)
- F-052 `walk::graph_walk_succs` interleaves backward-data and forward-control edges into one iterator without distinguishing them; `cfg_succs` is needed only because of this (Simplification, `src/walk.rs:73-79`)
- F-053 `pretty_label`'s `IntConst` rendering emits `format!("const {v:#x}{ty}")` where `v: u128` — for U256 IntConsts (which validate disallows but rendering doesn't) the value is u128 with high bits already lost (Correctness/Performance, `src/dot/label.rs:128-131`)

## Findings

### Correctness

### F-001 `build_int_const` panics on non-integer or U256 type

- **Category:** Correctness
- **Location:** `crates/ir/src/builder/nodes.rs:89-104`
- **What:**
  ```rust
  pub fn build_int_const(
      &mut self,
      val: impl Into<u128>,
      output_type: NodeOutputType,
  ) -> NodeOutputId {
      assert!(
          output_type.is_integer(),
          "build_int_const called with non-integer type {output_type:?}"
      );
      assert!(
          !matches!(output_type, NodeOutputType::U256),
          "build_int_const(U256) not yet supported — IntConst storage is u128"
      );
      let val = val.into() & output_type.bit_mask_u128();
      self.build_single_output_pure(NodeKind::IntConst(val), [], output_type)
  }
  ```
- **Why:** Two `assert!`s in a `pub` API contradict the project's `feedback_no_panic_unwrap` guidance ("errors must propagate via Result; never panic/unwrap/expect in normal error paths").  In-crate callers `coerce.rs::truncate_if_needed` (line 160) and `coerce.rs::extend_if_needed` (lines 188-189) thread `output_type` from arbitrary callers, so the panic is not just a contract violation hint — it propagates from a fold path triggered by real IR.  `output_type` can plausibly be `U256` if a Sleigh varnode of size 32 makes its way through `try_from(u32)` (line 218 in `output_type.rs`).
- **Proposed change:** Convert both asserts to early `Err(anyhow!(...))` returns and change the signature to `Result<NodeOutputId>`.  All in-crate callers already use `?`-propagating chains.  Update the `# Panics` doc to `# Errors`.
- **Confidence:** high
- **Risk if applied:** medium — changes the signature of a public method; ~30 call sites in `crates/opt`, `crates/strider`, `crates/pcode-lift` would need `?`.
- **Cross-refs:** F-002

### F-002 `Graph::make_int_const` accepts any `NodeOutputType` without validation

- **Category:** Correctness
- **Location:** `crates/ir/src/ops/consts.rs:63-70`
- **What:**
  ```rust
  pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType) -> Result<NodeOutputId> {
      let node = self.create_node(
          NodeKind::IntConst(u128::from(val)),
          [],
          [NodeOutputKind::OutputType(ty)],
      );
      Ok(self.node_outputs_exact::<1>(node)?[0])
  }
  ```
- **Why:** `Graph::make_int_const` does **not** check that `ty` is integer or that it isn't `U256`, while `FunctionBuilder::build_int_const` (F-001) panics on both.  Two `pub` methods named `make_int_const` / `build_int_const` with different validation contracts — same conceptual operation, different layers, opposite failure modes.
  Calling `Graph::make_int_const(0, NodeOutputType::F32)` is currently an error caught only by the validator at `build()` time (Layer A's kind mismatch on the Output slot — `IntConst`'s output sig is `INT_VAL = AnyInt`, F32 is `AnyFloat`).  A test that mutates the graph and re-runs analysis without re-validating could silently miscompile.
- **Proposed change:** Either (a) align the contracts: have both validate `ty.is_integer()` and not-U256, returning `Err` on violation; or (b) make `Graph::make_int_const` private (used only by tests).  Workspace-wide grep confirms no external caller — only `crates/ir/src/ops/consts.rs:127` (the `BuiltFunctionGraph` wrapper) and tests.
- **Confidence:** high
- **Risk if applied:** low — internal call site only.
- **Cross-refs:** F-001

### F-003 `Graph::int_const_val` silently narrows U128/U256 to `Option<u64>`

- **Category:** Correctness
- **Location:** `crates/ir/src/ops/consts.rs:17-27`
- **What:**
  ```rust
  pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64> {
      let ty = self.output_kind(out).as_value()?;
      if !ty.is_integer() {
          return None;
      }
      match *self.kind_of_output(out) {
          // IntConst stores u128; narrow to u64 for callers that only need <=64-bit values.
          NodeKind::IntConst(v) => ty.get_unsigned_int(v).and_then(|w| u64::try_from(w).ok()),
          _ => None,
      }
  }
  ```
- **Why:** A `U128 IntConst` whose value is `0` returns `Some(0)`; same const with value `1<<64` returns `None`.  Callers that branch `if g.int_const_val(...) == Some(c)` cannot distinguish "constant exists but doesn't fit in u64" from "not a constant at all".  The doc comment doesn't document the narrowing failure mode — only that the value is "masked to its declared type".
  CLAUDE.md confirms the underlying `NodeOutputType::get_unsigned_int(u128) -> Option<u128>` returns the full-precision value; this wrapper deliberately throws away the high bits.
  No `int_const_val_u128` / `int_const_val_i128` companion exists.
- **Proposed change:** Add `int_const_val_u128(out) -> Option<u128>` (and `_i128`) as the canonical accessors; have `int_const_val` either be removed or kept for the narrow-fast-path with a panicking-or-`Result` variant for "value didn't fit".  Document the narrowing.
- **Confidence:** high
- **Risk if applied:** medium — adds API surface; existing `int_const_val` callers in `crates/opt` (e.g. `constant_fold`, `known_bits`) use the u64 form pervasively.

### F-004 `region_entry_control` / `region_entry_memory` fall back to a fake `NodeOutputId::from_u32(0)`

- **Category:** Correctness
- **Location:** `crates/ir/src/region.rs:293-328`
- **What:**
  ```rust
  pub fn region_entry_control(&self, region: RegionId) -> Result<NodeOutputId> {
      let cs_id = self.regions[region].control_node;
      let outputs: Vec<NodeOutputId> = self.graph().node_outputs(cs_id).into_iter().collect();
      for &out in &outputs {
          if self.graph().output_kind(out).is_control() {
              return Ok(out);
          }
      }
      let first = *outputs.first().unwrap_or(&NodeOutputId::from_u32(0));
      let kind = self.graph().output_kind(first);
      Err(anyhow!(
          "output {first:?} is not a control edge (got {kind:?})"
      ))
  }
  ```
- **Why:** When `outputs` is empty (a malformed `ControlState` with no outputs at all), the `unwrap_or(&NodeOutputId::from_u32(0))` returns id 0 — which is **a real, live `NodeOutputId`** in the arena (typically `Entry`'s Control output, since Entry is built first).  The subsequent `self.graph().output_kind(NodeOutputId::from_u32(0))` indexes that real output and reports its kind.  The error message reads, e.g., `"output NodeOutputId(0) is not a control edge (got Control)"` — a self-contradiction.  `region_entry_memory` has the identical pattern (line 318).
  Practical risk: low — only fires when the graph is structurally broken.  But the report is misleading enough to hide bugs.
- **Proposed change:** Drop the fake-id dance.  Just `bail!("region {region:?} ControlState {cs_id:?} has no outputs")` when `outputs.is_empty()`, otherwise mention the actual offending output id.
- **Confidence:** high
- **Risk if applied:** low

### F-005 Layer A's `>= head_len()` rule trivially passes `ControlState`; zero-pred reachability check lives only in Layer C

- **Category:** Correctness
- **Location:** `crates/ir/src/validate/layer_a.rs:28-45`, `crates/ir/src/validate/layer_c.rs:49-84`
- **What:**
  ```rust
  // layer_a.rs (28-45)
  let input_arity_ok = if sig.inputs.is_variadic() {
      actual_inputs.len() >= input_head_len
  } else {
      actual_inputs.len() == input_head_len
  };
  if !input_arity_ok {
      errs.push(ValidationError::NodeInputCountMismatch {
          node, expected: input_head_len, actual: actual_inputs.len(),
      });
  }
  ```
  ```rust
  // node_signature.rs:307
  NodeKind::ControlState => sig!(inputs: []; in_tail: CTRL, outputs: [CTRL, PHI]),
  ```
- **Why:** `ControlState`'s signature is `head: []` + `tail: Some(CTRL)`.  Layer A's variadic rule allows `actual.len() >= 0` — which always holds.  A zero-input ControlState never trips Layer A's input-count check.  Layer C catches it explicitly (`layer_c.rs:64`), but only when reachable.
  This is **structurally correct** (Layer C scopes to reachable, Layer A scopes to reachable, and the only zero-input ControlState case the codebase produces is RedundantPhis zombies which are unreachable), but the asymmetry is undocumented and there is no test that exercises Layer A's `head_len = 0 + variadic + 0 inputs` corner.
- **Proposed change:** Document the contract:  Layer A's variadic-arity rule cannot enforce a non-zero floor when `head_len = 0`; the per-kind shape rule for ControlState (≥ 1 predecessor when reachable) lives in Layer C.  Add a dedicated test exercising the rule with a reachable empty ControlState.
- **Confidence:** medium
- **Risk if applied:** low (doc + test).

### F-006 `Inputs::index` and `Outputs::index` panic on out-of-bounds; only `Inputs::index` notes it

- **Category:** Correctness
- **Location:** `crates/ir/src/iterators.rs:34-40, 99-106`
- **What:**
  ```rust
  // Outputs::index, line 36-39
  fn index(&self, index: usize) -> &Self::Output {
      &self.0[index]
  }

  // Inputs::index, line 102-105
  fn index(&self, index: usize) -> &Self::Output {
      // Prefer `.get(index)` with proper error handling in production code.
      &self.graph.inputs[self.use_list[index]].output_id
  }
  ```
- **Why:** `Outputs::index` panics on OOB just like `Inputs::index`, but only `Inputs::index` carries the "prefer `.get()` with proper error handling" note.  Both are `Index` impls — by convention OOB panics — but the inconsistent commenting suggests one was reviewed and the other wasn't.
- **Proposed change:** Either drop the comment from `Inputs::index` (consistency with std `Index<Vec<_>>`) or add the same comment to `Outputs::index`.
- **Confidence:** high
- **Risk if applied:** low (comment only).

### F-007 `as_value_or_err` etc. are `#[track_caller]` but error messages don't show the caller

- **Category:** Correctness
- **Location:** `crates/ir/src/node/output_kind.rs:45-90`
- **What:**
  ```rust
  #[track_caller]
  pub fn as_value_or_err(self) -> crate::Result<NodeOutputType> {
      self.as_value()
          .ok_or_else(|| anyhow!("expected value output, got {self:?}"))
  }
  ```
- **Why:** `#[track_caller]` adds a hidden `#[track_caller_location]` arg passed through, but `anyhow!` doesn't capture / format it — the caller's `file:line` is lost in the rendered error.  `track_caller` here is decorative; if the team wants caller info in the error, it needs `Location::caller()` interpolated into the message.
- **Proposed change:** Either drop `#[track_caller]` from `as_value_or_err`, `as_integer_or_err`, `as_float_or_err` (pure cosmetic noise), or interpolate `core::panic::Location::caller()` into the anyhow message.
- **Confidence:** high
- **Risk if applied:** low

### F-008 97 doc references to `crate::ErrorKind::*` link types deleted by the anyhow migration

- **Category:** Correctness
- **Location:** Multiple files; full list below
- **What:** Every fallible method in the crate documents its error variants via `[\`ErrorKind::Foo\`]` links.  The `crate::ErrorKind` enum no longer exists (all errors now propagate via `anyhow::Result`).  Sample:
  ```rust
  /// # Errors
  ///
  /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is a control,
  /// memory, or control-phi edge.
  pub fn get_output_type(&self, output_id: NodeOutputId) -> Result<NodeOutputType> { ... }
  ```
  Counts by file (97 total):
  - `src/builder/nodes.rs` — 36
  - `src/builder/coerce.rs` — 14
  - `src/builder/call.rs` — 6
  - `src/builder/vars.rs` — 6
  - `src/builder/mod.rs` — 3
  - `src/region.rs` — 6
  - `src/graph/access.rs` — 3
  - `src/node/output_kind.rs` — 7
  - `src/ops/consts.rs` — 3
  - `src/ops/builder.rs` — 1
  - `src/ops/rewrite.rs` — 1
  - Other tests/comments — 11
- **Why:** Every one of these renders as a broken `rustdoc` intra-doc link (or silently the wrong link if there happens to be a same-named external `ErrorKind`).  Readers chasing the type land nowhere.  Same drift the cfg/opt/strider reviews flagged — the ir crate has the highest count.
- **Proposed change:** Replace every `[\`ErrorKind::Foo\`]` reference with plain prose ("Returns an error when ...") matching the actual `anyhow!` message.
- **Confidence:** high
- **Risk if applied:** low — comment-only change.

### F-009 `region.rs:177, 250` and `builder/mod.rs:372` cite the deleted `crate::ir_cache::RegionIrCache`

- **Category:** Correctness
- **Location:** `crates/ir/src/region.rs:177, 250-251`, `crates/ir/src/builder/mod.rs:372-374`
- **What:**
  ```rust
  // region.rs:249-256
  /// Returns the entry-boundary `ControlState` `NodeId` of `region`.
  /// Used by the strider [`crate::ir_cache::RegionIrCache`] (in the
  /// `strider` crate) to pin per-region phi-extension targets across
  /// orchestrator iterations.
  #[must_use]
  pub fn region_control_node(&self, region: RegionId) -> NodeId {
      self.regions[region].control_node
  }

  // builder/mod.rs:371-379
  /// Returns the [`rsleigh::Vn`] tracked at the given [`VarId`], or
  /// `None` if `var_id` is not in the variable map.  Used by the
  /// strider [`crate::ir_cache::RegionIrCache`] to convert per-region
  /// `(VarId, NodeOutputId)` pairs (returned by
  /// [`Self::region_initial_variables`]) into the `Vn`-keyed maps
  /// the cache stores.
  ```
- **Why:** CLAUDE.md and the strider review both confirm the strider restructure deleted `RegionIrCache`.  The actual consumers today are `crates/strider/src/strider/pipeline.rs:326-356` (the `RegionIndex`).  The `crate::ir_cache::RegionIrCache` link cannot resolve — `crate` here is `ir`, which never had an `ir_cache` module.
- **Proposed change:** Replace with "Used by the strider crate's per-iteration region index" or drop the consumer reference entirely (the method is independently useful).
- **Confidence:** high
- **Risk if applied:** low.

### F-010 `NodeKind::CastToInt` doc says "Reinterpret an integer value as `Bool`"

- **Category:** Correctness
- **Location:** `crates/ir/src/node/kind.rs:121`
- **What:**
  ```rust
  /// Reinterpret an integer value as `Bool` (`0` → `false`, non-zero → `true`).
  CastToInt,
  ```
- **Why:** This describes `CastToBool`, not `CastToInt`.  `CastToInt` is "any value → integer" — the IR's polymorphic int cast (its signature is `inputs: [ANY_VAL], outputs: [INT_VAL]` per `node_signature.rs:351`).  `CastToBool` is the inverse direction (line 140 of the same file).
  A reader reading this doc and using `CastToInt` for a "convert int to bool" rewrite would emit a graph that fails Layer A.
- **Proposed change:** Replace with: `/// Cast any value (int / bool / float) to an integer of the declared output type.`
- **Confidence:** high
- **Risk if applied:** low.

### F-011 `lib.rs` documents `NodeOutputType` as `Bool, U8/U16/U32/U64/U128/U256, F32/F64` — omits U80/F80

- **Category:** Correctness
- **Location:** `crates/ir/src/lib.rs:36-37`
- **What:**
  ```rust
  //! - [`node::NodeOutputType`] — `Bool`, integers `U8`/`U16`/`U32`/`U64`/`U128`/`U256`,
  //!   floats `F32`/`F64`
  ```
- **Why:** The actual enum (`output_type.rs:9-35`) has 11 variants including `U80` and `F80`.  CLAUDE.md also uses the abbreviated list (it's been outdated since U80/F80 was added to support x87 80-bit float register aliasing).  A user reading this expects an integer model that doesn't include the 80-bit ST*/Tx slots that are very real on x86 binaries.
- **Proposed change:** Replace with `Bool, integers U8/U16/U32/U64/U80/U128/U256, floats F32/F64/F80`.
- **Confidence:** high
- **Risk if applied:** low.

### F-012 `ExpectedOutputKind::AnyInt` doc misses U80

- **Category:** Correctness
- **Location:** `crates/ir/src/node_signature.rs:33-34`
- **What:**
  ```rust
  /// Any integer-typed value (U8, U16, U32, U64, U128, U256).
  AnyInt,
  ```
- **Why:** Same drift as F-011.  `AnyInt` matches via `t.is_integer()` (`validate/mod.rs:94`), and `is_integer()` returns `true` for `U80` (`output_type.rs:117`).  The doc lists 6 widths; the actual matcher accepts 7.
- **Proposed change:** Add `U80` to the list, in declaration order: `(U8, U16, U32, U64, U80, U128, U256)`.
- **Confidence:** high
- **Risk if applied:** low.

### F-013 `bit_mask_u128` returns `u128::MAX` for U256; downstream `get_unsigned_int(_, U256)` quietly returns `Some(val)` instead of `None`

- **Category:** Correctness
- **Location:** `crates/ir/src/node/output_type.rs:144-176`
- **What:**
  ```rust
  /// `U256` returns `u128::MAX` as a best-effort sentinel — this method is
  /// not meaningful for U256 and the IntConst path panics for U256 today;
  /// callers that genuinely need U256 must be revisited when U256 support
  /// is added.  Float types return `0` (defensive — no caller should ask).
  pub fn bit_mask_u128(self) -> u128 {
      if self.is_bool() { return 1; }
      let bits = self.bit_width();
      if bits == 0 || !self.is_integer() { return 0; }
      if bits >= 128 { return u128::MAX; }
      (1u128 << bits) - 1
  }

  pub fn get_unsigned_int(self, val: u128) -> Option<u128> {
      if !self.is_integer() { return None; }
      Some(val & self.bit_mask_u128())
  }
  ```
- **Why:** The two methods together silently support U256 with truncated semantics rather than rejecting it: `U256.get_unsigned_int(val) = Some(val & u128::MAX) = Some(val)`.  A caller that thought "U256 is unsupported, this returns None" gets `Some(val)` instead.  Conversely, `bit_mask_u128(U256) = u128::MAX` invites the caller to write `int_val & U256.bit_mask_u128()` and silently get a 128-bit truncation of a 256-bit value.
  The doc comment (line 145-148) is honest: "best-effort sentinel".  But the contract shape is fragile.  The build_int_const path's `assert!` (F-001) is the only thing currently keeping U256 IntConsts out — `Graph::make_int_const` doesn't enforce the same.
- **Proposed change:** Have `bit_mask_u128(U256) = 0` (matching the float defensive case), or have it return `Result`.  Have `get_unsigned_int(U256, _) = None` to match the doc claim.
- **Confidence:** medium
- **Risk if applied:** low — no current consumer relies on the U256 paths producing useful results, but a Workspace-wide grep would be needed to confirm.

### F-014 `extend_if_needed` SignExtend comment claims "reinterpret bits as u128 (sign-extended to i128 then cast)"

- **Category:** Correctness/Readability
- **Location:** `crates/ir/src/builder/coerce.rs:185-189`
- **What:**
  ```rust
  if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? {
      return Ok(match op {
          // signed_val is i64; reinterpret bits as u128 (sign-extended to i128 then cast)
          ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
          ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
      });
  }
  ```
- **Why:** Rust's `i64 as u128` does sign-extend the i64 to fill the upper 64 bits, then reinterprets the bit pattern as u128.  The comment's "sign-extended to i128 then cast" is technically wrong: there's no intermediate i128 step.  The result is correct (the bit pattern matches what an i128 → u128 cast would produce), but a future reader following the comment to "do the same with i64" might write `signed_val as i128 as u128` and get the same result — or write `signed_val.cast_unsigned()` and get a different result.
- **Proposed change:** Rewrite as `// signed_val is i64; sign-extending it to u128 fills the high 64 bits; build_int_const masks to output_type's width.`
- **Confidence:** medium
- **Risk if applied:** low (comment only).

### F-015 `extend_if_needed` returns `Ok(value_id)` early when "wider input → narrower target", silently producing a wrong-typed result

- **Category:** Correctness
- **Location:** `crates/ir/src/builder/coerce.rs:177-210`
- **What:**
  ```rust
  pub fn extend_if_needed(
      &mut self,
      output_id: NodeOutputId,
      output_type: NodeOutputType,
      op: ExtendOp,
  ) -> Result<NodeOutputId> {
      let curr_output_type = self.get_output_type(output_id)?;

      if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? { ... }

      if !output_type.is_integer() {
          return Err(anyhow!("output {output_id:?} is not an integer value"));
      }

      // Non-integer input (Bool / Float) into an integer extend ...
      if !curr_output_type.is_integer() {
          return self.convert_to_int_if_needed(output_id, output_type);
      }

      if curr_output_type.byte_size() >= output_type.byte_size() {
          return Ok(output_id);
      }
      Ok(self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type))
  }
  ```
- **Why:** When `curr_output_type.byte_size() >= output_type.byte_size()` the function returns the *original* `output_id` whose declared type is `curr_output_type` — wider than the requested `output_type`.  The caller asked for "extend to U16" and got back a value declared as U64.  Misleadingly named: not "no-op when already wide enough" but "no-op when already at-least-as-wide".
  A caller relying on the returned id's type matching `output_type` builds an IR with a width mismatch.  The validator catches it eventually (Layer A on the consumer), but the symptom is far from the cause.
- **Proposed change:** Either (a) rename to `extend_or_keep_wider_if_needed` and document the asymmetry, (b) emit a `Truncate` when curr > target so the type always matches, or (c) bail when curr > target.
- **Confidence:** medium
- **Risk if applied:** medium — would change widening semantics in pcode-lift / opt callers.

### F-016 `Layer C` uniqueness checks scan **all** nodes (including detached zombies); a zombie Entry would falsely fire

- **Category:** Correctness
- **Location:** `crates/ir/src/validate/layer_c.rs:16-45`
- **What:**
  ```rust
  pub(super) fn check_layer_c_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
      let mut entries: Vec<NodeId> = Vec::new();
      let mut initial_memories: Vec<NodeId> = Vec::new();

      for node in graph.nodes.keys() {  // scans ALL nodes, not just reachable ones
          match graph.node_kind(node) {
              NodeKind::Entry => entries.push(node),
              NodeKind::InitialMemory => initial_memories.push(node),
              _ => {}
          }
      }
      ...
  }
  ```
- **Why:** `validate.rs:50-76` scopes Layer A to reachable nodes (via `walk_graph`) but Layer C uniqueness scans `graph.nodes.keys()` — every node ever allocated, including detached zombies.  Today no opt pass creates a second `Entry` (since `build_entry` resets the graph and is called once), but there's nothing in the type system preventing it.  An optimizer that mistakenly synthesises an `Entry` and detaches it would trip `MultipleEntryNodes` with one of the two ids being a non-reachable zombie.
  More subtle: the `MultipleEntryNodes { first, second }` payload reports the first two by allocation order, so the offending zombie may be `first`, leaving the report pointing at a zombie while the live Entry is `second`.
- **Proposed change:** Pass `reachable: &HashSet<NodeId>` into `check_layer_c_uniqueness` and skip non-reachable nodes (mirroring Layer A's reachability scoping).
- **Confidence:** medium
- **Risk if applied:** low.

### F-017 `node_signature.rs` slot constants `ARG`/`RET`/`CALL_OUT` are near-identical

- **Category:** Duplication & unification
- **Location:** `crates/ir/src/node_signature.rs:216-237`
- **What:**
  ```rust
  const ARG: Slot = Slot {
      kind: AnyValue,
      name: "arg",
      role: R::Arg,
  };
  const RET: Slot = Slot {
      kind: AnyValue,
      name: "ret",
      role: R::Ret,
  };
  /// Call output tail: clobbered-register outputs. ...
  const CALL_OUT: Slot = Slot {
      kind: AnyValue,
      name: "val",
      role: R::Val,
  };
  ```
- **Why:** Three constants identical except for `name` and `role`.  Name + role drive the dot-rendering edge label and color.  The actual semantic narrow — "this is an arg vs a ret vs a clobbered output" — is encoded in the role.  But the role alone is enough; `name` is derivable from role (and vice versa).
- **Proposed change:** Either extract a `slot_for_role(role: SlotRole) -> Slot` helper (using a static role→name map) or accept the redundancy and add a comment explaining why the three exist.
- **Confidence:** medium
- **Risk if applied:** low (refactor only).

### F-018 `link_region_variables` and `link_region`'s embedded loop duplicate the per-var phi-input append

- **Category:** Duplication
- **Location:** `crates/ir/src/region.rs:136-149, 213-231`
- **What:**
  ```rust
  // 136-149
  pub(crate) fn link_region_variables(
      &mut self,
      region: RegionId,
      variables: &SecondaryMap<VarId, NodeOutputId>,
  ) -> Result<()> {
      for var_id in variables.keys() {
          let region_variable_output_id = self.regions[region].initial_variables[var_id];
          let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
          let current_variable = variables[var_id];
          self.graph_mut()
              .add_node_input(region_variable_id, current_variable)?;
      }
      Ok(())
  }

  // 213-231 (in link_region)
  for var_id in self.regions[region].variables.keys() {
      let region_variable_output_id = self.regions[region].initial_variables[var_id];
      let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
      let current_variable = self.regions[cur_region].variables[var_id];
      self.graph_mut()
          .add_node_input(region_variable_id, current_variable)?;
  }
  ```
- **Why:** The two loops are identical except for where the "current_variable" comes from (`variables[var_id]` vs `self.regions[cur_region].variables[var_id]`).  The first form takes a `&SecondaryMap`; the second iterates the destination's own variables (an awkward "I want the same keys" workaround).
- **Proposed change:** Have `link_region` call `link_region_variables(region, &self.regions[cur_region].variables.clone())` (after a careful borrow refactor) — or extract a helper `link_phi_inputs(&mut self, region: RegionId, source: impl Fn(VarId) -> NodeOutputId) -> Result<()>` that both call.
- **Confidence:** high
- **Risk if applied:** low — internal-only.

### F-019 `BuiltFunctionGraph::replace_all_uses` is a 1-line wrapper around `Graph::replace_all_uses`

- **Category:** Duplication
- **Location:** `crates/ir/src/ops/rewrite.rs:38-55`
- **What:**
  ```rust
  impl BuiltFunctionGraph {
      /// Back-compat wrapper around [`Graph::replace_all_uses`].
      ///
      /// Kept so existing callers that hold a `BuiltFunctionGraph` continue to
      /// compile after F2's `Optimizer` trait refactor moved the canonical
      /// definition onto `Graph`.
      ///
      /// # Errors
      ///
      /// Propagates [`Graph::replace_all_uses`].
      pub fn replace_all_uses(
          &mut self,
          old: NodeOutputId,
          new_val: NodeOutputId,
      ) -> Result<bool> {
          self.graph.replace_all_uses(old, new_val)
      }
  }
  ```
- **Why:** Six lines of indirection plus a doc comment to forward to `self.graph.replace_all_uses(...)` — which any caller can call directly.  Workspace-wide grep finds 4 callers using `fg.replace_all_uses(...)` (in opt) and 6 using `fg.graph.replace_all_uses(...)` (also opt + pattern).  The wrapper is keeping a 4-call surface alive at the cost of a documented "F2 back-compat" caveat that becomes load-bearing for future readers.
- **Proposed change:** Remove the wrapper.  Update the 4 callers to use `fg.graph.replace_all_uses(old, new_val)`.
- **Confidence:** high
- **Risk if applied:** low — change touches `crates/opt`, but is mechanical.
- **Cross-refs:** F-020

### F-020 Five `BuiltFunctionGraph` back-compat wrappers in `ops/builder.rs` + `ops/consts.rs` — same shape

- **Category:** Duplication
- **Location:** `crates/ir/src/ops/builder.rs:58-118`, `crates/ir/src/ops/consts.rs:103-148`
- **What:** `BuiltFunctionGraph::{make_value_node, make_int_bits_to_float_node, make_float_to_float_node, int_const_val, bool_const_val, float_const_val, make_int_const, make_bool_const, make_float_const}` are all 1-line forwarders to `self.graph.<same-name>(...)`.  Each block carries a `/// Back-compat wrapper around [\`Graph::foo\`]` doc.
- **Why:** Same as F-019 — the F2 refactor moved canonical definitions onto `Graph`, then preserved the old `BuiltFunctionGraph` surface verbatim.  The duplicated documentation invites the same maintenance question every time someone refactors a call site.
- **Proposed change:** Remove all five wrappers in one batch; convert the (small number of) callers to `fg.graph.foo(...)`.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-019

### F-021 `nodes.rs::build_return`/`build_branch`/`build_if` repeat the same control/memory kind check

- **Category:** Duplication
- **Location:** `crates/ir/src/builder/nodes.rs:466-559`
- **What:**
  ```rust
  // build_return (466-481)
  let ctrl_kind = self.graph().output_kind(res.control);
  if !ctrl_kind.is_control() {
      return Err(anyhow!(
          "output {:?} is not a control edge (got {:?})",
          res.control, ctrl_kind
      ));
  }
  let mem_kind = self.graph().output_kind(res.memory);
  if !mem_kind.is_memory() {
      return Err(anyhow!(
          "output {:?} is not a memory edge (got {:?})",
          res.memory, mem_kind
      ));
  }
  // ... same block in build_branch (502-517)
  // ... same block (minus mem) in build_if (542-548)
  ```
- **Why:** `region.rs:53-73` has `require_control_kind` / `require_memory_kind` helpers that do exactly this check, but `nodes.rs` doesn't use them; instead each of `build_return`/`build_branch`/`build_if` inlines the same anyhow-format invocation.
- **Proposed change:** Replace each inline check with `self.require_control_kind(res.control)?;` / `self.require_memory_kind(res.memory)?;`.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-022

### F-022 `region.rs::require_control_kind` / `require_memory_kind` are not used by `builder/nodes.rs`

- **Category:** Duplication / Simplification
- **Location:** `crates/ir/src/region.rs:53-73` vs `crates/ir/src/builder/nodes.rs:466-548`
- **What:** The helpers exist on `FunctionBuilder` (via `region.rs`'s `impl FunctionBuilder`).  But `nodes.rs` (also `impl FunctionBuilder`) inlines the equivalent check 3 times, with formatting-string drift between sites (some say `"output {:?} is not a control edge (got {:?})"`, others — same behaviour but the one in `region.rs` uses different `output_id` interpolation order).
- **Proposed change:** Same as F-021 — call the helpers instead of inlining.  The `region.rs` helpers should become the single source of truth; the inline checks are a sign someone wrote `nodes.rs` first and forgot to come back to it after `region.rs` factored the helper out.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-021

### Dead code

### F-023 `pub use ExpectedOutputKind` from `lib.rs` has zero callers outside `ir`

- **Category:** Dead code
- **Location:** `crates/ir/src/lib.rs:57`
- **What:**
  ```rust
  pub use node_signature::ExpectedOutputKind;
  ```
- **Why:** Workspace-wide `grep ExpectedOutputKind crates/` shows only `crates/ir/` hits.  No external consumer references the type.  `expected_signature` itself is `pub(crate)`, so a downstream user can't get a `Signature` to inspect — `ExpectedOutputKind` is type information that doesn't flow out of the crate.
- **Proposed change:** Drop the `pub use`; mark `ExpectedOutputKind` as `pub(crate)` so it can't accidentally drift into the public API.  Same for the rest of the `node_signature` types (`Slot`, `SlotList`, `Signature`, `SlotRole`) — none of these are used outside `ir`.
- **Confidence:** high
- **Risk if applied:** low.

### F-024 `pub use Node, NodeInput, NodeOutput` from `node/mod.rs` have zero callers outside `ir`

- **Category:** Dead code
- **Location:** `crates/ir/src/node/mod.rs:15`
- **What:**
  ```rust
  pub use data::{Node, NodeInput, NodeOutput};
  ```
- **Why:** All three structs have `pub(crate)` fields.  The constructors `Node::new` / `NodeInput::new` / `NodeOutput::new` are `pub` but only called from `crate::graph::store/uses`.  Downstream users can name the types but can't construct them or read their internals.  Dead public API surface.
- **Proposed change:** Make `pub(crate) use data::{...}`.  Drop `pub use` from `node/mod.rs:15`.
- **Confidence:** high
- **Risk if applied:** low.

### F-025 `NodeKind::is_phi` only used in its own test

- **Category:** Dead code
- **Location:** `crates/ir/src/node/kind.rs:259-267`
- **What:**
  ```rust
  /// Returns `true` for any phi node kind: [`NodeKind::ControlPhi`],
  /// [`NodeKind::MemPhi`], [`NodeKind::StackStorePhi`], or
  /// [`NodeKind::ValuePhi`].
  #[inline]
  #[must_use]
  pub fn is_phi(&self) -> bool {
      matches!(...)
  }
  ```
- **Why:** Workspace-wide `grep '\.is_phi()'` hits only `crates/ir/src/node/tests.rs:316-327` — its own test module.  The 4-variant `matches!` expression is short enough that callers prefer to write it inline.
- **Proposed change:** Remove `is_phi` and its test.  If a future caller needs it, they can re-add.
- **Confidence:** high
- **Risk if applied:** low.

### F-026 `NodeOutputKind::is_control_phi` is `pub` but only used internally

- **Category:** Dead code
- **Location:** `crates/ir/src/node/output_kind.rs:101-105`
- **What:**
  ```rust
  #[inline]
  #[must_use]
  pub fn is_control_phi(self) -> bool {
      self == Self::ControlPhi
  }
  ```
  Single caller: `crates/ir/src/builder/nodes.rs:727`.
- **Why:** No external consumer; the inline `self == Self::ControlPhi` is shorter than the helper.
- **Proposed change:** Either inline the comparison at the one call site or mark the helper `pub(crate)`.
- **Confidence:** high
- **Risk if applied:** low.

### F-027 `NodeOutputKind::as_float_or_err` is `pub` with zero non-test callers

- **Category:** Dead code
- **Location:** `crates/ir/src/node/output_kind.rs:84-91`
- **What:**
  ```rust
  #[track_caller]
  pub fn as_float_or_err(self) -> crate::Result<NodeOutputType> {
      let ty = self.as_value_or_err()?;
      if ty.is_float() {
          Ok(ty)
      } else {
          Err(anyhow!("type {ty:?} is not a float type"))
      }
  }
  ```
- **Why:** `grep` finds only the definition + test (`node/tests.rs:290-303`).  `as_value_or_err` and `as_integer_or_err` ARE used externally (opt + pattern).  `as_float_or_err` is the symmetric partner that nobody asked for.
- **Proposed change:** Remove `as_float_or_err` and its test, or note in a comment that it's kept symmetric for future callers.
- **Confidence:** high
- **Risk if applied:** low.

### F-028 `Graph::node_count` is `pub` with zero callers anywhere in the workspace

- **Category:** Dead code
- **Location:** `crates/ir/src/graph/mod.rs:114-118`
- **What:**
  ```rust
  /// Returns the total number of nodes ever allocated in the graph.
  ///
  /// Stable across `Clone` — detached zombies count toward the total
  /// because the entity arena never reclaims slots.
  #[inline]
  #[must_use]
  pub fn node_count(&self) -> usize {
      self.nodes.len()
  }
  ```
- **Why:** Workspace-wide `grep node_count\b` finds only `crates/cfg/...` callers, but those use `cfg.graph.node_count()` where `cfg.graph` is a `petgraph::StableDiGraph`, not `ir::Graph`.  No call site references `ir::Graph::node_count`.
- **Proposed change:** Delete the method.  If a future caller needs it, the trivial body is one-liner-recoverable.
- **Confidence:** high
- **Risk if applied:** low.

### F-029 `walk::GraphWalkSuccs::new` is `pub` with zero external callers

- **Category:** Dead code
- **Location:** `crates/ir/src/walk.rs:50-55`
- **What:**
  ```rust
  impl<'a> GraphWalkSuccs<'a> {
      /// Wraps `graph` in a `GraphWalkSuccs` adaptor.
      #[inline]
      #[must_use]
      pub fn new(graph: &'a Graph) -> Self {
          Self(graph)
      }
  }
  ```
- **Why:** `grep` finds only the in-crate call at `walk.rs:119` (in `walk_graph`).  No downstream user reaches into `GraphWalkSuccs::new`.
- **Proposed change:** Make `pub(crate)`.  Same recommendation for `GraphWalk` type alias (line 109) and `pub fn cfg_outputs` if not externally used.
- **Confidence:** high
- **Risk if applied:** low.

### F-030 `OutputIter` / `InputIter` `DoubleEndedIterator` and `ExactSizeIterator` impls are unused

- **Category:** Dead code
- **Location:** `crates/ir/src/iterators.rs:57-63, 126-132`
- **What:**
  ```rust
  impl DoubleEndedIterator for OutputIter<'_> {
      fn next_back(&mut self) -> Option<Self::Item> { ... }
  }

  impl ExactSizeIterator for OutputIter<'_> {}

  impl DoubleEndedIterator for InputIter<'_> {
      fn next_back(&mut self) -> Option<Self::Item> { ... }
  }

  impl ExactSizeIterator for InputIter<'_> {}
  ```
- **Why:** Workspace-wide grep for `.rev()` / `.next_back` over `node_inputs` / `node_outputs` returns nothing.  `.len()` is called via `Inputs::len` / `Outputs::len` directly (which delegate to slice lengths), not via the iterator's `ExactSizeIterator` trait.  The 4 impls are speculative API.
- **Proposed change:** Drop the impls.  Re-add when a real call site appears.
- **Confidence:** medium
- **Risk if applied:** low.

### F-031 `ops/consts.rs::tests` covers what `builder/tests.rs` already does

- **Category:** Dead code / Duplication
- **Location:** `crates/ir/src/ops/consts.rs:150-186`
- **What:** Three smoke tests (`make_and_read_int_const`, `make_and_read_bool_const`, `make_and_read_float_const`) test exactly what `builder/tests.rs:309-356` tests via builder helpers.
- **Why:** Both test paths run `FunctionBuilder::new_raw → make_*_const → assert kind`.  `builder/tests.rs` exercises through the builder; `ops/consts.rs` exercises the `BuiltFunctionGraph` wrapper — but the wrapper is itself a 1-line forwarder (F-020), so the only real coverage is "the wrapper compiles".
- **Proposed change:** Remove the test module if F-020 is applied (the wrappers go away); otherwise keep one combined wrapper-coverage test instead of three.
- **Confidence:** medium
- **Risk if applied:** low.

### Simplification

### F-032 `node/data.rs::Node::new` and friends are `pub` but only used in `graph/store.rs`

- **Category:** Dead code
- **Location:** `crates/ir/src/node/data.rs:25-67, 81-90`
- **What:** Three `pub fn new` constructors (`Node::new`, `NodeInput::new`, `NodeOutput::new`) used only by `graph/store.rs` and `graph/uses.rs`.
- **Why:** Same as F-024 — fields are `pub(crate)`, so external consumers can't construct or modify these directly.  `pub fn new` is unhelpful to them.
- **Proposed change:** Mark `pub(crate)` to match field visibility.
- **Confidence:** high
- **Risk if applied:** low.

### F-033 `region.rs::TerminatedRegion`'s `region_id` field is internal but `pub(crate)`

- **Category:** Simplification
- **Location:** `crates/ir/src/region.rs:42-48`
- **What:**
  ```rust
  pub(crate) struct TerminatedRegion {
      pub(crate) control: NodeOutputId,
      pub(crate) memory: NodeOutputId,
      pub(crate) region_id: RegionId,
  }
  ```
- **Why:** Used only by `nodes.rs::build_branch` / `build_if` to thread `region_id` into `link_region` (which already takes `cur_region: RegionId`).  Construction and consumption sites are both inside the same impl chain, so the struct is purely a return-value pack.
- **Proposed change:** Could be a tuple `(NodeOutputId, NodeOutputId, RegionId)` or even left as a struct.  No real action; flag for context.
- **Confidence:** low
- **Risk if applied:** low.

### Performance

### F-034 `create_node` allocates two `Vec`s on every cacheable hit

- **Category:** Performance
- **Location:** `crates/ir/src/graph/store.rs:78-86`
- **What:**
  ```rust
  let cache_key = if kind.is_cacheable() {
      let key = (node, inputs.to_vec(), output_kinds.to_vec());
      if let Some(node_id) = self.node_to_id.get(&key) {
          return *node_id;
      }
      Some(key)
  } else {
      None
  };
  ```
- **Why:** Hot path — `create_node` is called hundreds of times per IR build and an unbounded number of times during opt iteration.  When the key already exists in the cache (the common case for arithmetic constant folding and for re-visiting the same `IntConst(0)`/`BoolConst(false)`), the two `Vec` allocations from `to_vec()` happen, the lookup succeeds, and both Vecs are dropped.  The `key` is built from `SmallVec<[_; 4]>` slices (line 72-73), so for nodes with ≤4 inputs the SmallVec wouldn't allocate, but `.to_vec()` always heap-allocates regardless.
  Dedup-cache benchmarks aren't in the crate (no `benches/`), but the `opt` crate's `constant_fold` and `known_bits` benchmarks build hundreds-of-node chains that exercise this path.
- **Proposed change:** Use a `HashMap` with a borrowing key via the `raw_entry_mut` API or a `RawTable<(...)>`-wrapped key with `&[NodeOutputId]` + `&[NodeOutputKind]` slices, computing the hash from the slices.  Alternatively, key on `(NodeKind, blake3([NodeOutputId; ...] || [NodeOutputKind; ...]))` or a `(NodeKind, u64)` rolling hash to avoid the lookup-time allocation.
- **Confidence:** medium
- **Risk if applied:** medium — touches the hot dedup path; needs benchmarking.
- **Cross-refs:** F-035

### F-035 `evict_cache_entry_if_cacheable` allocates two new `Vec`s on every detach/update

- **Category:** Performance
- **Location:** `crates/ir/src/graph/store.rs:128-150`
- **What:**
  ```rust
  pub(super) fn evict_cache_entry_if_cacheable(&mut self, node_id: NodeId) {
      if !self.nodes[node_id].kind.is_cacheable() { return; }
      let input_outputs: Vec<NodeOutputId> = self.nodes[node_id].inputs
          .as_slice(&self.input_pool).iter()
          .map(|&iid| self.inputs[iid].output_id).collect();
      let output_kinds: Vec<NodeOutputKind> = self.nodes[node_id].outputs
          .as_slice(&self.output_pool).iter()
          .map(|&oid| self.outputs[oid].kind).collect();
      let key = (Node::new(self.nodes[node_id].kind), input_outputs, output_kinds);
      self.node_to_id.remove(&key);
  }
  ```
- **Why:** Called on every `update_input` and `detach_node_inputs` (i.e. every `replace_all_uses` cursor step on a cacheable node).  Two `Vec` allocations per call.  Same hot path as F-034.
- **Proposed change:** Same as F-034 — borrow-keyed map.
- **Confidence:** medium
- **Risk if applied:** medium.
- **Cross-refs:** F-034

### F-036 `cfg_reachable` allocates a `HashSet<NodeId>` from scratch each call

- **Category:** Performance
- **Location:** `crates/ir/src/walk.rs:22-34`
- **What:**
  ```rust
  pub fn cfg_reachable(graph: &Graph, entry: NodeId) -> HashSet<NodeId> {
      let mut visited = HashSet::new();
      let mut worklist = vec![entry];
      while let Some(node) = worklist.pop() { ... }
      visited
  }
  ```
- **Why:** `HashSet<NodeId>` uses sirhash and a Vec-backed table.  The denser, faster `entity_utils::set::DenseEntitySet<NodeId>` is already used by the more general `walk_graph`'s `PreOrder`.  Called by `opt::redundant_phis` on every fixed-point iteration.
- **Proposed change:** Use `DenseEntitySet<NodeId>` and return it.  Layer C uses `HashSet<NodeId>` from `validate.rs:51`'s `walk_graph` collect — could also benefit.
- **Confidence:** medium
- **Risk if applied:** low — `entity_utils::set::DenseEntitySet` already provides a `contains` API.

### Readability

### F-037 The five `BuiltFunctionGraph` back-compat wrappers carry `F2` codename in their docs

- **Category:** Simplification / Readability
- **Location:** `crates/ir/src/ops/rewrite.rs:38-55`, `crates/ir/src/ops/builder.rs:58-118`, `crates/ir/src/ops/consts.rs:103-148`
- **What:** All wrappers' docs cite "F2's `Optimizer` trait refactor" or "F2 trait refactor" without explanation of what F2 is.  Anyone reading the source sees a tea-leaf reference to a planning codename.
- **Proposed change:** Either inline-explain or drop the codename.  Combined with F-019/F-020, the cleanest option is to remove the wrappers entirely.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-019, F-020

### F-038 `require_cur_region`'s "no current region is set" error is opaque

- **Category:** Readability
- **Location:** `crates/ir/src/region.rs:77-86`
- **What:**
  ```rust
  pub(crate) fn require_cur_region(&self) -> Result<RegionId> {
      let region_id = self
          .cur_region
          .ok_or_else(|| anyhow!("no current region is set"))?;
      if self.regions[region_id].terminated {
          let id = region_id.as_u32();
          return Err(anyhow!("attempted to insert into terminated region {id}"));
      }
      Ok(region_id)
  }
  ```
- **Why:** When the error fires, the caller knows nothing about which builder method, what they were trying to do, or whether a region was ever set.  A `set_region` was probably forgotten — but the message doesn't suggest that.
- **Proposed change:** "no current region is set; call `set_region` or `set_entry_region` before this operation".
- **Confidence:** high
- **Risk if applied:** low.

### F-039 `FunctionGraph::new_invalid` is a transient "still invalid" placeholder that consumers never observe

- **Category:** Readability
- **Location:** `crates/ir/src/function.rs:25-38`
- **What:**
  ```rust
  /// An under-construction IR function graph.
  ...
  pub struct FunctionGraph {
      pub graph: Graph,
      pub entry: NodeId,
      pub entry_control: NodeOutputId,
      pub entry_memory: NodeOutputId,
  }

  impl FunctionGraph {
      pub(crate) fn new_invalid() -> Self {
          Self {
              graph: Graph::new(),
              entry: NodeId::reserved_value(),
              entry_control: NodeOutputId::reserved_value(),
              entry_memory: NodeOutputId::reserved_value(),
          }
      }
  }
  ```
- **Why:** `new_invalid` is called in `FunctionBuilder::new_raw` (line 327) just before `build_entry()` (line 339) overwrites all four fields with real ids.  No code path observes the "invalid" state because `new_raw` always finishes with valid ids before returning.  The struct is `pub` with `pub` fields, but any external consumer would see only the post-`build_entry` form (since `body()` returns `&FunctionGraph`).  The `new_invalid` constructor is `pub(crate)` and serves only this transient.
- **Proposed change:** Convert to `Default::default()` if the same shape works (it would via `derive`), or restructure so `FunctionGraph` is built from a complete shape rather than mutated through invalid.
- **Confidence:** medium
- **Risk if applied:** low.

### F-040 `BuiltFunctionGraph::from_graph_and_entry` doc cites "F2" without explanation

- **Category:** Readability
- **Location:** `crates/ir/src/function.rs:68-90`
- **What:**
  ```rust
  /// CORRECTNESS — used by F2's `opt::with_built` bridge so opt-pass
  /// impls that internally still operate on `&mut BuiltFunctionGraph`
  /// (because their helper functions and the `pattern` crate's
  /// rewrite machinery are typed against it) can be invoked from a
  /// callsite that only holds `(&mut Graph, NodeId)`.  The dummy
  /// fields are safe because opt impls never touch them — only
  /// `graph` and `entry` are read.  Not appropriate for downstream
  /// consumers that genuinely need the calling-convention metadata
  /// (use [`crate::FunctionBuilder::build`] for those).
  ```
- **Why:** Same as F-037 — `F2` is a codename for a refactor the reader can't look up.  Without it, the doc still reads as "this is a back-compat shim for opt".
- **Proposed change:** Drop the `F2` reference; keep the substance ("Used by `opt::with_built` to bridge `(&mut Graph, NodeId)` callers to `&mut BuiltFunctionGraph`-typed helpers").
- **Confidence:** high
- **Risk if applied:** low.

### F-041 `cloberred` typo in field name and 9 use sites

- **Category:** Readability
- **Location:** `crates/ir/src/builder/mod.rs:106, 274-292, 335`, `crates/ir/src/builder/call.rs:32-44, 66`
- **What:**
  ```rust
  pub(crate) call_cloberred_variables: Vec<rsleigh::Vn>,
  ```
  Used as `self.call_cloberred_variables` in builder/call.rs:32, 38; `cloberred_kinds` local at line 37, 44, 66.  The doc string at builder/mod.rs:116 also says `call_cloberred_variables`.
  Compare with builder/mod.rs:409 which spells the field correctly when unpacking into `BuiltFunctionGraph`: `call_clobbered: self.call_cloberred_variables.into_boxed_slice()`.
- **Why:** The `BuiltFunctionGraph` struct has the correctly-spelled `call_clobbered` field, but the builder's intermediate field is misspelled.  4 module-level mentions + 5 use sites.  Hard to find via grep ("clobbered" doesn't match).
- **Proposed change:** Rename `call_cloberred_variables` → `call_clobbered_variables`.  Mechanical refactor.
- **Confidence:** high
- **Risk if applied:** low.

### F-042 `SlotRole::Sp` is the only role used by exactly one slot constant

- **Category:** Readability
- **Location:** `crates/ir/src/node_signature.rs:54, 206-210`
- **What:**
  ```rust
  // 54
  Sp,
  // 206-210
  const SP: Slot = Slot {
      kind: AnyInt,
      name: "sp",
      role: R::Sp,
  };
  ```
- **Why:** `R::Sp` is referenced once (as the role of `SP`).  The role drives a color in `dot/mod.rs::role_color` (line 99: `R::Addr | R::Sp | R::Off => "purple"`) — same color as `R::Addr` and `R::Off`.  So `R::Sp` is purely "name-like" data; the color is no different from `R::Addr`.  Could collapse `R::Sp` into `R::Addr` (or an Anvil "alike" group) if the color rule is the only consumer.
- **Proposed change:** Either fold `R::Sp` into `R::Addr` (and rename `SP`'s role) or document why `Sp` exists as a separate role despite sharing color.
- **Confidence:** medium
- **Risk if applied:** low.

### F-043 `expected_signature` table is far from per-kind doc comments

- **Category:** Readability
- **Location:** `crates/ir/src/node_signature.rs:269-389`
- **What:** A 90-line `match kind { ... }` lives in one file while per-kind doc comments live in `node/kind.rs` (~200 lines away in another module).  A reader who wants to understand "what does a `Call` node expect as inputs?" has to read the doc on `kind.rs:79-83` ("Function call.  Clobbers caller-saved registers and the memory token.") and then jump to `node_signature.rs:324-327` to learn the actual slot list.
- **Proposed change:** Either (a) move signatures to per-kind doc comments via a doc-test-style convention, or (b) inline-comment the signature in the `match` arm with a short version of what the doc says.  Today the inline comments do this already for some kinds (`ControlState`, `MemPhi`, `ControlPhi`, `Call`, `Return`, `CallOther`) but not for the integer/float arithmetic kinds where the shape is more obvious.
- **Confidence:** low
- **Risk if applied:** medium — large doc-only change.

### F-044 `lib.rs` `Value` / `ValueType` aliases are barely used

- **Category:** Readability
- **Location:** `crates/ir/src/lib.rs:65-66`
- **What:**
  ```rust
  pub type Value = node::NodeOutputId;
  pub type ValueType = node::NodeOutputType;
  ```
- **Why:** Workspace-wide `grep ir::Value\b` and `ir::ValueType\b` finds ~10 callers (mostly in opt tests + opt::known_bits/constant_fold tests).  The ~100 sites that use the long form (`NodeOutputId` / `NodeOutputType`) would have to be touched to make the alias the canonical form.  Today the aliases create churn: a reader sees `ir::Value` in one file and `NodeOutputId` in the next.
- **Proposed change:** Either (a) commit to the aliases — rename usage workspace-wide and drop `node` re-exports — or (b) drop the aliases and use `NodeOutputId` / `NodeOutputType` everywhere.
- **Confidence:** medium
- **Risk if applied:** medium — touches many files.

### F-045 `BuiltFunctionGraph::ret_val_regs` is a prefix of `BuiltFunctionGraph::call_clobbered`

- **Category:** Simplification
- **Location:** `crates/ir/src/function.rs:45-66`
- **What:**
  ```rust
  pub call_clobbered: Box<[rsleigh::Vn]>,
  pub ret_val_regs: Box<[rsleigh::Vn]>,
  ```
  And `builder/mod.rs:283-292` documents:
  ```rust
  // call_cloberred_variables [is] emitted as the Call node's value
  // outputs in order (slot `i + 2` ↔ call_cloberred_variables[i]).
  // Front-load it with the calling convention's return registers so
  // .ret_output(0) indexes into ABI ret slot 0 (e.g. rax on x86_64),
  // then append the remaining caller-clobbered registers.
  ```
- **Why:** Two `Box<[Vn]>` carrying overlapping data.  `ret_val_regs` is the ABI prefix of `call_clobbered`.  Pattern matching's "find the call's return register" path could read `call_clobbered[0..ret_val_regs.len()]` instead of having a separate field.
- **Proposed change:** Replace `ret_val_regs: Box<[Vn]>` with `ret_val_count: usize`, derive ret regs as `&call_clobbered[..ret_val_count]`.
- **Confidence:** medium
- **Risk if applied:** low — internal refactor.

### F-046 `SlotList::head_len` is `head.len()` — could just inline

- **Category:** Simplification
- **Location:** `crates/ir/src/node_signature.rs:103-105`
- **What:**
  ```rust
  pub fn head_len(&self) -> usize {
      self.head.len()
  }
  ```
- **Why:** A pure getter for a public field.  Layer A could call `sig.inputs.head.len()` directly.  No type abstraction is gained.
- **Proposed change:** Drop the method.  Or, if you want to keep the abstraction, mark `head` `pub(crate)`.
- **Confidence:** medium
- **Risk if applied:** low.

### F-047 `expected_signature_covers_every_node_kind` test has hand-enumerated 41 variants

- **Category:** Correctness
- **Location:** `crates/ir/src/node_signature.rs:660-735`
- **What:** A `Vec<NodeKind>` with 41 entries.  When a new variant is added to `NodeKind`, the test silently doesn't cover it (proptest properties + Rust's exhaustiveness checks on `match` would catch it elsewhere, but this specific "covers every kind" claim is a maintenance burden).
- **Why:** Missing-arm in `expected_signature` is a compile-time error (Rust forces exhaustive `match`).  But "I added a kind, then forgot to update the test list" is silent — the test still passes for whatever it does iterate.
- **Proposed change:** This test could be a const-folded compile-time check via a helper like `fn cover() -> &[NodeKind]` returning a list whose exhaustiveness is asserted.  Practical option: have the test use `enum_iterator` or a similar macro to derive the list.  Alternatively, accept the maintenance burden but add a comment "if you add a NodeKind variant, append it here."
- **Confidence:** medium
- **Risk if applied:** medium — would add a dev-dep.

### F-048 `bit_width_is_eight_times_byte_size` and `is_integer_for_all_integer_output_types` tests omit U80/F80

- **Category:** Correctness
- **Location:** `crates/ir/src/node/tests.rs:71-89, 110-130`
- **What:**
  ```rust
  // 71-89
  for ty in [
      NodeOutputType::Bool,
      NodeOutputType::U8,
      NodeOutputType::U16,
      NodeOutputType::U32,
      NodeOutputType::U64,
      NodeOutputType::U128,
      NodeOutputType::U256,
  ] {
      assert_eq!(...);
  }
  ```
- **Why:** U80 has `byte_size = 10` and `bit_width = 80` — `10 * 8 = 80` — so the assertion would pass for U80 just like for the others.  But the test enumeration is a stale copy from before U80/F80 was added.  A regression in U80's `byte_size` or `bit_width` would not be caught.
  Same gap in `is_integer_for_all_integer_output_types` (line 113-119).
- **Proposed change:** Add `NodeOutputType::U80` to both test arrays.  Add `NodeOutputType::F80` to `is_integer_for_all_integer_output_types`'s "must NOT be integer" assertions.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-049

### F-049 `type_info_table_matches_variants` test omits U80/F80 from `cases`

- **Category:** Correctness
- **Location:** `crates/ir/src/node/tests.rs:330-353`
- **What:**
  ```rust
  let cases: &[(NodeOutputType, &str, usize, bool, bool, bool)] = &[
      (NodeOutputType::Bool, "bool", 1, false, true, false),
      (NodeOutputType::U8,   "u8",   1, true,  false, false),
      ...
      (NodeOutputType::U256, "u256", 32, true, false, false),
      (NodeOutputType::F32,  "f32",  4, false, false, true),
      (NodeOutputType::F64,  "f64",  8, false, false, true),
  ];
  // No U80/F80 entries
  ```
- **Why:** `TYPE_INFO` table in `output_type.rs:52-64` includes U80 and F80; the test's `cases` array doesn't.  The test verifies "table indices match discriminant order" — which would pass even if the test doesn't iterate U80/F80.  But a regression in U80's name `"u80"` (e.g. typo to `"U80"`) wouldn't be caught.
- **Proposed change:** Append U80 and F80 entries to `cases`.
- **Confidence:** high
- **Risk if applied:** low.
- **Cross-refs:** F-048

### F-050 `BUG-N` / `F2` codename references in 30+ comments

- **Category:** Readability
- **Location:** Multiple files
- **What:** Codenames `BUG-1`, `BUG-3`, `BUG-8`, `BUG-10`, `BUG-28`, `F2` appear in:
  - `src/builder/tests.rs` — 11 comments / section headers
  - `src/builder/mod.rs` — 3 (`BUG-1` MIPS MULT, `BUG-28` cause #1)
  - `src/function.rs:72` — `F2` reference
  - `src/graph/mod.rs:94` — `F2` reference
  - `src/ops/builder.rs:2-4` — F2
  - `src/ops/consts.rs:4` — F2
  - `src/ops/rewrite.rs:2-4, 41-42` — F2
- **Why:** `BUG-NN` are tracked tickets that may or may not be closed; `F2` is a codename for a refactor.  External readers can't look these up.  CLAUDE.md's "Critical recent context" lists "Capture-redesign", "anyhow migration", "strider-restructure" — those are the names readers can correlate with.  `F2` and the `BUG-NN` aren't in that list.
- **Proposed change:** Either add a `CODENAMES.md` mapping these to short descriptions, or replace each in-source mention with a short prose summary ("BUG-1: MIPS MULT 64-bit unique varnode aliasing fix" → "MIPS MULT writes a 64-bit unique then a Copy reads a narrow slice; the overlap filter keeps the wider varnode").
- **Confidence:** high
- **Risk if applied:** low (doc only).

### F-051 `pretty_label` for `BoolBinaryOp(op)` uses `{op:?}:bool` — `Debug` impl as user-facing label

- **Category:** Readability
- **Location:** `crates/ir/src/dot/label.rs:223-224`
- **What:**
  ```rust
  NodeKind::BoolBinaryOp(op) => format!("{op:?}:bool"),
  NodeKind::BoolUnaryOp(op) => format!("{op:?}:bool"),
  ```
- **Why:** `BoolBinaryOp` derives `Debug`; for `And` / `Or` / `Xor` the rendered label is `And:bool`, `Or:bool`, etc.  This works as long as the Debug output is identical to what the rendered graph should show, but it's brittle: a future refactor that adds derive variants or struct payloads (e.g. `BoolBinaryOp::Implies`) might silently change the label.  Same caveat applies to `IntBinaryOp(op)`, `IntUnaryOp(op)`, `IntCmpOp(op)`, `FloatBinaryOp(op)`, etc.
- **Proposed change:** Add `as_str()` methods to each of the op enums (similar to `NodeOutputType::as_str`).  Then `format!("{}:bool", op.as_str())`.
- **Confidence:** medium
- **Risk if applied:** low — but adds boilerplate.

### F-052 `walk::graph_walk_succs` interleaves backward-data and forward-control edges

- **Category:** Simplification
- **Location:** `crates/ir/src/walk.rs:73-79`
- **What:**
  ```rust
  pub fn graph_walk_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
      graph
          .node_inputs(node)
          .into_iter()
          .map(move |input| graph.output_definition(input).0)
          .chain(cfg_succs(graph, node))
  }
  ```
- **Why:** "Successor" here is a misnomer: data inputs are predecessors of the node, while control outputs go to successors.  The combined iterator visits both, but there's no separate `cfg_succs_only` or `data_preds_only` for callers that need just one.  The doc comment on `graph_walk_succs` does call out the mix.
- **Proposed change:** Rename to `graph_walk_neighbors` and document precisely what it returns.  Or split into two iterators and let `walk_graph` chain them — but that's the current shape.  Mostly a naming nit.
- **Confidence:** low
- **Risk if applied:** low.

### F-053 `pretty_label`'s `IntConst` rendering would silently truncate U256 values

- **Category:** Correctness
- **Location:** `crates/ir/src/dot/label.rs:128-131`
- **What:**
  ```rust
  NodeKind::IntConst(v) => {
      let ty = self.out_type_suffix(node, ":");
      format!("const {v:#x}{ty}")
  }
  ```
- **Why:** `v: u128`, `format!("{v:#x}", ...)` emits the full hex.  But if a future U256 IntConst ever sneaks in (currently F-001/F-002 prevent it), the rendered label is wrong by 128 bits — not a panic, just misleading output.  Today the panic in `build_int_const` keeps this rendering correct; remove that panic without replacing the label logic and you get silently wrong dot.
- **Proposed change:** Add a `match` on `out_type_suffix` width: for U256, render `<u256 const>` or surface a dedicated branch.  Tied to F-013.
- **Confidence:** low
- **Risk if applied:** low.
- **Cross-refs:** F-013

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| `Cargo.toml` | reviewed | No findings — clean dependency list. |
| `src/lib.rs` | reviewed | F-011 (NodeOutputType list omits U80/F80), F-023 (ExpectedOutputKind dead pub use), F-044 (Value/ValueType aliases sparsely used). |
| `src/error.rs` | reviewed | Clean. |
| `src/function.rs` | reviewed | F-039 (`new_invalid` placeholder), F-040 (F2 codename in `from_graph_and_entry` doc). |
| `src/region.rs` | reviewed | F-004 (fake-id fallback), F-009 (RegionIrCache stale ref), F-018 (link_region_variables vs link_region duplication), F-022 (require_*_kind not used in nodes.rs), F-033 (TerminatedRegion shape), F-038 (require_cur_region error message). |
| `src/iterators.rs` | reviewed | F-006 (Outputs/Inputs index panics), F-030 (DoubleEndedIterator/ExactSizeIterator unused). |
| `src/walk.rs` | reviewed | F-029 (GraphWalkSuccs::new dead pub), F-036 (cfg_reachable HashSet), F-052 (graph_walk_succs naming). |
| `src/node_signature.rs` | reviewed | F-012 (AnyInt doc misses U80), F-017 (ARG/RET/CALL_OUT slot constants), F-042 (SlotRole::Sp), F-043 (signature table location), F-046 (head_len getter), F-047 (covers_every_node_kind test). |
| `src/node/mod.rs` | reviewed | F-024 (Node/NodeInput/NodeOutput pub use unused). |
| `src/node/data.rs` | reviewed | F-032 (`Node::new` etc. could be pub(crate)). |
| `src/node/ids.rs` | reviewed | NodeOutputId derives Default while NodeId / NodeInputId don't — minor inconsistency, not flagged. |
| `src/node/kind.rs` | reviewed | F-010 (CastToInt doc says "Reinterpret as Bool"), F-025 (`is_phi` dead). |
| `src/node/output_kind.rs` | reviewed | F-007 (track_caller dead), F-026 (is_control_phi internal-only), F-027 (as_float_or_err dead). |
| `src/node/output_type.rs` | reviewed | F-013 (bit_mask_u128 / get_unsigned_int U256 semantics), F-048/F-049 indirectly via tests. |
| `src/node/tests.rs` | reviewed | F-048 (bit_width / is_integer omits U80/F80), F-049 (type_info_table_matches_variants omits U80/F80). |
| `src/builder/mod.rs` | reviewed | F-009 (RegionIrCache ref), F-041 (`cloberred` typo), F-050 (codenames). |
| `src/builder/vars.rs` | reviewed | F-008 (ErrorKind doc refs). |
| `src/builder/coerce.rs` | reviewed | F-008 (ErrorKind doc refs), F-014 (SignExtend comment), F-015 (`extend_if_needed` wider→narrower silently keeps wider). |
| `src/builder/call.rs` | reviewed | F-008 (ErrorKind doc refs), F-041 (`cloberred` typo). |
| `src/builder/nodes.rs` | reviewed | F-001 (build_int_const asserts), F-008 (ErrorKind doc refs), F-021 (control/memory kind check duplication). |
| `src/builder/tests.rs` | reviewed | F-050 (codename references). |
| `src/graph/mod.rs` | reviewed | F-028 (`node_count` dead). |
| `src/graph/store.rs` | reviewed | F-034 (cache key allocation), F-035 (eviction allocation). |
| `src/graph/uses.rs` | reviewed | Clean — `replace_all_uses` cursor semantics correct. |
| `src/graph/access.rs` | reviewed | F-008 (ErrorKind doc refs). |
| `src/graph/tests.rs` | reviewed | Comprehensive coverage. |
| `src/ops/mod.rs` | reviewed | Clean. |
| `src/ops/op_kinds.rs` | reviewed | Clean — F-051 (Debug-impl labels) is the dot-side concern. |
| `src/ops/builder.rs` | reviewed | F-008 (ErrorKind), F-020 (back-compat wrappers), F-050 (F2 codename). |
| `src/ops/consts.rs` | reviewed | F-002 (make_int_const no validation), F-003 (int_const_val narrowing), F-008 (ErrorKind), F-020 (wrappers), F-031 (test duplication). |
| `src/ops/rewrite.rs` | reviewed | F-019 (replace_all_uses wrapper), F-050 (F2 codename). |
| `src/dot/mod.rs` | reviewed | Clean. |
| `src/dot/render.rs` | reviewed | Clean — virtual-node logic well-tested. |
| `src/dot/label.rs` | reviewed | F-051 (Debug-impl for op labels), F-053 (IntConst rendering for U256). |
| `src/dot/tests.rs` | reviewed | Clean. |
| `src/validate/mod.rs` | reviewed | Clean — three-layer architecture is sound. |
| `src/validate/layer_a.rs` | reviewed | F-005 (head_len=0 + variadic = always passes input arity). |
| `src/validate/layer_b.rs` | reviewed | TODO at line 19 acknowledges `InputPointsToMissingOutput` is unimplemented but documented. |
| `src/validate/layer_c.rs` | reviewed | F-016 (uniqueness scans non-reachable nodes). |
| `src/validate/tests.rs` | reviewed | Comprehensive. |
| `tests/build_validate_roundtrip.rs` | reviewed | Clean. |
| `tests/builder_extended_use.rs` | reviewed | Clean. |
| `tests/dedup_cache.rs` | reviewed | Clean. |
| `tests/proptest_graph_invariants.rs` | reviewed | Clean. |
| `tests/walk_reachability.rs` | reviewed | Clean. |
| `tests/common/mod.rs` | reviewed | Clean. |
| `examples/graph_creator.rs` | reviewed | Clean (no `expected_signature` references; works as a tutorial). |

## Out-of-scope items observed

- `crates/opt/src/lib.rs:78, 116` and `crates/opt/tests/pipeline_subsets.rs` still reference `RegionIrCache` (cross-crate stale doc — flag in the cross-crate session).
- `crates/strider/src/strider/pipeline.rs:326-356` uses the `region_initial_variables` / `region_exit_variables` / `region_entry_*` accessors as their last-remaining external client; the per-iteration `RegionIndex` is the post-restructure consumer.
- The `pcode-lift` crate's value-lifter calls `make_int_const` / `build_int_const` from many sites; F-001's panic risk surfaces there for U256 inputs that `try_from(u32)` produces.

## Stopped here marker

Review complete.  All 46 `.rs` files (src + tests + examples) read front to back.  Findings 1-53 cover every concern surfaced.

## Outcomes

| Finding | Outcome | Notes |
| --- | --- | --- |
| F-001 | Applied | `build_int_const` returns `Result`; ~30 call sites updated. |
| F-002 | Applied | `Graph::make_int_const` rejects non-integer / U256. |
| F-003 | Applied | Added `int_const_val_u128`; kept narrowing `int_const_val`. |
| F-004 | Applied | Replaced fake-id fallback with explicit empty-outputs branch. |
| F-005 | Applied | Documented Layer A's variadic head_len=0 behaviour. |
| F-006 | Applied | Removed inconsistent `prefer .get()` comment. |
| F-007 | Applied | Dropped `#[track_caller]`; kept methods. |
| F-008 | Applied | Stripped 97 `[\`ErrorKind::*\`]` doc links across the crate. |
| F-009 | Applied | Rewrote `RegionIrCache` references to point at the post-restructure region index. |
| F-010 | Applied | `CastToInt` doc fixed. |
| F-011 | Applied | `lib.rs` lists U80 / F80. |
| F-012 | Applied | `AnyInt` / `AnyFloat` doc lists U80 / F80. |
| F-013 | Applied | Documented `bit_mask_u128` U256 caveat; left `get_unsigned_int` semantics intact (changing them broke opt's popcount/lzcount-on-U256-skip-cleanly tests). |
| F-014 | Applied | SignExtend comment accurately describes `i64 as u128`. |
| F-015 | Applied | `extend_if_needed` truncates when curr > target. |
| F-016 | Applied (doc) | Documented the all-nodes scan; reachable-scoping breaks `MissingInitialMemoryNode` for builder graphs that haven't wired memory yet. |
| F-017 | Skipped | Three slot constants kept; finding's confidence was medium and the role-tagging difference is load-bearing. |
| F-018 | Applied | `link_region` delegates to `link_region_variables`. |
| F-019 | Applied | `BuiltFunctionGraph::replace_all_uses` wrapper deleted; callers go through `fg.graph`. |
| F-020 | Applied | The other 8 `BuiltFunctionGraph` wrappers deleted. |
| F-021 | Applied | `nodes.rs` uses `require_control_kind` / `require_memory_kind`. |
| F-022 | Applied | (Same change as F-021.) |
| F-023 | Applied | `pub use ExpectedOutputKind` removed. |
| F-024 | Applied | `Node`/`NodeInput`/`NodeOutput` re-exports become `pub(crate)`. |
| F-025 | Applied | `NodeKind::is_phi` and tests removed. |
| F-026 | Applied | `is_control_phi` becomes `pub(crate)`. |
| F-027 | Applied | `as_float_or_err` and tests removed. |
| F-028 | Applied | `Graph::node_count` removed. |
| F-029 | Applied | `GraphWalkSuccs::new` becomes `pub(crate)`. |
| F-030 | Applied | Unused `DoubleEndedIterator` / `ExactSizeIterator` impls removed. |
| F-031 | Applied | Duplicate test module in `ops/consts.rs` removed. |
| F-032 | Applied | `Node::new` / `NodeInput::new` / `NodeOutput::new` become `pub(crate)`. |
| F-033 | Skipped | `region_id` field is read by 3 callers (build_branch / build_if / build_return); not unused. |
| F-034 | Skipped | Cache-key restructure needs benchmarking; out of scope for this batch. |
| F-035 | Skipped | Same as F-034. |
| F-036 | Skipped | Switching to `DenseEntitySet` is a perf change without measured pressure. |
| F-037 | Applied | F2 codename trail removed alongside F-019/F-020. |
| F-038 | Applied | `require_cur_region` error names the missing setter. |
| F-039 | Skipped | `new_invalid` is the natural shape for the in-progress builder; reshaping it adds invariants without a payoff. |
| F-040 | Applied | F2 codename stripped from `from_graph_and_entry`. |
| F-041 | Applied | `cloberred` -> `clobbered` everywhere in builder/. |
| F-042 | Skipped | `SlotRole::Sp` carries semantic distinction even when the rendered colour matches `Addr`. |
| F-043 | Skipped | Doc-only relocation; broad scope. |
| F-044 | Skipped | `Value` / `ValueType` aliases used at 50+ call sites; renaming is workspace-wide churn. |
| F-045 | Skipped | `ret_val_regs` is a separately-typed view; deriving from `call_clobbered` is a refactor without a concrete consumer benefit. |
| F-046 | Skipped | `head_len()` keeps the `head` field encapsulated. |
| F-047 | Applied (doc) | Comment on the test list noting that adds require manual append. |
| F-048 | Applied | U80 / F80 added to bit-width and is-integer iterations. |
| F-049 | Applied | U80 / F80 added to type_info_table_matches_variants cases. |
| F-050 | Applied | Stripped `BUG-1/3/8/10/28` and `F2` codenames from prose. |
| F-051 | Skipped | `{op:?}` style is widespread; an `as_str()` fix would touch every op enum and every dot label arm. |
| F-052 | Skipped | Naming nit; doc already explains the mix. |
| F-053 | Skipped | U256 IntConsts can't reach pretty_label today (build_int_const + make_int_const both reject U256). |

