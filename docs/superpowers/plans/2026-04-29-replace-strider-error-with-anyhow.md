# Replace `strider-error` with `anyhow` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The user requested a single worker, so subagent-driven mode is **not** used.

**Goal:** Delete `strider-error` and every per-crate typed `Error`/`ErrorKind` pair; convert all `Result<T, crate::Error>` to `anyhow::Result<T>`; rewrite construction sites to anyhow built-ins (`anyhow!`, `bail!`, `ensure!`, `.context()`).

**Architecture:** Pure anyhow (option A from the spec). No replacement macros. Per-crate `thiserror` enums delete entirely except for `ir::ValidationErrors` (a bundled-error aggregate, stays as `thiserror`) and three small sentinel structs in `pattern` that preserve test-asserted public API (`RewriteSkip`, `NotBuildable`, `MissingBinding`).

**Tech Stack:** Rust 2024, `anyhow` 1.x (added to workspace), `thiserror` 2.x (retained only in `ir` and `pattern` for the four named structs).

**Spec:** [`docs/superpowers/specs/2026-04-29-replace-strider-error-with-anyhow-design.md`](../specs/2026-04-29-replace-strider-error-with-anyhow-design.md)

## Conversion direction (top-down)

The migration proceeds **top-down** through the dependency graph: convert the topmost crate (`strider`) first, then its dependents, working toward the leaves. This is the correct direction because anyhow's `From<E: Error + Send + Sync + 'static>` blanket impl only lifts typed errors into `anyhow::Error`, not the reverse. When an upstream crate converts to `anyhow::Result`, every still-typed downstream consumer's `?` on the new `anyhow::Error` would fail (no `From<anyhow::Error> for DownstreamError` impl exists). Top-down avoids this: when an upstream converts, its already-converted downstream uses anyhow's identity `From<anyhow::Error>` impl, so `?` continues to lift cleanly.

Conversion order: **strider → cfg → opt → pattern → pcode-lift → ir → dot → target → reader → delete strider-error**.

(The spec describes a leaves-first order; that order is incorrect for the same reason. The plan supersedes the spec on ordering — the spec's "what" is right, but its "how" is wrong on this point.)

---

## Conversion templates (referenced by every per-crate task)

These templates are defined once and applied by name in each crate task. Each template is a concrete transformation, not a placeholder.

### Template C (Cargo.toml swap)

In `crates/<name>/Cargo.toml`'s `[dependencies]`:

- **Remove** the line `strider-error.workspace = true`.
- **Add** the line `anyhow.workspace = true`.
- **Remove** `thiserror.workspace = true` **unless** the crate keeps a `thiserror`-derived type (only `ir` and `pattern` do — see Tasks 8 and 6).

### Template E (error.rs replacement)

The crate's `src/error.rs` content becomes (for crates with no kept-typed-types):

```rust
//! Error type for the `<crate-name>` crate.
//!
//! All fallible operations return `anyhow::Result<T>`. Construction sites
//! use `anyhow!` / `bail!` / `ensure!`. Cross-crate `?` works through
//! anyhow's `From<E: Error + Send + Sync + 'static>` blanket impl.

pub type Result<T> = anyhow::Result<T>;
```

If the crate's `lib.rs` has `pub use error::{Error, ErrorKind, Result};` (or similar), change to `pub use error::Result;` and drop the `Error` / `ErrorKind` re-exports.

### Template S (construction-site sweep)

Each crate's source files contain construction sites in five shapes. Apply the matching transformation to every occurrence:

| # | Pattern (before) | Replacement (after) |
|---|---|---|
| S1 | `return Err(ErrorKind::X(args).into());` | `bail!("X message with {args}");` |
| S2 | `Err(ErrorKind::X(args).into())` (expression position) | `Err(anyhow!("X message with {args}"))` |
| S3 | `.ok_or(ErrorKind::X)?` | `.ok_or_else(\|\| anyhow!("X message"))?` |
| S4 | `.ok_or_else(\|\| ErrorKind::X(args).into())?` | `.ok_or_else(\|\| anyhow!("X message with {args}"))?` |
| S5 | `.ok_or_else(\|\| ErrorKind::X(args))?` (no `.into()`) | `.ok_or_else(\|\| anyhow!("X message with {args}"))?` |

The message text comes from each variant's existing `#[error("...")]` attribute on the `ErrorKind` enum — preserve that wording verbatim. When the variant carries structured data, embed the formatted fields the same way the `#[error]` attribute did.

After every task: add `use anyhow::{anyhow, bail};` (and `ensure` if used) at the top of any source file that gained `bail!` / `anyhow!` / `ensure!` invocations. Drop any `use crate::error::ErrorKind` import that no longer compiles.

### Template T (test command)

After each crate is fully converted, run:

```bash
cargo test --package <crate-name>
```

Output must be `test result: ok` for every reported test binary.

```bash
cargo clippy --package <crate-name> --all-targets
```

Output must be empty (no warnings). The workspace lints deny `panic` / `unwrap_used` / `expect_used`, so any new violation breaks the build.

### Template G (commit)

```bash
git add -A
git commit -m "$(cat <<'EOF'
anyhow: convert <crate-name> to anyhow

<one-line summary of structural changes>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

One commit per crate task. The summary line is the only thing that varies; the body follows the template.

---

## Task 1: Worktree setup

**Files:**
- Create: `.worktrees/anyhow-migration/` (a git worktree off `master`)

- [ ] **Step 1: Confirm clean working tree on `feature/ai`**

Run: `git status --short`
Expected output: only `.claude/` and `SIMPLIFICATION_REVIEW.md` listed as untracked. The most recent commit on `feature/ai` should include the spec + this plan; verify with `git log --oneline -2`.

- [ ] **Step 2: Create worktree on a fresh branch off `master`**

Run from the main checkout:
```bash
git worktree add -b feature/anyhow-migration .worktrees/anyhow-migration master
cd .worktrees/anyhow-migration
git status --short
```
Expected output: empty (fresh checkout of `master`). All subsequent tasks run from inside this worktree.

- [ ] **Step 3: Verify baseline build is green**

Run from inside the worktree:
```bash
cargo build --workspace
```
Expected: build succeeds. Warnings are fine — we're checking the starting state is buildable.

- [ ] **Step 4: Capture baseline test results**

Run:
```bash
cargo test --workspace 2>&1 | tail -30
```
Expected: every package reports `test result: ok`. This is the gate every later task must still pass.

- [ ] **Step 5: No commit for this task**

The worktree creation itself isn't a commit. Move to Task 2.

---

## Task 2: Add `anyhow` to workspace dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Read current workspace deps**

Read `Cargo.toml`. Confirm `anyhow` is **not** listed in `[workspace.dependencies]`.

- [ ] **Step 2: Add anyhow to workspace deps**

Edit `Cargo.toml`. In the `[workspace.dependencies]` block, after the `thiserror = "2.0.18"` line, add:

```toml
anyhow = "1.0"
```

- [ ] **Step 3: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: succeeds. Adding an unused workspace dep is a no-op.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "$(cat <<'EOF'
anyhow: add anyhow to workspace dependencies

Adds anyhow = "1.0" to [workspace.dependencies] so per-crate Cargo.toml
files can pick it up via `anyhow.workspace = true` in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Convert `strider` (54 sites, special: 6 cross-crate bridges go away)

**Files:**
- Modify: `crates/strider/Cargo.toml`
- Modify: `crates/strider/src/error.rs`
- Modify: `crates/strider/src/lib.rs` (re-exports)
- Sweep: every file in `crates/strider/src/` with an `ErrorKind::` reference (54 sites)
- Modify: `examples/strider.rs` if it constructs strider errors directly

The crate has 23 own variants plus 6 cross-crate bridge variants (`CfgError`, `IrError`, `OptError`, `TargetError`, `PcodeLiftError`, `PatternError`, plus `SleighError` via `#[from]`). Today, `?` on each upstream's typed error lifts via the `bridge_error!`-generated `From` impl. After this task, `strider` returns `anyhow::Result`, and every `?` on an upstream error (still typed) lifts via anyhow's blanket `From<E: Error>`. No upstream needs to be converted yet — the blanket impl handles all of them.

- [ ] **Step 1: Apply Template C to `crates/strider/Cargo.toml`**

Before:
```toml
[dependencies]
rsleigh.workspace = true
strider-error.workspace = true
thiserror.workspace = true
target.workspace = true
ir.workspace = true
dot.workspace = true
cfg.workspace = true
opt.workspace = true
pcode-lift.workspace = true
pattern.workspace = true
rustc-hash = { workspace = true }
reader.workspace = true
object.workspace = true
expect-test.workspace = true
paste.workspace = true
```
After (only the `strider-error` and `thiserror` lines change):
```toml
[dependencies]
rsleigh.workspace = true
anyhow.workspace = true
target.workspace = true
ir.workspace = true
dot.workspace = true
cfg.workspace = true
opt.workspace = true
pcode-lift.workspace = true
pattern.workspace = true
rustc-hash = { workspace = true }
reader.workspace = true
object.workspace = true
expect-test.workspace = true
paste.workspace = true
```

- [ ] **Step 2: Apply Template E to `crates/strider/src/error.rs`**

Replace the entire file with:

```rust
//! Error type for the `strider` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/strider/src/lib.rs` re-exports**

Search for `pub use error::` (or any `Error` / `ErrorKind` re-export). Reduce to `pub use error::Result;` only.

- [ ] **Step 4: Add `.with_context(...)` at the lift boundary**

In `crates/strider/src/`, find `Strider::build` (the public entry point that produces `BuiltFunctionGraph`). Add `use anyhow::Context;` at the top, then wrap top-level fallible work with context. Example:

```rust
// before
self.lift_function(addr)?
// after (illustrative — adapt to actual method names in the file)
self.lift_function(addr).with_context(|| format!("lifting function at {addr:#x}"))?
```

This is illustrative — the engineer reading this task will read `Strider::build` and decide which concrete sites benefit from context wrapping. At minimum, every public-API entry point that takes an address or binary parameter should have one wrap.

- [ ] **Step 5: Sweep construction sites in `crates/strider/src/`**

Run: `grep -rn "ErrorKind::" crates/strider/src`
Expected: 54 hits. Apply Template S. Sample transformations covering the major variant shapes:

```rust
// CfgNoRegion (S1)
return Err(ErrorKind::CfgNoRegion(rid).into());
→
bail!("no region {rid:?} in cfg");

// NoRegisterContainer (S2)
Err(ErrorKind::NoRegisterContainer(vn).into())
→
Err(anyhow!("register {vn:?} has no enclosing container in variable set"))

// MissingOutputVn (S1)
return Err(ErrorKind::MissingOutputVn(opcode).into());
→
bail!("instruction has no output varnode for opcode {opcode:?}");

// SubpieceOffsetOutOfRange named-fields (S2)
Err(ErrorKind::SubpieceOffsetOutOfRange { opcode, byte_offset, input_size }.into())
→
Err(anyhow!("Subpiece byte_offset {byte_offset} out of range for input size {input_size} (opcode {opcode:?})"))

// AssertionFailed (S2)
Err(ErrorKind::AssertionFailed(msg).into())
→
Err(anyhow!("assertion failed: {msg}"))

// WrongNodeKind named-fields (S2)
Err(ErrorKind::WrongNodeKind { node, expected }.into())
→
Err(anyhow!("node {node:?} does not have expected kind {expected}"))

// Unimplemented (S1)
return Err(ErrorKind::Unimplemented(desc).into());
→
bail!("not yet implemented: {desc}");

// IndirectResolutionDidNotConverge (S1)
return Err(ErrorKind::IndirectResolutionDidNotConverge(n).into());
→
bail!("indirect-branch resolver did not converge after {n} iterations");

// SwitchHasNoTargets (S1)
return Err(ErrorKind::SwitchHasNoTargets(rid).into());
→
bail!("switch terminator at region {rid:?} has no targets");

// SwitchTargetMissing (S1)
return Err(ErrorKind::SwitchTargetMissing(addr).into());
→
bail!("switch target machine address {addr:#x} has no CFG region");

// CfgError / IrError / OptError / TargetError / PcodeLiftError / PatternError —
// All bridge-driven via ?. After conversion, every ? site continues to work
// via anyhow's blanket From<E: Error>. No edit needed at the ? sites.
```

- [ ] **Step 6: Sweep tests in `crates/strider/`**

Run: `grep -rn "ErrorKind::" crates/strider 2>/dev/null | grep -v "//"`
Apply Template S to test sites. Test functions returning `Result<(), strider::Error>` change to `anyhow::Result<()>`.

- [ ] **Step 7: Update `examples/strider.rs` if needed**

Run: `grep -n "ErrorKind\|strider::Error" examples/strider.rs`
If hits exist, apply Template S. Most likely the example uses `?` and a simple `Result<(), Box<dyn Error>>` or `anyhow::Result<()>` — no change needed in that case.

- [ ] **Step 8: Run Template T**

```bash
cargo test --package strider
cargo clippy --package strider --all-targets
cargo run --example strider
```
Expected: all tests pass, clippy clean, the example produces `cfg.html` and `graph.html`.

- [ ] **Step 9: Apply Template G with summary line `strider: drop ErrorKind + 6 bridges, return anyhow::Result`**

---

## Task 4: Convert `cfg` (44 sites, special: 5 cross-crate bridges go away)

**Files:**
- Modify: `crates/cfg/Cargo.toml`
- Modify: `crates/cfg/src/error.rs`
- Modify: `crates/cfg/src/lib.rs` (re-exports)
- Sweep: every file in `crates/cfg/src/` with an `ErrorKind::` reference (44 sites)

The crate has 17 own variants plus `IrError`, `OptError`, `PcodeLiftError`, `SleighError`, `FormatError` (all `#[from]` and bridge-driven). All bridges delete; `?` continues working via anyhow's blanket `From`.

- [ ] **Step 1: Apply Template C to `crates/cfg/Cargo.toml`**

Before:
```toml
[dependencies]
thiserror.workspace = true
petgraph.workspace = true
rsleigh.workspace = true
strider-error.workspace = true
dot.workspace = true
ir.workspace = true
opt.workspace = true
pcode-lift.workspace = true
target.workspace = true
```
After:
```toml
[dependencies]
anyhow.workspace = true
petgraph.workspace = true
rsleigh.workspace = true
dot.workspace = true
ir.workspace = true
opt.workspace = true
pcode-lift.workspace = true
target.workspace = true
```

- [ ] **Step 2: Apply Template E to `crates/cfg/src/error.rs`**

Replace the entire file with:

```rust
//! Error type for the `cfg` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/cfg/src/lib.rs` re-exports**

Reduce `pub use error::` lines to expose only `Result`.

- [ ] **Step 4: Add `.with_context(...)` at the cfg-build boundary**

In `crates/cfg/src/`, find `Cfg::build` (the public entry point). Add `use anyhow::Context;` at the top of the relevant file, then wrap the top-level work with `.context("building CFG")` or similar. Concretely, find the outermost fallible call inside `build` and wrap its result.

- [ ] **Step 5: Sweep construction sites in `crates/cfg/src/`**

Run: `grep -rn "ErrorKind::" crates/cfg/src`
Expected: 44 hits. Apply Template S. Sample transformations:

```rust
// EmptyRegion (S1)
return Err(ErrorKind::EmptyRegion(addr).into());
→
bail!("region at {addr:?} has no instructions");

// InvalidBranchTargetVaErr two args (S2)
Err(ErrorKind::InvalidBranchTargetVaErr(vn, addr).into())
→
Err(anyhow!("invalid branch target variable {vn:?} at opcode {addr:?}"))

// MissingBranchTarget (S1)
return Err(ErrorKind::MissingBranchTarget(addr).into());
→
bail!("branch instruction at {addr:?} has no target operand");

// FailedSplitingRegion two args (S2)
Err(ErrorKind::FailedSplitingRegion(idx, addr).into())
→
Err(anyhow!("failed spliting region {idx:?} into 2 parts at {addr:?}"))

// UnresolvedIndirectBranch (S1)
return Err(ErrorKind::UnresolvedIndirectBranch(addr).into());
→
bail!("branch-indirect at {addr:?} could not be statically resolved");

// IrError / OptError / PcodeLiftError / SleighError / FormatError —
// All #[from]-driven via ?. After conversion, ? lifts via anyhow's
// blanket From — no code change at the ? sites.
```

- [ ] **Step 6: Sweep tests in `crates/cfg/`**

Run: `grep -rn "ErrorKind::" crates/cfg 2>/dev/null | grep -v "//"`
Apply Template S to test sites.

- [ ] **Step 7: Run Template T**

```bash
cargo test --package cfg
cargo clippy --package cfg --all-targets
```

- [ ] **Step 8: Apply Template G with summary line `cfg: drop ErrorKind + 5 bridges, return anyhow::Result`**

---

## Task 5: Convert `opt` (22 sites, special: 2 bridges + ValidationErrors hand-impl go away)

**Files:**
- Modify: `crates/opt/Cargo.toml`
- Modify: `crates/opt/src/error.rs`
- Modify: `crates/opt/src/lib.rs` (re-exports)
- Modify: `crates/opt/src/pipeline.rs` (`FixedPointLimitExceeded` site + `validate::validate` wrap point)
- Modify: `crates/opt/src/constant_fold/eval_int.rs` (its 2 construction sites)
- Modify: `crates/opt/src/constant_fold/rules.rs` (`pattern::Error::rewrite_closure` call sites — see step 5)
- Sweep: every other file in `crates/opt/src/` with an `ErrorKind::` reference (22 total)

The crate has 9 own variants plus `IrError`, `PatternError` and a hand-written `From<ir::ValidationErrors> for Error`. All three delete. The `pattern::Error::rewrite_closure` call sites need handling — see step 5.

- [ ] **Step 1: Apply Template C to `crates/opt/Cargo.toml`**

Before:
```toml
[dependencies]
ir.workspace = true
pattern.workspace = true
reader.workspace = true
rsleigh.workspace = true
target.workspace = true
strider-error.workspace = true
thiserror = { workspace = true }
ethnum.workspace = true
rustc-hash = { workspace = true }
```
After:
```toml
[dependencies]
ir.workspace = true
pattern.workspace = true
reader.workspace = true
rsleigh.workspace = true
target.workspace = true
anyhow.workspace = true
ethnum.workspace = true
rustc-hash = { workspace = true }
```

- [ ] **Step 2: Apply Template E to `crates/opt/src/error.rs`**

Replace the entire file with:

```rust
//! Error type for the `opt` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/opt/src/lib.rs` re-exports**

Reduce to `pub use error::Result;`.

- [ ] **Step 4: Add `.with_context(...)` at the pipeline boundary**

In `crates/opt/src/pipeline.rs`, find each pass invocation inside `OptimizerPipeline::run`. Add `use anyhow::Context;` at the top of the file, then wrap pass invocations:

```rust
// before
pass.run(graph)?;
// after
pass.run(graph).with_context(|| format!("running pass {}", pass.name()))?;
```

If `Pass` has no `name()` method, use `std::any::type_name_of_val(pass)` instead. The validator call (`ir::validate::validate(...)?` at the end of `OptimizerPipeline::run`) gets a similar wrap:

```rust
ir::validate::validate(&graph, entry).context("post-pipeline IR validation failed")?;
```

- [ ] **Step 5: Patch `crates/opt/src/constant_fold/rules.rs` to keep the build green during the pattern-API transition**

This file has two `pattern::Error::rewrite_closure` call sites (lines ~439 and ~481) and several `pattern::Error::skip` / `pattern::Error::skip()` references. The `pattern::Error::skip` references **stay unchanged** in this task — pattern's API is still typed here; Task 6 step 9 will switch them to the new `pattern::skip` free function.

The `rewrite_closure` sites need an intermediate workaround. They previously took `opt::ErrorKind` (a typed error) and wrapped it for surfacing through pattern's rewrite engine. After this task, `opt::ErrorKind` is gone and `eval_int_cmp` returns `anyhow::Error`. But `pattern::Error::rewrite_closure` still requires `E: std::error::Error + Send + Sync + 'static`, and `anyhow::Error` does **not** implement `std::error::Error`. The fix is a temporary `std::io::Error::other` shim that wraps the formatted message into a typed error:

```rust
// line ~439 — before
eval_int_cmp(op, l, r, input_ty).map_err(pattern::Error::rewrite_closure)?
// line ~439 — after (intermediate; cleaned up in Task 6 step 9)
eval_int_cmp(op, l, r, input_ty)
    .map_err(|e| pattern::Error::rewrite_closure(std::io::Error::other(format!("{e}"))))?

// line ~481 — before
.ok_or_else(|| {
    pattern::Error::rewrite_closure(ErrorKind::ExpectedIntegerType(input_ty))
})?
// line ~481 — after (intermediate; cleaned up in Task 6 step 9)
.ok_or_else(|| {
    pattern::Error::rewrite_closure(std::io::Error::other(format!(
        "expected integer type, got {input_ty:?}"
    )))
})?
```

This intermediate is ugly by design — it exists for exactly one task. Task 6 step 9 removes the `std::io::Error::other` shim and the `pattern::Error::rewrite_closure` wrap, leaving the natural anyhow form (`?` for the eval_int_cmp site, `anyhow!()` for the ok_or_else site).

- [ ] **Step 6: Sweep remaining construction sites in `crates/opt/src/`**

Run: `grep -rn "ErrorKind::" crates/opt/src`
Expected: 22 hits including the ones in step 5 (which used opt's own `ErrorKind::ExpectedIntegerType`). Apply Template S. Sample transformations:

```rust
// FixedPointLimitExceeded (S1, in pipeline.rs:225)
return Err(crate::error::ErrorKind::FixedPointLimitExceeded(MAX_ITERS).into());
→
bail!("optimizer pipeline did not converge after {MAX_ITERS} iterations");

// NoReturnNode (S3)
.ok_or(ErrorKind::NoReturnNode)?
→
.ok_or_else(|| anyhow!("no return node found in function"))?

// ExpectedNodeNotFound (S4)
.ok_or_else(|| ErrorKind::ExpectedNodeNotFound("Call", NodeKind::Call).into())
→
.ok_or_else(|| anyhow!("expected Call node, got {:?}", NodeKind::Call))

// ExpectedIntegerType (S4) — outside the rules.rs site already handled in step 5
.ok_or_else(|| ErrorKind::ExpectedIntegerType(ty).into())
→
.ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))

// AssertionFailed (S1)
return Err(ErrorKind::AssertionFailed("expected FloatConst result".into()).into());
→
bail!("assertion failed: expected FloatConst result");

// IrError / PatternError construction sites — none. These were always
// constructed via ? autoconversion through bridge_error!, not at user code.
// The hand-written From<ir::ValidationErrors> for Error impl in error.rs
// deletes with the file; ? on validate() output now lifts ValidationErrors
// directly via anyhow's blanket From.
```

- [ ] **Step 7: Sweep tests in `crates/opt/src/*/tests.rs` and `crates/opt/tests/`**

Run: `grep -rn "ErrorKind::" crates/opt 2>/dev/null | grep -v "//"`
Apply Template S to test sites. Test functions returning `Result<(), opt::Error>` change to `anyhow::Result<()>`.

- [ ] **Step 8: Run Template T**

```bash
cargo test --package opt
cargo clippy --package opt --all-targets
```

- [ ] **Step 9: Apply Template G with summary line `opt: drop ErrorKind + 2 bridges + ValidationErrors hand-impl, return anyhow::Result`**

---

## Task 6: Convert `pattern` (10 sites, special: keep RewriteSkip/NotBuildable/MissingBinding as typed structs; drop rewrite_closure)

**Files:**
- Modify: `crates/pattern/Cargo.toml`
- Modify: `crates/pattern/src/error.rs` (full rewrite — keep three sentinel structs, drop the wrapper + rewrite_closure)
- Modify: `crates/pattern/src/rewrite.rs` (the `e.is_skip()` site at line ~72)
- Modify: `crates/pattern/src/pat/node_pat.rs` (lines ~310, ~330)
- Modify: `crates/pattern/src/pat/traits.rs` (line ~98)
- Modify: `crates/pattern/src/pat/any.rs`, `consts.rs`, `variant_agnostic.rs`, `mod.rs` (the `MissingBinding` construction sites)
- Modify: `crates/pattern/src/lib.rs` (re-exports)
- Modify: `crates/pattern/tests/matching/rewrite.rs` (3 `NotBuildable` matches, 1 `MissingBinding` match, 1 `RewriteClosure` match)
- Modify: `crates/opt/src/constant_fold/rules.rs` (the call sites prepared in Task 5 step 5 — switch to free functions)

The crate has 6 `ErrorKind` variants. Three (`RewriteSkip`, `NotBuildable`, `MissingBinding`) are observed by tests via `matches!(err.kind(), ErrorKind::X(_))` and need typed-struct preservation for downcast-based assertions. Two (`AssertionFailed`, `RewriteClosure`) can become `anyhow!()` strings — `RewriteClosure` is dropped because anyhow's blanket `From<E: Error>` already wraps any closure error transparently. One (`IrError`) is bridge-driven and disappears with the bridge.

- [ ] **Step 1: Apply Template C to `crates/pattern/Cargo.toml` (keep thiserror)**

Before:
```toml
[dependencies]
ir.workspace = true
rsleigh.workspace = true
strider-error.workspace = true
thiserror.workspace = true
ethnum.workspace = true
bitflags = "2"
```
After (pattern keeps thiserror for the three sentinel structs):
```toml
[dependencies]
ir.workspace = true
rsleigh.workspace = true
anyhow.workspace = true
thiserror.workspace = true
ethnum.workspace = true
bitflags = "2"
```

- [ ] **Step 2: Replace `crates/pattern/src/error.rs`**

Replace the entire file with:

```rust
//! Error types for the `pattern` crate.
//!
//! Most fallible operations return [`Result`] (= [`anyhow::Result<T>`]).
//! Three named sentinel structs preserve test-asserted public API:
//!
//! - [`RewriteSkip`] — opts a rewrite rule out without surfacing a hard
//!   error. The `rewrite_rule` interpreter detects this via [`is_skip`]
//!   and converts it back to "no change".
//! - [`NotBuildable`] — pattern is match-only (wildcards, guards, control
//!   patterns) and has no build semantics. Surfaced when a pattern that
//!   doesn't implement `try_build` appears on the RHS of a rewrite rule.
//! - [`MissingBinding`] — a capture variable referenced by a builder is
//!   not bound by the LHS match. Indicates a pattern-authoring bug.
//!
//! Tests downcast the propagated `anyhow::Error` to these structs to
//! assert which error path fired.

/// Sentinel produced by [`skip`]; detected by the `rewrite_rule`
/// interpreter via [`is_skip`] and converted to "no change".
#[derive(Debug, thiserror::Error)]
#[error("rewrite rule opted to skip")]
pub struct RewriteSkip;

/// Returned by `Pattern::try_build` defaults for patterns that have no
/// build semantics (wildcards, guards, control patterns).
#[derive(Debug, thiserror::Error)]
#[error("pattern {0} is not buildable (match-only)")]
pub struct NotBuildable(pub &'static str);

/// A capture variable referenced by a builder is not bound by the LHS
/// match. Carries the capture **kind name** (e.g. `"IntVar"`) so the
/// site of the bug is obvious from the error message.
#[derive(Debug, thiserror::Error)]
#[error("missing binding for capture of kind {0}")]
pub struct MissingBinding(pub &'static str);

/// Returns an [`anyhow::Error`] carrying the [`RewriteSkip`] sentinel.
/// The `rewrite_rule` interpreter converts this back to "no change"
/// rather than treating it as a hard failure.
#[track_caller]
#[must_use]
pub fn skip() -> anyhow::Error {
    anyhow::Error::new(RewriteSkip)
}

/// Returns `true` if `err` is the [`RewriteSkip`] sentinel.
#[must_use]
pub fn is_skip(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RewriteSkip>().is_some()
}

/// Convenience `Result` alias.
pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/pattern/src/lib.rs` re-exports**

Replace any existing `pub use error::{Error, ErrorKind, Result};` with:

```rust
pub use error::{is_skip, skip, MissingBinding, NotBuildable, Result, RewriteSkip};
```

- [ ] **Step 4: Update `crates/pattern/src/rewrite.rs:72`**

Find the `e.is_skip()` site:

```rust
// before
Err(e) if e.is_skip() => return Ok(false),
// after
Err(e) if crate::error::is_skip(&e) => return Ok(false),
```

Drop any `use crate::error::Error;` import that's no longer used.

- [ ] **Step 5: Update `crates/pattern/src/pat/node_pat.rs` (lines ~310, ~330) and `crates/pattern/src/pat/traits.rs:98`**

```rust
// before
return Err(Error::not_buildable(std::any::type_name::<Self>()));
// after
return Err(anyhow::Error::new(crate::error::NotBuildable(std::any::type_name::<Self>())));
```

Drop the `use crate::error::Error;` imports.

- [ ] **Step 6: Update MissingBinding construction sites**

Files: `crates/pattern/src/pat/any.rs` (lines 41, 96), `crates/pattern/src/pat/ctor/consts.rs:126`, `crates/pattern/src/pat/ctor/variant_agnostic.rs` (lines 50, 78, 106), `crates/pattern/src/pat/mod.rs:93`.

Run: `grep -rn "ErrorKind::MissingBinding" crates/pattern/src`
Expected: 7 hits. Apply this transformation to each:

```rust
// before (S3)
.ok_or(ErrorKind::MissingBinding("Var"))?
// after
.ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding("Var")))?

// before (S4)
.ok_or_else(|| ErrorKind::MissingBinding($missing).into())?
// after
.ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding($missing)))?
```

Note: this preserves the typed struct so tests can downcast. Don't replace with `anyhow!("missing binding...")` — that would break the `MissingBinding(n) if n == "IntVar"` test assertion.

- [ ] **Step 7: Sweep remaining construction sites in `crates/pattern/src/`**

Run: `grep -rn "ErrorKind::" crates/pattern/src`
Expected: ~3 remaining hits (after steps 5-6). Apply Template S for `AssertionFailed`:

```rust
// AssertionFailed (S2)
Err(ErrorKind::AssertionFailed(msg).into())
→
Err(anyhow!("assertion failed: {msg}"))
```

`IrError` had no explicit construction sites (always `?`-driven through the bridge); the bridge deletes with `error.rs` and `?` continues working via anyhow's blanket `From`.

- [ ] **Step 8: Update `crates/pattern/tests/matching/rewrite.rs`**

Three test changes:

```rust
// rhs_wildcard_is_not_buildable / rhs_predicate_is_not_buildable / rhs_control_pattern_is_not_buildable
// before
assert!(matches!(err.kind(), ErrorKind::NotBuildable(_)));
// after
assert!(err.downcast_ref::<pattern::NotBuildable>().is_some(), "expected NotBuildable, got {err:?}");
```

```rust
// rhs_missing_binding (the test using MissingBinding)
// before
let kind = err.kind();
assert!(
    matches!(kind, ErrorKind::MissingBinding(n) if n == &"IntVar"),
    "expected MissingBinding(\"IntVar\"), got {:?}", kind
);
// after
let mb = err.downcast_ref::<pattern::MissingBinding>();
assert!(
    matches!(mb, Some(pattern::MissingBinding("IntVar"))),
    "expected MissingBinding(\"IntVar\"), got {err:?}"
);
```

```rust
// rhs_closure_error_wraps_as_rewrite_closure (line ~281)
// before
let res: pattern::Result<u128> = Err(pattern::Error::rewrite_closure(CustomErr));
res?
// ...
assert!(matches!(err.kind(), ErrorKind::RewriteClosure(_)));
// after
let res: pattern::Result<u128> = Err(anyhow::Error::new(CustomErr));
res?
// ...
// CustomErr propagates through anyhow directly. Test renamed for clarity.
assert!(err.downcast_ref::<CustomErr>().is_some(), "expected CustomErr, got {err:?}");
```

Rename the test to `rhs_closure_error_propagates_through_anyhow` (or similar — the old name `wraps_as_rewrite_closure` no longer reflects the behavior). Drop any imports of `ErrorKind` and `pattern::Error` that become unused.

Also drop the file's module-level doc comment line 3 reference: change `(NotBuildable, MissingBinding, RewriteSkip, RewriteClosure)` to `(NotBuildable, MissingBinding, RewriteSkip)`.

- [ ] **Step 9: Clean up `crates/opt/src/constant_fold/rules.rs`**

Task 5 step 5 used `std::io::Error::other(format!(...))` workarounds at the `pattern::Error::rewrite_closure` sites. Now that `pattern` no longer has `rewrite_closure`, simplify those sites (and update `pattern::Error::skip` to the new free function form):

```rust
// line ~437 before
let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
// after
let input_ty = in_ty.ok_or_else(pattern::skip)?;

// line ~439 before (the workaround from Task 5)
eval_int_cmp(op, l, r, input_ty)
    .map_err(|e| pattern::Error::rewrite_closure(std::io::Error::other(format!("{e}"))))?
// after — anyhow's identity From handles ? on anyhow::Error directly
eval_int_cmp(op, l, r, input_ty)?

// line ~455 before
ty.get_unsigned_int_u128(v).ok_or_else(pattern::Error::skip)?
// after
ty.get_unsigned_int_u128(v).ok_or_else(pattern::skip)?

// line ~477 before
let input_ty = in_ty.ok_or_else(pattern::Error::skip)?;
// after
let input_ty = in_ty.ok_or_else(pattern::skip)?;

// line ~481 before (the workaround from Task 5)
.ok_or_else(|| {
    pattern::Error::rewrite_closure(std::io::Error::other(format!(
        "expected integer type, got {input_ty:?}"
    )))
})?
// after
.ok_or_else(|| anyhow!("expected integer type, got {input_ty:?}"))?

// line ~485 / ~496 / ~499 / ~514 / ~517 — same .ok_or_else(pattern::Error::skip) → .ok_or_else(pattern::skip)

// line ~524 before
return Err(pattern::Error::skip());
// after
return Err(pattern::skip());
```

- [ ] **Step 10: Run Template T**

```bash
cargo test --package pattern
cargo test --package opt
cargo clippy --package pattern --all-targets
cargo clippy --package opt --all-targets
```

(Run opt's tests too because pattern's dev-deps include opt; opt's call-site updates need to verify against pattern's new API.)

- [ ] **Step 11: Apply Template G with summary line `pattern: drop ErrorKind + RewriteClosure wrapper, expose skip/is_skip + RewriteSkip/NotBuildable/MissingBinding structs`**

---

## Task 7: Convert `pcode-lift` (16 sites, special: 1 bridge to ir goes away)

**Files:**
- Modify: `crates/pcode-lift/Cargo.toml`
- Modify: `crates/pcode-lift/src/error.rs`
- Modify: `crates/pcode-lift/src/lib.rs` (re-exports)
- Sweep: every file in `crates/pcode-lift/src/` with an `ErrorKind::` reference (16 sites)

The crate has 11 own variants plus `IrError(ir::ErrorKind)` and `SleighError(rsleigh::error::BaseError)`. Both delete; `?` continues working via anyhow's blanket `From`.

- [ ] **Step 1: Apply Template C to `crates/pcode-lift/Cargo.toml`**

Before:
```toml
[dependencies]
rsleigh.workspace = true
ir.workspace = true
target.workspace = true
strider-error.workspace = true
thiserror.workspace = true
```
After:
```toml
[dependencies]
rsleigh.workspace = true
ir.workspace = true
target.workspace = true
anyhow.workspace = true
```

- [ ] **Step 2: Apply Template E to `crates/pcode-lift/src/error.rs`**

Replace the entire file with:

```rust
//! Error type for the `pcode-lift` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/pcode-lift/src/lib.rs` re-exports**

Reduce to `pub use error::Result;`.

- [ ] **Step 4: Sweep construction sites in `crates/pcode-lift/src/`**

Run: `grep -rn "ErrorKind::" crates/pcode-lift/src`
Expected: 16 hits. Apply Template S. Sample transformations:

```rust
// MissingOutputVn (S1)
return Err(ErrorKind::MissingOutputVn(opcode).into());
→
bail!("instruction has no output varnode for opcode {opcode:?}");

// SubpieceOffsetOutOfRange named-fields (S2)
Err(ErrorKind::SubpieceOffsetOutOfRange { opcode, byte_offset, input_size }.into())
→
Err(anyhow!("Subpiece byte_offset {byte_offset} out of range for input size {input_size} (opcode {opcode:?})"))

// WriteToConstSpace (S1)
return Err(ErrorKind::WriteToConstSpace(space).into());
→
bail!("attempted to write to CONST space: {space:?}");

// IrError / SleighError construction — none at user-code sites; both
// bridge/from-driven through ?. The bridge_error! and #[from] both delete
// with the file; ? continues via anyhow's blanket From.
```

- [ ] **Step 5: Run Template T**

```bash
cargo test --package pcode-lift
cargo clippy --package pcode-lift --all-targets
```

- [ ] **Step 6: Apply Template G with summary line `pcode-lift: drop ErrorKind + bridge to ir, return anyhow::Result`**

---

## Task 8: Convert `ir` (164 sites, special: keep `ValidationErrors` as `thiserror`)

**Files:**
- Modify: `crates/ir/Cargo.toml`
- Modify: `crates/ir/src/error.rs`
- Modify: `crates/ir/src/lib.rs` (re-exports — keep `ValidationErrors` exposed)
- Verify: `crates/ir/src/validate/mod.rs` (`ValidationErrors` impls `std::error::Error`)
- Sweep: every file in `crates/ir/src/` with an `ErrorKind::` reference (164 sites — the largest sweep in the migration)

The `ir` crate has 25 variants. After this task only `ValidationErrors` survives as a `thiserror`-derived type. All other variants disappear into `anyhow!` / `bail!` strings.

- [ ] **Step 1: Verify `ValidationErrors` already implements `std::error::Error`**

Read `crates/ir/src/validate/mod.rs`. Search for `ValidationErrors`. Confirm it derives or hand-implements `std::error::Error`. If it does (almost certainly via `thiserror::Error` since the crate uses `thiserror`), proceed. If it does not, add `#[derive(Debug, thiserror::Error)]` plus an `#[error("...")]` formatter to it before continuing — anyhow's `?` lift requires `Error + Send + Sync + 'static`.

- [ ] **Step 2: Apply Template C to `crates/ir/Cargo.toml` (keep thiserror)**

Before:
```toml
[dependencies]
strider-error.workspace = true
thiserror.workspace = true
```
After (ir keeps thiserror for ValidationErrors):
```toml
[dependencies]
anyhow.workspace = true
thiserror.workspace = true
```

- [ ] **Step 3: Replace `crates/ir/src/error.rs`**

Replace the entire file content with:

```rust
//! Error type for the `ir` crate.
//!
//! Most fallible operations return `anyhow::Result<T>`. The exception is
//! [`crate::validate::ValidationErrors`], which remains a `thiserror`-derived
//! aggregate so callers can `downcast_ref::<ValidationErrors>()` the
//! anyhow-wrapped error and inspect individual validation failures.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 4: Update `crates/ir/src/lib.rs` re-exports**

Search `crates/ir/src/lib.rs` for `pub use error::` and `pub use validate::`. The `Error`/`ErrorKind` re-exports go away; the `ValidationErrors` re-export stays. Final shape:

```rust
pub use error::Result;
pub use validate::ValidationErrors;
```

(Adjust to whatever path-shape the file already uses.)

- [ ] **Step 5: Sweep construction sites in `crates/ir/src/`**

Run: `grep -rn "ErrorKind::" crates/ir/src | wc -l`
Expected: 164. Apply Template S to every site. Sample transformations spanning the variant catalog:

```rust
// InvalidNumberOfParams (S2)
Err(ErrorKind::InvalidNumberOfParams(params, expected_count).into())
→
Err(anyhow!("expected {expected_count:?} params and got {params:?}"))

// NoCurrentRegion (S3)
.ok_or(ErrorKind::NoCurrentRegion)?
→
.ok_or_else(|| anyhow!("no current region is set"))?

// ExpectedControl (S1)
return Err(ErrorKind::ExpectedControl(output, kind).into());
→
bail!("output {output:?} is not a control edge (got {kind:?})");

// InputIndexOutOfBounds named-fields (S2)
Err(ErrorKind::InputIndexOutOfBounds { node, index, len }.into())
→
Err(anyhow!("input index {index} out of bounds for node {node:?} (len={len})"))

// AssertionFailed (S2)
Err(ErrorKind::AssertionFailed(msg).into())
→
Err(anyhow!("assertion failed: {msg}"))

// ExpectedValueOutput (in node/output_kind.rs)
Err(crate::ErrorKind::ExpectedValueOutput(self).into())
→
Err(anyhow!("expected value output, got {self:?}"))

// ValidationFailed — DELETED. ValidationErrors lifts directly via ? on
// anyhow's blanket From<E: Error>. The hand-written From<ValidationErrors>
// for Error impl in error.rs is removed; `validate::validate(...)?`
// continues to work, with the wrapped error downcast-able via
// err.downcast_ref::<ir::ValidationErrors>().
```

- [ ] **Step 6: Sweep test files in `crates/ir/`**

Run: `grep -rn "ErrorKind::" crates/ir 2>/dev/null | grep -v "//"`
Apply Template S to any test sites. Tests that returned `Result<(), ir::Error>` change to `anyhow::Result<()>`. Tests that asserted on specific `ErrorKind` variants (search for `matches!(...ErrorKind::`) need downcast updates — but ir's variants typically aren't asserted on (verify with grep).

- [ ] **Step 7: Run Template T**

```bash
cargo test --package ir
cargo clippy --package ir --all-targets
```
Expected: all tests pass. The `ValidationErrors`-bearing tests (validator suites) must still produce the expected error structure when downcast.

- [ ] **Step 8: Apply Template G with summary line `ir: drop ErrorKind, keep ValidationErrors as thiserror, return anyhow::Result`**

---

## Task 9: Convert `dot` (6 sites, special: drop generic Error<E>)

**Files:**
- Modify: `crates/dot/Cargo.toml`
- Modify: `crates/dot/src/error.rs` (entire hand-written generic wrapper deletes)
- Modify: `crates/dot/src/lib.rs` (re-exports + `GraphDotDumper::Error` bound + every `Result<_, G::Error>` signature)
- Delete: `crates/dot/tests/error.rs` (mirrors `strider-error`'s macro_contract test, becomes irrelevant)

The crate has three variants: `SvgConversionError(String)`, `IoError(std::io::Error)`, `DotDumpError(E)`. The generic-over-`E` wrapper goes away. Every `Result<String, G::Error>` signature becomes `anyhow::Result<String>`.

- [ ] **Step 1: Tighten `GraphDotDumper::Error` bound in `crates/dot/src/lib.rs`**

Find the trait at `crates/dot/src/lib.rs:50`. Change the associated type bound:

```rust
// before
pub trait GraphDotDumper {
    type Error: Debug;
    // ...
}
// after
pub trait GraphDotDumper {
    type Error: std::error::Error + Send + Sync + 'static;
    // ...
}
```

- [ ] **Step 2: Apply Template C to `crates/dot/Cargo.toml`**

Before:
```toml
[dependencies]
strider-error.workspace = true
thiserror.workspace = true
```
After:
```toml
[dependencies]
anyhow.workspace = true
```

- [ ] **Step 3: Replace `crates/dot/src/error.rs`**

Replace the entire file with:

```rust
//! Error type for the `dot` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 4: Update `crates/dot/src/lib.rs` re-exports**

Find the line `pub use error::{Error, ErrorKind, Result};` (around line 44). Change to:

```rust
pub use error::Result;
```

- [ ] **Step 5: Convert every method signature in `GraphDot<G>` impl**

In `crates/dot/src/lib.rs`, every method that returned `Result<T, G::Error>` becomes `anyhow::Result<T>`. The methods (around lines 336-460):

- `build_dot(&self) -> anyhow::Result<String>`
- `as_dot(&self) -> anyhow::Result<String>`
- `as_svg(&self) -> anyhow::Result<String>`
- `as_html_from_svg(&self) -> anyhow::Result<String>`
- `as_html_from_dot(&self) -> anyhow::Result<String>`
- `dump_as_html(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()>`
- `dump_as_dot(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()>`

- [ ] **Step 6: Sweep construction sites in `crates/dot/src/`**

Run: `grep -rn "ErrorKind::\|Error::from" crates/dot/src`
Expected: 6 hits. Apply Template S, plus these specific transformations:

```rust
// before (line ~342)
.map_err(|e| Error::from(ErrorKind::DotDumpError(e)))?
// after — anyhow's blanket From<E: Error + Send + Sync + 'static> handles G::Error
.map_err(anyhow::Error::new)?

// before (line ~370)
let mk_err = |msg: String| -> Error<G::Error> { ErrorKind::SvgConversionError(msg).into() };
// after
let mk_err = |msg: String| -> anyhow::Error { anyhow::anyhow!("svg conversion error {msg:?}") };
```

The `IoError` variant's `#[from]` auto-conversion: every `?` on `std::io::Error` keeps working via anyhow's blanket impl.

- [ ] **Step 7: Delete `crates/dot/tests/error.rs`**

```bash
git rm crates/dot/tests/error.rs
```

This file mirrors `strider-error/tests/macro_contract.rs` and tests the now-deleted hand-written `Error<E>` shape. With the generic gone, the test has no subject.

- [ ] **Step 8: Run Template T**

```bash
cargo test --package dot
cargo clippy --package dot --all-targets
```

- [ ] **Step 9: Apply Template G with summary line `dot: drop generic Error<E>, return anyhow::Result everywhere`**

---

## Task 10: Convert `target` (5 sites)

**Files:**
- Modify: `crates/target/Cargo.toml`
- Modify: `crates/target/src/error.rs`
- Modify: `crates/target/src/lib.rs` (re-exports)
- Sweep: every file in `crates/target/src/` with an `ErrorKind::` reference

The crate has one variant: `UnknownRegName(String)`.

- [ ] **Step 1: Apply Template C to `crates/target/Cargo.toml`**

Before:
```toml
[dependencies]
strider-error.workspace = true
thiserror.workspace = true
```
After:
```toml
[dependencies]
anyhow.workspace = true
```

- [ ] **Step 2: Apply Template E to `crates/target/src/error.rs`**

Replace the entire file content with:

```rust
//! Error type for the `target` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/target/src/lib.rs` re-exports**

Reduce to `pub use error::Result;`.

- [ ] **Step 4: Sweep construction sites in `crates/target/src/`**

Run: `grep -rn "ErrorKind::" crates/target/src`
Expected: 5 hits. Apply Template S:

```rust
// UnknownRegName (S1)
return Err(ErrorKind::UnknownRegName(name.to_string()).into());
→
bail!("unknown sleigh register name {name:?}");
```

- [ ] **Step 5: Run Template T**

```bash
cargo test --package target
cargo clippy --package target --all-targets
```

- [ ] **Step 6: Apply Template G with summary line `target: drop ErrorKind, use anyhow::Result`**

---

## Task 11: Convert `reader` (15 sites)

**Files:**
- Modify: `crates/reader/Cargo.toml`
- Modify: `crates/reader/src/error.rs`
- Modify: `crates/reader/src/lib.rs` (re-exports)
- Sweep: every file in `crates/reader/src/` with an `ErrorKind::` reference

The crate has four variants: `NotMapped(u64)`, `RegionOverflow { start_addr, len }`, `Io(std::io::Error)`, `Object(object::Error)`.

- [ ] **Step 1: Apply Template C to `crates/reader/Cargo.toml`**

Before:
```toml
[dependencies]
strider-error.workspace = true
thiserror.workspace = true
```
After:
```toml
[dependencies]
anyhow.workspace = true
```

- [ ] **Step 2: Apply Template E to `crates/reader/src/error.rs`**

Replace the entire file content with:

```rust
//! Error type for the `reader` crate.

pub type Result<T> = anyhow::Result<T>;
```

- [ ] **Step 3: Update `crates/reader/src/lib.rs` re-exports**

Reduce to `pub use error::Result;`.

- [ ] **Step 4: Sweep construction sites in `crates/reader/src/`**

Run: `grep -rn "ErrorKind::" crates/reader/src`
Expected: 15 hits. Apply Template S. Sample transformations:

```rust
// NotMapped (S1)
return Err(ErrorKind::NotMapped(addr).into());
→
bail!("address {addr:#x} is not mapped");

// RegionOverflow named-fields (S5)
.ok_or(ErrorKind::RegionOverflow { start_addr, len })?
→
.ok_or_else(|| anyhow!("region at {start_addr:#x} with length {len} would overflow u64"))?
```

For `Io` and `Object` variants: every `?` call site that previously relied on `#[from]` auto-conversion keeps working unchanged — anyhow lifts `std::io::Error` and `object::Error` automatically. Only explicit constructions (`Err(ErrorKind::Io(e).into())`) need rewriting; those become `Err(e.into())` or `bail!("...")` depending on context.

- [ ] **Step 5: Run Template T**

```bash
cargo test --package reader
cargo clippy --package reader --all-targets
```

- [ ] **Step 6: Apply Template G with summary line `reader: drop ErrorKind, use anyhow::Result`**

---

## Task 12: Delete `strider-error` crate

**Files:**
- Delete: `crates/strider-error/` (entire directory)
- Modify: `Cargo.toml` (workspace root) — remove the `strider-error` line from `[workspace.dependencies]`

By this point, every `crates/*/Cargo.toml` has had its `strider-error.workspace = true` line removed (Tasks 3-11). Verify and delete the crate.

- [ ] **Step 1: Verify no remaining references**

Run:
```bash
grep -rn "strider-error\|strider_error\|define_error\|bridge_error" crates Cargo.toml 2>/dev/null
```
Expected: zero hits in `crates/*` excluding `crates/strider-error/` itself, zero hits in workspace `Cargo.toml`. If any remain, fix them — those are leftovers from the per-crate tasks.

- [ ] **Step 2: Remove the workspace dep entry**

Edit `Cargo.toml`. Remove the line `strider-error = { path = "crates/strider-error" }`.

- [ ] **Step 3: Delete the crate directory**

```bash
git rm -r crates/strider-error
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: succeeds. (This is the first build that has no `strider-error` to compile.)

- [ ] **Step 5: Apply Template G**

```bash
git add -A
git commit -m "$(cat <<'EOF'
anyhow: delete strider-error crate

Removes the strider-error crate now that every per-crate Error type
has been converted to anyhow::Result<T>. Drops the workspace dependency
and the crate directory. The Traceback / LocationChain / format_traceback
machinery had zero downstream consumers (verified before the migration);
no replacement is needed at the Rust layer. The planned strider-py
PyO3 layer can capture per-? locations at the FFI boundary if it
later needs Python-traceback fidelity.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Final verification + PR

**Files:**
- None modified

This is the verification-before-completion gate. All five gates must pass before the migration is claimed complete.

- [ ] **Step 1: Workspace build with zero warnings**

```bash
cargo build --workspace 2>&1 | tee /tmp/build.log
grep -E "warning|error" /tmp/build.log
```
Expected: no `warning:` or `error:` lines in the build output.

- [ ] **Step 2: Workspace test suite green**

```bash
cargo test --workspace 2>&1 | tee /tmp/test.log
grep "test result:" /tmp/test.log
```
Expected: every `test result:` line says `ok`. Zero `FAILED`, zero `ignored` lines beyond the pre-migration baseline (Task 1 step 4).

- [ ] **Step 3: Workspace clippy clean**

```bash
cargo clippy --workspace --all-targets 2>&1 | tee /tmp/clippy.log
grep -E "warning|error" /tmp/clippy.log
```
Expected: empty.

- [ ] **Step 4: Integration smoke test**

```bash
cargo run --example strider
ls -lh cfg.html graph.html
```
Expected: `cfg.html` and `graph.html` exist and are non-empty (well over 10 KiB each).

- [ ] **Step 5: Confirm strider-error is fully gone**

```bash
grep -rn "strider-error\|strider_error\|define_error!\|bridge_error!\|format_traceback\|LocationChain\|Traceback\|ErrorFields" \
  crates Cargo.toml docs/superpowers/specs docs/superpowers/plans 2>/dev/null \
  | grep -v "docs/superpowers/specs/2026-04-29-replace-strider-error-with-anyhow-design.md" \
  | grep -v "docs/superpowers/plans/2026-04-29-replace-strider-error-with-anyhow.md"
```
Expected: empty. The only references to the migrated machinery should be in the spec and this plan (which document the migration itself).

- [ ] **Step 6: Push branch and open PR**

```bash
git push -u origin feature/anyhow-migration
gh pr create --title "Replace strider-error with anyhow" --body "$(cat <<'EOF'
## Summary

- Deletes the `strider-error` crate and every per-crate `Error`/`ErrorKind` pair (~700 LoC of macro machinery + per-crate boilerplate).
- Converts all `Result<T, crate::Error>` to `anyhow::Result<T>` across `target`, `reader`, `ir`, `dot`, `pcode-lift`, `pattern`, `opt`, `cfg`, `strider`.
- Construction sites use anyhow built-ins (`anyhow!`, `bail!`, `ensure!`); no replacement macros.
- `ir::ValidationErrors` stays as `thiserror`-derived (downcast-able from `anyhow::Error`); `pattern` keeps three small named structs (`RewriteSkip`, `NotBuildable`, `MissingBinding`) backing the `pattern::skip` / `is_skip` free functions and supporting test downcast assertions.
- `dot::Error<E>` (the hand-written generic wrapper) collapses; `GraphDotDumper::Error` bound tightens to `Error + Send + Sync + 'static`.
- Origin backtrace and source chain preserved via anyhow. Per-`?` location chain (Python-traceback feature) dropped — had zero downstream consumers; the planned `strider-py` layer can capture per-`?` locations at the FFI boundary if later needed.

Spec: [`docs/superpowers/specs/2026-04-29-replace-strider-error-with-anyhow-design.md`](docs/superpowers/specs/2026-04-29-replace-strider-error-with-anyhow-design.md)
Plan: [`docs/superpowers/plans/2026-04-29-replace-strider-error-with-anyhow.md`](docs/superpowers/plans/2026-04-29-replace-strider-error-with-anyhow.md)

## Test plan
- [x] `cargo build --workspace` clean
- [x] `cargo test --workspace` all green (per-crate test counts unchanged)
- [x] `cargo clippy --workspace --all-targets` clean
- [x] `cargo run --example strider` produces `cfg.html` and `graph.html`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 7: Invoke `feature-dev:code-reviewer` on the diff**

Per the project standing preference (memory: "every mission invokes ... feature-dev:code-reviewer (NOT coderabbit)"), run code review on the implementation against the spec before merging.

---

## Self-review checklist (run after writing this plan)

**1. Spec coverage:**
- [x] Strider-error deletion — Task 12
- [x] Per-crate Error/ErrorKind removal — Tasks 3-11 (one task per crate, top-down order)
- [x] Pure anyhow at construction sites — Template S applied per crate
- [x] No replacement macros — explicitly NOT introduced in any task
- [x] `ValidationErrors` preservation — Task 8 step 1
- [x] `pattern` skip/is_skip preservation as free functions; `RewriteSkip`/`NotBuildable`/`MissingBinding` as typed structs — Task 6 step 2
- [x] `dot::Error<E>` collapse + `GraphDotDumper::Error` bound — Task 9 steps 1-5
- [x] `with_context` at high-level boundaries — Tasks 3 (lift), 4 (cfg-build), 5 (pipeline)
- [x] Worktree creation — Task 1
- [x] Add `anyhow` to workspace deps — Task 2
- [x] Final verification gates (build, test, clippy, smoke test) — Task 13
- [x] PR creation + code review — Task 13 steps 6-7

**2. Placeholder scan:**
- No "TBD", "TODO", "implement later" — checked.
- No "appropriate error handling" / "handle edge cases" — every step shows the actual transformation.
- No "Similar to Task N" — Templates C/E/S/T/G are referenced by name, not by other-task copy-paste.
- Every code block contains the actual code an engineer types, with one exception: Task 5 step 5 uses an intermediate `std::io::Error::other` workaround that gets cleaned up in Task 6 step 9 — this is documented inline.

**3. Type consistency:**
- `pattern::skip()` returns `anyhow::Error`; `pattern::is_skip(&anyhow::Error) -> bool`; matches Task 6 step 2 and step 4.
- `RewriteSkip`/`NotBuildable`/`MissingBinding` are `thiserror`-derived structs, downcast-able from `anyhow::Error`; matches both step 2 (define) and steps 5-6 (consume) and step 8 (test downcast).
- `pattern::Error::rewrite_closure` and `pattern::Error::not_buildable` are dropped from the public API; Task 5 step 5 introduces a temporary workaround at opt's call sites, Task 6 step 9 cleans it up. The cleanup is internally consistent: Task 6 step 9 references the exact lines/forms introduced by Task 5 step 5.
- All per-crate `Result` aliases resolve to `anyhow::Result<T>`; consistent across Tasks 3-11.

**4. Build-greenness check (top-down ordering):**
- After each crate task commits, the workspace must `cargo build` cleanly. Top-down order ensures this:
  - Task 3 (strider): strider→anyhow. Upstream typed errors lift via blanket `From<E: Error>`. ✓
  - Task 4 (cfg): cfg→anyhow. strider's `?` on cfg's anyhow lifts via identity From. Other upstream typed errors continue to lift via blanket From. ✓
  - Tasks 5-11: same pattern — at every step, downstream is already on anyhow, upstream is either already anyhow (lifts via identity) or still typed (lifts via blanket).
  - Task 5→Task 6 boundary: Task 5 introduces an `std::io::Error::other` workaround so `pattern::Error::rewrite_closure(typed_err)` keeps compiling while pattern is still typed. Task 6 cleans up by removing the workaround as part of switching to the new pattern free functions.
  - Task 12 (delete strider-error): all per-crate dep entries already removed by Tasks 3-11; the crate directory deletes cleanly.
