# Round 11 — Coverage Manifest

Branch: `review/ai5` (forked from `review/ai4` HEAD `2cea799`)

## Baseline (Round 0)

| Check | Result |
|-------|--------|
| `cargo build --workspace --all-targets` | ✅ clean |
| `cargo clippy --workspace -- -D warnings` | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ❌ **89 errors** in `opt` (lib test) — pre-existing drift from rounds 9/10's trait migrations |
| `cargo test --workspace` | ✅ 0 failures |
| `cd crates/strider-py && uv run pytest` | ✅ 774 passed, 11 skipped |

**Baseline blocker (Round 0 finding):** the strict `--all-targets` clippy run surfaces stale test-file imports and useless `(&fg).into()` conversions in `opt/src/{stack_store/tests.rs, test_support.rs, constant_fold/tests.rs, ...}` left over from earlier `RewriteCtx` / `RewriteCtxView` migrations.  Round 1 wave 1C must catalogue every site and propose either the cleanup or the rationale for retention.  Documented here so that a Round 7 baseline regression check on the same command flags fewer errors than this manifest baseline.

## Workspace structure

12 crates under `crates/`:

- `cfg` · `dot` · `entity-utils` · `graphwalk` · `ir` · `opt` · `pattern` · `pcode-lift` · `reader` · `strider` · `strider-py` · `target`

## File counts

| Category | Count |
|----------|-------|
| `.rs` source + tests | 322 |
| `Cargo.toml` | 12 |
| `*.py` test (strider-py) | 50 |
| `README.md` (root + per-crate) | 13 |
| `SKILL.md` | 19 |

## Round 1 crate assignment (each agent ticks files in `inspected: yes/partial` per crate)

| # | Agent | Crates | Approx `.rs` count |
|---|-------|--------|---------------------|
| 1A | code-reviewer | `ir` | ~50 |
| 1B | code-reviewer | `pcode-lift` + `cfg` | ~35 |
| 1C | code-reviewer | `opt` | ~80 |
| 1D | code-reviewer | `pattern` | ~50 |
| 1E | code-reviewer | `strider` + `target` + `reader` | ~70 |
| 1F | code-reviewer | `strider-py` + `dot` + `graphwalk` + `entity-utils` | ~37 |

After Round 1 each agent appends to this manifest under `## Coverage by crate` with their `inspected fully` / `inspected partially` / `not inspected` counts.

## Coverage by crate

(Populated by Round 1 agents)
