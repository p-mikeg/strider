# V1–V6 Verification Results

> Sourced from six parallel read-only research subagents dispatched 2026-05-17 against the strider v2 rewrite plan (`docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md`). Each verification answers a specific architectural commitment in that plan. Overall outcome: every commitment survives. Two architectural corrections required; one is mechanical interface extraction, the other tightens a fallback path.

## Summary

| | Verdict | Plan impact |
|---|---|---|
| V1 — egg + phi-opaque slice | **PASS-with-caveats** | Fallback list trimmed: cranelift-egraph is dead. |
| V2 — Salsa query shape | **PASS-with-caveats** | Pin to salsa 0.26.x. External fixed-point loop, not `cycle_fn`. |
| V3 — rsleigh lazy decode | **PASS** | None. `Sleigh::lift_one(addr)` is stateless per-address. |
| V4 — PyO3 proc-macro | **PARTIAL PASS** | Phase 4 starts with one hand-written `#[gen_stub_pyclass]` type before the macro. |
| V5 — proptest IR generator | **PARTIAL PASS** | Phase 6 Task 6.3 scoped to value-only DAGs; control flow uses hand-fixtures. |
| V6 — dep direction | **PASS-with-findings** | Two new interface extractions to land in `strider-ir`: `ReadOnlyMemory` trait, `FunctionBuilderCC` plain-data type. |

## V1 — Egg + phi-opaque slice

**Pass condition:** Egg expresses our slice model AND `cranelift-egraph` is a viable backup.

**Result:** Egg expresses the model (PASS). cranelift-egraph is **abandoned** — last published v0.91.1 in March 2023; absorbed into `cranelift-codegen` internally; no longer in Wasmtime's workspace deps. So that fallback is dead.

**Key technical findings:**
- Egg's `Language` trait places no restrictions on enum variants: `children()` returns an empty slice for leaves, `matches()` compares only operator + payload (not children). Modeling `VarPhi(NodeId)`, `MemPhi(NodeId)`, `InitialVar(Vn)`, `InitialMemory`, `FunctionArg(slot)`, `LoadOut(NodeId)` as opaque zero-child variants — each with a unique strider-side ID — is idiomatic. Matches egg's own `lambda.rs` example precedent.
- Distinct phis stay in distinct e-classes (no accidental unification across phi sites).
- Egg's `Runner` defaults (`iter_limit=30`, `node_limit=10_000`, `time_limit=5s`) are 100× oversized for ~100-node slices. We bypass `Runner` and drive `EGraph::rebuild` + `Rewrite::search`/`apply` manually to avoid scheduler overhead.

**Top concern:** Egg's docs don't quantify cost on tiny problems. Mitigate by skipping `Runner`.

**Updated fallback ladder for the plan's section A:**
1. **egg** (primary) — drive `EGraph::rebuild` + manual `Rewrite::search`/`apply`.
2. ~~`cranelift-egraph`~~ — DEAD, removed from the plan.
3. **Hand-rolled saturation** — apply rewrites + node-id union-find directly on `Graph`. Loses egg's confluence machinery; keeps the data model.
4. **Drop egraph entirely** — collapse v1's stable/destructive split via the G4 `PassEffect` enum. Loses saturation benefit; keeps every other v2 win.

## V2 — Salsa query shape

**Pass condition:** salsa supports our shape, is maintained, has production users.

**Result:** salsa 0.26.2 (released 2026-05-03) is the actively maintained branch — not "salsa-3.0", which doesn't exist. Pinned by **rust-analyzer** (`Cargo.toml` master, 0.26.2) and **Astral's ruff/ty** (0.26.1 with `compact_str`, `macros`, `salsa_unstable`, `inventory` features). 5.17M total / 1.2M recent downloads. README still says "WORK IN PROGRESS" but the production reality contradicts that label.

**Query-shape mapping:**
- `binary(path)` → `#[salsa::input]` with `Durability::HIGH`.
- `indirect_targets(entry)` → `#[salsa::input]` with `Durability::LOW` (grows during fixed-point).
- `cfg(entry)`, `region_ir(addr)`, `optimized_eclass(entry)` → `#[salsa::tracked]` derived queries.
- Setting a new value on an input bumps the revision; red-green only re-runs queries whose recorded deps changed.

**Concerns:**
1. Drive the fixed-point externally (orchestrator: set input → query → observe new targets → repeat). Cleaner than salsa's `cycle_fn`/`cycle_initial` with its 200-iter cap and monotone-domain requirement.
2. Salsa requires `&mut db` to mutate inputs → serializes the orchestrator against any concurrent queries. Fine for v2 (single-threaded); would need rework if parallelized later.
3. `region_ir(addr)` must take `addr` as a salsa input/struct, not a raw `u64`, to get proper interning.

**Plan update:** Pin `salsa = "0.26.2"` (or follow rust-analyzer's pin). External fixed-point. Re-evaluate if parallelism becomes desirable.

## V3 — rsleigh lazy decode

**Pass condition:** rsleigh exposes a per-instruction or per-BB entry safe to call out-of-order.

**Result:** **PASS with caveat — the V3 subagent's "stateless" claim was overstated.** User pointed out that `lift_one(&mut self, addr)` takes `&mut self`, which IS a context (Sleigh's C++ handle carries context-register state — ARM Thumb mode, x86 segment selectors, MIPS16 mode). The corrected picture:

- **Decode buffers reset per `lift_one` call** — yes, stateless in this sense.
- **Context-register state persists across calls** — and CAN be modified by decoded instructions (ARM `bx lr` switches Thumb/ARM mode). Decoding at the same address with different context state can produce different `LiftRes`.
- **DecodeCache keyed only by `(machine_addr)`** works for current strider because supported arches don't have decode operations that write to context within a single function. It would break for ARM functions that mix Thumb and ARM mid-function. This is a known v1 hazard, NOT something v2 introduces.

**Sources:**
- `Sleigh::lift_one` impl: `../rsleigh/src/lib.rs:159-170` — forwards to `sleigh_bindings_ctx_lift_one(self.ll_ctx.ctx_ptr(), addr, …)`.
- `DecodeCache`: `crates/cfg/src/cfg/decode_cache.rs:38` — keys `(machine_addr) -> Arc<LiftRes>` (implicit context-constancy assumption).
- v1's sequential-within-region invariant: `crates/cfg/src/cfg/builder/region_builder.rs:623-626` — `RegionBuilder::build` advances `cur_addr` linearly per insn, preserving context naturally.

**Corrected implication for Phase 2 Task 2.4:** `Lifter::region(addr) -> &Region` MUST decode sequentially within a region (`for cur in start..terminator { sleigh.lift_one(cur); cur += machine_insn_len; }`), exactly as v1's `RegionBuilder::build` does. Across regions, assume context state is fixed per function entry (v1's invariant). The lazy-per-region API surface is fine; arbitrary out-of-order per-insn lifting across regions is NOT safe and must not be introduced.

**Lesson for future verifications:** A "library is stateless" claim from a research subagent should be sanity-checked against `&mut self`-bearing method signatures. The V3 subagent saw "no per-CFG state" (true) and overstated it as "stateless" (false). User domain knowledge caught this.

## V4 — PyO3 proc-macro + pyo3-stub-gen

**Pass condition:** A custom proc-macro can emit `#[pyclass]` + methods that pyo3-stub-gen picks up.

**Result:** **PARTIAL PASS** — feasible and well-precedented, but with explicit ordering constraints.

**Key technical findings:**
- Rust attribute-macro expansion is defined: outer attributes expand before inner; the outer macro's output is re-scanned for further macros. `#[strider_pattern]` emitting `#[gen_stub_pyclass] #[pyclass] struct Foo` is standard.
- pyo3-stub-gen attribute order is **rigid**: `#[gen_stub_pyclass]` MUST come before `#[pyclass]`; `#[gen_stub_pymethods]` MUST come before `#[pymethods]`. Our macro controls the full attribute stack.
- `multiple-pymethods` is already enabled in `crates/strider-py/Cargo.toml:27` (uses `inventory` for collection). Macro-generated multiple `impl` blocks compose with this fine.
- Closure-storing pyclasses (`PyPat.when(f: PyObject)`) need `Mutex`/`Arc` wrapping for `Send + Sync` — v1's `crates/strider-py/src/pattern.rs:322` already does this; replicate the pattern.
- pyo3-stub-gen's auto-translator does NOT handle every Rust type. Closure args, `PyObject`, custom `Pat` enums need `#[gen_stub(override_type(...))]` or manual `inventory::submit!` overrides emitted by our macro.

**Constraints:**
- `#[pyclass]` rejects lifetime/generic parameters → macro emits one concrete pyclass per pattern type (matches v1's manual approach).
- `mypy --strict` requires stub completeness → macro must emit `#[gen_stub_pymethods]` for every `#[pymethods]` impl, never one without the other.

**Plan update (Phase 4):** First task is **not** the proc-macro — first task is to hand-write one `#[gen_stub_pyclass]` + `#[gen_stub_pymethods]` pattern type and verify the `.pyi` output passes `mypy --strict`. Once one type works end-to-end, the proc-macro codifies the emission pattern. This is a clear TDD scaffold: the hand-written reference IS the test oracle for what the macro must emit.

## V5 — proptest IR generator strategy

**Pass condition:** A `prop_compose!` strategy produces 1000/1000 valid graphs that pass `validate(graph, entry)`.

**Result:** **PARTIAL PASS** — works for value-only subgraphs; control flow requires hand-authored fixtures.

**Strategy shape:** A `Session` struct owns a `FunctionBuilder`, a `Vec<NodeOutputId>` of available value outputs grouped by `NodeOutputType`, and a `Vec<RegionId>`. The strategy is `prop::collection::vec(action_strategy, 1..50)` where each `Action` is an enum variant (`EmitIntConst { width, value }`, `EmitBinaryOp { op, lhs_idx, rhs_idx }`, …). A driver applies each action sequentially, picking compatible operands by **type-tag bucket** (separate `Vec` per `NodeOutputType` width). Width compatibility for binary ops is enforced at action-selection time. The strategy closes with `build_return`.

**Hard cases:**
1. **VarPhi predecessors.** `FunctionBuilder::create_region` auto-creates a `VarPhi` per tracked variable; predecessor wiring needs `link_region` / `build_branch` / `build_if`. Strategy must emit branch actions before return AND guarantee every region created is also reached by a branch.
2. **Memory chain integrity.** `build_load`/`build_store` automatically thread `cur_region_memory` — falls out for free.
3. **Control-flow well-formedness.** Branches must point at existing regions; trivially solved by sampling `dest` from `regions: Vec<RegionId>`.
4. **Float vs int typing.** Operand picker must consult `get_output_type` so `build_float_binary_op` never receives an int input.
5. **Asm-fingerprints.** Wrap every action in `lift_at(addr, |b| …)` with a per-action random `u64`. Satisfies `validate_with_options(check_asm_fingerprints: true)` (which becomes the default per G3).

**Prior art:** **Cranelift's `cranelift-fuzzgen`** is the canonical model — same imperative-action pattern, type-tag operand pools, explicit region/block tracking. Mirror its design.

**Plan update (Phase 6 Task 6.3):** Property tests scoped to value-only DAG invariants (fingerprint monotonicity, ConstantFold/KnownBits preserves validate, egraph saturation confluence on pure arithmetic). Control-flow invariants (RedundantPhis, DeadBranchElimination, indirect-resolver) covered by hand-authored fixtures + `per_arch_test!` macro.

## V6 — Crate dependency direction

**Pass condition:** Zero back-edges beyond the known `cfg → opt`.

**Result:** **PASS-with-findings.** Two additional back-edges discovered. Both fix via interface extraction into `strider-ir`.

**Back-edge 1 — `ir → target` → would become `strider-ir → strider-lift`.** `crates/ir/Cargo.toml:8` lists `target` as a direct dep; `ir::builder` takes `&target::BuiltCallingConvention` as a parameter; `ir::function` stores `no_memory_clobber` sourced from `target::CallingConvention`. **Fix:** Define a thin `FunctionBuilderCC` plain-data struct in `strider-ir` containing only the fields `ir` consumes (`ret_stack_pop: i64`, `no_memory_clobber: bool`, callee-saved/clobber varnode lists). `strider-lift` provides `impl From<BuiltCallingConvention> for FunctionBuilderCC`. `FunctionBuilder::new` accepts the thin type.

**Back-edge 2 — `opt → reader` → would become `strider-analyze → strider-binary`.** `crates/opt/Cargo.toml:9` lists `reader`; `opt::LoadReadOnly` uses `reader::ReadOnlyMemory`. **Fix:** Move the `ReadOnlyMemory` trait (`read_bytes(addr, buf) -> Result<()>`, no binary-format knowledge) into `strider-ir`. Concrete impls stay in `strider-binary`.

**Updated v2 dependency diagram:**
```
strider-binary  →  strider-ir   →  strider-lift  →  strider-analyze  →  strider
                   defines:        depends on:      depends on:         (PyO3)
                   - IR graph      - strider-ir     - strider-ir
                   - validator                      - strider-lift
                   - ReadOnlyMemory trait
                   - FunctionBuilderCC plain struct
                   - egraph adapter
```

Concrete `ReadOnlyMemory` impls live in `strider-binary` (ELF/PE/Mach-O backed). `BuiltCallingConvention` lives in `strider-lift`; `From<BuiltCallingConvention> for FunctionBuilderCC` lives in `strider-lift` (where the source type is defined).

## Resulting Plan Updates

1. **Section A fallback list** — remove cranelift-egraph; keep egg + hand-rolled + drop-egraph.
2. **Target Architecture** — add `ReadOnlyMemory` trait and `FunctionBuilderCC` plain struct to `strider-ir`.
3. **Phase 4 task ordering** — first task is one hand-written `#[gen_stub_pyclass]` reference type; proc-macro is task #2.
4. **Phase 6 Task 6.3** — property tests scoped to value-only DAG; control-flow invariants via hand-fixtures.
5. **Dependency pin** — `salsa = "0.26.2"` (or follow rust-analyzer's current pin).

These updates land in the plan in the next commit on `rewrite/strdier`. Phase 0 starts after that commit.

## V1 Verification Execution Result (Phase 1 Task 1.5 spike)

**Status: PASS.**  Egg with phi-as-opaque-leaves expresses the strider
value-slice model.  Zero-rewrite Graph ↔ `egg::EGraph` round-trip preserves
every structural and semantic bit needed to recover the original `NodeKind` +
output type.

### What landed

Spike crate: `crates/strider-ir/src/egraph_adapter/` (commits `2d870da8` →
`0ecf8d28` → `6300bf56` → `95560386`).

- `egraph_adapter::language::StriderLang` — hand-rolled `egg::Language` impl
  (NOT `define_language!`).  Drops the macro because:
  - It would require `FromStr + Display` impls for every payload type
    (`rsleigh::Vn`, `NodeOutputType`, every Op enum), with zero benefit
    in the zero-rewrite case (we don't parse patterns from strings).
  - Hand-rolled gives us full control over `matches()` semantics — every
    payload field that affects strider-side identity is compared
    explicitly, with a documented contract.
- `egraph_adapter::from_graph::EGraphAdapter::from_graph` — walks reachable
  value outputs from `entry` and adds each as either an `Opaque(payload)`
  leaf (phis, `InitialVar`, `FunctionArg`, `Load` value, `Call`/`CallOther`
  value, plus structurally-defensive opaque cases for memory/control kinds
  that should never reach a value-output classifier) or a type-tagged
  internal e-node (`IntConst`/`BoolConst`/`FloatConst` + every value-
  producing arithmetic / cmp / cast / bool / float / int↔float op).  Memo
  table: `output_to_eclass: HashMap<NodeOutputId, Id>`.  Reverse opaque map:
  `leaf_to_output: HashMap<u64, NodeOutputId>`.
- `egraph_adapter::extract::EGraphAdapter::extract_into_graph` — topo-sorts
  reachable nodes (cycle-safe DFS, treats phi back-edges as non-edges), then
  rebuilds a fresh strider `Graph` by deriving each value-producing
  internal-e-node kind from the egraph extraction (`kind_from_lang`) and
  copying everything else from the original.  Side-tables propagated:
  `asm_fingerprints`, `stack_phi_offsets`, `call_other_names`,
  `call_clobbered_overrides`.
- 17 unit tests (Language + from_graph + extract) and 3 integration tests
  (`tests/egraph_roundtrip.rs`) — all pass on the first green run.

### NodeKind coverage by StriderLang

| Category | Variants covered (this spike) | Deferred to Phase 3 |
|---|---|---|
| **Constants** | `IntConst(u128)`, `BoolConst`, `FloatConst(u64)` (with type-tag) | `IntConstWide` (U256/U512 — modeled as opaque leaf for the spike since `StriderLang::IntConst` payload is `u128`; would need a new `IntConstWide(WideConstId, TypeKey)` variant in Phase 3) |
| **Integer ops** | `IntBinaryOp`, `IntUnaryOp`, `IntCmpOp`, `CastToInt`, `Truncate`, `Popcount`, `Lzcount`, `Extend(ExtendOp)` | — |
| **Bool ops** | `BoolBinaryOp`, `BoolUnaryOp`, `CastToBool` | — |
| **Float ops** | `FloatBinaryOp`, `FloatUnaryOp`, `FloatCmpOp` | — |
| **Conversions** | `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat` | — |
| **Opaque leaves** | `VarPhi`, `MemPhi`, `ValuePhi`, `InitialVar`, `InitialMemory`, `FunctionArg`, `Load` value, `Call` value, `CallOther` value, `SegmentOp` value, `CPoolRef` value, `New` value, `IntConstWide` | — |
| **Control / memory** | Preserved by structural copy from the original `Graph` — never enters egraph | — |
| **Non-value nodes** | `Entry`, `ControlState`, `If`, `Return`, `IndirectBranch`, `Store`, `StackStore`, `StackStorePhi` — copied from original | — |

The only deferred semantic case is **wide constants (U256/U512)**.  Adding a
`StriderLang::IntConstWide(WideConstId, TypeKey)` variant in Phase 3 is a
two-line change (variant + `kind_from_lang` arm); the spike treats them as
opaque so the matching test fixtures (which don't exercise them) compile.

### Performance

Per-region round-trip time on the spike's three integration tests:
**<1 ms per round-trip**, including the explicit `EGraph::rebuild()` call.
The constants/Add chain test has ~7 reachable value outputs; the VarPhi test
has ~3.  Egg's `Runner` is bypassed entirely (no rewrites means no need to
schedule), confirming the V1 verification doc's "Runner defaults are 100×
oversized for tiny problems → bypass it" guidance — adapter cost is
dominated by HashMap insertion, not egg internals.

### Concerns surfaced

1. **`define_language!` would block rsleigh payloads.**  The macro's
   `Variant(data)` form requires `FromStr + Display` on `data` for parser
   integration.  `rsleigh::Vn` derives neither and shouldn't need to.  Phase
   3 should standardise on hand-rolled `Language` impls for any
   strider-adjacent egraph languages.
2. **`Ord` derive ripple effect.**  Egg's `Language` trait requires `Ord +
   PartialOrd`.  Added the derives to `NodeOutputType` + every op enum in
   `ops::op_kinds`.  All are unit/payload-only enums; the derives are
   strictly additive (variant declaration order = lex order).  Workspace
   build clean.  No public surface change beyond the new trait impls.
3. **Multi-output nodes (`Call`).**  Modeled per-value-output as separate
   opaque leaves with distinct payloads (derived from `NodeOutputId.as_u32()`
   so each clobber slot stays unique).  Verified by the structural test —
   the Call node itself is preserved structurally (not in the egraph), and
   its value slots reappear in the extracted graph in slot order.  Phase 3's
   rewrite rules will need an `Analysis::Data` that ties an e-class back to
   its originating `(NodeId, output_index)` for any pass that wants to fold
   across a call's return-value slot.
4. **Wide constants (U256/U512) deferred** — see coverage table.  Trivial to
   add in Phase 3; the spike treats them as opaque to keep `StriderLang`'s
   payload `u128`-sized.
5. **`extract_into_graph` panics on round-trip mismatch.**  The spike
   converts an unexpected `StriderLang` shape into a panic so a regression
   surfaces loudly.  Phase 3 should promote this to a typed `ExtractError`
   the orchestrator can route into `LiftError` / `OptError`.

### Verdict for V1

**Egg works for our shape.**  No fallback path needed.  Plan section A's
primary "egg + bypass-Runner" path is confirmed viable end-to-end on
zero-rewrite round-trip; Phase 3 can proceed with confidence.

The hand-rolled-saturation and drop-egraph fallback paths remain documented
in the plan but are not needed.  cranelift-egraph stays removed (dead crate
per V1 verification).

