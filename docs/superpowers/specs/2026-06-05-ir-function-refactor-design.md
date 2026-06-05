# strider-ir function-module refactor — design

Date: 2026-06-05
Branch: `develop` (merge to `master` after approval + green gate)

Four independent refactors in `strider-ir` (plus one in `strider-graph`).
None changes the lifted-IR semantics; the goal is to put shared vocabulary on
the right traits, record register args where they're already known, give the
function data structures one logical home, and drop a dead generic walker.

## Item 1 — relocate three constructor/check functions

Today these are inherent on `FunctionBuilder`, which means no other builder
(notably `EditFunction`, used by opt passes) can reach them.

### `require_control_kind` / `require_memory_kind` → `IRViewer`

Both are pure reads (`self.function().value_kind(value)` + an error). Their
four siblings — `require_value_kind`, `require_bool_value`,
`require_phi_token_kind`, `require_value_type` — are already default methods on
`IRViewer` (`viewer.rs`). Move these two there too, so the whole `require_*`
family lives in one place on the read trait.

- Delete the two `pub(crate)` inherent methods from `region.rs`.
- Add them as default methods on `IRViewer`, after `require_phi_token_kind`.
- `require_terminator_kinds` stays on `FunctionBuilder` (it's
  `TerminatedRegion`-specific) and calls the two through the trait.
- All existing call sites (`region.rs`, `nodes.rs`) are unchanged: every caller
  is a `FunctionBuilder`, which gets the methods via its `IRViewer` impl.

### `build_int_const_wide` → `IRBuilderExt`

It's a constructor, so it belongs on the build-extension trait. The blocker is
that its body interns through `self.function_mut().intern_wide_const(value)`,
and the `IRBuilder` seam currently exposes only `create_node_attributed`.

Resolution: add **`fn function_mut(&mut self) -> &mut Function;`** as a required
method on the `IRBuilder` trait — the symmetric write-side counterpart to
`IRViewer::function(&self) -> &Function`. Both `FunctionBuilder` and
`EditFunction` already have an inherent `function_mut()`, so the trait impls
forward to that one-liner; concrete `self.function_mut()` calls still resolve to
the inherent method, and the trait method is reached only through a generic
`B: IRBuilder` bound.

- Move `build_int_const_wide` from `nodes.rs` to `builder_ext.rs` as an
  `IRBuilderExt` default method; the body is unchanged.
- Add `function_mut` to the `IRBuilder` trait + both impls.
- Update the `IRBuilder` doc comment: it previously claimed the trait "only
  creates and exposes read access." Note that `function_mut()` is a mutable
  escape hatch that bypasses `EditFunction`'s live/roots bookkeeping (mirroring
  the warning already on `EditFunction::function_mut`); default methods must not
  mutate graph *structure* through it (interning a wide const is side-table-only,
  hence safe).
- Fix the stale `FunctionBuilder::build_int_const_wide` doc reference in
  `build_int_const`'s error message / rustdoc.

This also makes wide-const construction available to `EditFunction` (opt
passes), which is the point of the move.

## Item 2 — record register args at builder entry

Register-passed argument detection doesn't need the optimized graph: at builder
entry the calling convention is known and one `InitialVar` per tracked variable
is created. Record the register args there instead of re-deriving them in a
post-pass.

Per the agreed contract: **always record the largest-container `InitialVar` for
every `arg_passing_regs[i]`, unconditionally.** No liveness filter and no
sub-register fallback — the container is what's seeded into the tracked set in
`FunctionBuilder::new`, and an arg that the function never reads is culled by DCE
and dropped from `arg_index_to_values` by `Function::compact` (which already
remaps that table by surviving `ValueId`). Patterns then naturally won't find it.

### Builder change (`builder/vars.rs::set_entry_region`)

After the `InitialVar`-creation loop, iterate `default_cc.arg_passing_regs` in
ABI order; for each `reg` at index `i`, look up its `VarId`
(`var_table.key_of(reg)`), take that variable's `InitialVar` output value from
the `initial_variables` map, and call `function_mut().register_arg_value(i, value)`.

Notes:
- The register index `i` is the canonical positional index for register slots
  (stack slots follow at `arg_passing_regs.len()`), matching
  `PositionalArgLayout::register_args()`.
- `dedup_overlapping_largest` guarantees the seeded container is the tracked
  variable, so the `InitialVar` is always the largest container.

### `FunctionArgDetect` becomes stack-only (`strider-opt/src/function_args/`)

- Delete `detect_register_args`, `largest_sub_in`, the sub-register-fallback
  docs, and the register-arg portion of the module docs.
- Keep `detect_stack_args` and its memory-SSA shadow machinery unchanged.
- Stop calling `clear_arg_values()` (it would wipe the build-time register
  args). The stack path already groups + registers by stack index; confirm
  during implementation that re-running it is idempotent without the global
  clear (the stack indices it repopulates are disjoint from the register
  indices, so a targeted clear of only the stack indices — or relying on the
  stack path overwriting its own indices — preserves idempotency across the
  orchestrator's stable iterations). Pick the minimal correct mechanism.

### Test updates (`function_args/tests.rs`)

- `unused_register_arg_yields_no_node`: invert — an unused arg register is now
  registered at build time, and is removed only after DCE + `compact`. Re-target
  the assertion accordingly (or relocate it to a builder-level test that asserts
  build-time registration + post-compact removal).
- The register-arg detection tests move to asserting the builder-entry behavior
  (build a function, check `arg_index_to_values(i)` carries the `InitialVar`
  before any opt pass runs).
- Stack-arg tests are unaffected.

## Item 3 — consolidate the function data structures into `function/`

Today they're scattered: `function.rs`, `edit/` (`mod.rs` + `function_state.rs`),
`function_dot/`. New layout:

```
crates/strider-ir/src/function/
  mod.rs     — thin root: module docs, submodule decls, re-exports
  data.rs    — the Function struct + impls         (was function.rs)
  state.rs   — FunctionState                        (was edit/function_state.rs)
  edit.rs    — EditFunction                         (was edit/mod.rs)
  dot/       — FunctionDotDumper et al.             (was function_dot/)
    mod.rs, label.rs, raw.rs, render.rs, tests.rs
```

- `lib.rs`: replace `mod function; mod edit; mod function_dot;` with `mod
  function;`. Keep every crate-root `pub use` re-export identical
  (`crate::Function`, `crate::EditFunction`, `crate::FunctionState`, the dot
  types) so downstream crates see no change.
- `function/mod.rs` declares `mod data; mod state; mod edit; mod dot;` and
  re-exports their public items.
- Internal path fixups: `crate::function::Function` still resolves (re-exported
  from `data` via `mod.rs`); `crate::edit::EditFunction` references become the
  crate-root `crate::EditFunction` (or `crate::function::edit::…`);
  `crate::function_dot::…` → `crate::function::dot::…` (notably the
  `FunctionDotDumper` / `build_arg_reverse_map` references inside `data.rs`).
- Pure move + path rename; no behavior change. Module-level docs move with their
  code.

## Item 4 — delete `strider-graph/src/walk.rs`

The generic def→use walkers (`preorder_seeds`, `reverse_postorder_seeds`) have no
cross-crate users; the complex control-aware walkers live in strider-ir's own
`walk` module. The only caller is strider-graph's own
`tests/proptest_invariants.rs:275`, which uses `preorder_seeds` as a reachability
oracle.

- Delete `crates/strider-graph/src/walk.rs` and `mod walk;` from
  `strider-graph/src/lib.rs`.
- Re-anchor the proptest's reachability check to an inline backward-input
  reachability computation (a few lines, same oracle) rather than dropping the
  property — coverage preserved.

## Item 5 — collapse the four `value_vn` accessors into one value-keyed pair

`Function` currently exposes four accessors over the single `value_vn`
(`FxHashMap<ValueId, Vn>`) map:

- `clobbered_vn(ValueId) -> Option<Vn>` / `set_clobbered_vn(ValueId, Vn)` —
  already a direct value-keyed `get`/`insert`.
- `phi_var_tag(NodeId) -> Option<Vn>` / `set_phi_var_tag(NodeId, Vn)` —
  node-keyed wrappers that derive `node_outputs(n).first()` first, then do the
  same `value_vn` `get`/`insert`.

Collapse all four into a single value-keyed pair:

```
fn get_vn_for_value(&self, value: ValueId) -> Option<Vn>
fn set_vn_for_value(&mut self, value: ValueId, vn: Vn)
```

- Delete all four old accessors; the node-keyed `phi_var_tag`/`set_phi_var_tag`
  go away entirely.
- Every node-keyed caller (pattern matcher `node_builders/phi.rs`, the
  indirect-branch classifiers in `indirect_branch_resolve/classify.rs`,
  `rewrite/mod.rs`, the dot renderers, and the ~15 test sites) derives the
  value first: `let v = f.node_outputs(n)[0];` then `f.get_vn_for_value(v)`.
- **Empty-output caveat.** A handful of callers scan *arbitrary* nodes (e.g.
  `function.rs` compaction tests doing
  `all_node_ids().any(|n| phi_var_tag(n) == Some(dead_vn))`). `Return` has zero
  outputs, so indexing `[0]` would panic where `phi_var_tag`'s `.first()?`
  previously returned `None`. Those scans must guard the empty case —
  `f.node_outputs(n).first().copied().and_then(|v| f.get_vn_for_value(v))` —
  preserving the prior `None` behavior. Callers already inside a
  `NodeKind::Phi` arm (a Phi always has one output) can index directly.
- Value-keyed call sites (`clobbered_vn`/`set_clobbered_vn`) are a straight
  rename.
- Update doc references that name the old accessors (`node/kind.rs`,
  `node_signature.rs`, `function_dot/raw.rs`, `match_result.rs`,
  `strider-py/src/matcher.rs`). `CLAUDE.md` also references `phi_var_tag`;
  that's left for a separate CLAUDE.md pass (out of scope here).

This spans `strider-ir`, `strider-opt`, `strider-orchestrator`,
`strider-pattern`, and `strider-py` plus their tests — the widest-reaching item,
but purely a rename + key-type adaptation with no semantic change.

## Item 6 — remove the dead `stack_offsets()` iterator

`Function::stack_offsets(&self) -> impl Iterator<Item = (NodeId, ValueId, i64)>`
(the iterate-all-entries accessor) has zero callers in the workspace. Delete the
method. The singular `stack_offset(id)` lookup, `set_stack_offset`, the
`stack_offsets` field, and its `compact` remap all stay — they back the pattern
DSL's `stack_only` / `stack_offset(k)` filters, `CallStackArgCollect`,
`StackOffsetDetect`, and dot rendering.

## Item 7 — remove `set_asm_fingerprint`, migrate to `extend_asm_fingerprint`

`Function::set_asm_fingerprint` has no production callers (only `#[cfg(test)]`
modules and `strider-ir-test-utils`). Remove it; the fingerprint mutation API
becomes the two no-shrink mutators `extend_asm_fingerprint(id, &[u64])` and
`extend_asm_fingerprint_from(dst, src)`.

Migration (test-utils + ~20 test sites across `strider-ir`, `strider-opt`,
`strider-orchestrator`):

- **Replace-vs-union check per site.** `set` replaced; `extend` unions. For a
  node whose fingerprint is empty at the call, `f.extend_asm_fingerprint(n,
  &[A])` is identical to the old `set`. For a node that already carries a
  fingerprint (notably test-utils' auto-stamped `SENTINEL`), the union would
  leak the prior entry into an exact-match assertion — those sites must be
  reworked so the node starts empty before stamping (e.g. build it raw rather
  than through the auto-stamping helper, or assert against the union explicitly).
- `strider-ir-test-utils/src/lib.rs:383` (the `SENTINEL` stamper) switches to
  `extend_asm_fingerprint` — it stamps freshly-created nodes, which are empty, so
  the behavior is identical.
- Signature change: callers pass `&[u64]` (slice) instead of `Vec<u64>`.
- Update the doc comment on `extend_asm_fingerprint` / the side-table field that
  references `set_asm_fingerprint` as the test entry point.

Each migrated test must pass unchanged in intent — the verification gate's full
test run is the backstop for any missed replace-vs-union site.

## Verification gate

Per the workspace merge rule:
- `cargo test --workspace` (all Rust tests)
- `cargo clippy --workspace` (zero warnings)
- `uv run pytest` in `strider-py`

Work on `develop`, commit per item, push to `origin develop` after each commit.
Prompt the user before merging `develop` → `master`.

## Non-goals

- No change to lifted-IR semantics or the pattern-matching surface.
- No change to the stack-arg detection algorithm.
- No proc-macro / codegen changes.
