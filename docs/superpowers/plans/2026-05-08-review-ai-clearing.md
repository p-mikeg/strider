# review/ai clearing — finish every remaining audit item

**Goal:** Land every audit finding outstanding after `2026-05-08-review-ai-finalize.md`. End state: `cargo build --workspace` clean (no warnings), `cargo clippy --workspace -- -D warnings` clean, `pytest` green (excluding pre-existing arm64).

**Strategy:** Parallelise mechanical/test work via agents; do correctness + type-design + structural refactors myself.

---

## Phase 1 — parallel mechanical agents

### Agent A — small-scope items (parallel)
- **L4** `pattern::CastMask::empty()` classmethod alias (cosmetic).
- **L5** `crates/strider-py/README.md` minor inaccuracy on Strider/Sleigh ordering.
- **A7** `x86_64_all_preserving.ret_stack_pop=0` comment review (currently by-design — improve doc).
- **node_signature visibility** — demote `pub` → `pub(crate)` for `Signature/SlotList/Slot/ExpectedOutputKind/SlotRole` if no external consumers.
- **N-2** `LoopState.sleigh: Option<Sleigh>` cleanup — collapse `.take().ok_or(literal-string)` calls behind a helper.
- **N-12** `EdgeKind` BTreeSet collapse for resolved-targets fingerprint.
- **F-18** `apply_in_place_edit` duplicate clobber-recompute — already partially addressed by E3 hoist; finish.
- **build_call_other naming** — `_terminal` / `_modeled` / no-op asymmetry — cosmetic doc clarification.

### Agent B — generalization (parallel)
- **Cat. 1** `WorkSet::seeded_kind` consolidation across opt-pass sites.
- **Cat. 9** `RegionEdgeKind` dispatch trait or method.
- **F-15** `locate_and_write` helper extraction in `apply_elf_relocations`.

### Agent C — tests (parallel)
- **O3** vn_io sub-register partial-write with phi-live parent (synthetic FunctionBuilder fixture).
- **O5** Python typed-error end-to-end tests (one fixture per typed exception).
- **O9** Stack-array indirect-branch shape (synthetic IR via FunctionBuilder).
- **O12-O15** P4 Criterion benchmarks under `crates/strider/benches/scaling.rs`.

---

## Phase 2 — correctness fixes (sequential, mine)

1. **C2 (correctness)** FlagCmpCanonicalize Rule 2 shared-capture brittleness — investigate; if real bug, fix; if intractable, document.
2. **C4** ConstantFold `ZeroExtend(IntConst v)` defensive width-mask.
3. **C2 (strider-target-reader)** `handle_call_other` magic `[1]` memory index — add named accessor.
4. **H5** `stall_budget` reset across Rebuild transitions.
5. **M6** `add_region_from_elf(apply_relocations=True)` autoload consistency.
6. **V-2** `check_layer_c_phis` skip unreachable / zombie nodes.
7. **A1 (pcode-lift)** `vn_mask` AVX-2 / AVX-512 widening (32 / 64 bytes).
8. **M1 (py-support)** GIL release in `strider.run` for the pure-Rust `MemoryMap` fast path.

---

## Phase 3 — type-design (sequential, mine)

1. **BuiltCallingConvention** field privacy + accessors.
2. **FunctionBuilder::set_lift_addr** scope-guard `lift_at(addr, |b| ...)`.
3. **Full RewriteCtx newtype** — replace dummy-BuiltFunctionGraph trick.
4. **FB-3** `Region.variables` sparse-init Option (ir-internal — defer if ripple is wide).

---

## Phase 4 — selected generalizations (sequential)

- **Cat. 2** `entity_utils::Memo<K, V>` newtype for opt-pass memos.
- **Cat. 5** dedup-cache borrowed key (perf).

---

## Phase 5 — skipped (audit explicitly recommended skip)

- **Cat. 3** `FixedPointDriver<F>` — audit said HIGH risk: three loops with genuinely different cap/stall semantics; forcing shared driver loses correctness invariants.
- **Cat. 6** `BindingTable` shared between matcher + flag-cmp rule engine — small duplication; abstraction overhead exceeds savings.

---

## Phase 6 — final clean

- `cargo build --workspace` no warnings.
- `cargo clippy --workspace -- -D warnings` clean.
- `cargo test --workspace` green.
- `cd crates/strider-py && uv run maturin develop --release && uv run pytest tests/python/ --ignore=tests/python/test_arm64_kernel_lift_bugs.py` green.
- Fix any deprecation warnings introduced earlier (e.g. `from_graph_and_entry` deprecation lint in test sites).
