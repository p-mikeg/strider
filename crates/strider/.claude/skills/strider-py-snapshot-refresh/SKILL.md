---
name: strider-py-snapshot-refresh
description: Refresh the strider-py public-API snapshot test (test_public_api_snapshot.py) when EXPECTED_TOP / EXPECTED_ERRORS / EXPECTED_OPT / EXPECTED_PATTERN drifts.
---

# strider-py-snapshot-refresh

## When to use

The snapshot test `crates/strider-py/tests/python/test_public_api_snapshot.py`
fails after a deliberate strider-py API change and the EXPECTED_*
sets need to be updated to match the new surface. Triggers include:

- "Snapshot test fails with `missing names: {...}`."
- "Snapshot test reports unexpected extras after I added a new
  pattern ctor / opt pass / error subclass."
- A binding refactor renamed or removed a public symbol.
- Adding a new pyclass / pyfunction surfaced in `strider`,
  `strider.errors`, `strider.opt`, or `strider.pattern`.

## When NOT to use

- The snapshot fail is real — i.e. you accidentally removed a public
  symbol. Restore the symbol; do NOT silently shrink the EXPECTED set.
- The fail is a side effect of a stale build (`uv run maturin develop`
  not run after a Rust edit). Rebuild first.
- The test passes already. Snapshot drift is only a problem when the
  test fails.

## Inputs the skill expects

- The set of new / removed / renamed symbols, named explicitly.
- The intent — addition (forward-compatible), removal (breaking), or
  rename (breaking).

## Procedure

1. Confirm the actual surface from a Python REPL after a fresh
   `uv run maturin develop`:
   ```
   uv run python -c "import strider; print(sorted(dir(strider)))"
   uv run python -c "import strider.errors; print(sorted(dir(strider.errors)))"
   uv run python -c "import strider.opt; print(sorted(dir(strider.opt)))"
   uv run python -c "import strider.pattern; print(sorted(dir(strider.pattern)))"
   ```
2. Diff each list against the corresponding EXPECTED_TOP /
   EXPECTED_ERRORS / EXPECTED_OPT / EXPECTED_PATTERN set in
   `crates/strider-py/tests/python/test_public_api_snapshot.py`. The
   test reports `missing` (in EXPECTED but not present — usually a
   real bug) and `extras` (present but not in EXPECTED — soft
   warning, usually a deliberate addition that needs the EXPECTED
   set updated).
3. Update the EXPECTED set to match the new intentional surface.
   - Additions: append the new symbol name. Group by category if the
     existing set is grouped (the `pattern` set clusters by builder
     family).
   - Removals: delete the symbol from the EXPECTED set ONLY if the
     removal is intentional. Confirm with the user / the diff.
   - Renames: remove the old name and add the new one as a single
     edit; flag the rename in the commit message so downstream
     consumers see it.
4. Re-run the snapshot test to confirm clean:
   `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py -v`.
5. Commit with an annotation classifying the change:
   `API addition: <names>` for forward-compatible additions,
   `API removal: <names>` for breaking removals,
   `API rename: <old> -> <new>` for renames. The annotation lives
   in the commit body so consumers reading `git log` know to update
   their downstream code.

## Verification

- `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py -v`
  — all four subtests (`top`, `errors`, `opt`, `pattern`) clean.
- `uv run pytest crates/strider-py/tests/python` — full Python suite,
  to catch any test that imported a removed symbol.
- `cargo build --workspace` — if the snapshot drift came from a Rust
  edit, the Rust side must still build clean.

## Exit criteria

- Snapshot test passes for all four EXPECTED_* sets.
- The commit message annotates the change kind (addition / removal /
  rename) so downstream consumers can update.
- No other Python test in the suite imports a removed symbol.
- `uv run maturin develop` rebuilt the wheel after the underlying
  Rust edit.

## Pitfalls

- **Silently shrinking EXPECTED to make the test pass.** If a symbol
  is missing, restore it; don't drop it from the EXPECTED set unless
  the removal is the intent.
- **Running `pytest` outside `uv run`.** The system Python imports a
  stale wheel from a prior install; the snapshot then matches the
  stale surface, not the freshly-rebuilt one.
- **Forgetting the rebuild after Rust edits.** A new pyclass added in
  `crates/strider-py/src/pattern.rs` won't appear in `dir(strider.pattern)`
  until `uv run maturin develop` reruns. Without the rebuild, the
  snapshot diff is misleading.
- **Adding a symbol to EXPECTED without exposing it via `#[pymodule]`.**
  The dir listing only sees registered symbols; an EXPECTED entry
  with no pyclass / pyfunction registration silently fails on every
  CI run.
- **Not annotating the commit.** Downstream code consuming the
  binding can't tell from `git log --oneline` whether their imports
  still resolve.

## Related skills

- `strider-py-binding` — when the new symbol's underlying binding is
  itself new (this skill picks up after the binding lands and the
  snapshot needs to follow).
- `strider-public-api-encapsulation` — when the change is a
  visibility tightening on the Rust side; the Python snapshot
  follows in lockstep.
- `strider-doc-line-number-refresh` — for refreshing references to
  the snapshot test in CLAUDE.md / READMEs after a major surface
  change.
