# Round 11 — R5: simplifications

## Per-category totals

| Category | Entries | Net LOC delta |
|----------|---------|----------------|
| 1. Delete | 14 | -1100 |
| 2. Merge | 11 | -180 |
| 3. Inline single-callsite helpers | 9 | -55 |
| 4. Stdlib-idiom replacements | 7 | -25 |
| 5. Visibility tightening | 8 | 0 |
| 6. Wrappers to drop | 4 | -50 |
| 7. Partial-state types to convert | 8 | -100 |
| **Total** | **61** | **~-1510** |

Pre-implementation workspace LOC: ~55,134 (Rust crates/)
Projected post-implementation: ~53,600
Net reduction: ~2.7%

Note on totals: visibility-tightening entries do not change LOC; they change the API surface. The biggest single line-count win is the `IndirectBranchResolve` struct + `Optimizer` impl + opt's own integration test (S1.1, ~700 LOC removed) — the pass exists only to be tested, since strider's orchestrator drives the classifier directly.

---

## Section 1 — Code to delete

### S1.1 — `opt::IndirectBranchResolve` struct, its `Optimizer` impl, `add_anchor`/`clear_anchors`/`new`/`Default` and the dedicated tests

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:144-191` (struct), `:228-255` (`AnchorAddr` ctors / accessors), `:257-307` (`new`, `add_anchor`, `clear_anchors`, `Default`), `:309-410` (`Optimizer` impl), `:440-684` (inline `tests` module, ~245 LOC), and `crates/opt/tests/indirect_branch_resolve.rs:1-113` (integration test). Mention in `lib.rs:74-78` and `pipeline.rs:138-140`.
- **Why dead:** `grep -rn "IndirectBranchResolve" crates/strider*` returns zero hits in production; only opt's own tests instantiate the pass. Strider's orchestrator (`crates/strider/src/orchestrator.rs:526-548`) calls `classify_anchor_with_rom_and_sp` and the inplace functions directly. `add_anchor` and `clear_anchors` have **zero** callers anywhere. The `IndirectBranchResolve::Optimizer` impl is only exercised by the two tests in `opt/tests/indirect_branch_resolve.rs`, which themselves exist to "pin the integration contract" — but that contract has no production consumer.
- **Net LOC delta:** ~-700 (struct/impl/tests).
- **Net cognitive delta:** removes a public type that misleads readers into thinking the orchestrator runs `IndirectBranchResolve` like a normal pass; the doc comment at `pipeline.rs:138-140` even apologises for the asymmetry. After deletion, the pipeline-shape test at the strider orchestrator becomes the single source of truth for "how indirect-branch resolution is wired".
- **Migration:** keep `find_placeholder_return_for_anchor`, `apply_link_register`, `apply_tail_call`, `classify_anchor_with_rom_and_sp`, `ResolvedTargets`, `AnchorCallingContext`. Delete `IndirectBranchResolve` and `AnchorAddr` together. Delete `crates/opt/tests/indirect_branch_resolve.rs` outright.

### S1.2 — `opt::AnchorAddr` newtype + ctors/accessors

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:213-255` (struct + 4 methods).
- **Why dead:** Only used inside the `IndirectBranchResolve` struct (S1.1). Once that struct is deleted, `AnchorAddr` has no consumers. Its `parts()` method was unused even before. The doc-comment claims it's "transparent to strider (which casts it back to its `cfg::PcodeInsnAddr`)" — but `grep -rn "AnchorAddr" crates/strider/` returns zero hits.
- **Net LOC delta:** -45 (entire impl + accessors).
- **Net cognitive delta:** removes a "packed pair" newtype that adds zero invariant over `(u64, u64)` and was only used as a `HashMap` key inside the dead pass.
- **Migration:** delete with S1.1.

### S1.3 — `entity-utils::DenseEntitySet::test_and_set`

- **Where:** `crates/entity-utils/src/set.rs:69-76`.
- **Why dead:** `grep -rn "test_and_set" crates/` returns only the definition itself, its inline-tests doc, and the test. No production caller. The doc says it's "an alternative surface" for visited-set walks but no walk uses it; every site uses `insert(...)` or `contains(...)` directly.
- **Net LOC delta:** -8 (method + 11-line test = -19; conservative estimate).
- **Net cognitive delta:** removes a sibling method whose only stated benefit is a more readable name for `!set.insert(_)`. With the method gone, the `insert` doc-comment can drop the comparison.
- **Migration:** delete the method and its test (`set.rs:283-294`).

### S1.4 — `cfg::Builder::new` (deprecated) + the workspace's 9 `#![allow(deprecated)]` files

- **Where:** `crates/cfg/src/cfg/builder/mod.rs:98-108` (`Builder::new`); `:117-148` (`Builder::with_endianness`, also deprecated). 16 files use `#![allow(deprecated)]` (`grep -rln "#!\[allow(deprecated)\]" crates/` from earlier scan), most for these two ctors.
- **Why dead:** Production code path (`crates/strider/src/orchestrator.rs:940`) goes through `Builder::for_arch`. The deprecation has been live for several rounds. The lone exception is `crates/strider-py/src/cfg.rs:53` which arguably has the wrong arch preset (round11-1F F-2). Audit reports flag this as the round in which migration should land.
- **Net LOC delta:** -75 (two deprecated ctors + their internal scaffolding) + ~20 LOC across 9 test files dropping the `#![allow(deprecated)]` headers + ~30 LOC for migration of test call sites = net ~-50.
- **Net cognitive delta:** removes a footgun ctor that defaults `preset = X86_64`; the `#[deprecated]` workspace warnings disappear; a class of "non-x86 cfg silently misclassifies CallOthers" bugs is eliminated.
- **Migration:** mechanical: replace `Builder::new(sleigh, addr, opts)` → `Builder::for_arch(&arch, sleigh, addr, opts)`. Each test fixture has a known arch.

### S1.5 — `ir::Graph::make_int_const` / `make_bool_const` / `make_float_const` (duplicate of `FunctionBuilder::build_int_const` etc.)

- **Where:** `crates/ir/src/ops/consts.rs:82-142` (61 LOC of three constructors).
- **Why dead-ish (semantic duplicate):** `make_int_const` does the same `if !ty.is_integer()` / wide-reject / `bit_mask_u128` / `create_node` / `node_outputs_exact::<1>` sequence as `FunctionBuilder::build_int_const` (`crates/ir/src/builder/nodes.rs:86-104`). The doc-comment on `make_int_const` (`consts.rs:99`) even says "`FunctionBuilder::build_int_const` already masks; mirror it here so `Graph::make_int_const` agrees." Three callers exist: `cfg::indirect_resolve` (line 319), `opt::known_bits` (line 502), and `opt::load_readonly` (line 92) — all opt-side. Audit 2B (`crates/ir/src/ops/consts.rs:82`) flags this as redundant.
- **Net LOC delta:** -50 (delete the three `Graph::make_*_const` methods; opt callers route through a tiny inline helper).
- **Net cognitive delta:** one canonical constructor for IR int-consts. Today the two paths drift independently (the wide-rejection error message has different wording at lines 88-93 vs `nodes.rs:96-100`).
- **Migration:** the opt-side callers don't have a `FunctionBuilder` (they take `&mut Graph`). Move the same logic into `Graph::int_const` / `Graph::bool_const` / `Graph::float_const` (drop the `make_` prefix, keep the contract); delete the duplicate path.

### S1.6 — `ir::WideConstStorage::from_le_bytes_u256` and `from_le_bytes_u512`

- **Where:** `crates/ir/src/wide_const.rs:67-96` (~30 LOC).
- **Why dead:** `grep -rn "from_le_bytes_u256\|from_le_bytes_u512"` shows usage only inside `wide_const.rs`'s own tests (lines 182-201).
- **Net LOC delta:** -30 (methods) -22 (tests of those methods).
- **Net cognitive delta:** the `to_le_bytes` direction is a real production round-trip path; the `from_*` direction was speculative.
- **Migration:** delete the two methods + their three tests.

### S1.7 — `cfg::test_api` re-exports for `ProcessInsnRes` (could collapse)

- **Where:** `crates/cfg/src/test_api.rs:7-21` and `crates/cfg/src/cfg/builder/region_builder.rs:677-820` (the inline `test_api` mod).
- **Why redundant:** Two `pub use ... test_api ...` re-exports in `crates/cfg/src/cfg/mod.rs:11-21` (5 doc-hidden items) plus a thin `crates/cfg/src/test_api.rs` that re-re-exports them. The structure exists because `test_api` traverses private modules — but the `pub use crate::cfg::test_api::*` star-import collapses this naturally.
- **Net LOC delta:** -15 (drop intermediate file; integration tests import directly from `cfg::test_api`).
- **Net cognitive delta:** one fewer re-export hop.
- **Migration:** delete `crates/cfg/src/test_api.rs`; update lib.rs to do `#[doc(hidden)] pub use crate::cfg::test_api;`.

### S1.8 — `pattern::Bindings::bind_capture` test-leaking visibility (delete OR demote)

- **Where:** `crates/pattern/src/matcher/bindings.rs:81-89`.
- **Why dead-ish:** Audit 1D F-5 documents that `bind_capture` is `pub` despite the doc claiming "External callers see `Bindings` as read-only". `Match::new_for_test` is the real test-side construction surface. No external production caller of `bind_capture`.
- **Net LOC delta:** 0 (visibility change only).
- **Net cognitive delta:** the docstring at lines 50-54 stops contradicting the visibility annotation.
- **Migration:** demote to `pub(crate)`. (Listed under "delete" because the public API surface is what we're removing.)

### S1.9 — `from_graph_and_entry_for_rewrite` (already deprecated)

- **Where:** `crates/ir/src/function.rs:194-214`.
- **Why dead-ish:** `#[deprecated]` + `#[doc(hidden)]`. Only 5 callers, all in pattern tests. The `pattern::RewriteCtx::new(&mut graph, entry)` ctor covers every use. Round 10 deferred this; round 11 round1A audit (line 56) doesn't push hard but the ctor remains a weight.
- **Net LOC delta:** -20 (method body + extensive doc).
- **Net cognitive delta:** removes the only reason 4 pattern test files have `#![allow(deprecated)]` headers.
- **Migration:** rewrite each pattern-test caller to construct a `pattern::RewriteCtx` instead.

### S1.10 — `dot_test_api`, `indirect_resolve_test_api`, `region_builder_test_api` triple-pub re-exports

- **Where:** `crates/cfg/src/cfg/mod.rs:11-21`.
- **Why redundant:** Three `#[doc(hidden)]` re-exports each go via a sub-module's `test_api` named module that itself only re-exports a struct. After S1.7 these can collapse.
- **Net LOC delta:** -10 (three re-export lines + their inline targets).
- **Net cognitive delta:** the `cfg` crate's public-but-doc-hidden surface shrinks from 5 paths to 1.
- **Migration:** combine with S1.7.

### S1.11 — `strider::indirect_resolve::classify_anchor` and `classify_anchor_with_rom` (test-only delegating shims)

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:20-42` (~23 LOC).
- **Why dead:** Both are thin one-line forwarders to `opt::classify_anchor` / `opt::classify_anchor_with_rom`, and the only callers are integration tests. Production strider uses `classify_anchor_with_rom_and_sp` (which it passes a cached `KnownBitsMap` into). The "convenience" of the simpler signature exists only for the tests' benefit.
- **Net LOC delta:** -23.
- **Net cognitive delta:** the strider-side classifier surface shrinks to one function (`classify_anchor_with_rom_and_sp`) and the test scaffolding plumbs its own `analyze_known_bits` call (already done in the bigger function).
- **Migration:** delete the two shims; tests adopt the same idiom orchestrator uses (compute KB via `analyze_known_bits`, call `classify_anchor_with_rom_and_sp` directly).

### S1.12 — `opt::classify_anchor` and `opt::classify_anchor_with_rom` (1-line delegators)

- **Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:66-115` (~50 LOC of mostly doc).
- **Why dead:** Each of the two simpler variants computes `analyze_known_bits` and calls `classify_anchor_with_rom_and_sp`. Only callers are the strider shims (S1.11) and tests. With S1.11 gone, these are dead.
- **Net LOC delta:** -50.
- **Net cognitive delta:** the classifier's public surface shrinks to one function with named optional arguments (rom, sp). Current N²-naming explosion (`classify_anchor` / `_with_rom` / `_with_rom_and_sp`) is flagged in audit 2B (line 139) as a maintainability anti-pattern.
- **Migration:** delete the two shims; rename `classify_anchor_with_rom_and_sp` to `classify_anchor` (single canonical name).

### S1.13 — `BuiltCallingConvention::from_parts_unchecked` (test-only escape hatch)

- **Where:** `crates/target/src/calling_convention/mod.rs:165-188`.
- **Why dead-ish:** `#[doc(hidden)]` + the docstring explicitly says "Test-only escape hatch". `try_from_parts` is the validating sibling. Audit 2D #11 explicitly recommends deletion.
- **Net LOC delta:** -25 (method + scattered test usage to migrate).
- **Net cognitive delta:** the documented "skip validation" path goes away — every test fixture must traverse the same invariant set as production.
- **Migration:** rename callers from `from_parts_unchecked` to `try_from_parts(...).unwrap()` (validation is cheap in tests).

### S1.14 — strider-py `errors.rs` `#[allow(dead_code)]` cluster

- **Where:** `crates/strider-py/src/errors.rs:30, 41, 57` and `crates/strider-py/src/opt.rs:412-422` (×6) and `crates/strider-py/src/graph.rs:54, 63` and `crates/strider-py/src/sleigh.rs:25, 43`.
- **Why suspect:** PyO3 callsites that have `#[allow(dead_code)]` annotations covering ~10 functions. Need an audit pass: whichever are still genuinely covered by the `pyfunction` registration can drop the allow; whichever are truly dead can be removed. Round 11 audit 1F flagged the snapshot-file regression as the gating contract; the dead_code allows themselves are smell.
- **Net LOC delta:** uncertain pending audit; conservative -20 to -50.
- **Net cognitive delta:** removes "deferred-cleanup" annotations.
- **Migration:** for each `#[allow(dead_code)]` annotation, run the type-name through `cargo build --release` without the allow; if the build is clean, the function was already in use and the allow is misleading; if dead, delete.

---

## Section 2 — Code to merge

### S2.1 — `WideConstStorage::from_le_bytes_u256` + `..._u512` → `from_le_bytes(width, bytes)`

- **Where:** `crates/ir/src/wide_const.rs:67-96`.
- **Three-occurrence threshold:** N/A (just 2 occurrences, but identical logic).
- **Load-bearing-different vs accidentally-different:** structurally identical except for the array size (4 vs 8 limbs) and variant constructor. Same loop body byte-for-byte.
- **Net LOC delta:** -15 (single generic-N helper covers both).
- **Net cognitive delta:** clearer that the only difference is width.
- **Migration:** if S1.6 lands first this is moot; otherwise unify into one `from_le_bytes(width: usize, bytes: &[u8]) -> Option<Self>` that switches on width.

### S2.2 — `require_control_kind(res.control)?; require_memory_kind(res.memory)?;` repeated 6 times

- **Where:** `crates/ir/src/builder/nodes.rs:529, 562, 584, 610` and `crates/ir/src/builder/call.rs:203-204`.
- **Three-occurrence threshold:** YES, 5+ occurrences of the exact two-line pair.
- **Load-bearing-different vs accidentally-different:** structurally identical; each callsite uses the same `res` (a `RegionTerminationSnapshot`) and the kinds are the documented invariant.
- **Net LOC delta:** -10 (5 sites × 2 lines becomes 5 sites × 1 line).
- **Net cognitive delta:** the per-builder method shows the structural shape (validate + create node) rather than two micro-checks.
- **Migration:** add `pub(crate) fn require_terminator_kinds(&self, res: &RegionTerminationSnapshot) -> Result<()>` on `FunctionBuilder` that runs both checks; replace each pair.

### S2.3 — `strider-py::dot::dump_html / dump_dot / html_str` are 3-line wrappers

- **Where:** `crates/strider-py/src/dot.rs:24-34` (3 functions, each 2 lines body).
- **Three-occurrence threshold:** YES, 3 wrapper functions, 6+ call sites in `cfg.rs` + `graph.rs`.
- **Load-bearing-different vs accidentally-different:** all three wrappers do the same `.dump_*(Path::new(path)).map_err(into_strider_err)` pattern. The only variance is which method is called.
- **Net LOC delta:** -15 (collapse to one closure-based helper or just inline at the 6 call sites, since they're already in `cfg.rs`/`graph.rs`).
- **Net cognitive delta:** the call sites in `cfg.rs:65-77` and `graph.rs:101-127` already have all the imports; inlining `d.dump_as_html(Path::new(path)).map_err(into_strider_err)` is no worse.
- **Migration:** inline at each `to_html` / `to_dot` / `html_str` method body in the two `Py*` wrappers; drop the `dot::` shim entirely (or keep `dot_style_for` only).

### S2.4 — Two opt passes' "with_rewrite_ctx splash" wrappers (`Optimizer` + `OptimizerOnBuilt` blanket)

- **Where:** `crates/opt/src/pipeline.rs:117-162`. The trait `Optimizer` (`fn optimize(graph, entry)`) and `OptimizerOnBuilt` (`fn optimize_built(ctx)`) plus the blanket `impl<T: OptimizerOnBuilt> Optimizer for T`.
- **Three-occurrence threshold:** YES — 17 passes implement `OptimizerOnBuilt`; 1 pass (`IndirectBranchResolve`) implements `Optimizer` directly.
- **Load-bearing-different vs accidentally-different:** With S1.1 deleting `IndirectBranchResolve`, every remaining pass goes through the blanket impl. The `Optimizer` trait reduces to a one-shape interface.
- **Net LOC delta:** -45 (after S1.1 lands, drop the `Optimizer` trait and rename `OptimizerOnBuilt` to `Optimizer`).
- **Net cognitive delta:** one trait, one method — the doc that explains why two traits exist (lines 100-140) goes away.
- **Migration:** depends on S1.1. After it: delete `OptimizerOnBuilt`, rename `Optimizer::optimize_built` → `Optimizer::optimize`, keep the blanket-deleted entirely.

### S2.5 — `default_pipeline` is the catenation of `stable + destructive`

- **Where:** `crates/opt/src/lib.rs:165-202`.
- **Three-occurrence threshold:** N/A but the duplication is visible side-by-side.
- **Load-bearing-different vs accidentally-different:** `default_pipeline` adds `ConstantFold + KnownBits + FlagCmpCanonicalize + IfCondInversion + RedundantPhis + DeadBranchElimination`; `stable + destructive` adds the same 6 in the same order. The doc at line 182 even says "Equivalent to running `stable_default_pipeline` followed by `destructive_default_pipeline` in order."
- **Net LOC delta:** -10 (rewrite `default_pipeline` to extend rather than re-list).
- **Net cognitive delta:** removes the divergence risk — a future addition to `stable_default_pipeline` won't be silently absent from `default_pipeline`.
- **Migration:** rewrite as `let mut p = stable_default_pipeline(); for o in destructive_default_pipeline().drain_optimizers() { p.add_dyn(o); } p` — needs a `drain_optimizers` method, or just re-construct from a static `&[fn(&mut OptimizerPipeline)]` list. Cleanest: factor a `pub fn add_default_passes(&mut self)` that the three top-level constructors call.

### S2.6 — `Outputs::is_empty` and `Inputs::is_empty` use `len() == 0` instead of `.is_empty()` on the inner slice

- **Where:** `crates/ir/src/iterators.rs:16-18, 80-82`.
- **Three-occurrence threshold:** N/A (2 sites in one file).
- **Load-bearing-different:** identical pattern on different inner types. Both delegate to `len()` which itself delegates to the inner slice's `.len()`. Direct `.is_empty()` on the inner slice would be 1-line.
- **Net LOC delta:** -2.
- **Net cognitive delta:** trivial — clippy already has a lint for this.
- **Migration:** `self.0.is_empty()` and `self.use_list.is_empty()`.

### S2.7 — Three `set_lift_addr(Some(addr))…work…set_lift_addr(None)` paired blocks

- **Where:** `crates/strider/src/strider/insn/mod.rs:44-46, 53-55` and similar pattern in `control.rs`.
- **Three-occurrence threshold:** YES.
- **Load-bearing-different vs accidentally-different:** all three do the lift-time fingerprint funnel; the body in each is genuinely different.
- **Net LOC delta:** -8 (each callsite drops 2 lines for the bracketing).
- **Net cognitive delta:** the panic-leak hazard called out in audit 1A (finding "lift_addr non-RAII funnel") is mitigated.
- **Migration:** add a `set_lift_addr_scoped<F: FnOnce(&mut FunctionBuilder)>` that handles the `Some/None` pair via a `Drop` guard, mirroring `lift_at`. The mutable-borrow constraint that prevents direct `lift_at` use is solvable by passing a closure that re-borrows.

### S2.8 — `WideConstStorage::to_le_bytes` U256/U512 arms are identical

- **Where:** `crates/ir/src/wide_const.rs:60-65`.
- **Three-occurrence threshold:** N/A (2 arms).
- **Load-bearing-different:** identical body, different inner array size.
- **Net LOC delta:** -3 (collapse to single `match self { Self::U256(l) | Self::U512(l) => l.iter()...` — though syntactically can't `or-pat` different inner types; need to project to a common iterator).
- **Net cognitive delta:** marginal.
- **Migration:** add `fn limbs(&self) -> &[u64]` and rewrite `to_le_bytes` as `self.limbs().iter().flat_map(|l| l.to_le_bytes()).collect()`.

### S2.9 — `read_or_init_var` → exposed `vn_size_to_output_type` error helper appears at 3 sites

- **Where:** `crates/strider/src/orchestrator.rs:806-815, 856-867`. Same pattern (`NodeOutputType::try_from(vn.size).map_err(|_| anyhow!(...))?`) appears in `read_or_init_var` and `build_anchor_calling_context`.
- **Three-occurrence threshold:** 2 in orchestrator, 1 in `pcode-lift`. Marginal but documented.
- **Load-bearing-different:** the error message text differs between sites; the validation is identical.
- **Net LOC delta:** -10 (one shared helper with a `purpose: &str` arg for the message).
- **Net cognitive delta:** removes a class of "supported sizes are 1, 2, 4, 8, 10, 16, 32, 64" copy-paste; if the supported list changes, one site changes.
- **Migration:** `pub(crate) fn vn_size_to_node_output_type(vn: &Vn) -> Result<NodeOutputType>` returning the canonical error. Three callers swap.

### S2.10 — `validate_value_inputs` + the open-coded `if !kind.is_value()` checks

- **Where:** `crates/ir/src/builder/mod.rs:192-200` (helper). Open-coded versions at `crates/ir/src/builder/nodes.rs:638-648` (`build_segment_op` does it twice manually for `segment` / `offset`).
- **Three-occurrence threshold:** the helper exists; some callers don't use it.
- **Load-bearing-different:** same check, same error shape.
- **Net LOC delta:** -8 (replace `build_segment_op`'s two manual checks with `validate_value_inputs(&[segment, offset])?`).
- **Net cognitive delta:** consistent error message + one canonical check site.
- **Migration:** swap manual `if !kind.is_value() { return Err(...) }` chains for a single `validate_value_inputs(&[...])?` call.

### S2.11 — `pattern::sub` / `int_le` / `int_sle` lowered-shape ctors duplicate per-shape build code

- **Where:** `crates/pattern/src/pat/ctor/int.rs:56-90` and similar patterns in `float.rs`.
- **Three-occurrence threshold:** YES — 6 lowering ctors (sub, le, sle, float_sub, float_le, float_ne) all do the same "build a primitive shape from a non-primitive opcode name" pattern.
- **Load-bearing-different vs accidentally-different:** the shape each builds is genuinely different (Add+Neg vs BoolNeg(Less) vs BoolNeg(Equal) etc.). They are NOT mergeable into one helper — flagging as out-of-scope.
- **Verdict:** SKIP — this is the right shape; just cross-checking that they aren't merge candidates.

---

## Section 3 — Single-callsite helpers to inline

### S3.1 — `strider-py::dot::dump_html`, `dump_dot`, `html_str`

- **Where:** `crates/strider-py/src/dot.rs:24-34`.
- **Body length each:** 1-2 lines.
- **Call sites:** 6 (in `cfg.rs` + `graph.rs`).
- **Note:** technically multi-callsite but the helpers are 2-line wrappers around `.dump_as_html(Path::new(path)).map_err(into_strider_err)`. Inlining at each call site would make the locale-aware path conversion explicit and remove a hop.
- **Net LOC delta:** -10 (drop the wrapper module; inline 6 call sites).
- **Net cognitive delta:** call site goes from `dump_html(&d, path)` to `d.dump_as_html(Path::new(path)).map_err(into_strider_err)` — slightly longer but reads top-down without needing to chase the helper.
- **Migration:** inline; delete `dump_html` / `dump_dot` / `html_str`.

### S3.2 — `Strider::arch()` and `Strider::calling_convention()` accessors are single-callsite

- **Where:** `crates/strider/src/strider/pipeline.rs:166-177`.
- **Body length:** 1 line each.
- **Call sites:** 1 each (orchestrator).
- **Net LOC delta:** -10 (delete the two accessors; orchestrator reads `pub(super)` field directly).
- **Net cognitive delta:** orchestrator becomes dependent on `Strider`'s field layout, but the field layout is already stable. The accessors don't add any invariant.
- **Migration:** demote `Strider::arch` / `Strider::calling_convention` from method to direct field read, OR keep them and inline the orchestrator call.

### S3.3 — `FunctionBuilder::body()` and `body_mut()`

- **Where:** `crates/ir/src/builder/mod.rs:152-159`.
- **Body length:** 1 line each.
- **Call sites:** ~6 external calls in strider+opt; many internal ones.
- **Note:** the `body()` accessor names a `FunctionGraph` internal, which (per audit 2D #6) is a partial-state type. Inlining `self.body().graph` to `&self.function.graph` (after the field gets a less-confusing name) collapses two indirections.
- **Net LOC delta:** -8.
- **Net cognitive delta:** every call site reading `b.body().graph.X` becomes `b.function_graph().X` (or `b.graph().X` if the inner alias hops).
- **Migration:** depends on the larger `FunctionGraph → ?` rename audit 2B flags.

### S3.4 — `cfg::Cfg::start_addr_to_region_id` field accessors

- **Where:** `crates/cfg/src/cfg/query.rs:143` (region_id_at_start).
- **Note:** `region_id_at_start` is the canonical accessor and used widely. The other one (`region_id_at_machine_addr`) is a 4-line one-off that two tests use; could be inlined.
- **Verdict:** SKIP — multiple call sites, not a clear win.

### S3.5 — `pattern::var(c)` returns `var_pat(c).into()` — could inline

- **Where:** `crates/pattern/src/pat/ctor/wildcards.rs:25-32`.
- **Body length:** 2 lines.
- **Call sites:** 4 in tests, 0 in production lib code outside of `pattern::sub` (which is a free ctor).
- **Verdict:** SKIP — too many docstrings rely on `var(c)` as the canonical free-ctor name.

### S3.6 — `Graph::int_const_val_u128` thin wrapper around inline-able match

- **Where:** `crates/ir/src/ops/consts.rs:34-43`.
- **Body length:** 8 lines.
- **Call sites:** 0 in production (only test files).
- **Net LOC delta:** could delete (-12) — but verifying no production callers; per `grep -rn` only in test scaffolds.
- **Net cognitive delta:** the U64-narrowing variant `int_const_val` is the production path; the U128 variant is doc-only.
- **Verdict:** delete-candidate; flagged here for possible dual-classification with S1.

### S3.7 — `intern_str` (strider-py)

- **Where:** `crates/strider-py/src/pattern.rs:127`.
- **Body length:** ~10 lines.
- **Call sites:** widely used inside `pattern.rs`.
- **Verdict:** SKIP — load-bearing helper.

### S3.8 — `strider::indirect_resolve::ResolvedTargets` re-export from `cfg::test_api`

- **Where:** `crates/strider/src/indirect_resolve/mod.rs:12`.
- **Body length:** 1 line.
- **Call sites:** strider's orchestrator.
- **Net LOC delta:** 0 (one re-export; nothing to inline).
- **Verdict:** the path `cfg::test_api::ResolvedTargets` is itself a smell — this type isn't truly test-only, it leaks into strider production via the re-export. The fix is to promote `ResolvedTargets` out of `cfg::test_api`. Listed under S5 (visibility) instead.

### S3.9 — `OptimizationResult::after_replace` migration-history doc paragraph

- **Where:** `crates/opt/src/pipeline.rs:32-39`.
- **Note:** Audit 1C #2 flags lines 36-39 as "migration-history prose that has nothing to do with the function's contract." Pure doc cleanup.
- **Net LOC delta:** -4 (doc-comment lines).
- **Net cognitive delta:** documentation about how to call the function instead of how it got renamed.
- **Migration:** drop those four doc lines.

---

## Section 4 — Bespoke patterns to replace with stdlib idioms

### S4.1 — `Worklist::enqueue` two-pass `contains` + `insert`

- **Where:** `crates/entity-utils/src/worklist.rs:55-60`.
- **Pattern:** `if !self.workset.contains(entity) { self.workset.insert(entity); self.worklist.push_back(entity); }` — two bitset accesses.
- **Stdlib idiom:** `if self.workset.insert(entity) { self.worklist.push_back(entity); }` (relying on `insert` returning `bool`, which is part of `DenseEntitySet`'s contract since round 7).
- **Net LOC delta:** -2.
- **Net cognitive delta:** matches `HashSet::insert` semantics, halves bitset accesses.
- **Migration:** flagged in audit 1F F-6.

### S4.2 — `Outputs::is_empty` / `Inputs::is_empty` use `self.len() == 0` rather than `self.0.is_empty()`

- **Where:** `crates/ir/src/iterators.rs:16-18, 80-82`.
- **Pattern:** `self.len() == 0`.
- **Stdlib idiom:** `self.0.is_empty()` / `self.use_list.is_empty()`.
- **Net LOC delta:** -2.
- **Net cognitive delta:** trivial; clippy lint `len_zero` already flags this.
- **Migration:** trivial.

### S4.3 — `if !ty.is_integer() { return Err(...) }; if matches!(ty, U256 | U512) { return Err(...) }` — two-step rejection at every wide-int site

- **Where:** `crates/ir/src/builder/nodes.rs:91-101` (build_int_const) and `crates/ir/src/ops/consts.rs:83-93` (make_int_const).
- **Pattern:** two sequential rejection checks, identical between the two functions.
- **Stdlib idiom:** factor `fn check_int_const_carrier_ty(ty) -> Result<()>` returning Ok if integer and not wide.
- **Net LOC delta:** -10.
- **Net cognitive delta:** one canonical rejection contract; the two duplicate functions in S1.5 share the helper.
- **Migration:** if S1.5 lands, this reduces to one site; otherwise factor.

### S4.4 — Worklist `extend` uses a manual loop instead of `for ... { enqueue }`

- **Where:** `crates/entity-utils/src/worklist.rs:84-89`.
- **Pattern:** explicit for-loop.
- **Stdlib idiom:** the loop is fine; nothing to change. Skipping.
- **Verdict:** SKIP.

### S4.5 — `[res.control, res.memory].into_iter().chain(ret_inputs)` for variadic-tail builders

- **Where:** `crates/ir/src/builder/nodes.rs:535` (build_return) and `crates/ir/src/builder/call.rs:128`.
- **Pattern:** array → iter → chain → collect.
- **Stdlib idiom:** could use `Vec::with_capacity` + `.extend` for clarity but the current form is idiomatic.
- **Verdict:** SKIP — current is fine.

### S4.6 — `if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)` 

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:431-435`.
- **Pattern:** swallow Err, treat shape mismatch as "not the right node".
- **Stdlib idiom:** the `if let Ok(...)` is correct here (signature-guaranteed shape), but the audit 2C F-5 flags that this should have an inline comment naming the rationale.
- **Net LOC delta:** 0.
- **Net cognitive delta:** the audit recommends a one-line "by IndirectBranch signature" comment. Trivial.
- **Migration:** add comment.

### S4.7 — `as u8` truncation in EXTRACT/INSERT operands

- **Where:** `crates/pcode-lift/src/value/cast.rs:113-114, 161-162`.
- **Pattern:** `let lsb = insn.inputs[1].addr_off as u8;` (silent truncation).
- **Stdlib idiom:** `u8::try_from(insn.inputs[1].addr_off).map_err(|_| anyhow!("..."))?`.
- **Net LOC delta:** +6 (more verbose).
- **Net cognitive delta:** correctness — surface-level error instead of silent truncation. Audit 1B finding flags this.
- **Verdict:** CORRECTNESS-improving; LOC +6 but worth it.

---

## Section 5 — Visibility to tighten

### S5.1 — `cfg::PcodeInsnAddr.machine_addr` and `.insn_index`

- **Where:** `crates/cfg/src/cfg/types.rs:60-100`.
- **Current:** `pub` fields with documented "use the canonical accessors" caveat.
- **Proposed:** `pub(crate)` (audit 2D #1 flagged for this round).
- **Net LOC delta:** 0 (visibility annotation).
- **Net cognitive delta:** removes the migration disclaimer; field-set invariant becomes type-enforced.
- **Migration:** swap ~14 chained `.machine_addr.addr` reads to `.machine_addr_u64()`; add a `pub fn new(machine_addr, insn_index) -> Self` ctor for the ~7 struct-literal sites.

### S5.2 — `cfg::MachineInsnAddr.addr`

- **Where:** `crates/cfg/src/cfg/types.rs:24-49`.
- **Current:** `pub addr: u64`.
- **Proposed:** `pub(crate)` after the chained access pattern is gone (subsumed by S5.1).
- **Net LOC delta:** 0.
- **Net cognitive delta:** newtype invariant is the type distinction, not the field.

### S5.3 — `pattern::RewriteCtxView.graph` and `.entry`

- **Where:** `crates/pattern/src/rewrite.rs:215-229`.
- **Current:** `pub` fields; doc says "Caution".
- **Proposed:** `pub(crate)` accessors.
- **Net LOC delta:** 0 (+ accessor functions).
- **Net cognitive delta:** removes the rebinding hazard called out in audits 1D F-4 and 2D #3.

### S5.4 — `pattern::RewriteCtx.graph` and `.entry`

- **Where:** `crates/pattern/src/rewrite.rs:154-157`.
- **Current:** `pub graph: &'g mut Graph, pub entry: NodeId`.
- **Proposed:** `pub(crate)` (audit 2D #4 — HIGH severity).
- **Net LOC delta:** 0.
- **Net cognitive delta:** the `Deref<Target=Graph>` already covers ergonomics; removing field access removes the rebind footgun.

### S5.5 — `cfg::ProcessInsnRes` enum

- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:48-55`.
- **Current:** `pub enum`; only used inside cfg crate (and exposed via `test_api`).
- **Proposed:** `pub(crate)` and re-export only via `test_api` (audit 2D #22).
- **Net LOC delta:** 0.

### S5.6 — `target::SleighArch` four `pub` fields

- **Where:** `crates/target/src/arch.rs:118-130`.
- **Current:** all 4 `pub` with "(R9-2D ...)" migration tags in the docstrings.
- **Proposed:** `pub(crate)` (audit 2D #9, low cost).
- **Net LOC delta:** 0.
- **Net cognitive delta:** kills the four `(R9-...)` breadcrumb doc strings flagged by audit 3B (findings 1-6).

### S5.7 — `ir::BuiltFunctionGraph` five CC-bearing fields

- **Where:** `crates/ir/src/function.rs:45-109`.
- **Current:** `pub variables`, `pub call_clobbered`, `pub ret_val_regs`, `pub call_other_clobbered` (each with "Caution" docstrings).
- **Proposed:** `pub(crate)` and route reads through the existing `_regs()` accessors (audit 2D #5 — HIGH severity).
- **Net LOC delta:** 0.
- **Net cognitive delta:** removes the silent-miscompile hazard via `Match::get_vn` slot positional indexing into a mutated array.

### S5.8 — `cfg::Cfg::start_addr_to_region_id`

- **Where:** `crates/cfg/src/cfg/mod.rs:62-73`.
- **Current:** `pub`; documented hazard.
- **Proposed:** `pub(crate)` + a `#[doc(hidden)] pub fn from_parts_for_tests` for the cfg_query test (audit 2D #7).
- **Net LOC delta:** 0 to +5 (adds a constructor).
- **Net cognitive delta:** prevents direct mutation that desyncs the index.

---

## Section 6 — Wrappers to drop

### S6.1 — `cfg::test_api` flat re-export module

- **Where:** `crates/cfg/src/test_api.rs:1-22`.
- **Wrapper-ness:** `pub use crate::cfg::test_api::*` and three more star-imports — each `cfg/src/test_api.rs` line directly mirrors a single re-export from a sub-module.
- **Net LOC delta:** -22 (entire file).
- **Net cognitive delta:** one fewer module on the `cfg::test_api::*` path; tests already use the path directly.
- **Migration:** delete the file; tests already do `use cfg::test_api::Foo` which resolves through the same path.

### S6.2 — `dot::write_html` (referenced in skill but doesn't exist) — N/A but symptom of S6.4

### S6.3 — `strider-py::dot::dump_html / dump_dot / html_str`

- **Where:** `crates/strider-py/src/dot.rs:24-34`.
- **Wrapper-ness:** each is a 2-line `d.dump_as_*(Path::new(path)).map_err(into_strider_err)`.
- **Net LOC delta:** -15 (delete file body; inline at 6 call sites).
- **Net cognitive delta:** one less hop.
- **Migration:** see S2.3.

### S6.4 — `opt::AnchorAddr` newtype with no invariant beyond `(u64, u64)`

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:213-255`.
- **Wrapper-ness:** newtype around `(machine_addr: u64, insn_index: u64)` with 4 trivial methods (`from_packed`, `machine_addr`, `insn_index`, `parts`). The doc claims it's "opaque to opt, transparent to strider" but the public accessors expose the layout, defeating the opacity.
- **Net LOC delta:** -45.
- **Net cognitive delta:** dies with S1.2.
- **Migration:** see S1.2.

### S6.5 — `strider::indirect_resolve` thin shim module (3 functions, all 1-line delegators)

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:20-72`.
- **Wrapper-ness:** three forwarders to opt; the inner crate is the source of truth.
- **Net LOC delta:** subsumed by S1.11+S1.12.
- **Migration:** strider tests use `opt::classify_anchor_with_rom_and_sp` directly.

---

## Section 7 — Partial-state types to convert

### S7.1 — `cfg::Options` partial state (`fn_max_size: Option<u64>` + `allow_code_before_start_addr: bool` silently ignored when bounded)

- **Where:** `crates/cfg/src/cfg/options.rs:59-95`.
- **Current:** Two scalar fields; `function_boundary()` accessor reconstructs the enum at read time.
- **Proposed enum:** `enum FunctionBoundary { Bounded(u64), Unbounded { allow_code_before_start: bool } }` — and migrate the storage too, not just the accessor (audit 2D #16).
- **Net LOC delta:** -12.
- **Net cognitive delta:** the "silently-ignored" trap goes away — invalid states unrepresentable.

### S7.2 — `strider::RunConfig` (`fn_max_size: Option<u64>` + `allow_code_before_start_addr: bool` partial pair)

- **Where:** `crates/strider/src/orchestrator.rs:60-106`.
- **Current:** same partial-state shape as S7.1, replicated at the strider boundary.
- **Proposed:** swap to `boundary: FunctionBoundary` field once S7.1 lands (audit 2D #17).
- **Net LOC delta:** -10 (per-callsite checking gone).
- **Migration:** ~6 call sites.

### S7.3 — `cfg::Builder::endianness / preset` defaults (silently `X86_64`)

- **Where:** `crates/cfg/src/cfg/builder/mod.rs:51-85`.
- **Current:** `pub(super) endianness, pub(super) preset` with "defaults to X86_64 in deprecated ctors" hazard. Two ctors (`new`, `with_endianness`) silently default; one (`for_arch`) sets correctly.
- **Proposed:** delete the deprecated ctors, then the partial state goes away (audit 2D #25; subsumed by S1.4).
- **Net LOC delta:** subsumed by S1.4.

### S7.4 — `ir::FunctionGraph::new_invalid` / sentinel-state ctor

- **Where:** `crates/ir/src/function.rs:13-34`.
- **Current:** `entry`, `entry_control`, `entry_memory` set to `reserved_value()` until `build_entry` runs.
- **Proposed enum:** `enum FunctionGraph { Invalid, Initialised { graph, entry, entry_control, entry_memory } }`.
- **Net LOC delta:** mostly visibility (-15 plus accessor wrappers); structural improvement.
- **Migration cost:** small (audit 2D #6 — only one external caller chain).

### S7.5 — `opt::ResolvedTargets::Multiple(Vec<u64>)` non-empty invariant on a tuple variant

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:81-99`.
- **Current:** the `Multiple(Vec)` form can be constructed empty by tuple-pattern, bypassing the validating `Self::multiple` ctor.
- **Proposed:** `Multiple(NonEmptyVec<u64>)` newtype (audit 2D #15).
- **Net LOC delta:** ~+8 (the newtype's lines exceed the validator's lines, but invariants are unrepresentable).

### S7.6 — `strider::AnalyzeOptions { all_vns: Option<Vec<Vn>> }`

- **Where:** `crates/strider/src/strider/pipeline.rs:89-116`.
- **Current:** None means "compute fresh"; Some means "must be sorted+complete".
- **Proposed enum:** `VarnodeSet { Compute, Cached(SortedVns) }` with `SortedVns` newtype enforcing sortedness (audit 2D #18).
- **Net LOC delta:** ~+15 (the newtype's accessors), then -8 in callers that no longer have to defend against unsorted input.

### S7.7 — `opt::IndirectBranchResolve { rom: Option<Arc>, link_register_vn: Option<Vn>, ... }` partial config

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:144-191`.
- **Current:** all `Option<...>` fields, populated lockstep via `add_anchor` / `clear_anchors`.
- **Proposed:** subsumed by S1.1 (struct deletion).
- **Net LOC delta:** subsumed.

### S7.8 — `target::CallingConvention::syscall_number_reg_name: Option<&'static str>`

- **Where:** `crates/target/src/calling_convention/mod.rs` (search for `syscall_number_reg_name`).
- **Current:** Only some presets (the syscall ones) populate this; others leave it `None`.
- **Proposed enum:** `enum ConventionFlavour { Userland, KernelInternal, Syscall { number_reg: &'static str } }` to make per-flavour invariants explicit.
- **Net LOC delta:** ~+10 in the type definition, -5 at construction (the syscall presets' `Some(...)` plumbing).
- **Migration cost:** medium — touches every preset constructor.
- **Verdict:** flagged for completeness; arguably out-of-scope this round.

---

## Top-5 cognitive-load assessment per category

### Section 1 — Code to delete
1. **S1.1** (`IndirectBranchResolve` struct + integration test) — biggest LOC win and removes a misleading "this pass plugs into the pipeline" mental model that doesn't reflect production wiring.
2. **S1.4** (`cfg::Builder::new` + the `#![allow(deprecated)]` headers) — every "look at this test, also note: there's a deprecated annotation we silently allow" disappears.
3. **S1.5** (`Graph::make_int_const` duplicate) — kills two-paths-for-one-thing drift.
4. **S1.13** (`from_parts_unchecked`) — one less escape hatch.
5. **S1.6** (`from_le_bytes_u256/u512`) — round-trip API was speculatively added; dropping it tightens the public surface.

### Section 2 — Code to merge
1. **S2.5** (`default_pipeline` from stable+destructive concatenation) — eliminates a class of bugs where adding a pass to one half forgets the third copy.
2. **S2.4** (`OptimizerOnBuilt` collapses into `Optimizer` after S1.1) — one trait, one method.
3. **S2.2** (`require_terminator_kinds` helper) — 5 call-site cleanup with no semantics change.
4. **S2.9** (`vn_size_to_node_output_type` shared helper) — consolidates a 3-occurrence error message that mentions specific byte sizes; one place to update.
5. **S2.3** (strider-py dot wrappers inlined) — reduces hop count on a hot rendering path.

### Section 3 — Single-callsite helpers to inline
1. **S3.9** (`OptimizationResult::after_replace` migration-history doc) — pure-doc; readers stop being confused by historical references.
2. **S3.6** (`int_const_val_u128` zero callers) — accessor no caller exercises.
3. **S3.2** (`Strider::arch()` / `calling_convention()`) — single-callsite accessors that hide nothing.
4. **S3.3** (`body()` / `body_mut()`) — only meaningful after the `FunctionGraph` rename.
5. **S3.1** (strider-py dot helpers) — see S2.3.

### Section 4 — Stdlib-idiom replacements
1. **S4.1** (Worklist::enqueue single-pass) — measurable perf win on hot graph-traversal paths; audit 1F F-6 specifically called this out.
2. **S4.7** (EXTRACT/INSERT `as u8` truncation) — correctness; audit 1B flagged.
3. **S4.3** (factored `check_int_const_carrier_ty`) — DRY for the constructor pair.
4. **S4.2** (Outputs/Inputs is_empty) — clippy lint.
5. **S4.6** (signature-guaranteed Ok pattern annotation) — pure doc; clarifies intent.

### Section 5 — Visibility to tighten
1. **S5.4** (`RewriteCtx.graph`/`.entry`) — HIGH severity; rebinding hazard.
2. **S5.7** (`BuiltFunctionGraph` CC fields) — HIGH severity; silent miscompile of `Match::get_vn`.
3. **S5.3** (`RewriteCtxView`) — HIGH severity; companion to S5.4.
4. **S5.1**+**S5.2** (`PcodeInsnAddr` / `MachineInsnAddr` fields) — round 10's deferred work.
5. **S5.6** (`SleighArch` fields) — kills the `(R9-2D ...)` breadcrumb cluster.

### Section 6 — Wrappers to drop
1. **S6.4** (`AnchorAddr`) — newtype with no invariant; biggest single deletion.
2. **S6.5** (strider's `indirect_resolve::classify_*` shims) — production strider doesn't need them.
3. **S6.3** (strider-py dot helpers) — see S2.3.
4. **S6.1** (`cfg::test_api` flat re-export module) — single-purpose re-export file.

### Section 7 — Partial-state types
1. **S7.1**+**S7.2** (`Options` / `RunConfig` `fn_max_size`+`allow_code_before_start_addr` partial pair) — high user-facing visibility; eliminates "user knob silently ignored" trap.
2. **S7.4** (`FunctionGraph::new_invalid`) — invalid states currently observable through `pub` fields; sum type closes the gap.
3. **S7.5** (`ResolvedTargets::Multiple(Vec)` non-empty) — small change, eliminates an invariant tax.
4. **S7.6** (`AnalyzeOptions::all_vns`) — sortedness invariant becomes type-enforceable.
5. **S7.3** (cfg::Builder default-arch state) — gone with S1.4.
