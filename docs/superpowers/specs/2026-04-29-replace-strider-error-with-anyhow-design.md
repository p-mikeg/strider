# Replace `strider-error` with `anyhow`

**Status:** Approved (design phase)
**Date:** 2026-04-29
**Worker:** single agent, isolated git worktree
**Branch:** `feature/anyhow-migration`

## Why

The `strider-error` crate (~700 LoC) provides a `define_error!` macro, a `bridge_error!` macro, a `Traceback` trait, an `ErrorFields` payload (origin `Backtrace` + per-`?` `LocationChain`), and a `format_traceback` renderer. It exists for a planned `strider-py` PyO3 layer that does not exist today.

A grep across the workspace confirms:

- **No code matches on `ErrorKind` variants.** Every `ErrorKind::X` reference is a *constructor* (`Err(ErrorKind::X.into())`, `.ok_or(ErrorKind::X)?`). Nothing branches on the typed kind.
- **No code reads `format_traceback`, `location_chain`, `origin_backtrace`, or `Traceback`** outside the `strider-error` crate itself and `dot::error::Error<E>` (which only *implements* the trait, never reads it).

The machinery has zero downstream consumers and exists for a hypothetical future surface. `anyhow::Error` already provides every feature today's callers actually use (origin backtrace, source chain, `?` autoconversion across crates), and removes the wrapper boilerplate. When `strider-py` is built, it can capture per-`?` locations at the FFI boundary without the workspace carrying the cost.

## Goal

Delete `strider-error` and every per-crate typed `Error`/`ErrorKind` pair. Convert all `Result<T, crate::Error>` → `anyhow::Result<T>`. Use only anyhow built-ins at construction sites — no new macros, no replacement crate.

## Scope

**Crates converted:**

| Crate | Today | After |
|---|---|---|
| `cfg` | `cfg::Error` (define_error!) | `anyhow::Result<T>` |
| `dot` | hand-written generic `Error<E>` | `anyhow::Result<T>` |
| `ir` | `ir::Error` (define_error!) | `anyhow::Result<T>` |
| `opt` | `opt::Error` (define_error! + bridge_error!) | `anyhow::Result<T>` |
| `pattern` | `pattern::Error` (define_error! + bridge_error!) | `anyhow::Result<T>` |
| `pcode-lift` | `pcode_lift::Error` (define_error! + bridge_error!) | `anyhow::Result<T>` |
| `reader` | `reader::Error` (define_error!) | `anyhow::Result<T>` |
| `strider` | `strider::Error` (define_error! + bridge_error! ×6) | `anyhow::Result<T>` |
| `target` | `target::Error` (define_error!) | `anyhow::Result<T>` |
| `strider-error` | exists | **deleted** |

**Crates not touched:** `entity-utils`, `graphmock`, `graphwalk` (no error types).

**Workspace `Cargo.toml`:** `strider-error` removed from `[workspace.dependencies]`. `anyhow` added to `[workspace.dependencies]` (it is not present today, transitive or otherwise). Each converted crate replaces its `strider-error.workspace = true` line with `anyhow.workspace = true`. Each converted crate also drops `thiserror.workspace = true` *unless* it still defines a `thiserror`-derived struct (currently only `ir` does, for `ValidationErrors`).

**Workspace clippy lints stay clean.** The workspace denies `panic`, `unwrap_used`, `expect_used`, `unreachable`, `todo`. anyhow's `bail!` / `anyhow!` / `ensure!` do not panic and do not trip these lints. The migration must not introduce any new `unwrap()` / `expect()` / `panic!()` sites; the existing `.ok_or(...)?` / `.ok_or_else(...)?` pattern carries forward unchanged.

## Construction-site mapping

The migration replaces typed-variant constructors with anyhow's macros. The patterns:

```rust
// before
return Err(ErrorKind::FixedPointLimitExceeded(MAX_ITERS).into());
.ok_or(ErrorKind::NoReturnNode)?
.ok_or_else(|| ErrorKind::ExpectedNodeNotFound("Call", NodeKind::Call).into())
return Err(ErrorKind::ExpectedControl(output, kind).into());
return Err(ErrorKind::IrError(e).into());

// after
bail!("fixed-point limit exceeded after {MAX_ITERS} iterations");
.ok_or_else(|| anyhow!("no return node"))?
.ok_or_else(|| anyhow!("expected Call node not found, got {:?}", NodeKind::Call))?
bail!("expected control output, got {kind:?} at {output:?}");
return Err(e.into());                          // anyhow's blanket From<E: Error>
```

Cross-crate boundaries that previously used `bridge_error!` now rely on anyhow's `From<E: Error + Send + Sync + 'static>` blanket impl — `?` lifts any source error into `anyhow::Error` automatically. The `bridge_error!` calls all delete.

`.with_context(|| format!("..."))` is added at high-level boundaries to give the source chain real content. Target sites:

- `OptimizerPipeline::run` — wrap each pass invocation: `.with_context(|| format!("running pass {}", pass.name()))`.
- `Strider::build` (or equivalent entry point) — wrap with `.with_context(|| format!("lifting function at {addr:?}"))`.
- `FunctionBuilder::build` — wrap with `.with_context(|| "building IR")`.
- Each `cfg::Cfg::build` call site — wrap with the binary path / entry address.

Construction sites otherwise convert one-for-one without contextual prefixes.

## Special cases

### `dot::error::Error<E>`

Currently a hand-written generic wrapper that holds an inner error and an `ErrorFields` payload. After migration: deleted. `dot` returns `anyhow::Result<T>` directly. The `Traceback` impl deletes with it. The `dot/tests/error.rs` test (which mirrors `strider-error`'s macro_contract test) deletes.

### `ir::validate::ValidationErrors`

This is the bundled-validation-errors aggregate, *not* a wrapper. It stays as a plain `thiserror::Error` (or hand-written `Error` impl) struct. `?` lifts it into `anyhow::Error` cleanly — anyhow does not unwrap structured errors, so the aggregation behavior is preserved. Callers that want to inspect individual validation errors can still downcast via `anyhow::Error::downcast_ref::<ValidationErrors>()`.

### Constructors with structured payloads

A few `ErrorKind` variants carry structured data that the original message format relied on (e.g. `ExpectedNodeNotFound(name, kind)` formats both fields). These convert to `anyhow!("expected {name} node not found, got {kind:?}")` — the data is captured in the formatted message, with no struct preserved. This is the loss accepted under "What's lost" below.

### `format_traceback`, `LocationChain`, `Traceback`, `ErrorFields`

All deleted. Zero downstream consumers (verified by grep). Tests in `crates/strider-error/tests/` (`fields.rs`, `format.rs`, `macro_contract.rs`) delete with the crate.

## What's preserved

- **Origin `Backtrace`.** `anyhow::Error` captures a backtrace on construction when `RUST_BACKTRACE` is set, identically to `ErrorFields::new()` today.
- **Source chain.** `anyhow::Error::source()` walks the chain. `.context()` adds new layers. Equivalent to the existing source-chain walk in `format_traceback`.
- **Cross-crate `?` propagation.** `anyhow::Error: From<E: Error + Send + Sync + 'static>` replaces every `bridge_error!` call.
- **Display output.** `format!("{err}")` and `format!("{err:#}")` (alternate-flag chain print) cover the human-readable rendering today's `format_traceback` produces, minus the per-`?` location chain.

## What's lost (accepted)

- **Per-`?` `LocationChain`.** No call site reads it today. If `strider-py` later wants Python-traceback fidelity, it can capture per-`?` locations at the FFI boundary using `#[track_caller]` wrappers — without the workspace carrying the cost.
- **Variant-name grep-ability.** `grep -r "FixedPointLimitExceeded"` no longer hits the construction site; only the message string survives. Mitigation: error messages stay literal-string heavy, so message-text grep is still useful.
- **Typed downstream matching.** Already not used today; the option to add it later is foreclosed without re-introducing a typed enum.

## Migration sequence (inside the worker)

The production dependency graph among error-bearing crates (verified by reading each `Cargo.toml`):

| Tier | Crate | Production deps within this set |
|---|---|---|
| L0 | `target`, `reader`, `ir`, `dot` | none |
| L1 | `pcode-lift` | `ir`, `target` |
| L1 | `pattern` | `ir` |
| L2 | `opt` | `ir`, `pattern`, `reader`, `target` |
| L3 | `cfg` | `ir`, `opt`, `pcode-lift`, `dot`, `target` |
| L4 | `strider` | all of the above |

(Note: `cfg` depends on `opt` in production, contrary to what `CLAUDE.md` implies. The Cargo.toml is authoritative and the spec uses the actual graph.)

The work proceeds leaves-first so each crate's downstream consumers are already on `anyhow` when their turn comes. Each crate's `?` continues to compile against its upstream regardless of order — anyhow's `From<E: Error + Send + Sync + 'static>` blanket impl lifts any source error into `anyhow::Error` — so leaves-first is for cleanliness of progression, not correctness.

1. **Workspace setup.** Add `anyhow = "1"` to `[workspace.dependencies]` in root `Cargo.toml`.
2. **Tier L0.** Convert `target`, `reader`, `ir`, `dot` (any order; the worker does them sequentially).
3. **Tier L1.** Convert `pcode-lift`, `pattern`.
4. **Tier L2.** Convert `opt`.
5. **Tier L3.** Convert `cfg`.
6. **Tier L4.** Convert `strider`.
7. **Delete `strider-error/`** directory; remove its `[workspace.dependencies]` entry.
8. **Final verification.** `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo run --example strider`. All must pass with zero warnings.

Each crate's conversion follows the same template: replace `define_error!` block + `bridge_error!` calls in `src/error.rs` (the file may delete entirely if it had no other content), then sweep all `Err(ErrorKind::X.into())` / `.ok_or(ErrorKind::X)?` / `.ok_or_else(|| ErrorKind::X(...).into())?` sites in that crate to `bail!` / `anyhow!`, then in `Cargo.toml`: replace `strider-error.workspace = true` with `anyhow.workspace = true`, and drop `thiserror.workspace = true` unless the crate still has a `thiserror`-derived type (only `ir`'s `ValidationErrors` should). After each crate, `cargo test --package <crate>` runs and must pass before moving to the next crate.

## Testing strategy

No new tests. The migration is mechanical and per-crate test suites already cover behavior. The verification gates:

- **Per-crate.** `cargo test --package <crate>` passes after that crate's conversion.
- **Workspace-wide (final gate).** `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets` pass with zero warnings (project standard). `cargo run --example strider` produces `cfg.html` and `graph.html` matching pre-migration baselines (visual diff or byte-equal — visual is fine for the smoke test since CFG/IR layout is deterministic).

## Workspace + worker setup

- **Worktree.** Created via `superpowers:using-git-worktrees` from `master`. Path: `.worktrees/anyhow-migration/` (or whatever the skill chooses).
- **Branch.** `feature/anyhow-migration`.
- **Worker.** Single agent does the whole conversion in one session. No parallel sub-agents — the migration order is strictly sequential (dependency chain), and the changes are mechanical enough that subagent overhead would dominate.
- **PR.** One PR back to `master` after final verification passes.

## Out of scope

- Adding any error-construction macros (`M1` only — anyhow built-ins).
- Migrating `rsleigh` (external, not in this workspace).
- Building `strider-py`. Its design is unchanged by this work; when built, it will consume `anyhow::Result<T>` from the `strider` crate and produce Python exceptions however its author chooses.
- Changing error messages beyond what's needed to embed previously-structured payload data into the message string. Existing message wording is preserved.

## Risk and rollback

**Risk:** medium. The change is mechanical but workspace-wide; a missed `bridge_error!` or a stale `ErrorKind::` reference will fail to compile. The compiler catches these — there is no runtime risk surface.

**Rollback:** the worktree is isolated. If the migration is abandoned, `master` is untouched and the worktree deletes. If merged and later regretted, `git revert` of the merge commit restores the prior state cleanly (no schema changes, no data migrations, no external consumers to coordinate with).
