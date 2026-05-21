# Round 13 — Coverage Manifest

Branch: `review/ai7` (forked from `review/ai6` HEAD `54f00bb`; prompt-bump commit `fa4d671`).

## Baseline (Round 0)

| Check | Result |
|-------|--------|
| `cargo build --workspace` | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo test --workspace` | ✅ 3178 passed, 0 failed |
| `cd crates/strider-py && uv run pytest` | (skipped — pre-existing PyO3 ABI3 linker constraint noted in `round12-coverage-manifest.md`) |

## Workspace structure

12 crates under `crates/`: `cfg` · `dot` · `entity-utils` · `graphwalk` · `ir` · `opt` · `pattern` · `pcode-lift` · `reader` · `strider` · `strider-py` · `target`.

## File counts

| Category | Count |
|----------|-------|
| `.rs` source + tests | 330 |
| `Cargo.toml` | 12 |
| `*.py` test (strider-py) | 51 |
| `README.md` (root + per-crate) | 14 |

Per-crate `.rs` counts:

| Crate | `.rs` files |
|-------|-------------|
| cfg | 38 |
| dot | 3 |
| entity-utils | 3 |
| graphwalk | 5 |
| ir | 56 |
| opt | 51 |
| pattern | 74 |
| pcode-lift | 12 |
| reader | 11 |
| strider | 54 |
| strider-py | 14 |
| target | 9 |

(Skills under `crates/strider/.claude/skills/*/SKILL.md` are explicitly out of scope this round — round 11 last audited them.)

## Round 1 crate assignment

| # | Agent | Crates | Approx `.rs` count |
|---|-------|--------|---------------------|
| 1A | code-reviewer | `ir` | ~56 |
| 1B | code-reviewer | `pcode-lift` + `cfg` | ~50 |
| 1C | code-reviewer | `opt` | ~51 |
| 1D | code-reviewer | `pattern` | ~74 |
| 1E | code-reviewer | `strider` + `target` + `reader` | ~74 |
| 1F | code-reviewer | `strider-py` + `dot` + `graphwalk` + `entity-utils` | ~25 |

## Recent landings (post-round-12 context)

Round 12 commits (already on `review/ai7` via `54f00bb`):

* W1 (c2ad6ff) — H3 / M2 (4 callother) / M3 / EC-1 / EC-3 / M1 / IRP-2.
* W2-W3 (e691e86) — doc + breadcrumb strip + R12-T-N / R12-T-P / R12-T-C / TY-2 / TY-3.
* W4 (2b64232) — S1.6 + S2.1 + S3.1.
* W5a (4276247) — R12-T-A `RewriteCtx{,View}` fields → `pub(crate)`.
* W5b (5ee2abb) — `Cfg::{graph,entry,sleigh,into_sleigh}()` accessors.
* W5c+e+f+h (54f00bb) — delete deprecated `Builder::new`/`with_endianness`; `OptionsBuilder::set_function_boundary`; `RunConfig::start_addr` newtype; R-9 stale comment fix.

Round 13 should NOT re-verify these as authoritative — re-derive from current code.
