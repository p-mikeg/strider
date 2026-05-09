# Round 9 — type-design audit

Branch: `review/ai3`. Audited every `pub` struct / enum / trait in the round-9 focus areas. Findings ranked HIGH / MED / LOW. Each finding cites `file:line` plus the invariant the type fails to express plus the proposed rewrite.

Coverage: cfg::types, cfg::builder, ir::graph, ir::function, ir::builder, ir::node::{ids, kind, output_kind}, ir::wide_const, target::{arch, calling_convention}, opt::{pipeline, indirect_branch_resolve, known_bits}, pattern::{var, matcher, pat::{traits, mod}, rewrite}, strider::{orchestrator, strider, rewrite, indirect_resolve}.

---

## HIGH

### H1. `ir::BuiltFunctionGraph` — public constructor materialises a partial-state form (round-8 finding still live)

`crates/ir/src/function.rs:117`. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry)` returns a `BuiltFunctionGraph` with `variables: PrimaryMap::new()`, `call_clobbered: Box::new([])`, `ret_val_regs: Box::new([])`, `call_other_clobbered: Box::new([])`. Every consumer that touches a CC field on this form silently gets an empty answer (e.g. `Match::get_vn` returning `None` when the user thinks they captured a Call clobber slot, but the CC list is empty because the graph was wrapped via `from_graph_and_entry_for_rewrite`).

The doc comment is explicit: "Callers MUST pass it only to consumers that touch `graph` and `entry`; consulting any other field returns a meaningless empty value silently." This is exactly the partial-state contract that a sum type prevents.

External callers (3 sites in `pattern/tests/get_vn_with_callother_clobber.rs`, 1 in `pattern/tests/get_vn_with_call_override.rs`, 1 in `pattern/tests/matching/control_flow.rs`) do reach for it, plus `opt::pipeline::with_built` (the `mem::take` adapter) and `strider::GraphRewriter::wrap_built`'s indirect path through `RewriteCtx`.

**Severity: HIGH.** Round 8 flagged this; round 8 also added `RewriteCtx { graph, entry }` (the simpler 2-field form, in `pattern/src/rewrite.rs:109`), which IS the right shape for the rewrite-only path — but `from_graph_and_entry_for_rewrite` was kept for opt/`with_built` and remained `pub`. The cost-benefit has shifted: with `RewriteCtx` already in place, the next step is to **delete the `BuiltFunctionGraph` constructor** and migrate `with_built` + the 3 pattern test files to `RewriteCtx::new` / `Matcher::for_graph`. The pattern crate already exposes `Matcher::for_graph(graph, entry)` (`pattern/src/matcher/mod.rs:116`) so the test sites have a paved migration path.

**Proposed rewrite:**
1. Delete `BuiltFunctionGraph::from_graph_and_entry_for_rewrite`.
2. Migrate `opt::pipeline::with_built` to use `RewriteCtx::new` + a thin `optimize_with_ctx` trait method (or split `OptimizerOnBuilt` into `OptimizerOnRewriteCtx` for passes that don't need CC fields, leaving the few that do — see CallStackArgCollect, FunctionArgDetect — on `OptimizerOnBuilt` proper).
3. Migrate the 5 test sites to construct via `Matcher::for_graph` (which doesn't need the CC fields at all).

This eliminates a publicly-constructible partial state from the IR's public surface.

---

### H2. `cfg::PcodeInsnAddr` — public fields with documented "do not reorder" invariant; deep-field access leaks across ~30 sites

`crates/cfg/src/cfg/types.rs:50-55`. The struct has `pub machine_addr: MachineInsnAddr` and `pub insn_index: u64`. The doc warns: "Do not reorder the fields — the `#[derive]` ordering relies on declaration order" (lexicographic Ord on machine_addr first, then insn_index).

This invariant is unenforced: any external constructor can drop the constraint by reorganising the type. More importantly, the `pub` fields leak the implementation detail of "compound key encoded as two pub fields" across ~30 sites:

- `crates/cfg/src/cfg/dot.rs:65,78` — `node.start_addr.machine_addr.addr` (triple-deep).
- `crates/cfg/src/cfg/builder/region_builder.rs:224,289,345,555,628,669` — same triple-dot pattern.
- `crates/cfg/tests/cfg_integration.rs:152`, `indirect_dispatch.rs:104,379`, `region_terminator.rs:182,184`, `cfg_query.rs:109` — tests reaching deep.
- `crates/strider/src/strider/pipeline.rs:387` — `wrapped.addr.machine_addr.addr`.

`MachineInsnAddr` is itself a thin newtype with `pub addr: u64` (`types.rs:29`) — both layers leak.

**Severity: HIGH** because (a) the invariant is genuinely load-bearing for `Ord`/`Hash` correctness and (b) the duplicated triple-dot accessor wastes reviewer attention on every site. The cost of fixing is moderate (mechanical, ~30 sites).

**Proposed rewrite:**
1. Make `PcodeInsnAddr` fields `pub(crate)`. Add an associated function: `PcodeInsnAddr::new(machine_addr: MachineInsnAddr, insn_index: u64) -> Self` (or the `at_machine_start(addr)` ctor that already exists, generalised).
2. Add accessors `machine_addr() -> MachineInsnAddr`, `insn_index() -> u64`, and a convenience `machine_addr_u64() -> u64` so `pat.machine_addr().addr` becomes `pat.machine_addr_u64()`.
3. Make `MachineInsnAddr.addr` field `pub(crate)`; add `as_u64() -> u64` and the existing `From<u64>` ctor.
4. Migrate ~30 sites; the `From<u64>` impl + accessors keep call sites short.

This pins the `Ord` invariant in the type's layout and removes the triple-dot pattern.

---

### H3. `target::BuiltCallingConventionParts` — public-fields construction bypasses the `BuiltCallingConvention`'s "disjoint sets" architectural invariant

`crates/target/src/calling_convention/mod.rs:127`. Every field is `pub`. `BuiltCallingConvention::from_parts(parts)` consumes it by struct destructuring and copies the fields verbatim (`mod.rs:157-182`) — **no validation**.

The invariants the regular `CallingConvention::build` path implicitly maintains:
- `arg_passing_regs` and `callee_saved_regs` are disjoint (the CC presets enforce this; see the `assert_disjoint` checks documented at line 750: "Disjointness between `arg_passing_regs` and `callee_saved_regs` is an architectural invariant").
- `stack_arg_offsets.len()` typically matches the number of positional stack args — but there's no check.
- `ret_stack_pop` is `0` on link-register ISAs (where `link_register_vn.is_some()`) and the pointer size on stack-push ISAs — the doc on `CallingConvention::ret_stack_pop` (line 65) says so but `from_parts` doesn't check.
- `syscall_number_vn` is `Some` only on syscall CCs.

**Severity: HIGH.** Tests in `pattern/tests/get_vn_with_call_override.rs:31` and `ir/tests/build_call_with_cc.rs:67,132` use this constructor with hand-rolled values; a typo introducing overlap between `arg_passing_regs` and `callee_saved_regs` would produce IR that the analyser believes is using callee-saved regs as arg slots — silent miscompilation.

**Proposed rewrite:**
- Make `BuiltCallingConventionParts` fields `pub(crate)`; expose a `Builder` style `BuiltCallingConventionParts::new(stack_ptr_vn) -> Self` + setters (`with_arg_passing_regs(regs)`, `with_callee_saved_regs`, …).
- Move the validation currently implicit in `CallingConvention::build` into a `from_parts_validated` (or fail-fast inside `from_parts`): assert disjointness, assert ret-stack-pop ↔ link-register coupling, assert syscall-number-vn-only-on-syscall-CCs.
- Document the test-only escape hatch as `from_parts_unchecked` if validation is too costly for tests.

---

### H4. `ir::Graph::stack_phi_offsets` / `call_other_names` / `call_clobbered_overrides` accessed via `pub(crate)` fields — but `BuiltFunctionGraph` Derefs to `Graph`, leaking through

`crates/ir/src/graph/mod.rs:60,76,101,119`. The side-tables themselves are `pub(crate)` fields. Good. **But** `BuiltFunctionGraph: Deref<Target = Graph>` (`function.rs:82-87`), so any external caller who has a `&BuiltFunctionGraph` can read these fields via auto-deref — and if any code does `&mut graph.stack_phi_offsets` (it doesn't today, but the surface allows it), the dedup-cache invariant on `IntConstWide` shared `WideConstId` would silently break.

Verified that nothing currently mutates these fields externally — `crates/pattern/src/matcher/match_result.rs:198,239` and `crates/strider/src/orchestrator.rs:726` only read `graph.call_clobbered`, `graph.call_other_clobbered`. But `call_clobbered`, `ret_val_regs`, `call_other_clobbered` on `BuiltFunctionGraph` itself are `pub` (`function.rs:59-79`) — external code can do `bfg.call_clobbered = Box::new([])` and then run pattern matches; the `Match::get_vn` method on `pattern/src/matcher/match_result.rs:187-241` would silently return `None` for slots it would otherwise resolve.

**Severity: HIGH** for the `BuiltFunctionGraph` `pub` fields, MED for the Deref-leak (uses `pub(crate)` already on the `Graph` side).

**Proposed rewrite:**
- Make `BuiltFunctionGraph::{call_clobbered, ret_val_regs, call_other_clobbered, variables, entry, graph}` all `pub(crate)`.
- Expose accessors: `entry() -> NodeId`, `graph() -> &Graph`, `graph_mut() -> &mut Graph`, `call_clobbered() -> &[Vn]`, etc. (Most of these methods already exist for Strider's use; making them the only path is a few hours of mechanical work.)
- The Deref impl can stay (read-only Deref doesn't widen mutation).

---

### H5. `IndirectBranchResolve` — public-field "builder" pattern with no impossibility guarantee that `unresolved_anchors` and `anchor_contexts` are in lockstep

`crates/opt/src/indirect_branch_resolve/mod.rs:112-159`. Fields are all `pub`, including `unresolved_anchors: Vec<(AnchorAddr, NodeOutputId)>` and `anchor_contexts: HashMap<AnchorAddr, AnchorCallingContext>`. The doc on line 132-135 explicitly states: "**Precondition:** each anchor must appear at most once" and the doc at line 149-152 says "the orchestrator populates `anchor_contexts` and `unresolved_anchors` in lockstep — a missing entry here means an upstream contract was broken."

The pass already raises an `Err` on a missing entry (line 287-291), which is good — but the type's shape doesn't *prevent* the lockstep break, it only catches it at run time. Round-8-2C H5 already added the typed Err; this is the next step.

**Severity: HIGH.** The struct is publicly constructible field-by-field by anyone (tests do exactly this, lines 433-488). A user who forgot to populate `anchor_contexts` would only learn at optimize time. The orchestrator path uses its own internal `RegionIndex` rather than this struct (per CLAUDE.md), so this is mainly a soundness aid for direct-use callers.

**Proposed rewrite:**
- Make `unresolved_anchors` and `anchor_contexts` `pub(crate)`. Replace with a single `Vec<(AnchorAddr, NodeOutputId, AnchorCallingContext)>` — the lockstep is encoded in the type. Or keep the two-list shape and add `add_anchor(addr, output, ctx)` builder method that populates both.
- Make `is_tail_call` `pub(crate)`, expose via setter `with_tail_call_predicate(f)`.
- Same for `link_register_vn`, `stack_ptr_vn`, `rom` — small churn; one-liner setters keep call sites clean.

---

## MED

### M1. `pattern::AnchorAddr` (re-exported as `opt::AnchorAddr`) — public fields shadow the `cfg::PcodeInsnAddr` newtype with raw `u64`s

`crates/opt/src/indirect_branch_resolve/mod.rs:191-198`. Documented as "opaque to opt, transparent to strider (which casts it back to its `cfg::PcodeInsnAddr`)". Both fields are `pub u64` — but the type is just a copy of `PcodeInsnAddr`'s shape, used because opt can't depend on cfg.

This is a LAYERING workaround, but the public-fields surface lets callers rebuild it from scratch (instead of going via `PcodeInsnAddr::into()`-style). The `From<PcodeInsnAddr>` / `Into<PcodeInsnAddr>` round-trip is the real contract; encoding it via traits would prevent drift if `PcodeInsnAddr` ever grew a third field.

**Severity: MED.**

**Proposed rewrite:**
- Make `AnchorAddr` fields `pub(crate)`; add `from_packed(machine: u64, insn_index: u64)` ctor and accessors. Add `From<cfg::PcodeInsnAddr>` and `From<AnchorAddr> for cfg::PcodeInsnAddr` impls — but those would create the cycle the comment is avoiding. Alternative: a single `pub fn parts(&self) -> (u64, u64)`.

---

### M2. `cfg::Cfg<R>` — all-pub fields including `start_addr_to_region_id` lookup index

`crates/cfg/src/cfg/mod.rs:37-67`. Fields `sleigh`, `graph`, `entry`, `start_addr_to_region_id` are all `pub`. The `start_addr_to_region_id` is a derived/cached lookup; mutating `graph` directly without updating it would silently break `region_id_at_start` lookups.

The `query.rs` and `dot.rs` modules read these fields directly. The `strider::orchestrator::build_lift_stable` destructures `let Cfg { sleigh: harvested, .. } = cfg;` (`orchestrator.rs:876`) — needs `sleigh` field access for the harvest pattern.

**Severity: MED.** No mutation paths today (tests only read), but the public surface allows it.

**Proposed rewrite:**
- Make `start_addr_to_region_id` `pub(crate)`; expose via existing `region_id_at_start` (which `query.rs` already provides).
- Either keep `sleigh`, `graph`, `entry` `pub` for the destructure pattern, or add `into_parts(self) -> (Sleigh, RegionGraph, NodeIndex)` so the harvest is explicit. The destructure form is fine if a comment pins "no mutation".

---

### M3. `strider::AnalyzeOutcome`, `strider::RegionLiftHandles`, `strider::RunConfig`, `strider::AnalyzeOptions` — public fields with arity invariants

- `AnalyzeOutcome` (`pipeline.rs:56-72`) — `unresolved_branches: Vec<(PcodeInsnAddr, ir::Value)>` documented as "Empty in the common case". No arity invariant; LOW.
- `RegionLiftHandles` (`pipeline.rs:21-47`) — every field `pub`. Invariants: `entry_var_phis` keyed by Vn; `exit_vn_to_value` is `Arc`-shared because "never mutated post-build". The Arc's "never mutated" property is enforced by the `Arc` itself, but the field being `pub` lets callers replace it with a non-shared `Arc::new(map)` — fine, but the field-by-field construction is fragile. MED.
- `RunConfig` (`orchestrator.rs:60-106`) — every field `pub`. Invariants: `fn_max_size` interacts with `allow_code_before_start_addr` (the doc says when `fn_max_size.is_some()`, `allow_code_before_start_addr` is ignored). This is a real coupling that the type doesn't express. MED.
- `AnalyzeOptions` (`pipeline.rs:90-107`) — `all_vns` documented as "must be sorted by `pcode_lift::vn_sort_key`". Public field; can't enforce. MED.

**Proposed rewrite (RunConfig only — most acute):**
- Replace `(fn_max_size, allow_code_before_start_addr)` pair with a sum type:
  ```rust
  pub enum FunctionBoundary {
      Unbounded { allow_code_before_start: bool },
      Bounded { max_size: u64 },
  }
  ```
  This makes the "ignored when bounded" rule unrepresentable rather than documented.
- For `AnalyzeOptions::all_vns`, add a constructor `AnalyzeOptions::with_sorted_vns(vec)` that takes a sorted `Vec<Vn>` (use a phantom type or a `SortedVns` newtype to encode the property).
- For `RegionLiftHandles`, expose a builder + private fields if the construction is to remain stable; otherwise leave field-by-field but document the per-field arity contract more loudly.

---

### M4. `opt::Optimizer` and `opt::OptimizerOnBuilt` traits are public with no `Send`/`Sync` bound on the impl, but `OptimizerPipeline` boxes as `Box<dyn Optimizer>` (no auto-trait bound)

`crates/opt/src/pipeline.rs:114-141, 151-162, 180-191`. Both traits are `pub`. `OptimizerPipeline::optimizers: Vec<Box<dyn Optimizer>>` — no `Send + Sync` bounds. This means a pass that holds non-Send state (`Rc<...>`) is acceptable, but the surrounding `Strider: Clone` impl (`strider/src/strider/pipeline.rs:131`) and the orchestrator's expectation that pipelines are reusable across iterations could break with a thread-pool extension.

**Severity: MED** — current code doesn't multi-thread, so no live bug. But the trait shape is part of the public API and adding `Send + Sync` later is a breaking change. Sealing the traits *now* (`pub trait Optimizer: Send + Sync { … }`) costs nothing.

**Proposed rewrite:**
- Add `: Send + Sync` to both trait headers. All current passes already satisfy it (no `Rc` / `RefCell` in any field — verified by inspection of `ConstantFold`, `KnownBits`, `IndirectBranchResolve`, `StackStoreDetect`, `StackLoadForward`, `RedundantPhis`, `DeadBranchElimination`, etc.).

---

### M5. `pattern::Capture` — public new-id allocator via process-wide atomic; no `Capture::from_id(u32)` for round-tripping through Python

`crates/pattern/src/var.rs:36`. `Capture(u32)` with `pub fn new()` and `pub fn id()` getter. The doc says: "The raw id is meant only as an *opaque identifier*; callers must not rely on the value space being dense or sequential."

The strider-py bindings use the id as a HashMap key (per CLAUDE.md: "str-keyed capture interning"), which is fine. But there's no constructor from a `u32` — so a Python caller cannot reconstruct a `Capture` from a saved id. Today this is a feature (forces the Python wrapper to intern), but the asymmetry between `id()` and `new()` is brittle: someone may add `Capture(u32)` field constructor later and the round-8 sealing concern returns.

**Severity: MED — minor.** The `pub struct Capture(u32)` (single tuple field) is currently `pub(crate)`-style if the field is private, which it is (`u32` not `pub u32`). The `id()` getter is the only escape.

**Proposed rewrite:** No change needed. The current shape is well-encapsulated. (Filed as MED for completeness; could be downgraded to LOW.)

---

### M6. `opt::ResolvedTargets::Multiple(Vec<u64>)` — empty Vec is constructible but classifier has explicit "must be non-empty" invariant

`crates/opt/src/indirect_branch_resolve/mod.rs:82-91`. The classifier in `classify.rs:172-179` checks `if targets.is_empty() { None } else { Some(ResolvedTargets::Multiple(targets)) }` — pinning the invariant at one site. But the type itself accepts `Multiple(vec![])`, and downstream consumers (the orchestrator's `edge_set_of` at `orchestrator.rs:892-911`) iterate the Vec without checking.

A bug-prone path: a future classifier arm forgetting the empty check would emit `Multiple(vec![])`, which `edge_set_of` skips silently (loop body iterates zero times → no edges added → loop diverges with "no progress").

**Severity: MED.** The invariant is sound today but unenforced.

**Proposed rewrite:**
- `Multiple(NonEmptyVec<u64>)` (introduce a small `NonEmptyVec<T>` newtype in `entity-utils` or use the `nonempty` crate). `NonEmptyVec::try_from_vec(v) -> Result<Self, EmptyVecError>`.
- Or fold the invariant into a checked ctor: `ResolvedTargets::multiple(targets) -> Result<Self, ...>`.

---

## LOW

### L1. `ir::node::{NodeId, NodeOutputId, NodeInputId}` — `entity_impl!` newtypes; well-encapsulated

`crates/ir/src/node/ids.rs:11-23`. Each is `pub struct NodeId(u32)` with no public field; `cranelift_entity::entity_impl` provides typed conversions. **No finding.** This is the right shape.

### L2. `ir::WideConstId` — same as L1

`crates/ir/src/wide_const.rs:28`. `pub struct WideConstId(u32)` opaque, `entity_impl!`. **No finding.** Doc explicitly notes "ids from other graphs are not portable" — handled.

### L3. `ir::NodeOutputKind` — closed sum type, well-encapsulated

`crates/ir/src/node/output_kind.rs:9`. Includes `is_value`, `as_value`, `is_control`, `is_memory` accessors. The `is_phi_token` helper is `pub(crate)` (line 78) — that's the right call. **No finding.**

### L4. `ir::node::NodeKind` — large but closed sum type

`crates/ir/src/node/kind.rs:27-243`. Doc per variant; payloads either `Copy` scalars or interned `WideConstId`. The side-table indirections (`StackStorePhi { space }` with offsets out-of-band, `CallOther { user_op_id }` with name out-of-band) are deliberate to keep `NodeKind: Copy`. **No finding.**

### L5. `target::SleighArch` — `pub` fields, but `rsleigh::sla_spec::SlaSpec` and `rsleigh::pspec::PSpec` are themselves opaque newtypes

`crates/target/src/arch.rs:119-130`. All fields `pub`. The doc warns "Set by each preset constructor; not user-overridable" for `preset` field — but the `pub` field allows override. External access at `cfg/src/cfg/builder/mod.rs:139-140` reads `arch.endianness, arch.preset`.

**Severity: LOW.** The invariants are loose: a user setting `preset: ArchPreset::X86` while `sla_spec: SLA_SPEC_AARCH64` would silently misclassify CallOthers, but they'd have to actively try to break things. Encapsulating costs little:

**Proposed rewrite:** Make fields `pub(crate)`; add `endianness() -> Endianness`, `preset() -> ArchPreset`, `sla_spec() -> SlaSpec`, `pspec() -> PSpec` accessors. The `for_arch(...)` Builder constructor is the only construction path; presets are factories.

### L6. `target::ArchPreset` — closed enum, no fields; well-encapsulated

`crates/target/src/arch.rs:91-107`. **No finding.**

### L7. `target::Endianness` — closed enum with helper methods

`crates/target/src/arch.rs:3-46`. **No finding.**

### L8. `target::CallingConvention` — all fields `pub(crate)`, exposed only via factory ctors

`crates/target/src/calling_convention/mod.rs:29-93`. Fields are all private (the `pub` keyword is absent). The `*_linux_kernel` / `*_linux_syscall` factories mutate fields via `let mut cc = ...; cc.foo = ...; cc` — that's the only construction path. **No finding.** Good encapsulation.

### L9. `pattern::Match` — clean accessor surface; `bindings_clone()` is the only escape

`crates/pattern/src/matcher/match_result.rs:14-23`. Fields `pub(super) root` and `pub(super) bindings`. External surface is the typed extractor methods (`get_uint`, `get_int`, `get_vn`, `stack_offset`, `stack_phi_offsets`, `asm_fingerprint`, `get_wide_bytes`). **No finding.**

### L10. `pattern::Pat` — opaque wrapper around `Arc<dyn Pattern>`

`crates/pattern/src/pat/mod.rs:94`. `Pat(crate::pat::traits::DynPat)` — single private tuple field, exposed only through `from_dyn` (pub(crate)) and `as_dyn` (pub(crate)). **No finding.** Excellent encapsulation.

### L11. `pattern::RewriteCtx` — `pub` fields but tiny + clearly-scoped

`crates/pattern/src/rewrite.rs:109-112`. Two fields, `pub graph: &'g mut Graph` and `pub entry: NodeId`. This is the round-8 simpler-form replacement for `BuiltFunctionGraph::from_graph_and_entry_for_rewrite`. **No finding.** The struct is effectively a transparent argument bundle; field access is the natural API.

### L12. `pattern::BuildCtx` — `pub` fields, similar to RewriteCtx

`crates/pattern/src/pat/traits.rs:118-123`. Four `pub` fields (`graph`, `bindings`, `root`, `root_ty`). Used inside `try_build` impls; the struct is a transparent context bag. **No finding.**

### L13. Round-8 deferred sealing of `Pattern` trait — already mooted by `pub(crate) mod traits`

`crates/pattern/src/pat/mod.rs:10` declares `pub(crate) mod traits;`, so even though `Pattern` is `pub trait Pattern` inside that module, it's not reachable from outside the crate. Confirmed empirically: `cargo doc -p pattern` does not emit `trait.Pattern.html`. **No finding** — round 8 over-flagged this. The cost-benefit is clear: do nothing.

### L14. Round-8 deferred tightening of `MatchCtx::graph` — same as L13

`MatchCtx` lives in `pub(crate) mod traits;`. External callers cannot name it. The `pub graph: &'g ir::Graph` field at line 31 is internal only. **No finding** — also already addressed.

### L15. `cfg::Region` — all-pub fields including `terminator`

`crates/cfg/src/cfg/types.rs:182-205`. `start_addr`, `insns`, `terminator` all `pub`. The doc says `insns` is "Never empty" — but this is unenforced, and `add_region` (`builder/mod.rs:182-189`) explicitly bails with "region has no instructions ... only Branch is permitted for empty regions" so the invariant is actually conditional. The current `Region::contains_addr` impl handles the empty case gracefully (returns `false`).

**Severity: LOW.** The invariant is mildly fuzzy and the unenforced-`pub` field isn't load-bearing in practice.

### L16. `cfg::RegionTerminator::Switch` — `target_value: Option<ir::Value>` documents a soundness contract that's only doc-enforced

`crates/cfg/src/cfg/types.rs:131-148`. The doc on the `target_value` field says: "When `Some`, strider's `handle_switch` uses it directly instead of re-reading `target_vn`, pinning the soundness contract that the comparison value is the SAME value the IR-level indirect-branch resolver classified."

The "same value" property is asserted only in prose. Today the cfg builder always sets this to `None`, so the bug is latent. **Severity: LOW** — observe as a watch-item if the orchestrator ever populates it.

### L17. `pattern::MatcherOptions` — all-pub fields, but is a transparent flag bag

`crates/pattern/src/matcher/mod.rs:42-59`. Two `pub` fields. Mutated only via `Matcher::ignore_*` setters. **No finding.**

### L18. `opt::Kb` — known-bits payload; `pub` fields with explicit `ones & zeros == 0` invariant

`crates/opt/src/known_bits/mod.rs:38-43`. Doc says: "Both `ones` and `zeros` are masked to the output type's width and must never overlap". `Kb::merge` checks contradiction (line 71-77) — this is the enforcement point. The `pub` fields let an arbitrary caller construct a `Kb { ones: 0xff, zeros: 0xff }` — invalid — but `KnownBits` only feeds `Kb` through `merge` and `from_const`, both of which are checked.

**Severity: LOW.** No external callers construct `Kb` directly.

### L19. `opt::AnchorCallingContext` — public-fields builder; small struct, all `Vec<...>` and `Default`

`crates/opt/src/indirect_branch_resolve/mod.rs:163-180`. Three `Vec<...>` fields with no cross-field arity invariants. **No finding.**

### L20. `strider::Strider` — `pub(super)` fields; well-encapsulated

`crates/strider/src/strider/pipeline.rs:131-140`. All fields `pub(super)`. Construction via `Strider::new` only. Accessors `calling_convention()`, `arch()` (pub(crate)). **No finding.**

### L21. `strider::GraphRewriter` — private fields with `&mut Graph + entry`

`crates/strider/src/rewrite.rs:59-70`. Fields `graph: &'a mut Graph, entry: NodeId` are private. Constructed via `wrap_built` only. **No finding.**

### L22. `cfg::DecodeCache` — single private `Arc<Mutex<...>>` field

`crates/cfg/src/cfg/decode_cache.rs:38`. Well-encapsulated. **No finding.**

### L23. `ir::FunctionGraph` — `pub` struct in `function.rs` but NOT re-exported from `ir::lib.rs`

`crates/ir/src/function.rs:14`. `pub struct FunctionGraph { pub graph: Graph, pub entry: NodeId, pub entry_control: NodeOutputId, pub entry_memory: NodeOutputId }`. Exposed externally only through `FunctionBuilder::body() -> &FunctionGraph` and `body_mut() -> &mut FunctionGraph`. The `lib.rs` module doesn't re-export `FunctionGraph` — so external callers can't name the type.

**Severity: LOW** because it's de facto private. **However**, the absence of a re-export is fragile (someone may add `pub use function::FunctionGraph` thinking it's for completeness). The fields being `pub` would then leak. Recommend marking the fields `pub(crate)` defensively; the only intra-crate consumer is `FunctionBuilder` itself.

### L24. `ir::FunctionBuilder` — all fields `pub(crate)`, well-encapsulated

`crates/ir/src/builder/mod.rs:100-146`. Construction via `new` / `new_raw` / `empty`; mutation via documented API. **No finding.**

### L25. `ir::LiftAddrGuard` — RAII guard with private fields

`crates/ir/src/builder/lift_addr.rs:16-19`. Both fields private. Drop impl restores the previous value. **No finding.** Good shape.

### L26. `cfg::RegionEdgeKind` — closed enum, no fields

`crates/cfg/src/cfg/types.rs:9-22`. **No finding.**

### L27. `cfg::Builder<R>` — `pub(super)` fields with `with_*` chaining ctors

`crates/cfg/src/cfg/builder/mod.rs:51-84`. All fields `pub(super)`. **No finding.**

### L28. `pattern::Matcher<'g>` — fields are `pub(super)` / `pub(crate)` / private

`crates/pattern/src/matcher/mod.rs:73-98`. `graph: pub(super)`, `entry: pub(super)`, `options: pub(crate)`, `function_arg_index/preorder/kind_index: private`. Construction via `new`/`for_graph` and chainable `ignore_*` setters. **No finding.**

---

## Summary of recommended actions (by priority)

1. **HIGH H1**: delete `BuiltFunctionGraph::from_graph_and_entry_for_rewrite`; migrate to `RewriteCtx::new` + `Matcher::for_graph` (5 test sites + `opt::with_built`).
2. **HIGH H2**: tighten `PcodeInsnAddr` and `MachineInsnAddr` field visibility to `pub(crate)`; add accessors. Migrate ~30 sites.
3. **HIGH H3**: validate disjointness invariants in `BuiltCallingConvention::from_parts` (or split into checked / unchecked variants).
4. **HIGH H4**: tighten `BuiltFunctionGraph::{call_clobbered, ret_val_regs, call_other_clobbered, variables, entry, graph}` to `pub(crate)`; expose accessors only.
5. **HIGH H5**: fold `IndirectBranchResolve::{unresolved_anchors, anchor_contexts}` lockstep into a single Vec; make remaining setup fields `pub(crate)` + setters.
6. **MED M3**: replace `RunConfig::{fn_max_size, allow_code_before_start_addr}` with `FunctionBoundary` sum type; introduce `SortedVns` newtype for `AnalyzeOptions::all_vns`.
7. **MED M4**: add `Send + Sync` to `Optimizer` and `OptimizerOnBuilt` trait headers (zero-cost; pre-empts breaking change later).
8. **MED M6**: introduce `NonEmptyVec<u64>` for `ResolvedTargets::Multiple`.
9. Defer / decline: round-8 carryovers L13 (Pattern trait sealing) and L14 (`MatchCtx::graph`) — already mooted by `pub(crate) mod traits;`. The cost-benefit has shifted to "do nothing"; remove these from the deferral list.
