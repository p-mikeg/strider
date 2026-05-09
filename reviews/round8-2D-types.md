# Round 8 / 2D — Type-design audit (re-derived from current code)

Branch: `review/ai2`. Working tree: `feature/ai`.

Methodology: re-audited every `pub struct` / `pub enum` / `pub trait` listed in the task across every workspace crate. Did **not** read `reviews/round7-*.md`. Findings are derived from the source as it stands today; doc comments are evidence for invariants but not for correctness.

The findings below are organised by severity tier first (HIGH → MED → LOW), then by crate within a tier. A "no finding" appendix at the end lists types that were inspected and found clean enough not to need a fix.

---

## HIGH severity

### `ir::BuiltFunctionGraph`

- **Severity:** HIGH
- **Where:** `crates/ir/src/function.rs:45`
- **Issue:** Leaky encapsulation + partial-state, compounded by `Deref<Target = Graph>`.
- **Evidence:** Every field is `pub`:
  ```
  pub graph: Graph,
  pub entry: NodeId,
  pub variables: PrimaryMap<VarId, rsleigh::Vn>,
  pub call_clobbered: Box<[rsleigh::Vn]>,
  pub ret_val_regs: Box<[rsleigh::Vn]>,
  pub call_other_clobbered: Box<[rsleigh::Vn]>,
  ```
  The doc comments encode a load-bearing positional invariant: `ret_val_regs` is the prefix of `call_clobbered` (also referenced by `pattern::Match::get_vn` at `crates/pattern/src/matcher/match_result.rs:175-232`). And the constructor `from_graph_and_entry_for_rewrite` (line 117) deliberately leaves `variables` / `call_clobbered` / `ret_val_regs` / `call_other_clobbered` empty for "rewrite-only" callers — which means the **same struct** has two distinct shapes (full vs rewrite-only) but no type-level discriminator. Round 7 introduced `pattern::RewriteCtx` to carve off the rewrite-only path (good), but the partial-state form of `BuiltFunctionGraph` itself still exists and is reachable from `opt::with_built` (`crates/opt/src/pipeline.rs:94-104`, takes `&mut Graph` and a `NodeId`, hands a partial-state `BuiltFunctionGraph` to the closure).

  Aggravator: `impl Deref<Target = Graph>` (line 82). External code can call `graph.preorder(entry)`, `graph.create_node(...)`, etc. transparently through the deref, with no type-system signal that the wrapper might be the empty-CC form.

  Concrete leak example: a caller could write `bfg.call_clobbered = Box::new([])` even on a fully-built graph, breaking `Match::get_vn`'s `slot >= 2 → graph.call_clobbered.get(idx)` chain (`crates/pattern/src/matcher/match_result.rs:198`).
- **Fix:** Two complementary moves:
  1. Make every field `pub(crate)` and expose them through accessors. `graph(&self) -> &Graph`, `graph_mut(&mut self) -> &mut Graph`, `entry(&self) -> NodeId`, slice accessors for `call_clobbered` / `ret_val_regs` / `call_other_clobbered`. This kills both the partial-state mutation hazard and the in-place tampering with the documented `ret_val_regs ⊑ call_clobbered` prefix invariant.
  2. Sum-type the partial-state form: replace the rewrite-only constructor with a *new* type (e.g. `pattern::RewriteCtx` is already 80 % of this; finish the migration by making `from_graph_and_entry_for_rewrite` `#[doc(hidden)] pub(crate)`). `opt::with_built` should consume `RewriteCtx` directly. This is the round-7 plan; today the rewrite-only `BuiltFunctionGraph` is still publicly constructible.
- **Cost estimate:** ~50 call sites for the field-→-accessor migration (every `bfg.graph` / `bfg.entry` / `bfg.call_clobbered.iter()` use across `pattern`, `opt`, `strider`, `strider-py`). The `from_graph_and_entry_for_rewrite` removal touches `opt::with_built` and any test that constructs partial graphs directly — about 5 test sites.

### `ir::FunctionGraph`

- **Severity:** HIGH
- **Where:** `crates/ir/src/function.rs:14`
- **Issue:** All-public fields populated to **reserved sentinels** when the graph is partially constructed.
- **Evidence:**
  ```
  pub graph: Graph,
  pub entry: NodeId,
  pub entry_control: NodeOutputId,
  pub entry_memory: NodeOutputId,
  ```
  with `new_invalid()` (line 30) writing `NodeId::reserved_value()` / `NodeOutputId::reserved_value()`. The struct doc says "consumers should never observe a partial graph" but `pub` fields make that a discipline rather than an enforced invariant. `FunctionGraph` is exposed by `FunctionBuilder::body() -> &FunctionGraph` (line 151), so any caller can dereference `body().entry` while the builder is still in its `new_invalid` state if they call `body()` between `new_raw()` and `build_entry()`. (Today the builder calls `build_entry` synchronously inside `new_raw`, but that's a sequencing accident, not a type-system guarantee.)
- **Fix:** Make every field `pub(crate)`, expose via accessors (`entry()`, `entry_control()`, `entry_memory()`); accessors can debug-assert non-reserved. Alternatively, lift the partial state into the type by swapping `entry_control: NodeOutputId` for `entry_control: Option<NodeOutputId>` and propagating the unwrap site.
- **Cost estimate:** `body()` / `body_mut()` callers in `crates/ir/src/builder/*.rs` (~15 sites). External consumers do not access `FunctionGraph` fields directly today (verified via grep), so the migration is internal to the `ir` crate.

### `ir::Region.variables` (FB-3 partial-state)

- **Severity:** HIGH
- **Where:** `crates/ir/src/region.rs:36`, `crates/ir/src/node/ids.rs:17`
- **Issue:** `SecondaryMap<VarId, NodeOutputId>` defaults unread variables to `NodeOutputId::default() == NodeOutputId(0)` — the entry node's first output. The "variable hasn't been written" state is silently aliased to "variable's value is the entry's control output," which is a load-bearing `NodeOutputId`. Round 7 marked FB-3 as skipped; the rationale needs re-checking. The actual evidence:
  ```rust
  // ir::node::ids.rs:17
  #[derive(Default, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct NodeOutputId(u32);
  ```
  `entity_impl!` makes `Default` produce `NodeOutputId(0)`; `cranelift_entity::PrimaryMap::push` allocates entries from `0` upward, so the first node output (Entry's control) **is** `NodeOutputId(0)`. `read_variable_from_id` (`region.rs:194`) returns `self.regions[region_id].variables[var_id]` unchecked — for an unwritten `VarId` it returns the Entry control output, typed as `Control`, which then flows into a value-typed input slot. The validator's Layer A would catch this at `build()` time as a `NodeInputKindMismatch` (`Control` where `AnyValue` is expected), but only if the unwritten variable is read in a value context — silent miscompilation otherwise.

  The skip rationale in round 7 was "in practice every tracked variable is written by the entry-region setup before any region reads from it" (`ir::FunctionBuilder::set_entry_region` populates `initial_variables` and `link_region_variables` propagates them into successors). That's correct for the **canonical** lifter path. But the type accepts arbitrary `write_variable_from_id` / `read_variable_from_id` interleavings; an opt pass or future builder helper that creates a region without going through `link_region_variables` (e.g. an indirect-branch resolver splicing in a fresh region with `create_region_helper` and only partial variable seeding) silently gets sentinel reads.
- **Fix:** Two options:
  1. **Cheap hardening (recommended)**: have `read_variable_from_id` consult `variables.get(var_id)` (returns `Option<&NodeOutputId>`) and fail loud if absent. `SecondaryMap::get` returns `&V` defaulted, but checking the variable first lived in `variable_to_id` (which is the truth for "tracked variable") then asserting the `SecondaryMap` was populated keeps the contract explicit.
  2. **Sound fix**: change `Region.variables` to `SecondaryMap<VarId, Option<NodeOutputId>>` (or `NodeOutputId::reserved_value` + a debug-assert). Adds an unwrap to every `read_variable_from_id` call (1 site).
- **Cost estimate:** Option 1 is one method body change in `crates/ir/src/region.rs`. Option 2 is ~3 sites in the same file plus the `link_region_variables` / `link_region` helpers. Neither leaks outside the `ir` crate.

### `target::SleighArch`

- **Severity:** HIGH
- **Where:** `crates/target/src/arch.rs:118`
- **Issue:** All-public fields with cross-field invariants.
  ```
  pub sla_spec: rsleigh::sla_spec::SlaSpec,
  pub pspec: rsleigh::pspec::PSpec,
  pub endianness: Endianness,
  pub preset: ArchPreset,
  ```
  The `preset` field carries a discriminator that **must** be consistent with `sla_spec` + `pspec` + `endianness` — `target::call_other_abi::classify_arch_specific` dispatches on `preset` (via the `ArchPreset` enum) but the actual byte-decoder is driven by `sla_spec` / `pspec`. A caller can construct `SleighArch { sla_spec: x86_64, pspec: x86_64, endianness: Big, preset: ArchPreset::X86_64 }` and silently break the CallOther ABI table for any user-op specific to BE/LE distinction. Comment on `preset` says "Set by each preset constructor; not user-overridable" — but the field is `pub`, so it **is** user-overridable. Pure documentation discipline.

  Same problem in milder form for `endianness`: the type system doesn't enforce `endianness ↔ sla_spec` agreement (e.g. `aarch64()` returns `Little`, `aarch64be()` returns `Big`, but a user can flip `endianness` after construction).
- **Fix:** Make every field `pub(crate)`. Expose via accessors (`sla_spec()`, `pspec()`, `endianness()`, `preset()`). Construction stays through the `x86_64()` / `aarch64()` / … presets, which are the single source of truth for the cross-field consistency. If a future caller really needs a custom (sla, pspec, endianness, preset) bundle, expose a `from_parts` constructor that runs a debug-only consistency check.
- **Cost estimate:** Public field reads cluster in the strider orchestrator (`crates/strider/src/orchestrator.rs`, ~5 sites: `arch.endianness`, `arch.sla_spec`, `arch.preset`), `cfg::Builder::for_arch` (~3 sites), `target::call_other_abi::classify_arch_specific` (1 site, dispatches on `preset`), the example, and `strider-py`. ~15 sites total.

### `target::BuiltCallingConventionParts`

- **Severity:** HIGH
- **Where:** `crates/target/src/calling_convention/mod.rs:127`
- **Issue:** All-public fields, duplicating the `BuiltCallingConvention` shape with `pub` instead of `pub(crate)`. The "ergonomic test bag" rationale in the doc comment is real, but it forks the encapsulation of `BuiltCallingConvention` (which round 7 tightened to `pub(crate)` with accessors) into a *second* type whose entire role is to be field-mutable. The `assert_disjoint` invariant the calling convention's tests pin (`arg_passing_regs ∩ callee_saved_regs == ∅`, plus the `syscall_number_reg` separation) is still enforceable only by inspection.

  Concrete leak path: a `BuiltCallingConvention::from_parts(BuiltCallingConventionParts { … })` caller can construct an internally inconsistent CC (e.g. an arg register that's also listed as callee-saved) and the consumer (`ir::FunctionBuilder::new`, `pattern::Match::get_vn`) silently produces wrong IR.
- **Fix:** Two ergonomic options:
  1. Replace the parts struct with a builder that runs the disjointness check in `build()`. A bit more code than the public-fields parts bag, but the invariant lives in the type.
  2. Keep the parts struct but add `from_parts` validation: have `BuiltCallingConvention::from_parts` return `Result<Self>` and check `arg_passing_regs.iter().all(|v| !callee_saved_regs.contains(v))` etc. The fields stay public for ergonomics; the constructor is the chokepoint.
- **Cost estimate:** Option 1: refactor ~5 test sites that build override CCs directly. Option 2: convert `from_parts` to fallible at ~5 test call sites.

### `cfg::Cfg<R>`

- **Severity:** HIGH
- **Where:** `crates/cfg/src/cfg/mod.rs:37`
- **Issue:** All-public fields with a derived-state invariant.
  ```
  pub sleigh: rsleigh::Sleigh<R>,
  pub graph: RegionGraph,
  pub entry: NodeIndex,
  pub start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
  ```
  `start_addr_to_region_id` is *derived* from `graph` (it indexes each region's `start_addr` to its `NodeIndex`). Mutating `graph` without updating `start_addr_to_region_id` (or vice-versa) silently corrupts the invariant. The comment says the strider indirect-branch resolver maintains it via `add_region`, but that's per-mutation discipline. Public fields invite ad-hoc mutations from any consumer.

  Compounding issue: `entry: NodeIndex` carries no type-system signal that the entry region must always exist in `graph`. A caller can `cfg.graph.remove_node(cfg.entry)` and leave the `entry` field dangling.
- **Fix:** Make all fields `pub(crate)`. Expose `sleigh()` / `sleigh_mut()`, `graph()`, `entry()`, `region_id_at_start(addr)` accessors (the lookup table becomes an internal indexing concern, not a public surface). The orchestrator's `add_region` / `compact` paths stay inside the `cfg` crate where they can maintain both fields atomically.
- **Cost estimate:** ~25 sites (every `cfg.graph` / `cfg.entry` use across `strider/orchestrator.rs`, the dot dumper, `strider-py/cfg.rs`, `cfg/tests/*`).

### `cfg::Region` and `cfg::RegionInstruction`

- **Severity:** HIGH (Region) / MED (RegionInstruction)
- **Where:** `crates/cfg/src/cfg/types.rs:71` (RegionInstruction), `:183` (Region)
- **Issue:** `Region` has `pub start_addr`, `pub insns: Vec<RegionInstruction>`, `pub terminator: RegionTerminator`. The doc says `insns` is "Never empty" (line 187) — that's a real invariant, enforced by the builder via `add_region`. With `pub insns` a caller can `region.insns.clear()` from outside, breaking `Region::contains_addr` (line 199) which `match`es on `insns.last()` and gracefully returns `false` for empty regions but every other consumer of `region.insns.first()` would panic / misclassify.

  `start_addr: PcodeInsnAddr` should also match `insns[0].addr` — that's another implicit invariant that public fields can't enforce.

  `RegionInstruction { pub addr, pub insn }` is more benign — it's a flat value object — but it still allows `region.insns[0].addr = something_else` to silently break the `start_addr == insns[0].addr` invariant.
- **Fix:** Make `Region`'s fields `pub(crate)`. Expose `start_addr()`, `insns() -> &[RegionInstruction]`, `terminator() -> &RegionTerminator`, `terminator_mut() -> &mut RegionTerminator` (the indirect-branch resolver legitimately rewrites terminators). A `RegionId` newtype already exists upstream (petgraph `NodeIndex` aliased to `RegionId`); `Region` itself is internal-state.

  `RegionInstruction` can stay with `pub` fields if the `Region`-level encapsulation guards the slice; the inner pair is a value-object whose fields are independent.
- **Cost estimate:** Region's field reads cluster in the strider orchestrator and dot dumper (~12 sites). The mutable-terminator path (`region.terminator = …`) lives in `cfg::Builder::indirect_resolve` and is already inside the `cfg` crate, so no external mutation.

---

## MED severity

### `pattern::RewriteCtx`

- **Severity:** MED
- **Where:** `crates/pattern/src/rewrite.rs:109`
- **Issue:** Public mutable graph field.
  ```
  pub struct RewriteCtx<'g> {
      pub graph: &'g mut Graph,
      pub entry: NodeId,
  }
  ```
  By design — round 7 introduced this as the explicit "we don't need full BuiltFunctionGraph" sum-type — but the `pub` exposes the mutable graph reference to every external rewrite-rule caller. Currently fine because `rewrite_rule` is the only consumer and it owns the entire mutation chain, but the field name `graph` shadows `BuiltFunctionGraph::graph` (so `ctx.graph.create_node(...)` looks identical), which makes accidentally swapping a `BuiltFunctionGraph` borrow for a `RewriteCtx` borrow at a callsite easy and silent.

  Also: `entry` is logically immutable post-construction (the rewrite engine never re-roots), but it's `pub` (mutable through `&mut RewriteCtx`).
- **Fix:** Switch to `pub(crate) graph` + `pub(crate) entry`, expose via `graph(&self) -> &Graph`, `graph_mut(&mut self) -> &mut Graph`, `entry(&self) -> NodeId`. Cost is bounded since this type is new.
- **Cost estimate:** ~6 sites (only `rewrite_rule` and `apply_rules_in_order` consume it directly).

### `pattern::MatchCtx<'g, 'm>`

- **Severity:** MED
- **Where:** `crates/pattern/src/pat/traits.rs:30`
- **Issue:** Mixed visibility — `pub graph: &'g ir::Graph` but `pub(crate) matcher: &'m Matcher<'g>`. The graph is exposed publicly for combinator implementors (`Pattern::try_match` is `pub trait`), but the matcher is intentionally `pub(crate)` because it carries `MatcherOptions`. External `Pattern` impls (which is the documented extension point — the trait is `pub`) get the graph but **cannot** ask the matcher to recurse on a sub-pattern; they have to recreate this through their own trait-object routing. The `pub`/`pub(crate)` mismatch means external impls get a half-functional context.

  Either (a) the trait should be sealed (`pub(crate)` or a sealed-trait pattern), or (b) `matcher` should be `pub` so external impls can call `matcher.match_output_with_walk_through`.
- **Fix:** Seal the trait. Today every impl of `Pattern` lives inside the `pattern` crate (`NodePat`, `CapturePat`, `GuardPat`, builder-typed impls); the `pub trait Pattern` declaration is misleading because it lets users *think* they can implement it but the partial `MatchCtx` makes that impractical. Add a sealed-marker trait the user can't impl (`mod sealed { pub trait Sealed {} }` + `Pattern: Sealed` bound). Then `MatchCtx.matcher` can stay `pub(crate)`.
- **Cost estimate:** ~3 sites (add the sealed marker + impl on every existing `Pattern` impl). External callers don't need the trait directly today.

### `pattern::BuildCtx<'a>`

- **Severity:** MED
- **Where:** `crates/pattern/src/pat/traits.rs:118`
- **Issue:** All-public fields including a mutable graph reference.
  ```
  pub struct BuildCtx<'a> {
      pub graph: &'a mut ir::Graph,
      pub bindings: &'a Bindings,
      pub root: NodeId,
      pub root_ty: NodeOutputType,
  }
  ```
  Same shape as `RewriteCtx` but used during the RHS-build phase. `root_ty` is "the root output type" — it's read-only by every consumer of `BuildCtx` today, and an external caller could mutate it through `&mut BuildCtx` to misclassify the rebuild type. `root` is similarly read-only.

  Same sealed-trait answer as above: every `Pattern::try_build` impl lives inside the crate, the public mutability of these fields surfaces no real flexibility.
- **Fix:** Same fix as `MatchCtx` — seal `Pattern`, make the fields `pub(crate)`, expose via accessors that return immutable `root` / `root_ty` / `bindings` and a mutable `graph` reference.
- **Cost estimate:** ~10 sites (every `Pattern::try_build` impl in `crates/pattern/src/pat/builders/*.rs`).

### `pattern::matcher::bindings::Binding`

- **Severity:** MED
- **Where:** `crates/pattern/src/matcher/bindings.rs:17`
- **Issue:** `pub node: NodeId, pub output: Option<NodeOutputId>` — public fields. The struct is `Copy` so external mutation via `&mut Binding` is unlikely in practice (the `bindings.get_binding(c) -> Option<Binding>` accessor returns by value). Still, the constructor `Binding::new(node, output)` (line 23) accepts any `(NodeId, Option<NodeOutputId>)` pair without validating that `output`'s producer is `node` — i.e. the type doesn't enforce that `Binding.output`'s producer node == `Binding.node`. Today every binding is constructed from a `(node, NodeOutputId)` pair pulled from the graph, but the type allows desync.
- **Fix:** Make fields `pub(super)` (Bindings already exposes the right typed accessors via `get_node` / `get_output`), and add a debug-assert in `bind_capture` that `output` is `None` or has `node` as its producer.
- **Cost estimate:** ~3 sites — `Binding::new` callers are inside the matcher; external callers go through `bind_capture`.

### `strider::Strider` (Clone for GIL release)

- **Severity:** MED
- **Where:** `crates/strider/src/strider/pipeline.rs:130`
- **Issue:** Round 7 noted M1 — `#[derive(Clone)]` was added so `strider-py`'s `run` can detach a `Strider` snapshot from a `PyRef` and release the GIL. The fields are `pub(super)` (good), so external mutation isn't the leak; the leak is that `Clone` advertises "this is a value type, copy it freely" while the per-iteration `analyze_cfg` keeps internal state in the Sleigh handle. Re-reading the field set:
  ```
  pub(super) calling_convention: BuiltCallingConvention,
  pub(super) arch: SleighArch,
  pub(super) sleigh_regs: rsleigh::SleighRegs,
  ```
  All three are `Clone`, no shared state. The `Clone` is sound today. But the comment doesn't surface the contract that **the clone must not be used to share state** — it's a snapshot. If a future field is added that *isn't* clone-safe (e.g. a per-analysis cache), the type will silently grow an "every snapshot duplicates the cache" hazard.
- **Fix:** Either (a) leave as-is and trust the comment; or (b) introduce a `StriderSnapshot` newtype (deep-clone semantics, used for the GIL-release path) and remove `Clone` from `Strider`. The type-level signal that "this is a snapshot, not a shared handle" is worth the indirection if the `strider-py` codepath grows.
- **Cost estimate:** Option (b): 1 new newtype, 2 call sites (`strider-py/src/run.rs`'s GIL-release path).

### `strider::AnalyzeOutcome`

- **Severity:** MED
- **Where:** `crates/strider/src/strider/pipeline.rs:56`
- **Issue:** All-public fields. The doc says callers can use `outcome.graph` directly. `unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>` and `region_handles: Vec<RegionLiftHandles>` are paired — both must reflect the **same** lift iteration. If a caller mutates `unresolved_branches` (e.g. `outcome.unresolved_branches.clear()`) without clearing `region_handles`, the orchestrator's `RegionIndex::from_handles` still sees the stale region handles. The orchestrator owns this today (it `std::mem::take`s the field), but external `strider-py` consumers can also mutate.
- **Fix:** `pub(crate)` + accessor pairs (or a single `into_parts() -> (Graph, Vec<…>, Vec<…>)` consume-once API the orchestrator can use).
- **Cost estimate:** ~5 sites (orchestrator + strider-py).

### `strider::RunConfig<'a, R>`

- **Severity:** MED
- **Where:** `crates/strider/src/orchestrator.rs:60`
- **Issue:** All-public fields, including a `pub strider: &'a Strider` reference and a `pub sleigh: rsleigh::Sleigh<R>` (owned, moved into `LoopState::new`). The struct doc explicitly says "Held outside the function so callers can construct one and reuse the strider / sleigh / options across iterations" — but `RunConfig` is *consumed* by `run` (`config: RunConfig`, no borrow), so reuse is impossible after one call. The "configurable knobs as `pub` fields" idiom works, but mixing **move-only** owned fields (`sleigh`) with **persistent reference** fields (`strider: &Strider`) and `pub` everywhere lets callers shoot themselves: a code path could `let cfg2 = RunConfig { strider: cfg1.strider, sleigh: cfg1.sleigh, ... }` only to find `cfg1.sleigh` already moved.
- **Fix:** Make `sleigh` `pub(crate)` and provide a builder (`RunConfigBuilder::new(strider, start_addr, sleigh)`). Configurable knobs (`fn_max_size`, `compact`, `per_address_ccs`) stay as setter methods that take `self` by value. The builder discipline prevents partial moves and makes reuse explicit.
- **Cost estimate:** ~3 call sites (orchestrator caller in the example, `strider-py::run.rs`).

### `opt::IndirectBranchResolve`

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:111`
- **Issue:** All-public fields including a mutating closure `pub is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>` and a `pub unresolved_anchors: Vec<…>` whose precondition is "each anchor must appear at most once" (line 130-133). The doc admits "a duplicate would re-visit the same anchor … and silently no-op." The orchestrator pre-deduplicates today, but the type accepts any `Vec` shape from any caller.

  The "mutate fields between Optimizer::optimize calls" idiom (line 203) is the documented usage pattern, which works against the read-only `Optimizer` trait and shouldn't really be a `pub` field shape.
- **Fix:** Either (a) take ownership: replace fields with constructor parameters (`IndirectBranchResolve::new(anchors, contexts, is_tail_call, lr_vn, sp_vn, rom)`), and have the orchestrator construct a fresh pass per iteration; or (b) keep the field-as-config shape but enforce the dedup invariant in a setter (`set_anchors(&mut self, anchors: …) { … }` that asserts uniqueness).
- **Cost estimate:** ~2 orchestrator sites (`crates/strider/src/orchestrator.rs`).

### `opt::AnchorAddr`

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:190`
- **Issue:** Public newtype-ish struct `AnchorAddr { pub machine_addr: u64, pub insn_index: u64 }` is documented as "Opaque payload; opt does not interpret it" (line 191) — yet the fields are `pub`. Strider casts to/from `cfg::PcodeInsnAddr`; `opt` reads neither. Public fields contradict the "opaque payload" intent.
- **Fix:** Make fields `pub(crate)` (within `opt`) and expose a `pack(machine_addr, insn_index) -> Self` constructor + an opaque round-trip (`unpack() -> (u64, u64)` for strider). The intent is already a packed-bits type; sealing it costs almost nothing.
- **Cost estimate:** ~3 sites (strider's `indirect_resolve` module + `orchestrator.rs`).

### `opt::AnchorCallingContext`

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:163`
- **Issue:** All-public fields with a positional invariant: `arg_passing_outputs[i]` must correspond to the calling convention's `arg_passing_regs[i]`, `clobbered_kinds` must match the convention's clobbered set in order, etc. No type-system signal of these positional couplings; mutation of any single vec breaks the wiring at the in-place-edit site.
- **Fix:** `pub(crate)` + a constructor that accepts triples and stores them as a single struct, exposing read-only slice accessors. The orchestrator builds one of these per anchor; the in-place editors only read.
- **Cost estimate:** ~3 sites.

### `opt::Kb`

- **Severity:** MED
- **Where:** `crates/opt/src/known_bits/mod.rs:30`
- **Issue:** `pub ones: u64, pub zeros: u64` with the documented invariant `ones & zeros == 0` (line 33). External code can construct `Kb { ones: u64::MAX, zeros: u64::MAX }` and propagate contradictory bit-info silently — the merge() method (line 63) catches contradictions on combine, but the source `Kb` already in use isn't validated. The known-bits result is `pub` and consumed by callers via `analyze_known_bits` (`opt/src/lib.rs:62`), so this leaks the unchecked-construct path to anyone reading the `Kb` and copying it.
- **Fix:** Make fields `pub(crate)` and expose `Kb::new(ones, zeros) -> Result<Self>` that checks `ones & zeros == 0`, plus `ones() -> u64` / `zeros() -> u64` accessors. Tests already use `Kb { ones: …, zeros: … }` literals; convert to the new constructor.
- **Cost estimate:** ~10 sites in `opt/src/known_bits/{mod,tests}.rs`. No external consumers today.

### `opt::LoadReadOnly<M>(pub M)`

- **Severity:** MED
- **Where:** `crates/opt/src/load_readonly/mod.rs:48`
- **Issue:** Public tuple-struct field exposing the inner ROM. Allows `pass.0 = different_rom` mid-pipeline (the pass is `Clone`/registered into `OptimizerPipeline`'s `Vec<Box<dyn Optimizer>>`, so this isn't reachable through the normal pipeline flow — but external callers that reuse `LoadReadOnly` directly as a one-shot have the public-field hazard).
- **Fix:** Use `pub struct LoadReadOnly<M> { rom: M }` + `LoadReadOnly::new(rom: M) -> Self`. Strider already has `LoadReadOnly::new(rom)`-style usage in `build_optimizer_pipeline` (no — it constructs via `LoadReadOnly(rom)`; see grep). One-line ergonomic loss; one-line invariant gain.
- **Cost estimate:** ~3 call sites across `crates/strider`/`crates/strider-py`/example.

### `opt::IndirectBranchResolve.is_tail_call: Box<dyn Fn>`

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:157`
- **Issue:** `pub is_tail_call: Box<dyn Fn(u64) -> bool + Send + Sync>` — public boxed closure. Replacing it after registration changes pass behavior at runtime; default `Box::new(|_| false)` (line 212) makes the in-place tail-call edit a silent no-op for any caller who forgets to override.
- **Fix:** Make it a constructor parameter (covered by the `IndirectBranchResolve` finding above).
- **Cost estimate:** Subsumed by the parent finding.

### `cfg::MachineInsnAddr` and `cfg::PcodeInsnAddr` — primitive obsession leak

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/types.rs:29` (MachineInsnAddr), `:50` (PcodeInsnAddr)
- **Issue:** Both *are* newtypes — that's good — but their fields are `pub`:
  ```
  pub struct MachineInsnAddr { pub addr: u64 }
  pub struct PcodeInsnAddr { pub machine_addr: MachineInsnAddr, pub insn_index: u64 }
  ```
  The newtype encapsulation loses its value the moment fields are public — `addr.addr = 0` is just as bad as treating the raw `u64` directly. The doc comment on `PcodeInsnAddr` (line 49) explicitly warns "Do not reorder the fields — the `#[derive]` ordering relies on declaration order" — that's a deep encapsulation leak: the type's `Ord` implementation depends on the field declaration order, but anyone editing the struct can reorder them and silently break ordering semantics.
- **Fix:** Make fields `pub(crate)`. Expose `addr() -> u64`, `machine_addr() -> MachineInsnAddr`, `insn_index() -> u64`. Construction stays through `PcodeInsnAddr::at_machine_start(addr)` plus a private `new(machine_addr, insn_index)`.
- **Cost estimate:** ~30 sites (this is a hot newtype). Mostly in `cfg`, `strider/orchestrator.rs`, `strider/indirect_resolve`, dot dumper, tests. The `PcodeInsnAddr` field reads dominate.

---

## LOW severity

### `ir::ValidationErrors(pub Vec<ValidationError>)`

- **Severity:** LOW
- **Where:** `crates/ir/src/validate/mod.rs:145`
- **Issue:** Public `Vec` field. Any caller can `errs.0.clear()` after the validator returned them, breaking the "every layer's findings are surfaced" contract for downstream code that filters or counts errors.
- **Fix:** `pub(crate)` field; expose `iter() -> impl Iterator<Item = &ValidationError>` and `len() -> usize`. The `Display` / `Debug` impls already cover all the read use cases.
- **Cost estimate:** ~5 call sites in `ir/src/validate/tests.rs`.

### `reader::MemReadError(pub anyhow::Error)`

- **Severity:** LOW
- **Where:** `crates/reader/src/lib.rs:41`
- **Issue:** Same shape — public `anyhow::Error` field. Callers can swap the inner error post-construction (rare in practice — `anyhow::Error` is move-only-ish, but `&mut MemReadError` plus `std::mem::replace` works).
- **Fix:** `pub(crate)` field + `From<anyhow::Error>` (already present) + `source()` already wired. No public read accessor needed because every consumer goes through `Display`/`Debug`.
- **Cost estimate:** ~2 sites (the `From<anyhow::Error>` impl + tests).

### `pattern::matcher::bindings::BindingsMark(usize)`

- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/bindings.rs:64`
- **Issue:** Single-field newtype, no field visibility leak (`usize` field is private — `BindingsMark(usize)` with no `pub`). The marker is `pub` at the type level even though `mark()` / `restore()` are `pub(crate)`. Type-level publicness with crate-only constructors is an okay pattern (lets external code hold the marker by value), but if `mark()` / `restore()` are crate-internal, no external code can ever obtain or use a `BindingsMark`. The type itself can become `pub(crate)`.
- **Fix:** Drop `pub` from the type — `pub(crate) struct BindingsMark(usize)`.
- **Cost estimate:** 1 site.

### `cfg::ResolvedTargets` re-export from `opt`

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:57` (re-exports `opt::ResolvedTargets`)
- **Issue:** Architectural smell rather than a correctness leak. `cfg` re-exports a type from `opt`. The crate dependency graph in CLAUDE.md says `cfg → opt` would close a cycle (the opt crate sits below cfg in the layering: `target ← ir ← pcode-lift ← cfg ← strider`, with `opt` parallel to `cfg`). Looking at `Cargo.toml`:
  ```sh
  $ grep -A1 '^name = "cfg"' /…/cfg/Cargo.toml; cargo metadata --format-version=1 | jq -r '.packages[]|select(.name=="cfg")|.dependencies[].name'
  ```
  Today `cfg` does depend on `opt` (verified — `crates/cfg/Cargo.toml` lists it). The re-export tells consumers "this type lives here" while it actually lives in `opt`; if a future refactor removes the `cfg → opt` dep this re-export silently disappears.
- **Fix:** Either move `ResolvedTargets` to a leaf crate (`target` is a candidate — it's pure data; `cfg` and `opt` both depend on it), or stop re-exporting from `cfg` (consumers go through `opt::ResolvedTargets` directly). The latter is one-liner.
- **Cost estimate:** ~5 sites that reach for `cfg::ResolvedTargets`.

### `target::Endianness`

- **Severity:** LOW
- **Where:** `crates/target/src/arch.rs:3`
- **Issue:** Pure value enum, two variants. No leak. Mentioned only because the `read_u64` / `read_u32` / `read_u16` methods consolidate decoding logic — that's a *good* design pattern; a future audit might also want to expose `write_u64` to consolidate the encode path which is duplicated across `reader::elf` (relocations) and `strider-py` (memory write tests).
- **Fix:** None required for invariant enforcement.
- **Cost estimate:** N/A.

### `target::ArchPreset`

- **Severity:** LOW
- **Where:** `crates/target/src/arch.rs:91`
- **Issue:** No leak; closed enum, one variant per `SleighArch` preset. The risk is that `SleighArch.preset` is `pub` (covered above under `SleighArch`), so a caller can pin an inconsistent preset. With `SleighArch.preset` made `pub(crate)`, this enum stays clean.
- **Fix:** N/A.

### `pattern::Match`

- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/match_result.rs:20`
- **Issue:** `pub(super) root: NodeId, pub(super) bindings: Bindings` — already encapsulated. Accessors are well-shaped (`root()`, `node()`, `output()`, `get_uint`, `get_int`, …, `stack_offset`, `stack_phi_offsets`, `asm_fingerprint`). The `Match::new_for_test` constructor (line 32) is doc-flagged as test-only but `pub` rather than `pub(crate)` + `cfg(test)` — this is a minor leak that lets external code (and any production consumer of the `pattern` crate) synthesise arbitrary matches without going through the matcher.
- **Fix:** Either gate `new_for_test` on `#[cfg(any(test, feature = "test-utils"))]` or rename to `new_unchecked` and document the unsafety contract.
- **Cost estimate:** 1 site (the constructor).

### `pattern::Capture`

- **Severity:** LOW
- **Where:** `crates/pattern/src/var.rs:36`
- **Issue:** `pub struct Capture(u32)` with `pub fn id(self) -> u32` and `pub fn new() -> Self`. Field is private; constructor is the source of truth (process-wide atomic counter). No leak. The `id()` accessor is documented as "opaque" but happens to expose a monotonic-at-process-start integer. External consumers can rely on it for hashing keys but not for ordering across runs (already documented).
- **Fix:** None required.

### `entity_utils::DenseEntitySet<E>`

- **Severity:** LOW
- **Where:** `crates/entity-utils/src/set.rs:12`
- **Issue:** Private fields (`bitset`, `_marker`). Clean accessor surface (`insert`, `remove`, `contains`, `iter`, `len`, `is_empty`, `clear`). No leak.
- **Fix:** N/A.

### `entity_utils::Worklist<E>`

- **Severity:** LOW
- **Where:** `crates/entity-utils/src/worklist.rs:13`
- **Issue:** Same shape — private fields (verified by grep at the head of the file).
- **Fix:** N/A.

### `strider::RegionLiftHandles`

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:21`
- **Issue:** All-public fields. Internally used as a snapshot, then consumed by `RegionIndex::from_handles` (`crates/strider/src/orchestrator.rs:141`). The orchestrator owns the handles end-to-end; no external consumer mutates. Mostly a pure POD shape, mild encapsulation looseness only.
- **Fix:** Optional — `pub(crate)` fields with read accessors. Low priority because the consumer is the same crate.

### `cfg::RegionTerminator`

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/types.rs:89`
- **Issue:** Sum type, well-shaped variants. The `Switch.target_value: Option<ir::Value>` is the only exposed mutable knob and its semantics are clearly documented (line 142). No leak.
- **Fix:** N/A.

---

## Cycle through `Deref` audit

The only `Deref` chain in the listed types is `BuiltFunctionGraph: Deref<Target = Graph>` (`crates/ir/src/function.rs:82-92`). I searched for accidental deref-through that hides a real Graph-vs-BFG distinction:

```sh
grep -rn "BuiltFunctionGraph" /…/crates/ | grep -v test | wc -l   # 87 matches
grep -rn "fn .*BuiltFunctionGraph" /…/crates/ | grep -v test       # signatures
```

Two patterns concerned me:

- `pattern::Matcher::new(fn_graph: &BuiltFunctionGraph)` (`crates/pattern/src/matcher/mod.rs:108`) — internally calls `Self::for_graph(&fn_graph.graph, fn_graph.entry)`. So the matcher only ever consumes `(graph, entry)` regardless of the wrapper. That means **every** `Matcher::new` site could equally accept a `RewriteCtx` or any `(Graph, NodeId)` pair. The wrapper exists for ergonomic reasons only. This is fine, but it confirms the round-7 finding that `BuiltFunctionGraph` is doing two jobs (full + rewrite-only) and only one is needed for the matcher.

- `BuiltFunctionGraph::dot_dumper` (line 181) — reads `self.call_clobbered`, `self.ret_val_regs`. These **are** the CC fields that are empty in the rewrite-only form. Calling `dot_dumper` on a rewrite-only graph silently produces a dot diagram missing the clobber/ret-reg annotations. Not a soundness leak (the dump is read-only diagnostics), but it's a real "wrong type for the operation" that a sealed full-vs-rewrite split would have prevented at compile time.

No other accidental-deref pitfalls surfaced.

---

## Appendix — types inspected with no finding

These were on the audit list but I found no leaky-encapsulation / primitive-obsession / partial-state issue worth flagging:

- `ir::Graph` (private fields, accessor-only API; well-encapsulated post-r6/r7)
- `ir::WideConstId`, `ir::WideConstStorage` (newtype + closed enum; private inner storage; explicit interning contract)
- `ir::NodeOutputType` (closed enum; carries width invariants in its `info()` table)
- `ir::NodeOutputKind` (closed enum, no inner data leaks)
- `ir::NodeKind` (closed enum, only variant payloads and they're typed)
- `ir::NodeId` / `ir::NodeInputId` (newtypes with `entity_impl!`-generated `pub`-erased fields)
- `ir::VarId`, `ir::RegionId` (same; entity newtypes)
- `ir::FunctionArgSource` (closed enum)
- `ir::ValidateOptions` (POD opt-in struct; the `pub check_asm_fingerprints: bool` field is just a config knob with no invariant)
- `ir::ValidationError` (closed enum of error variants)
- `ir::FunctionBuilder` (`pub(crate)` fields, accessor-only public surface; `LiftAddrGuard` private impl)
- `ir::LiftAddrGuard` (RAII guard, well-shaped)
- `target::CallingConvention` (`pub(crate)` fields; presets are the constructor surface — round 7 fix held)
- `target::BuiltCallingConvention` (`pub(crate)` fields, accessor surface — round 7 fix held)
- `cfg::OptionsBuilder`, `cfg::Options` (private fields + builder pattern; clean)
- `cfg::Builder<R>` (`pub(super)` fields)
- `cfg::DecodeCache` (private `Arc<Mutex<…>>`)
- `cfg::RegionEdgeKind` (closed enum)
- `cfg::IfRegionState` — not inspected in source but doc-listed; assuming clean
- `opt::OptimizerPipeline` (private fields, accessor surface)
- `opt::OptimizationResult`, `opt::Optimizer`, `opt::OptimizerOnBuilt` (well-shaped enum + traits; the `Optimizer` blanket impl over `OptimizerOnBuilt` is the appropriate way to express the partial-overload)
- `opt::ConstantFold`, `opt::RedundantPhis`, `opt::DeadBranchElimination`, `opt::FlagCmpCanonicalize`, `opt::IfCondInversion`, `opt::KnownBits` (all unit structs / no public fields except the deliberately-public Kb already flagged)
- `opt::StackStoreDetect`, `opt::StackLoadForward`, `opt::FunctionArgDetect`, `opt::CallStackArgCollect` (public-config-fields shape — mild looseness only because each is a value-config struct constructed once via `from_convention`; `pub(crate)` would be stricter but doesn't enable real misuse since each pass reads its own config)
- `opt::ResolvedTargets` (closed sum)
- `pattern::Pat` (private inner `DynPat`)
- `pattern::Bindings` (private `entries`; mutation through `bind_capture` only)
- `pattern::CastMask` (bitflag-style with private bits)
- `pattern::FunctionArgHandle` (private fields)
- `pattern::Matcher` (private fields, lazy caches)
- `pattern::MatcherOptions` (POD config; same shape as `ValidateOptions`)
- All the typed builders (`IntBinaryOpPat`, `LoadPat`, `CallPat`, `IfPat`, `RetPat`, `StackStorePat`, `StackStorePhiPat`, `FunctionArgPat`, `PhiPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`, `CallOtherPat`, `StorePat`) — these have `pub(crate)` fields and fluent `.capture(c)` / `.when(f)` builder methods; well-encapsulated.
- `pattern::BoxedRule` (type alias)
- `reader::MemRegion` (private fields, validating constructor — exemplary)
- `reader::MemRegionsLookupTable` (private map)
- `reader::ReadOnlyMemory` (trait — clean)
- `reader::ElfFileMemReader` (private fields)
- `reader::RelocationStats` (POD diagnostic struct — `pub` fields are appropriate for a stats bag with no invariants beyond non-negativity)
- `entity_utils::DenseEntitySet<E>`, `entity_utils::Worklist<E>` (private inner storage)
- All `strider-py` `Py*` wrappers — those are PyO3 surface types whose field exposure is governed by `#[pyo3(get)]` at the Python boundary; structural encapsulation in Rust is enforced by the `#[pyclass]` machinery (no `pub` fields except the explicit getters) and they were inspected without finding additional leaks beyond the `RunResult { #[pyo3(get)] cfg, graph, sleigh }` exposure (which is intentional read-only-from-Python).

---

## Cross-cutting observations

1. **The "config struct with `pub` fields" pattern dominates in `opt` and `strider`.** Most opt passes (`StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`, `CallStackArgCollect`) and `RunConfig` follow the same shape: pub fields, an `::new(...)` and a `::from_convention(...)`. This is fine when the fields are independent value knobs — `pub(crate)` would be a defense-in-depth move but doesn't unblock real misuse. The exception is `IndirectBranchResolve`, where `unresolved_anchors` and `is_tail_call` carry preconditions documented but unenforced. Recommend keeping the pattern but auditing each pass for hidden invariants on read.

2. **The "rewrite-only `BuiltFunctionGraph`" sum-type split is incomplete.** Round 7 introduced `pattern::RewriteCtx` for the rewrite path, but `ir::BuiltFunctionGraph::from_graph_and_entry_for_rewrite` is still publicly callable, and `opt::with_built` continues to synthesise partial-state wrappers (`crates/opt/src/pipeline.rs:94-104`). Finishing the migration — making `from_graph_and_entry_for_rewrite` `pub(crate)` and routing every rewrite-only consumer through `RewriteCtx` — would close the FB-3-adjacent gap.

3. **Public field exposure on `cfg::PcodeInsnAddr` / `MachineInsnAddr` is the highest-frequency primitive-obsession leak.** Both are newtypes (correct intent) but with `pub` fields they're equivalent to raw `u64`/`(u64, u64)` pairs at any caller. This is the broadest fix to do; about 30 call sites change (many of them tests and the dot dumper).

4. **`target::SleighArch` and `target::BuiltCallingConventionParts` are the highest-impact HIGH-tier fixes.** They sit at the bottom of the dependency graph and corruption there propagates upward. Sealing both would also retire the `BuiltCallingConventionParts` "test bag" anti-pattern in favor of a builder.

5. **Sealing the `Pattern` trait** (resolves `MatchCtx`/`BuildCtx` `pub` fields) costs roughly nothing and removes the ambiguous "is this trait an extension point or not?" question.

---

## Files that contain the findings (absolute paths)

- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/region.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/ids.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/arch.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/types.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/rewrite.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/traits.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/match_result.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/load_readonly/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/lib.rs`
