# Round 12 — Coverage Manifest

Branch: `review/ai6` (forked from `review/ai5` HEAD `008f530`).

## Baseline (Round 0)

| Check | Result |
|-------|--------|
| `cargo build --workspace` | ✅ clean |
| `cargo build --workspace --all-targets` | ❌ `strider-py` lib-test target fails to link (pre-existing pyo3 issue; the strider-py library itself builds, only the test binary linker step fails because PyO3 symbols aren't available without a Python framework link). Not a round-12 regression. |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo test --workspace` | ✅ 0 failures (tests passing) |
| `cd crates/strider-py && uv run pytest` | ✅ 776 passed, 11 skipped |

The `--all-targets` build failure is the same pre-existing strider-py linker issue from round 11. `cargo clippy --all-targets` works because clippy doesn't link the test binary.

## Workspace structure

12 crates under `crates/`: `cfg` · `dot` · `entity-utils` · `graphwalk` · `ir` · `opt` · `pattern` · `pcode-lift` · `reader` · `strider` · `strider-py` · `target`.

## File counts

| Category | Count |
|----------|-------|
| `.rs` source + tests | 320 |
| `Cargo.toml` | 12 |
| `*.py` test (strider-py) | 51 |
| `README.md` (root + per-crate) | 13 |

(Skills under `crates/strider/.claude/skills/*/SKILL.md` are explicitly out of scope this round per the round-12 prompt — round 11 audited them; nothing should re-audit here.)

## Round 1 crate assignment

| # | Agent | Crates | Approx `.rs` count |
|---|-------|--------|---------------------|
| 1A | code-reviewer | `ir` | ~50 |
| 1B | code-reviewer | `pcode-lift` + `cfg` | ~35 |
| 1C | code-reviewer | `opt` | ~80 |
| 1D | code-reviewer | `pattern` | ~50 |
| 1E | code-reviewer | `strider` + `target` + `reader` | ~70 |
| 1F | code-reviewer | `strider-py` + `dot` + `graphwalk` + `entity-utils` | ~37 |

## Coverage by crate

(Populated by Round 1 agents; appended at Round 7 consolidation.)
