---
name: strider-silent-failure-audit
description: Audit a strider crate for silent-failure patterns (`.ok()?`, `unwrap_or_default`, blanket `Err(_)` arms, `eprintln!` + `return None`) and convert them to typed errors that surface to the caller.
---

# strider-silent-failure-audit

## When to use

Triggers:
- "audit `crates/<X>/src/` for silent failures"
- "this `unwrap_or_default` looks suspicious"
- "an analysis silently returns no results — find the swallow"
- a code reviewer flags an `eprintln!` + `return None` / `return false` pattern
- round-9 R9-2C, R9-EA1, or R9-correctness-borrowing surfaced a "swallowed error" finding

## When NOT to use

- Truly best-effort code (e.g. dot rendering for visualisation) where a missing label is acceptable. Document the choice with an inline comment instead of trying to convert to `Result`.
- `Option<T>` returns where `None` is a legitimate "not found" semantic, not an error suppression. The bar: if `None` could conceal an upstream contract violation, surface as `Result`. If `None` is structurally normal, leave it.

## Patterns to flag

| Pattern | Typical fix |
|---|---|
| `let x = foo().ok()?;` | `let x = foo()?;` |
| `eprintln!("failed: {e}"); return None;` | `return Err(e.into());` (and propagate the return type to `Result<Option<_>>`) |
| `let _ = result;` (discarding `Result`) | `let _ = result?;` or `if let Err(e) = result { ... }` with explicit handling |
| `try_into().ok()?` for sizes | Surface as a typed `UnsupportedSize { vn, size }` error |
| Blanket `Err(_) => { e.print(py); false }` (PyO3) | Distinguish base exceptions (`KeyboardInterrupt`, `SystemExit`) and re-raise via `PyErr::restore` |
| `unwrap_or_default()` on a parser/lookup | If the default is silently wrong downstream, surface as `Err` |

## Procedure

1. Grep for the patterns:
   ```
   rg "\.ok\(\)\?|unwrap_or_default|eprintln!.*return (None|false)|let _ = .*\(\);" crates/<target>/src/
   ```
2. For each hit, read the surrounding 10 lines to determine: is the swallow a documented best-effort choice, or a real contract gap?
3. For real gaps:
   - Trace the caller chain. The fix usually requires propagating `Result` up through 1-3 callers.
   - Pick or define an error variant. Strider uses `anyhow::Error` for internal paths and typed exceptions for the PyO3 boundary. Reach for an existing variant (`StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`, `UnresolvedIndirectBranchError`, `UnknownCallOtherError`) before adding a new one.
   - Update tests to use `.expect("...")` (preserves test intent) or `assert!(result.is_err())` for the new error path.
4. For PyO3 (`crates/strider-py/src/`):
   - Boundary functions that wrap matcher walks must `PyErr::take(py)` after the walk and return `Err(taken)` if a Python exception was restored from inside a callback. Without this, the runtime raises `SystemError: returned a result with an exception set`.
   - Distinguish `KeyboardInterrupt` and `SystemExit` from ordinary predicate exceptions: `e.is_instance_of::<PyKeyboardInterrupt>(py)` → `e.restore(py)`; otherwise `e.print(py)`.
5. Verify with `cargo test --workspace --exclude strider-py` and `uv run pytest`. New tests should pin both the success path and the new error path.

## Anti-patterns

- Converting `Option` returns to `Result<Option<_>>` with an unused `Err` arm — adds boilerplate without benefit. Only convert when the swallow conceals a real contract gap.
- Adding a fresh error variant per call site — bloats `StriderError`. Group related conditions under one variant with a descriptive `String` payload.
- Returning `Result<()>` from a function that should also report a partial result — use `Result<MyOutput>` instead.

## Verification

- All failing tests now pass with the new error type, OR the test was updated to assert the error.
- `rg "\.ok\(\)\?|unwrap_or_default" crates/<target>/src/` shows no remaining suspicious hits.
- For PyO3 changes: `uv run pytest` passes; `with pytest.raises(strider.errors.X)` covers the new error.

## Round 9 examples

Fixed in this round (see commit messages for details):
- H-6: `classify_anchor*` family converted from `eprintln! + None` to `Result<Option<ResolvedTargets>>` so KB-merge contradictions surface.
- H-8: `wrap_when` now distinguishes `KeyboardInterrupt`/`SystemExit` and re-raises via `PyErr::restore`; `Graph::find_all` / `find_all_requirements` take pending exceptions via `PyErr::take(py)` after the walk.

Verified-but-deferred (see `reviews/round9-fix-verification.md`):
- H-4 `read_or_init_var`, H-5 `build_anchor_calling_context` clobber loop: `vn.size.try_into().ok()?` silent drop. No real CC has unsupported reg sizes; defensive Err-conversion would be a big API change for theoretical benefit.
- H-7 `wrap_when` `try_borrow` failure path: failure mode requires an active mutable borrow; `clear_graph_ptr` is `&self`, so essentially unreachable.
