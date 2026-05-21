# Round 12 — Ask 8 pass 1: typing / signature / arity audit

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

Scope: public Rust signatures in `crates/*/src/**/*.rs` (no `tests/`). Method: code inspection only.

## Findings

### TY-1 — `classify_jump_table` carries dead public parameter `_link_register_vn`
- **Severity:** HIGH
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:64`
- **Current signature:**
  ```rust
  pub fn classify_jump_table(
      ctx: pattern::RewriteCtxView<'_>,
      anchor_output: NodeOutputId,
      rom: Option<&dyn ReadOnlyMemory>,
      _link_register_vn: Option<rsleigh::Vn>,
      known: &crate::KnownBitsMap,
  ) -> Option<ResolvedTargets>
  ```
- **What's wrong:** `_link_register_vn` is suppressed (underscore prefix) and never read. The inline comment keeps it "symmetric with other arms in case a future refactor needs it" — speculative reason to pollute a public API. Every call site (`classify.rs:211`) computes and forwards a value that has no effect.
- **Proposed signature:** remove the parameter.
- **Migration impact:** One call site in `classify.rs:211`; drop the forwarded `link_register_vn` argument. Public re-exports (`mod.rs:53`, `opt::lib.rs`) update their signature automatically.

### TY-2 — `ResolvedTargets::multiple` returns `Result<_, anyhow::Error>` for a pure emptiness guard
- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:107`
- **Current signature:** `pub fn multiple(targets: Vec<u64>) -> std::result::Result<Self, anyhow::Error>`
- **What's wrong:** The sole failure is `targets.is_empty()` — a programmer invariant, not a recoverable runtime condition. `anyhow::Error` is the wrong carrier. The function is never called in production today (all production paths use tuple-construct `Multiple(targets)` directly), making it cheap to get the type right now. `Option<Self>` is the idiomatic Rust shape.
- **Proposed signature:** `pub fn multiple(targets: Vec<u64>) -> Option<Self>`
- **Migration impact:** No current production callers.

### TY-3 — `OptimizationResult::after_replace` returns `Result` for graph-corruption-only error
- **Severity:** MED
- **Where:** `crates/opt/src/pipeline.rs:39`
- **Current signature:**
  ```rust
  pub fn after_replace(
      self,
      function: &mut pattern::RewriteCtx<'_>,
      old: ir::node::NodeOutputId,
      new: ir::node::NodeOutputId,
  ) -> crate::Result<Self>
  ```
- **What's wrong:** The only `?` inside delegates to `replace_all_uses` (`crates/ir/src/ops/rewrite.rs:22`), whose `Err` branch fires only on use-list corruption — "a graph-construction bug, not user error" per its own doc-comment. Every call site `?`-propagates a branch that cannot fire on a well-formed graph. The function should `expect` internally and return `Self`.
- **Proposed signature:** `pub fn after_replace(self, function: &mut RewriteCtx<'_>, old: NodeOutputId, new: NodeOutputId) -> Self`
- **Migration impact:** One call site (`load_readonly/mod.rs:93`); remove `?`.

### TY-4 — Public relocation API mixes `&mut [MemRegion]` and `&mut Vec<MemRegion>` across siblings
- **Severity:** MED
- **Where:**
  - `crates/reader/src/elf.rs:476` — `apply_elf_relocations(regions: &mut [MemRegion], …)`
  - `crates/reader/src/elf.rs:682` — `apply_elf_relocations_with_extender(regions: &mut Vec<MemRegion>, …)`
  - `crates/reader/src/elf.rs:768` — `apply_elf_relocations_autoload(regions: &mut Vec<MemRegion>, …)`
- **What's wrong:** `apply_elf_relocations` correctly uses `&mut [MemRegion]` (it only patches in place). The two sibling functions need `&mut Vec` to push new regions. Internal delegation (`apply_elf_relocations(regions, obj)` inside `with_extender` at line 723) coerces via Deref. The public surface now presents two ownership semantics for what is conceptually one operation with optional extension.
- **Proposed signature:** Either uniformly take `&mut Vec<MemRegion>` (the extension variants already require it; the patch-only entry doesn't care) or document the distinction at the module level so callers can reason about it from signature alone.
- **Migration impact:** All current callers already hold `Vec<MemRegion>`; no source changes.

### TY-5 — `FunctionArgDetect` stores never-grown `Vec<T>` fields; `Box<[T]>` is more precise
- **Severity:** LOW
- **Where:** `crates/opt/src/function_args/mod.rs:50-77`
- **Current signature:**
  ```rust
  pub struct FunctionArgDetect {
      pub arg_passing_regs: Vec<rsleigh::Vn>,
      pub stack_ptr_vn: rsleigh::Vn,
      pub stack_arg_offsets: Vec<i64>,
  }
  pub fn new(arg_passing_regs: Vec<rsleigh::Vn>, stack_ptr_vn: rsleigh::Vn, stack_arg_offsets: Vec<i64>) -> Self
  ```
- **What's wrong:** Both `Vec` fields are set at construction (`new` / `from_convention`) and never mutated. `Optimizer::optimize` reads them via `&self.arg_passing_regs`. `Vec<T>` carries an unused capacity word and implies "can grow". `from_convention` calls `.to_vec()` on CC slices to satisfy the `Vec` parameter — an allocation never exploited. `Box<[T]>` eliminates the capacity word and communicates "fixed at construction".
- **Proposed signature:**
  ```rust
  pub struct FunctionArgDetect {
      pub arg_passing_regs: Box<[rsleigh::Vn]>,
      pub stack_ptr_vn: rsleigh::Vn,
      pub stack_arg_offsets: Box<[i64]>,
  }
  pub fn new(arg_passing_regs: impl Into<Box<[rsleigh::Vn]>>, stack_ptr_vn: rsleigh::Vn, stack_arg_offsets: impl Into<Box<[i64]>>) -> Self
  ```
- **Migration impact:** `from_convention` and pipeline constructors (`crates/strider/src/strider/pipeline.rs:204,231`) change `.to_vec()` → `.into()`.

## Categories verified clean

✓ **Functions that take `&mut Graph` but only read** — sampled across `walk.rs`, `validate/*.rs`, `node_signature.rs`; reads use `&Graph` consistently.

✓ **`Option<Vec<T>>` where `Vec<T>` would be honest** — three sites checked; each `Option<Vec>` distinguishes "not yet computed" from "empty result" (e.g. `AnalyzeOptions::all_vns`).

✓ **`Vec<T>` arguments the callee immediately `.iter()`s** — pattern builders take `Vec<u64>` for set-membership where callers usually own the values (avoids cloning); `&[u64]` would force re-allocation. Trade-off intentional.

✓ **Mismatched variadics** — `node_signature::expected_signature` enforces head_len + variadic-tail rules; no caller assumes len ≤ N silently.

✓ **`Box<dyn Trait>` where `impl Trait` works** — `OptimizerPipeline` uses `Box<dyn OptimizerRaw>` deliberately for type-erased storage in `Vec`; cannot be replaced by `impl Trait`.

✓ **`impl Into<X>` clarity** — pattern builder methods use `impl Into<Pat>` extensively for ergonomic composition; the trait impl set is documented in CLAUDE.md.

✓ **Returning `u64` for byte counts** — `vn_mask` returns `u128` for type widths up to 64 bytes; `vn.size: u8` for byte widths; `addr: u64` for machine addresses. Consistent.

## Files reviewed

- `crates/opt/src/{pipeline.rs,function_args/mod.rs,indirect_branch_resolve/{jump_table.rs,classify.rs,mod.rs,stack_array.rs,inplace.rs}}`
- `crates/reader/src/elf.rs`
- `crates/ir/src/{builder/nodes.rs,graph/{store,access,uses,compact}.rs,ops/{rewrite,builder,consts}.rs}`
- `crates/cfg/src/cfg/{builder/{mod,region_builder}.rs,types.rs,query.rs}`
- `crates/strider/src/{orchestrator.rs,strider/{pipeline,vn_io}.rs,strider/insn/{mod,control}.rs}`
- `crates/pattern/src/{matcher/{mod,bindings,match_result}.rs,rewrite.rs,pat/builders/*.rs}`
- `crates/strider-py/src/{run,opt,pattern,reader,graph,strider_cls}.rs`
- `crates/target/src/{arch.rs,calling_convention/mod.rs,call_other_abi.rs}`
- `crates/pcode-lift/src/{lib,vn_io,value/*}.rs`
