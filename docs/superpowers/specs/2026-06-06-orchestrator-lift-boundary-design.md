# Orchestrator / lift crate-boundary redesign

**Status:** design approved (2026-06-06); to be implemented on a fresh
branch after `feature/indirect-branch-redesign` merges to `develop`.

**Goal:** put the crate boundary where the concepts are — `strider-lift`
owns *all* of binary → IR, `strider-orchestrator` owns the
resolve/optimize loop behind one generic `Strider` handle, and the Python
layer exposes that handle directly (`ElfStrider` = `Strider` + ELF symbol
logic) with no `_LoadedElf` shim.

This is three stacked sub-projects, built in order; each gets its own
implementation plan:

- **A — lift move:** relocate CFG→IR lifting from `strider-orchestrator`
  into `strider-lift`. Foundation; mostly mechanical, behavior-preserving.
- **B — generic `Strider`:** replace `RunConfig`/`RunOptions`/`run` with a
  `Strider<R>` struct holding per-binary invariants and one per-function
  `analyze` method. Depends on A.
- **C — Python `ElfStrider`:** replace `load`/`load_elf`/`Program`/
  `_LoadedElf` with `strider.load_elf(path) -> ElfStrider` and
  `strider.strider(...) -> Strider`. Depends on B.

---

## The boundary problem (today)

Two things are misplaced:

1. **CFG→IR lift lives in the orchestrator.** `PerRegionDriver`
   (`strider-orchestrator/src/strider/{mod.rs,insn/,vn_io.rs}`),
   `analyze_cfg`/`analyze_cfg_with`, `AnalyzeOutcome`, and `AnalyzeOptions`
   turn a `Cfg` into the IR `Function`. They use **zero**
   `strider_opt`/`strider_pattern` — pure lift sitting in the wrong crate,
   only there for historical bundling with the cc/arch driver and the
   (now-deleted) cfg-time indirect resolver.

2. **`LiftDriver` conflates lift with opt wiring.** It carries both the
   lift state (arch, cc, regs) *and* opt wiring (`build_optimizer_pipeline`,
   `alias_mode: strider_opt::AliasMode`). And `RunConfig`/`RunOptions` mix
   per-binary invariants (arch, sleigh+reader, rom) with per-function knobs
   (entry, cc, fn_max_size, compact) and opt knobs (alias_mode) in one
   grab-bag rebuilt per run.

The redesign separates: **per-binary invariants** (on `Strider`),
**per-function knobs** (method args), **lift options** (strider-lift),
**opt options** (strider-opt/`OptCtx`), and **the pipeline** (caller).

---

## Component A — move CFG→IR lift into `strider-lift`

`strider-lift` already owns pcode→IR (`pcode_lift`) and CFG construction
(`cfg`). Add the region driver alongside them in a new `strider_lift::lift`
module and expose one entry point:

```rust
// strider-lift::lift
pub struct LiftOptions {
    pub fn_max_size: Option<u64>,
    pub allow_code_before_start_addr: bool,
    /// Pre-scanned tracked-varnode set (the orchestrator's cross-rebuild
    /// VnCache supplies this; None means "scan the cfg here").
    pub all_vns: Option<Vec<rsleigh::Vn>>,
    /// Per-target-address calling-convention overrides, already built.
    pub per_address_ccs: FxHashMap<u64, BuiltCallingConvention>,
}

pub struct LiftOutcome {
    pub function: strider_ir::Function,
    /// (pcode addr, IndirectBranch placeholder NodeId) for each deferred
    /// BranchIndirect — drives the orchestrator's resolve loop.
    pub unresolved_branches: Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>,
}

/// Build the CFG (seating terminators from `known_targets`) and lift it
/// region-by-region to IR.  No optimization, no resolve loop.
pub fn lift_function<R: MemReader>(
    sleigh: &mut Sleigh<R>,
    arch: SleighArch,
    entry: MachineInsnAddr,
    cc: &BuiltCallingConvention,
    options: &LiftOptions,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Result<LiftOutcome>;
```

**Moves into `strider-lift`:** `PerRegionDriver` and its `insn/` + `vn_io`
submodules, `analyze_cfg`/`analyze_cfg_with` (becoming `lift_function`),
`AnalyzeOutcome` → `LiftOutcome`, `AnalyzeOptions` → `LiftOptions`, and the
lift fields of `LiftDriver` (arch, cc, regs). Their unit/integration tests
move with them. `ResolvedTargets` and `PcodeInsnAddr` already live in
`strider-lift`, so the `known_targets` feedback crosses no new boundary,
and `lift_function` is `strider_opt`-free (preserving the dependency DAG).

**Stays in `strider-orchestrator`:** the opt half of `LiftDriver`
(`build_optimizer_pipeline`, `alias_mode`) folds into `Strider` (Component
B); the cross-rebuild `VnCache` stays an orchestration concern (it spans
loop iterations) and feeds `LiftOptions::all_vns`.

**Result:** `strider-lift` = "binary → IR given a resolved-targets map",
with an explicit signature. No behavior change — the gate is "the moved
tests stay green."

---

## Component B — generic `Strider` in the orchestrator

```rust
// strider-orchestrator
pub struct Strider<R: MemReader> {
    arch: SleighArch,
    sleigh: Sleigh<R>,                 // owns the MemReader
    rom: Option<Box<dyn ReadOnlyMemory>>,
}

pub struct OptOptions {
    pub alias_mode: AliasMode,
    pub call_clobbers_args: bool,
    pub compact: bool,
}

impl<R: MemReader> Strider<R> {
    pub fn new(arch: SleighArch, sleigh: Sleigh<R>, rom: Option<Box<dyn ReadOnlyMemory>>) -> Self;

    /// Lift the function at `entry`, run the optimizer to fixed point,
    /// resolve its indirect branches, and return the final IR.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &BuiltCallingConvention,
        lift_opts: &LiftOptions,
        opt_opts: &OptOptions,
    ) -> Result<Function>;
}
```

`analyze` is the rebuild-driven fixed-point loop, now reading cleanly
across the three crates each iteration:
`strider_lift::lift_function` → run the optimizer pipeline +
`IndirectBranchClassify` post-pass (strider-opt) → record resolutions /
decide → rebuild or finalize. `&mut self` because `Sleigh::lift_one` is
stateful; one `Strider` analyzes many functions sequentially, reusing the
Sleigh (as the loop does today). `rom` is borrowed into each run's
`OptCtx`.

**Pipeline:** built internally — `analyze` constructs the default
optimizer pipeline (configured from `opt_opts`) fresh per rebuild
iteration and appends `IndirectBranchClassify`. (A fresh pipeline per
iteration is required because pipelines drain on use.) Callers who need a
custom pipeline get a separate entry later if a use case appears; the
default path needs no caller-supplied pipeline.

**`cc` is per-function** (a method arg), supporting different conventions
per function; per-address overrides ride in `LiftOptions`.

**Delete `orchestrator::run` / `RunConfig` / `RunOptions` outright** (no
shim). All call sites migrate to `Strider::new(...).analyze(...)`.

---

## Component C — Python `ElfStrider`

Remove the `_LoadedElf` Python class entirely; its ELF-parse + symbol
table becomes internal state of `ElfStrider`.

- `strider.strider(arch, cc, mem, rom=None) -> Strider` — standalone
  (address-only) handle wrapping the Rust `Strider<AnyMemReader>`.
  `Strider.analyze(entry_addr, **opts) -> Function`.
- `strider.load_elf(path, apply_relocations=True) -> ElfStrider` —
  `ElfStrider` **owns a `Strider` + the ELF symbol table**. It adds
  symbol resolution on top of the `Strider` surface:
  `elf.analyze("symbol_name" | addr, **opts)` resolves a symbol name to an
  address then delegates to the inner `Strider::analyze`; plus `symbol` /
  `symbols` / `symbol_size` / `entry_point` / `read`.

**No trait** — `ElfStrider` is a concrete struct that *has-a* `Strider`
and forwards/extends it; abstracting "has a Strider" over one implementor
buys nothing. `load` / `load_elf` (old) / `Program` / `_LoadedElf` /
`Analyzer` are removed or folded into this surface (the configure-once
"analyze many functions" role is just holding an `ElfStrider`/`Strider`
and calling `analyze` repeatedly).

---

## Error handling

Unchanged in kind: fallible operations return `anyhow::Result` in Rust;
an unresolved indirect branch at fixed point surfaces as today's
`anyhow` error from `Strider::analyze`. Python continues to map every
Rust error to the single flat `strider.errors.StriderError`.

## Testing & migration

- **A:** the moved tests follow the code into `strider-lift`; behavior is
  preserved, so the bar is "same tests green" plus `cargo test --workspace`
  / `clippy --workspace --all-targets`.
- **B:** orchestrator integration tests migrate from `run(RunConfig)` to
  `Strider::new(...).analyze(...)`; `run`/`RunConfig`/`RunOptions` deleted.
- **C:** pytest migrates from `load`/`Program`/`Analyzer` to
  `load_elf -> ElfStrider` / `strider`. `.pyi` stub + examples updated.
- Full gate (`cargo test --workspace` + `clippy --workspace --all-targets`
  + `uv run pytest`) after each component.

## Sequencing

A → B → C, each its own implementation plan and its own green gate. Lands
on a fresh branch cut from `develop` after `feature/indirect-branch-redesign`
merges.
