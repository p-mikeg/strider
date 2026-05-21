# Round 13 — Ask-8 pass 1: typing / signature / arity audit

Branch: `review/ai7`.

## Verdict

**2 LOW findings.** Both are spurious-`Result` paths in `ir` ops helpers; rest of public surface verified clean.

## Findings

### TY13-1 — `Graph::make_value_node` returns `Result` but the error is structurally unreachable
- **Severity:** LOW (confidence 85)
- **Where:** `crates/ir/src/ops/builder.rs:16-25`
- **What:** `create_node` receives a literal 1-element array `[NodeOutputKind::OutputType(ty)]` and allocates exactly that many output entries.  `node_outputs_exact::<1>(node)?` therefore cannot fail.  The spurious `Result` propagates outward through `pattern/src/pat/node_pat.rs:336` (calls with `?`) up through `try_build → rewrite_rule`.  Two delegating wrappers inherit the same issue: `Graph::make_int_bits_to_float_node` (line 32) and `Graph::make_float_to_float_node` (line 45).
- **Fix:** Change return type to `NodeOutputId`; replace `Ok(self.node_outputs_exact::<1>(node)?[0])` with `self.node_outputs_exact::<1>(node).expect("invariant: create_node produces exactly as many outputs as output_kinds")[0]`.  Update the two delegating helpers.  Drop the `?` in `pattern/src/pat/node_pat.rs:336`.

### TY13-2 — `Graph::make_bool_const` returns `Result` but structurally never errors
- **Severity:** LOW (confidence 83)
- **Where:** `crates/ir/src/ops/consts.rs:102-109`
- **What:** Same root cause as TY13-1.  `create_node` called with `[NodeOutputKind::OutputType(NodeOutputType::Bool)]` (one element); `node_outputs_exact::<1>?` cannot fire.  Doc says "Returns an error when the freshly-created node does not have exactly one output" but that condition is structurally impossible.  Zero current production callers.  Inconsistent with the already-infallible `FunctionBuilder::build_boolean_const` at `builder/nodes.rs:16`.
- **Fix:** Change return to `NodeOutputId`, use `expect`.

## Categories verified clean

✓ **`&mut Graph` where `&Graph` suffices** — all `&mut Graph` parameters in `opt`/`pattern`/`ir` call mutation methods.

✓ **`impl Into<X>` clutter** — `pattern`'s `impl Into<Pat>` is intentional ergonomics (converts 7+ distinct builder types).

✓ **Unused generic params** — none found.

✓ **`Box<dyn Trait>` where `impl Trait` works** — `OptimizerPipeline` stores `Vec<Box<dyn OptimizerRaw>>` for heterogeneous passes; correct because `Vec<impl Trait>` cannot type-erase.

✓ **`Option<Vec<T>>` where `Vec<T>` (empty = none) would suffice** — none in public signatures.

✓ **Names that promise X but body does Y** — no mismatches found.  `apply_stall_guard` modifies budget and returns Err on stall; `classify_anchor*` family classifies without mutating.

✓ **`FunctionBuilder::empty()` `Result`** — doc says "currently never errors for empty input but `Result` is preserved for forward-compatibility"; `new_raw` can fail on non-empty.  Honest.

✓ **`Graph::make_int_const`/`make_float_const` `Result`** — real reachable error paths (non-integer / U256-U512 rejection / non-float).  Correct.

✓ **`build_entry()` `Result`** — same structural-unreachability as TY13-1 but `build_entry` is `pub` and called from external `pcode-lift` tests that already handle `Result`.  Lower priority; can be folded into TY13-1 fix wave.

## Files reviewed

- `crates/ir/src/ops/{builder,consts,rewrite}.rs`
- `crates/ir/src/builder/{mod,nodes}.rs`
- `crates/pattern/src/pat/node_pat.rs`
- `crates/pattern/src/rewrite.rs`
- `crates/opt/src/pipeline.rs`
- `crates/opt/src/flag_cmp_canonicalize/mod.rs`
- `crates/strider/src/orchestrator.rs`
- `crates/ir/src/graph/store.rs`, `crates/ir/src/function.rs`, `crates/ir/src/validate/mod.rs`
- `crates/target/src/calling_convention/mod.rs`
- `crates/reader/src/lib.rs`
