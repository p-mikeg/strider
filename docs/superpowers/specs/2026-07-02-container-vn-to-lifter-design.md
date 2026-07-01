# Move all container-vn logic into the lifter

## Goal
`strider-ir`'s **prod** surface owns zero machine-register container logic. The
container-resolution primitives become lifter-private; `strider-ir` keeps only
`#[cfg(test/test-util)]` copies for fixtures that fake the lifter. Pattern
matching on an explicit varnode does a local greedy containment check instead of
relying on the IR to canonicalise.

## Constraint that shapes it
`strider-lift → strider-ir`, and `strider-lift` dev-deps `strider-ir-test-utils`
which deps `strider-ir`. So geometry needed by BOTH lift-prod and ir-tests must
be duplicated: one prod copy in lift, one `#[cfg(test/test-util)]` copy in ir.
No cycle-free single home exists (target deliberately rejects owning it).

## Moves

### strider-lift (new prod owner) — `lift/container.rs`
- `largest_container_in(vns, vn)` — copied from ir.
- `build_container_map(tracked, queries)` — copied from ir (+ its one test).
- `dedup_overlapping_largest(raw)` — copied from ir.
- `seed_cc_regs(&mut Vec<Vn>, cc)` — the CC-seed loop lifted out of
  `FunctionBuilder::new`.
- `FunctionLifter::new`: seed → dedup → build map → `FunctionBuilder::new(tracked)`.
- `cc_projection::container_of` fallback uses the local `largest_container_in`.

### strider-ir (prod cleaned)
- `FunctionBuilder::new`: drop CC-seed + `dedup_overlapping_largest`; forward the
  given (already-canonical) set to `Function::new` (which still sorts+interns —
  sorting is determinism, not container logic).
- Drop `pub use largest_container_in, build_container_map` from `lib.rs` +
  `function/mod.rs`.
- Keep, `#[cfg(test/test-util)]`-gated, for the fixtures/RegisterSet:
  - `largest_container_in`
  - `dedup_overlapping_largest` (+ its existing tests)
  - `canonicalize_tracked(raw, cc) -> Vec<Vn>` = seed + dedup (RegisterSet + tests)
  - `cc_ret_and_clobber_vns` (already gated)
- `build_container_map` leaves ir entirely (its one test moves to lift).

### strider-ir-test-utils
- `RegisterSet::build_fn`: canonicalize before `FunctionBuilder::new`.

### strider-pattern (behavior change — TDD)
- Private `vn_contains(outer, inner) -> bool` (pure geometry).
- `InitialVarFor` predicate: `initial_vn(id) == want` → `vn_contains(initial_vn(id), want)`.
- `phi_var_limit`: exact `== Some(want)` → `get_vn_for_value(v).is_some_and(|g| vn_contains(g, want))`.
- Effect: pinning a sub-register (`eax`) now matches the container `InitialVar(rax)`.

## Verification
Full gate: `cargo test --workspace` + `cargo clippy --workspace` + pytest 873.
TDD the pattern change (failing test first). The duplicated dedup/geometry in
lift vs ir is marked with a cross-reference comment.
