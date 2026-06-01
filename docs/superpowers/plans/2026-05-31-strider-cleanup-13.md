# Strider 13-point cleanup

## Context

Refactor pass against `rewrite/strider`. User clarified: this is a refactor, change anything needed — "keep as-is" recommendations from exploration agents are explicitly overruled where the simplification has value.

Branch: `rewrite/strider-cleanup` off `rewrite/strider`. Multiple logical commits (one per point, or a few combined when they share a touch surface). FF-merge into `rewrite/strider` at the end; delete the branch.

Project rules carry through:
- Production Rust returns errors, never panics on Python-reachable paths. Test code may unwrap.
- `cargo clippy --workspace --all-targets -- -D warnings` must stay clean.
- Per-commit gate: `cargo test --workspace`, clippy, `cargo doc --workspace --no-deps`, `cd crates/strider-py && uv run maturin develop && uv run pytest -q`. Pytest baseline: 844 passed / 0 skipped on `rewrite/strider`.
- Commit messages: lowercase imperative, no plan/phase/group identifiers, trailing `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

The 13 points are interlocked — sequencing is deliberate so each step compiles and the diff stays reviewable.

---

## Sequence overview

Logical commit order (each is a self-contained reviewable unit):

1. **Delete `IntUnaryOp::BitNot`** — replace with `Xor(x, all_ones)` at lift time.
2. **Delete `OffsetCapture`** — pattern surface shrink + bindings journal simplification.
3. **Refactor `Bindings`** to `enum { Node(NodeId), Output(NodeOutputId) }` — depends on #2.
4. **Delete `OutputsSpec` + `ConsumersSpec` + `next_unique_consumer` + `match_unique_output_consumer`** — the consumer-walk subsystem; loses the `call().preserves_memory()` consumer-walk feature.
5. **Merge `Strider` + `Config<'a, R>`** into one type.
6. **Drop duplicate fields** across structs (the `function + entry` family).
7. **Refactor optimizer pipeline + drop `Send + Sync` + drop `Arc<dyn ReadOnlyMemory>`** — enables #8.
8. **Delete `resolve_const_loads`** — directly call the (now non-`'static`) `LoadReadOnly` helper.
9. **Lambda + `&dyn Fn` coercion noise cleanup**.
10. **Fix the bounded-lift overflow bug** — sequential decode past `fn_max_size` raises an error.
11. **Switch ELF loader to segments + add object-file path with duplicate-VMA handling**.
12. **Confirm "keeps" (decode cache, 16-byte padding)** — document the invariants; no code change.

(That's the natural ordering of the user's 13 points renumbered to match implementation dependencies.)

---

## Per-point detail

### 1. Delete `IntUnaryOp::BitNot` → `Xor(x, all_ones)` at lift time

**Current state.**
- Variant: `crates/strider-ir/src/ops/op_kinds.rs:82` (`IntUnaryOp::BitNot`).
- Lift dispatch: `crates/strider-lift/src/pcode_lift/value/mod.rs:54-57` — `OPCODE_TO_INT_UNARY` maps `Opcode::IntNeg → IntUnaryOp::BitNot`, and `OPCODE_TO_BOOL_UNARY` maps `Opcode::BoolNeg → IntUnaryOp::BitNot`. Also produced by `arithmetic.rs` / `float.rs` for the lift-time canonicalisations `IntNotEqual → BitNot(IntEqual(...))`, `IntLessEqual → BitNot(IntLess(b,a))`, `FloatNotEqual → BitNot(FloatEqual(...))`, `FLOAT_NAN(x) → BitNot(FloatEqual(x,x))`.
- Pattern builders: `bit_not` (`crates/strider-analyze/src/pattern/pat/ctor/int.rs:73`, via `decl_pat_unary_ops!`), `bool_not` (`crates/strider-analyze/src/pattern/pat/ctor/bool_.rs:79-81` — builds `BitNot` at I1 = logical NOT).
- Optimizer rules touching `BitNot`: `FlagCmpCanonicalize` (~7 rules use `bool_not(...)` LHS and/or RHS), `IfCondInversion` (matches `If(BitNot(C))` → `If(C){B}{A}`), `KnownBits` (BitNot ⇒ bit-invert lattice), `ConstantFold` (`BitNot(IntConst)` and `BitNot(BitNot(x)) → x`).
- PyO3 accessor: `PyMatch.bool_unary_op` (`crates/strider-py/src/matcher.rs:201`) returns `"BitNot"` for matched bool-not nodes.

**Target.** Replace every `BitNot(x)` emission with `Xor(x, IntConst(all_ones_for_width))`, then delete the `BitNot` variant. `all_ones` per width: `I1 → 1`, `I8 → 0xFF`, `I16 → 0xFFFF`, …, `I64 → u64::MAX`, `I128 → u128::MAX`. (Strider currently lifts `BitNot` only at widths ≤ I128, so no `IntConstWide` needed; verify in the lifter.)

**Changes.**
- `crates/strider-ir/src/ops/op_kinds.rs`: drop `IntUnaryOp::BitNot`. Update doc on `IntUnaryOp::Neg` (remaining variant).
- `crates/strider-lift/src/pcode_lift/value/mod.rs`: remove `IntUnaryOp::BitNot` rows from `OPCODE_TO_INT_UNARY` + `OPCODE_TO_BOOL_UNARY`. Replace with explicit handlers that emit `Xor(operand, all_ones_const)` of the right output type. Add a helper `fn all_ones_const_for(ty: NodeOutputType) -> u64`.
- `crates/strider-lift/src/pcode_lift/value/arithmetic.rs` + `float.rs`: every `make_int_unary_node(BitNot, ...)` / `make_value_node(NodeKind::IntUnaryOp(BitNot), ...)` becomes the Xor emission. Affected canonicalisations: `IntNotEqual`, `IntLessEqual` (`BitNot(IntLess(b,a))` becomes `Xor(IntLess(b,a), IntConst(1))` at I1), `FloatNotEqual`, `FLOAT_NAN`.
- `crates/strider-analyze/src/pattern/pat/ctor/int.rs`: drop `bit_not` from the `decl_pat_unary_ops!` invocation list. Replace with a free function: `pub fn bit_not(x: impl Into<Pat>) -> Pat { value_xor_with_all_ones(x.into()) }` — i.e. an integer Xor whose RHS is a width-keyed `int_const`. Use the existing `xor` builder + an `any_int_const` predicate (since the all-ones constant is width-derived at match time).
- `crates/strider-analyze/src/pattern/pat/ctor/bool_.rs`: `bool_not(x)` becomes `xor(x, int_const_with_value(1))` at I1 (`bool_binary("Xor", x, int_const(1))`).
- Optimizer migrations:
  - `crates/strider-analyze/src/opt/flag_cmp_canonicalize/mod.rs:140-200`: every `bool_not(...)` in rule LHS/RHS becomes the new `bool_not` (still spelled `bool_not` since the pattern builder absorbs the lowering). RHS-side construction at runtime emits the Xor shape. **No rule logic changes** — only IR shape underneath.
  - `crates/strider-analyze/src/opt/if_cond_inversion/mod.rs`: replace `NodeKind::IntUnaryOp(BitNot)` match (line ~97) with a check that the cond is `Xor(_, IntConst(1))` at I1. Helper: `is_i1_xor_with_one(function, node)`.
  - `crates/strider-analyze/src/opt/known_bits/...`: the BitNot bit-invert fold becomes a Xor-with-constant fold (already handled by the generic Xor rule? — verify; if so, the rule deletion is net simpler).
  - `crates/strider-analyze/src/opt/constant_fold/...`: `BitNot(IntConst(K))` fold becomes the existing `Xor(IntConst(K), IntConst(M))` fold (already handled by ConstantFold's Xor rule). The double-negation rule (`BitNot(BitNot(x))`) becomes `Xor(Xor(x, ones), ones) → x` (XOR-self-inverse cascade); likely subsumed by the existing `Xor(x, IntConst(0)) → x` + commutative-xor folds; if not, add the missing fold.
- `crates/strider-py/src/matcher.rs:201`: delete `bool_unary_op` accessor (no longer meaningful — the bool not shape is now `Xor`, not a unary op). Update PyO3 stubs + Python tests accordingly.
- Inline tests in `strider-ir` and `strider-analyze`: rewrite tests that construct or match `BitNot` directly to use Xor.

**Risk + verification.**
- Lift-time canonicalisations change the IR shape — asm-fingerprint snapshots may shift. Re-baseline any snapshot test that pins the post-lift shape (run `cargo test`, accept diffs only where the new shape is the expected `Xor`).
- The `bool_not` / `bit_not` pattern builders preserve their public surface but emit different IR. Pattern callers that previously matched a `BitNot` node by kind (rather than via the builder) will break — grep for `NodeKind::IntUnaryOp(IntUnaryOp::BitNot)` and fix.

---

### 2. Delete `OffsetCapture`

**Current state.**
- Type: `crates/strider-analyze/src/pattern/var.rs:62-99` — opaque `OffsetCapture(u32)`.
- Bindings storage: `crates/strider-analyze/src/pattern/matcher/bindings.rs:41-183` — `offset_entries: Vec<(OffsetCapture, (NodeOutputId, i64))>` + `offset_index: FxHashMap<...>` (a parallel journal to the main capture journal).
- Builders: `Load::offset_capture(c)`, `Store::offset_capture(c)` in `pattern/pat/builders/memory.rs`.
- Match accessor: `Match::captured_offset(c) -> Option<i64>` and `Bindings::get_offset_binding`.
- PyO3 mirror: `PyOffsetCapture` (`crates/strider-py/src/pattern.rs`), `PyMatch.captured_offset` (`crates/strider-py/src/matcher.rs:243-248`).
- Tests: `crates/strider-analyze/tests/pattern_matching/load_store_stack_offset_capture.rs` + uses in `matcher_api.rs` cross-pattern join tests.
- README: top-level README has a "Stack-offset recovery" example using `OffsetCapture`.

**Target.** Delete entirely. Users recover SP-offsets via the existing `Function::stack_offsets` side-table directly: capture the Load/Store node (regular `Capture`), then read `function.stack_offset(match.node(cap))`. The README example becomes a two-line snippet using that route.

**Changes.**
- `crates/strider-analyze/src/pattern/var.rs`: delete `OffsetCapture` struct + `next_id()` if no other capture types use the id counter (verify — `Capture` likely shares it).
- `crates/strider-analyze/src/pattern/matcher/bindings.rs`: delete `offset_entries`, `offset_index`, `bind_offset`, `get_offset`, `get_offset_binding`, `offset_iter`. Adjust `BindingsMark` to drop the second-cursor field.
- `crates/strider-analyze/src/pattern/pat/builders/{memory.rs,load.rs,store.rs}`: delete `offset_capture` builder methods + the relevant `Pat` field. The macro mirror in `strider-py` follows.
- `crates/strider-analyze/src/pattern/matcher/match_result.rs`: delete `Match::captured_offset`.
- `crates/strider-py/src/pattern.rs`: delete `PyOffsetCapture` + its `add_class`. Delete `offset_capture` field from `PyLoadPat`/`PyStorePat` defs.
- `crates/strider-py/src/matcher.rs:243-248`: delete `PyMatch.captured_offset`.
- `crates/strider-py/strider/__init__.pyi` + `pattern.pyi`: drop `OffsetCapture` symbol.
- `crates/strider-py/tests/python/test_*`: migrate offset-recovery tests to use `function.stack_offset(match.node(cap))`. If no tests survive the migration as standalone offset-recovery tests, delete them — the `function.stack_offset` accessor is exercised by tests elsewhere.
- `crates/strider-analyze/tests/pattern_matching/load_store_stack_offset_capture.rs`: same — migrate or delete.
- `README.md` + `crates/strider-py/README.md`: rewrite the "Stack-offset recovery" example to use the direct accessor.
- `CLAUDE.md`: remove `OffsetCapture` from the pattern DSL bullet.

---

### 3. Refactor `Bindings` to `enum { Node, Output }`

**Current state.** `crates/strider-analyze/src/pattern/matcher/bindings.rs:13` — `pub(crate) struct Binding(pub(crate) NodeId, pub(crate) Option<NodeOutputId>);`. Two arms encoded via the `Option`: value capture (`Some(out)`) vs control-flow capture (`None`).

**Target.**
```rust
pub enum Binding {
    Node(NodeId),
    Output(NodeOutputId),
}
```
For the `Output(out)` arm, callers recover the owning `NodeId` via `function.node_for_output(out)` (accessor confirmed to exist on `Function`).

**Changes.**
- Replace the struct with the enum.
- `Bindings::get_node(c, function)` — now takes `&Function`, returns:
  - `Binding::Node(n)` → `Some(n)`
  - `Binding::Output(o)` → `Some(function.node_for_output(o))`
- `Bindings::get_output(c)` — returns `Some(o)` only for `Binding::Output(_)`; `None` for the node arm.
- All `Match::get_*` accessors (uint/int/bool/float_bits/has/__getitem__/op accessors) call through `get_output`/`get_node` and already handle the value-vs-node distinction; minimal touch surface.
- Matcher core: replace the two construction sites with `Binding::Output(out)` (value capture) vs `Binding::Node(nid)` (control-flow / region capture).
- PyO3 mirror is unaffected at the surface — `PyMatch.uint`, `.has`, `.node`, etc. all keep their semantics.

Done in the same commit as #2 (or immediately after) since both rewrite `bindings.rs`.

---

### 4. Delete `OutputsSpec` + `ConsumersSpec` + the consumer-walk subsystem

**Current state.**
- `crates/strider-analyze/src/pattern/pat/node_pat.rs:258` — `OutputsSpec` (`None | Indexed(Vec<(slot_idx, Pat)>)`).
- Same file `:270` — `ConsumersSpec` (`None | One(Pat)`).
- `crates/strider-analyze/src/pattern/matcher/consumer.rs` — `next_unique_consumer` helper.
- `crates/strider-analyze/src/pattern/pat/builders/consumer_match.rs` — `match_unique_output_consumer`.
- Users (the consumer-walk path):
  - `crates/strider-analyze/src/pattern/pat/builders/call.rs:194-255` — `Call` pattern uses `match_unique_output_consumer` to validate the call's mem-out / value-out has a specific consumer.
  - `crates/strider-analyze/src/pattern/pat/builders/memory.rs:392` — `Store` consumer-walk.
  - The `call().preserves_memory()` / `mem_out_consumer(...)` / `value_out_consumer(...)` builder methods rely on it.

**User stance** ("they do nothing currently and we don't really need them"). The features ARE wired and have tests, but they're niche (consumer-walk through a single call/store output). User accepts losing this functionality.

**Changes.**
- `crates/strider-analyze/src/pattern/pat/node_pat.rs`: drop the `outputs: OutputsSpec` and `consumers: ConsumersSpec` fields from `NodePat`; drop the enums + `with_outputs`/`with_consumers` setters; drop the match-time branches at `:473, :490`.
- `crates/strider-analyze/src/pattern/matcher/consumer.rs`: delete the file.
- `crates/strider-analyze/src/pattern/pat/builders/consumer_match.rs`: delete the file.
- `crates/strider-analyze/src/pattern/pat/builders/call.rs`: remove `mem_out_consumer` / `value_out_consumer` / `preserves_memory` methods (whichever exist) + their `Def` macro fields. Same for `Store`.
- `crates/strider-py/src/pattern.rs`: same removals from the PyO3 mirror (the `#[strider_pattern]` Def structs lose the consumer-walk fields, which the macro emits).
- Tests: `crates/strider-analyze/tests/complex_patterns.rs` references "ConsumersSpec walk" in comments — verify whether those tests still pass post-deletion (they likely use the consumer-walk feature; either migrate them to a different pattern or delete them).
- "Skipping regions" — the user's wording referred to the `ignore_regions: bool` flag on `find_all`/`find_one`/`find_joined`. **This is genuinely useful** (it tells the matcher to walk into `Region` nodes when matching). Verify the user's intent; if they truly want it deleted, the matcher's region-walk default changes. Default proposal: **keep `ignore_regions`** — it's a legitimate matcher option, not dead code. Flag this in the plan for user confirmation.

---

### 5. Merge `Strider` + `Config<'a, R>`

**Current state.**
- `Strider` (`crates/strider-analyze/src/strider/pipeline.rs:123-136`): holds `calling_convention`, `arch`, `sleigh_regs`, `alias_mode`. Stable per arch/ABI. `Clone`.
- `Config<'a, R>` (`crates/strider-analyze/src/orchestrator/mod.rs:63-111`): `strider: &'a Strider`, plus `start_addr`, `sleigh`, `rom`, `fn_max_size`, `allow_code_before_start_addr`, `compact`, `per_address_ccs_unbuilt`.

**Target.** Merge into one type `RunConfig<R>` (or keep the name `Strider` and grow it; or `Strider` becomes a builder for `RunConfig`). The user's framing: a single configured handle to run an analysis, no separate "stable Strider" / "per-run Config" split.

**Proposal.** Replace `Strider` + `Config<'a, R>` with a single `Strider<R>` struct that holds everything:

```rust
pub struct Strider<R: rsleigh::MemReader> {
    pub arch: strider_target::SleighArch,
    pub calling_convention: strider_target::BuiltCallingConvention,
    pub sleigh_regs: rsleigh::SleighRegs,        // cached at construction
    pub alias_mode: crate::opt::AliasMode,
    pub start_addr: strider_lift::cfg::MachineInsnAddr,
    pub sleigh: rsleigh::Sleigh<R>,              // owned, lent to cfg builder via &mut
    pub rom: Option<Box<dyn ReadOnlyMemory>>,    // see #7 — no Arc
    pub fn_max_size: Option<u64>,
    pub allow_code_before_start_addr: bool,
    pub compact: bool,
    pub per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,  // pre-resolved at construction
}
```

`Strider::new(arch, cc, sleigh, start_addr, ...)` resolves regs + per-address CCs at construction. `orchestrator::run(strider) -> Result<Function>` takes the whole thing.

**Implications.**
- Drops the "reusable across runs" pattern Strider had. Each run constructs a fresh `Strider<R>`. The user's framing accepts this — the per-arch regs/cc resolution is the only "reuse" gain, and it's cheap. If callers truly want to reuse arch+regs across runs, they re-construct (and the SleighRegs probe runs again — fine, it's not on the hot path).
- The `LoopState.params: RunParams` field collapses too (it was just a per-iteration snapshot of Config fields).
- `Config` callers in `strider_analyze::run`: signature becomes `run(strider: Strider<R>) -> Result<Function>`.
- PyO3 wrapper `strider.run(...)`: builds the new `Strider<R>` from the kwargs (already does most of this — just changes which type it constructs).

**Touch surface.**
- `crates/strider-analyze/src/strider/pipeline.rs:123-136` — replace `Strider` struct.
- `crates/strider-analyze/src/orchestrator/mod.rs:63-111` — delete `Config`. Delete `RunParams`. Update `LoopState::new` signature.
- `crates/strider-analyze/src/lib.rs` — update `pub use` re-exports.
- `crates/strider-py/src/run.rs` — `run_via_orchestrator` constructs the merged type.
- Examples + benches + every test that constructs `Strider::new(...)` then a `Config { strider: &..., ... }`: collapse to one constructor call.
- The `examples/orchestrator_demo.rs` + `memory_demo.rs` are the canonical user-facing examples; rewrite to show the merged API.

---

### 6. Drop duplicate fields across structs

**Findings (from the audit).**
- `GraphRewriter` (`crates/strider-analyze/src/rewrite.rs:59-68`): holds `function` + `entry: NodeId`. `entry == function.entry().unwrap()`. **Action**: drop the `entry` field; expose as `pub fn entry(&self) -> NodeId { self.function.entry().expect("post-build invariant") }`. Update internal callers + the public `GraphRewriter::entry` method (which already exists, just sources from the field; switch to derive).
- `LoopState.params: RunParams` (`crates/strider-analyze/src/orchestrator/mod.rs:263-277, 318-363`): redundant with `Config` fields. **Action**: covered by #5 (Strider+Config merge). When `Config` collapses into `Strider`, `LoopState` borrows the Strider directly; `RunParams` is deleted entirely.
- `Matcher.entry` (`crates/strider-analyze/src/pattern/matcher/mod.rs:59-83`): **keep** — it's a perf cache (avoid `function.entry()` calls inside hot match loops); the cache is constant for the matcher's lifetime.
- `AnalyzeOutcome` / `RegionLiftHandles`: no within-struct duplication (the handles get *consumed* into a `RegionIndex` — moving data is fine, not duplicating).
- `FunctionBuilder` (`crates/strider-ir/src/builder/mod.rs:168`): audit during execution — its fields are `Function`-builder-state, no obvious derivability.
- `Builder<'a, R>` (cfg builder, `crates/strider-lift/src/cfg/builder/mod.rs:52`): each field carries distinct state — keep.

**Action.** Single commit doing the GraphRewriter fix. LoopState falls out of #5.

---

### 7. Drop `Send + Sync` + `Arc<dyn ReadOnlyMemory>`; refactor optimizer pipeline

This is the load-bearing change that unlocks #8 (delete `resolve_const_loads`).

**Current state.**
- `IndirectResolverFn<R>` (`crates/strider-lift/src/cfg/builder/indirect_resolver.rs:115-126`): `Arc<dyn Fn(...) + Send + Sync>`. Used single-threaded — only the cfg builder calls it sequentially during one build.
- `Arc<dyn ReadOnlyMemory>` plumbed through `Options::read_only_memory` (cfg builder) and the orchestrator (`Config::rom`, `RunParams::rom`). The Arc is needed because `LoadReadOnly` stores the rom and `Box<dyn Optimizer + 'static>` requires the rom's lifetime be `'static`.
- `Optimizer` trait: `Box<dyn Optimizer + 'static>` in `OptimizerPipeline`. Every pass is `'static`.

**Target.**
- `IndirectResolverFn<R>` becomes `Box<dyn Fn(...)>` (no Send + Sync, no Arc). The cfg builder holds it as an `Option<Box<...>>`; passes `&dyn Fn(...)` to the region builder when needed.
- `Optimizer` trait gains a method that receives an `OptCtx<'mem>` with optional `rom: Option<&'mem dyn ReadOnlyMemory>`:
  ```rust
  pub struct OptCtx<'mem> {
      pub rom: Option<&'mem dyn ReadOnlyMemory>,
  }
  pub trait Optimizer {
      fn run(&self, function: &mut Function, entry: NodeId, ctx: &OptCtx<'_>) -> Result<bool>;
  }
  ```
- `LoadReadOnly` becomes a unit struct (no stored rom). It reads `ctx.rom` at run time and bails if `None`.
- `OptimizerPipeline.run(function, entry, ctx)` takes the ctx and threads it to every pass.
- Orchestrator passes `OptCtx { rom: self.strider.rom.as_deref() }` per iteration.

**Changes.**
- `crates/strider-lift/src/cfg/builder/indirect_resolver.rs`: drop `+ Send + Sync` from the type alias; switch `Arc` → `Box`. Update consumers (`Builder.indirect_resolver: Option<IndirectResolverFn<R>>`, `with_indirect_resolver`, the call site in `region_builder.rs:454`).
- `crates/strider-lift/src/cfg/options.rs`: `Options::read_only_memory` becomes `Option<Box<dyn ReadOnlyMemory>>` OR remove from Options entirely if the rom is now threaded through the orchestrator's `OptCtx` rather than the cfg builder. Decide: the cfg builder needs ROM only for the indirect resolver's const-load lookups — if the resolver receives `rom` as a parameter from the orchestrator, Options doesn't need it. **Cleaner**: remove from Options; the resolver closure captures the rom by reference at construction.
- `crates/strider-analyze/src/opt/`: every pass's signature updates to `fn run(&self, f, e, ctx)`. The peephole-pass macro generator (if there is one) updates once. Most passes ignore `ctx`.
- `crates/strider-analyze/src/opt/load_readonly/mod.rs`: drop the `rom: Arc<dyn ReadOnlyMemory>` field + the constructor that takes it. Add `LoadReadOnly` as a unit struct. Use `ctx.rom` at run time.
- `crates/strider-analyze/src/orchestrator/mod.rs`: orchestrator's `Strider.rom` is `Option<Box<dyn ReadOnlyMemory>>` (no Arc). Threaded into `OptCtx` per iteration.
- `crates/strider-py/src/run.rs` and `opt.rs`: PyO3 wrappers update — `LoadReadOnly()` no longer takes a memory arg from Python; the rom comes from the orchestrator's config.
- Python README + `_api.py`: `Analyzer` / `Program.analyze` already takes a `rom=` kwarg; no Python-surface change.

**Note**: the existing `LoadReadOnly::new(rom)` Python constructor is currently used in custom pipelines (e.g. `strider/_api.py::_build_user_pipeline_with_fcc` in tests, the user's `opt.LoadReadOnly(mem)` in the FCC test). With the refactor, custom pipelines stop taking the rom at pass-construction; instead, the user passes `rom=` to `strider.run` / `Analyzer.analyze` and it threads through `OptCtx`. Update those tests. (Trade-off: custom pipelines lose the per-pass-rom override; the assumption is one rom per run, which has always been the case.)

---

### 8. Delete `resolve_const_loads` — call LoadReadOnly's logic directly

Depends on #7. With `LoadReadOnly` no longer requiring `'static` rom, the indirect resolver's `resolve_const_loads` (`crates/strider-analyze/src/indirect_resolver.rs:335-391` — a verbatim copy of the `LoadReadOnly::try_rewrite` core) collapses.

**Changes.**
- Extract the per-node fold into a shared helper: `pub(crate) fn fold_const_load(function, node_id, rom) -> Result<bool>` in `crates/strider-analyze/src/opt/load_readonly/mod.rs`.
- `LoadReadOnly::try_rewrite` calls `fold_const_load(ctx.function, root, ctx.rom)`.
- `crates/strider-analyze/src/indirect_resolver.rs::resolve_const_loads` deletes its body and becomes a `walk + fold_const_load per node` loop (or just inlines the fold helper). Or — even simpler — the indirect resolver's mini-IR runs the LoadReadOnly pass directly via the new ctx (run a tiny pipeline with just LoadReadOnly).
- Drop the lockstep-comment + the duplicate code.

---

### 9. Lambda + `&dyn Fn` coercion noise cleanup

**Specific sites.**
- `crates/strider-analyze/src/strider/insn/mod.rs:62, 66` — the user's example:
  ```rust
  let lookup: &dyn Fn(strider_lift::cfg::RegionId) -> Result<strider_ir::RegionId> = &region_lookup;
  self.handle_branch(region_id, lookup)?
  ```
  becomes
  ```rust
  self.handle_branch(region_id, &region_lookup)?
  ```
  (the `&region_lookup` already coerces to `&dyn Fn(...)` at the call site; the explicit type binding adds noise). Same for the sibling `handle_cond_branch` call at line 66.
- `crates/strider-analyze/src/strider/insn/control.rs:112, 146, 177` — `_region_lookup: &dyn Fn(...)` parameter type. If the callee receives an `impl Fn(...)` (monomorphized), it can stay generic; if it receives `&dyn`, that's fine — the **caller** doesn't need to pre-coerce.
- `apply_rules_in_order` callers — `let apply = apply_rules_in_order(&rules); apply(ctx, node)?` at four sites (`crates/strider-analyze/src/rewrite.rs:169`, `crates/strider-analyze/src/opt/flag_cmp_canonicalize/mod.rs:82`, two test sites in `pattern/rewrite.rs:734, 755, 777`). Simplify the call sites where the temporary is bound only to be invoked once: `apply_rules_in_order(&rules)(ctx, node)?` — though that's terser than readable. Alternative: keep the helper but inline its body where it's only called once. **Inspect at execution time** and decide per site (some may genuinely need the binding for reuse; others don't).
- General IIFE scan: agent found no widespread cases. Confirm at execution time.

**Action.** Single commit; small touch surface.

---

### 10. Bounded-lift overflow → error, not silent tail call

**Current state.** `crates/strider-lift/src/cfg/builder/region_builder.rs::detect_fallthrough_oob_tail_call` (~lines 682-690): when sequential decode reaches an address ≥ `start_addr + fn_max_size` with no explicit terminator, it silently emits `RegionTerminator::TailCall { target }`. **Bug.**

**Target.** Raise an error (`bail!`) instead. Explicit `Branch` / `CondBranch` opcodes whose target is OOB stay correctly classified as `TailCall` — those go through `is_branch_tail_call_nocheck` from the *opcode* path, not from `detect_fallthrough_oob_tail_call`.

**Changes.**
- Replace the `Ok(true)` branch with `bail!("function boundary error at {addr:?}: sequential decoding overflowed past [start={:#x}, start + fn_max_size = {:#x}); function is unterminated within the recorded bound", ...)`.
- Add a regression test in `crates/strider-lift/tests/cfg_build_end_to_end.rs`: synthetic x86 bytes that decode several instructions without a terminator, `fn_max_size` chosen to clip mid-function. Assert the lift errors with the new message.
- Reproducer the user named ("tzcount as an object file") is fixed transitively once ELF object-file lifting works (#11) — but the bug fix here is independent of #11.

---

### 11. ELF: use segments for executables/ET_DYN; handle ET_REL via sections with VMA dedup

**Current state.** `crates/strider-reader/src/elf/sections.rs:29-45` (`collect_sections_as_mem_regions`) iterates `obj.sections()`. Works for stripped executables / ET_DYN but produces an incomplete map for object files (ET_REL) — and is wrong-axis for executables (program headers are the canonical loadable view).

**Target.**
- For `obj.file_kind() == FileKind::Executable | FileKind::Dylib` (and ET_DYN PIE): walk `obj.segments()` (program headers; PT_LOAD entries) — that's the runtime memory layout.
- For `FileKind::Relocatable` (.o, ET_REL): no program headers exist. Fall back to walking sections, but dedupe by VMA: a `.o` file's `.text` and `.text.startup` can both sit at VMA 0 before linking, which the current `MemRegionsLookupTable` rejects or silently overwrites — explicitly handle the collision (last-wins or first-wins, document the choice).
- `apply_elf_relocations` becomes aware: ET_DYN uses `obj.dynamic_relocations()` (existing path); ET_REL needs per-section relocation tables (`obj.sections().filter(SHF_REL || SHF_RELA)`) applied before lifting.

**Changes.**
- `crates/strider-reader/src/elf/sections.rs`: rename or restructure — `collect_loadable_regions` that dispatches on file kind.
- `crates/strider-reader/src/elf/reader.rs:37-46`: caller updates trivially.
- `crates/strider-reader/src/elf/relocations.rs`: add ET_REL relocation walk + apply.
- `crates/strider-reader/src/lib.rs::MemRegionsLookupTable`: confirm the dedup policy (first-wins or last-wins, with a debug log on conflict).
- Add a test fixture: `fixtures/cases/tzcount.c` (small function that exhibits the lift bug from the user's reproducer) compiled to `.o`. Add a test that loads the `.o` and lifts the function. (Also fixes the user's reproducer for #10 transitively.)
- Update `crates/strider-py/strider/_api.py::load` documentation: object files now work; describe the auto-detection.

---

### 12. Confirm "keeps" — decode cache + 16-byte padding

**Decode cache** (`crates/strider-lift/src/cfg/decode_cache.rs`): rsleigh has no internal cache; strider's cache amortizes decoding across CFG rebuilds in the fixed-point loop (the orchestrator can rebuild N times). **Keep.** Action: add a one-line comment to the module doc clarifying it's not duplicating an rsleigh feature.

**16-byte padding**: rsleigh's `MAX_MACHINE_INSN_LEN = 16` (rsleigh/src/lib.rs:30-34) means `Sleigh::lift_one` over-reads 16 bytes per instruction. Strider's nop-padding in test fixtures is load-bearing (satisfies rsleigh's contract). **Keep.** No code change.

(These two are user-stated "verify if X, if not remove"; verification confirms keep.)

---

## Verification gate (per commit)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cd crates/strider-py && uv run maturin develop && uv run pytest -q
```

Baseline: 844 Python passed / 0 skipped on `rewrite/strider`. Expected end state: the same or higher (a few Python tests will lose their `OffsetCapture` / `BitNot` matchers and get rewritten or deleted). Rust tests will gain a few new regression pins (object-file lift, bounded-lift overflow error) and lose some that exercised deleted features.

## Sanity greps after final commit (must be empty)

```bash
rg -n 'IntUnaryOp::BitNot|IntUnaryOp\s*::\s*BitNot' crates/ -g '!**/target/**' --type rust
rg -n 'OffsetCapture' crates/ -g '!**/target/**'
rg -n 'OutputsSpec|ConsumersSpec|next_unique_consumer|match_unique_output_consumer' crates/ -g '!**/target/**' --type rust
rg -n '\bConfig<' crates/strider-analyze/ -g '!**/target/**' --type rust   # the merged type uses `Strider<R>`
rg -n 'Send \+ Sync|Arc<dyn ReadOnlyMemory>' crates/ -g '!**/target/**' --type rust
rg -n 'resolve_const_loads' crates/ -g '!**/target/**' --type rust
rg -n 'detect_fallthrough_oob_tail_call' crates/ -g '!**/target/**' --type rust  # the function name may move; the behavior changes to error
```

## Branch flow

```bash
git checkout -b rewrite/strider-cleanup rewrite/strider
# ... 12 commits ...
git push origin rewrite/strider-cleanup
# Final review (pr-review-toolkit:code-reviewer over rewrite/strider..HEAD)
git checkout rewrite/strider && git merge --ff-only rewrite/strider-cleanup
git push origin rewrite/strider
git branch -d rewrite/strider-cleanup && git push origin --delete rewrite/strider-cleanup
```

## Resolved decisions

1. **`OutputsSpec` / `ConsumersSpec` deletion** — confirmed delete.
2. **"Skipping regions"** — keep `ignore_regions: bool` matcher flag.
3. **Merged type name** — `RunConfig<R>`.
4. **VMA dedup policy** — first-wins.
5. **`Optimizer` trait** — Option A: `OptCtx<'mem>` arg on `run()`.

## Re-verification corrections (vs. earlier agent reports)

- **Point 1 (decode cache) — FLIPPED.** GHIDRA C++ has `DisassemblyCache` (`rsleigh/sleigh/src/sleigh.hh:107-120`, used by `Sleigh::oneInstruction` at `sleigh.cc:741, 772`). Strider's `DecodeCache` is the duplicate. **Delete strider's** — see new point below.
- **Point 5 (16-byte padding) — no code change.** rsleigh DOES require 16-byte over-read (`MAX_MACHINE_INSN_LEN = 16`, C++ `ParserContext::buf` is 16). Production strider code never pads (ELF regions extend naturally); only synthetic test byte arrays pad, which is the legitimate contract. **User wants a final re-prompt on this at the end** of execution.
- **Point 3 (Send + Sync + Arc) — scope-expanded.** Pervasive in pattern crate: `trait Pattern: Send + Sync`, `trait Optimizer: Send + Sync`, every closure type alias (`OutputPredicate`, `PostMatchFn`, `KindCheckFn`, `BuildFn`, `BuildValueFn`, `BoxedRule`) carries `+ Send + Sync`. Drop everywhere it's not load-bearing; keep on `Error: Send + Sync + 'static` trait bounds (anyhow ergonomics — that's `std::error::Error`, not our types).

## End-of-execution re-prompts

User requested deferred prompts at the end of execution (after all other commits land, before merge):

- **Padding (point 5)** — re-prompt for any production change desired.
- **Point 14 (IntCmpOp/IntBinaryOp + FloatCmpOp/FloatBinaryOp merge)** — re-prompt to decide.
- **Metadata merge (`phi_var_tag` + `call_clobbered_overrides` → `NodeVnMeta`)** — re-prompt to decide.

## Point 14 — deferred (re-prompt at end)

Merge `IntCmpOp` into `IntBinaryOp` (and `FloatCmpOp` into `FloatBinaryOp`). Variants today differ on output type only (`IntCmpOp::*` always `I1`; `IntBinaryOp::*` `InheritRoot`). Touch surface:
- `crates/strider-ir/src/ops/op_kinds.rs` — merge enums.
- `crates/strider-ir/src/node.rs` — `NodeKind` variants `IntCmpOp(IntCmpOp)` + `IntBinaryOp(IntBinaryOp)` collapse.
- `crates/strider-ir/src/node_signature.rs` — signature dispatch table; cmp ops keep `BuildTy::Fixed(I1)`, binary ops keep `InheritRoot` — the *node-signature* table now distinguishes by per-op metadata instead of by enum.
- `crates/strider-lift/src/pcode_lift/value/*` — lift dispatch tables.
- `crates/strider-analyze/src/pattern/pat/ctor/int.rs` + `float.rs` — `int_eq`, `int_lt`, etc. now build the unified op; the type-output assertion moves to the builder.
- `crates/strider-analyze/src/opt/*` — every rule that pattern-matches on the cmp family unifies with the binary family.
- `crates/strider-analyze/src/pattern/pat/builders/binary_op.rs` — `BinaryOpKind` trait gains a `is_cmp() -> bool` helper or the `BuildTy` selector reads it; otherwise builders unify.
- All tests + `cross_arch_shape` snapshot.

## Sequence (final)

Numbered commits in dependency order:

1. **Delete strider's `DecodeCache`** (flipped point 1) — `decode_cache.rs` deleted, `Builder::with_decode_cache` deleted, orchestrator threading deleted. ~6 files.
2. **`IntUnaryOp::BitNot` → `Xor(x, all_ones)`** (point 8) — lift dispatch + every optimizer rule + pattern builders + ~20 test files.
3. **Delete `OffsetCapture`** (point 11) — pattern surface + bindings parallel journal + PyO3 mirror + Python tests + README.
4. **`Bindings` → `enum { Node(NodeId), Output(NodeOutputId) }`** (point 12) — depends on #3 since both edit `bindings.rs`.
5. **Delete `OutputsSpec` + `ConsumersSpec` + consumer-walk subsystem** (point 6) — `node_pat.rs`, `consumer.rs` (delete), `consumer_match.rs` (delete), `call`/`store` builders, complex_patterns.rs tests.
6. **Merge `Strider` + `Config<'a, R>` → `RunConfig<R>`** (point 13) — orchestrator, run.rs, examples, tests.
7. **`GraphRewriter.entry` field deletion** (point 2) — derive from `function.entry()`.
8. **`OptCtx<'mem>` + drop `Send + Sync` + drop `Arc<dyn ReadOnlyMemory>` + drop redundant `entry` arg + delete `resolve_const_loads`** (points 3, 4, 7 + extra) — biggest single change.
   - Optimizer trait gains `ctx` arg.
   - `LoadReadOnly` becomes unit struct.
   - `IndirectResolverFn` becomes `Box<dyn Fn(...)>`.
   - `Arc<dyn ReadOnlyMemory>` → `Option<Box<dyn ReadOnlyMemory>>` in `RunConfig`.
   - Pattern-crate `Send + Sync` removed where vestigial.
   - `resolve_const_loads` deleted in favor of `LoadReadOnly` helper.
   - **`entry` argument dropped from `Optimizer::run(&self, &mut Function, ...)` and `OptimizerPipeline::run`** — derivable via `function.entry().expect(...)`. ~25 call sites collapse (`pipeline.run(&mut function, entry)` → `pipeline.run(&mut function)`). Same applies to `apply_rules_in_order` callers + `RewriteCtx` if it carries entry redundantly.
9. **Lambda + `&dyn Fn` coercion noise** (point 7) — `strider/insn/mod.rs:62,66` + `apply_rules_in_order` callers.
10. **Bounded-lift overflow → error** (point 10) — `detect_fallthrough_oob_tail_call` bails.
11. **ELF segments for executables/ET_DYN; ET_REL via sections with first-wins VMA dedup** (point 9) — `strider-reader/src/elf/*` + new fixture (`tzcount.c` compiled to `.o`) + test.
12. **Wrap-up** — final code-review pass over `rewrite/strider..HEAD`, re-prompt user on padding + point 14 + metadata merge, ff-merge into `rewrite/strider`, delete branch.

---

## What's NOT changing

Points the user listed where investigation found no action needed:
- Decode cache (point 1) — confirmed kept (rsleigh has none; strider's amortizes CFG rebuilds).
- 16-byte padding (point 5) — confirmed kept (rsleigh requires it).

Adjacent items the user did NOT list — intentionally untouched:
- Per-Function side-tables (`stack_offsets`, `arg_index_to_nodes`, etc.) — orthogonal.
- `RegionTerminator` enum — clean from the previous refactor.
- PySleigh wrapper — clean from the previous refactor.
- ELF symbol table + add_elf — orthogonal to segments-vs-sections.
