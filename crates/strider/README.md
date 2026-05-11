# `strider` — top-level binary-analysis pipeline driver

The crate that ties everything together: it builds a [`cfg::Cfg`](../cfg) from
a binary, lifts each region to [`ir`](../ir) using [`pcode-lift`](../pcode-lift),
runs the [`opt`](../opt) pipelines, and drives the indirect-branch fixed-point
loop until the IR converges. The end result is a [`ir::BuiltFunctionGraph`]
ready for [`pattern`](../pattern) queries.

## Public surface

- `run(config: RunConfig<'_, R>) -> Result<ir::BuiltFunctionGraph>` —
  the canonical top-level entry. Builds the CFG, lifts to IR, runs
  optimisation pipelines, resolves indirect branches.
- `RunConfig<'a, R>` — input bundle: `strider: &Strider`,
  `start_addr: cfg::MachineInsnAddr` (construct via
  `cfg::MachineInsnAddr::new(addr)` or `addr.into()`),
  owned `sleigh: rsleigh::Sleigh<R>`, optional `rom: Arc<dyn ReadOnlyMemory>`,
  `fn_max_size: Option<u64>`, `allow_code_before_start_addr: bool`,
  `compact: bool`, `per_address_ccs: HashMap<u64, CallingConvention>`.
- `Strider` — per-iteration handle wrapping arch + sleigh-resolved CC.
  Construct via `Strider::new(arch, sleigh_regs, cc)`.
- `Strider::analyze_cfg(&cfg) -> Result<AnalyzeOutcome>` — per-iteration
  lift driver used by `run`.
- `AnalyzeOutcome { graph, unresolved_branches, region_handles }` — bundle
  returned by one analyse pass.
- `AnalyzeOptions<'a>` — per-call overrides (extra CC overrides, ROM, …).
- `RegionLiftHandles` — per-region snapshot consumed by the orchestrator's
  `RegionIndex`.
- `Strider::build_optimizer_pipeline()` / `build_stable_optimizer_pipeline()`
  / `build_destructive_optimizer_pipeline()` — three pre-configured
  pipelines layered on top of `opt`'s defaults with stack-aware passes.
- `GraphRewriter<'a>` (`rewrite.rs`) — thin façade over
  `pattern::rewrite_rule` for post-orchestrator rewrites
  (`re_optimize` shortcut included).
- `indirect_resolve` module — the indirect-branch resolver glue:
  `classify_anchor`, `inplace::apply_link_register`, `inplace::apply_tail_call`.
- `UnresolvedIndirectBranch` — typed error returned when the fixed-point
  exits with anchors still unresolved.
- Re-exports from [`target`](../target): `BuiltCallingConvention`,
  `CallingConvention`, `Endianness`, `SleighArch`.

## Architecture

The orchestration flow is:

1. **Build CFG**: [`cfg::Builder`](../cfg) walks pcode emitted by Sleigh,
   carving regions until each terminator. `BranchIndirect` opcodes leave
   placeholder anchors. The orchestrator constructs the builder via
   `Builder::for_arch(arch, sleigh, start_addr, opts)` so endianness and
   `ArchPreset` are derived from the `target::SleighArch` atomically — the
   weaker `Builder::new` defaults to LE + x86_64 and would silently
   misclassify CallOthers / decode bytes wrong on non-x86_64 binaries.
2. **Lift to IR**: `Strider::analyze_cfg` walks the CFG region by region,
   handing each region's pcode to an internal `IrStrider` which dispatches
   value-producing opcodes through [`pcode-lift::ValueLifter`](../pcode-lift)
   and routes control-flow / call / store opcodes itself. Per-insn
   `set_lift_addr(Some(addr))` calls funnel asm-fingerprint attribution
   into one place.
3. **Stable optimise**: run
   `Strider::build_stable_optimizer_pipeline()` — the rewrites that
   survive a later iteration adding new phi inputs (`ConstantFold`,
   `KnownBits`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`,
   `FunctionArgDetect` post-pass). The graph is kept growable.
4. **Resolve indirect branches**: for each placeholder anchor, call
   `indirect_resolve::classify_anchor` (which delegates to
   `opt::classify_anchor_with_rom_and_sp`) to inspect the producer shape,
   then apply the verdict in-place via `apply_link_register` (returns) or
   `apply_tail_call` (constant-target tail calls). New jump-table targets
   are fed back into the cfg builder via `ResolvedTargets`.
5. **Re-iterate** until the fixed-point predicate (`LoopState::Decision`)
   reports `FixedPoint` or `StableOnly`.
6. **Destructive optimise**: run
   `Strider::build_destructive_optimizer_pipeline()` — node-removal passes
   safe only at fixed point (`RedundantPhis`, `DeadBranchElimination`,
   `CallStackArgCollect` post-pass).
7. **Optional compaction** (`config.compact = true`, default): drop
   detached zombie nodes from the IR arena. Pre-compaction `NodeId`s
   become invalid across the call.

`src/orchestrator.rs` houses the loop driver. `src/strider/` houses the
per-iteration `Strider` handle and the per-region `IrStrider` that drives
`pcode-lift`. `src/indirect_resolve/` is the glue between the resolver in
[`opt`](../opt) and the orchestrator's `RegionIndex` (which pins per-iteration
phi NodeIds to their exit-control varnode maps). `src/rewrite.rs` is the
post-orchestrator graph-rewriter façade.

## Key invariants

- **Single Sleigh per analysis**: `RunConfig::sleigh` is owned and threaded
  through every iteration. `cfg::Cfg` retains it across rebuilds — the SLA
  spec is loaded once. See `crates/cfg/tests/sleigh_reuse.rs`.
- **Stable vs destructive pipeline split**: destructive passes run **once**
  at fixed-point exit. Running them mid-iteration would invalidate the
  orchestrator's per-iteration `RegionIndex` (its phi `NodeId`s would
  point at detached zombies). See
  `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`.
- **Per-address CC overrides** are pre-resolved once at `LoopState::new` so
  unresolved register names surface before iteration starts (rather than
  on the iteration where the call is first emitted).
- **Asm-fingerprint funnel**: every `IrStrider::process_insn` is wrapped in
  a `set_lift_addr(Some(addr)) … set_lift_addr(None)` pair. Special
  terminator handlers do the same. All lift-time attribution flows through
  one funnel so the contract is centralised.
- **Bounded-lift contract**: `fn_max_size` controls the outer bound. A
  `Single(K)` resolution with `K >= start_addr + fn_max_size` is treated as
  a tail call rather than followed.

## Tests

Per-feature integration tests in `crates/strider/tests/` (one file per
concern: `arithmetic.rs`, `calls.rs`, `floats.rs`, `memory.rs`,
`indirect_branch.rs`, `indirect_resolve_*.rs`, `orchestrator_indirect_resolution.rs`,
`per_address_cc*.rs`, `asm_fingerprints.rs`, `bounded_lift_tail_call.rs`,
…). Some inline tests in `src/rewrite_tests.rs`. Examples in
`examples/strider.rs` (the canonical end-to-end demo) and
`examples/dump_arch_cmps.rs`.

```
cargo test --package strider
cargo test --package strider <test_name>
cargo run -p strider --example strider
cargo bench --package strider --bench scaling
```

## Gotchas

- `RunConfig::sleigh` is **moved** into `run`; you can't reuse it after.
  If you need it back, recover it from the returned `BuiltFunctionGraph`'s
  upstream chain or by explicit construction.
- `compact: true` (the default) invalidates pre-call `NodeId`s. If you
  want to compare ids before and after, set `compact: false` and run
  `Graph::compact` manually after preserving any references you need.
- The orchestrator runs the **destructive** pipeline exactly once after
  fixed-point. If you append further rewrites via `GraphRewriter`, you
  may want to call `re_optimize` to re-run the standard passes over the
  rewritten graph.
- `UnresolvedIndirectBranch` errors mean the resolver couldn't classify
  every anchor — typically a jump-table whose stride or base address
  isn't a constant after `KnownBits`. See
  `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
  for the resolver's structural-shape rules.
- Depends on every other strider crate plus `rsleigh` and `object`. The
  Python bindings live in `crates/strider-py/`.
