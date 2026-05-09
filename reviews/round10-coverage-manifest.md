# Round 10 — Coverage Manifest

Generated at the start of round 10.  Every file enumerated below MUST
be inspected by at least one round-1 / round-2 / round-3 subagent.
The round-7 final consolidation reports per-crate coverage status.

## Branch + tip

- Branch: `review/ai4` (forked from `review/ai3`).
- Round-9 tip: `c6593f1` (docs: bump next-round prompt to round 10).
- Round-9 implementation tip: `d4846e1` (wave 31).
- Round-9 wave count: 31 implementation waves between `66b7d3a`
  (round 0 — coverage manifest) and `d4846e1` (wave 31).
- Round-9 commit count above `review/ai2` (round-8 base): 37.

## Baselines (start of round 10)

| Check | Status |
|-------|--------|
| `cargo build --workspace` | ✓ green |
| `cargo test --workspace` | ✓ 123 binaries, 0 failures |
| `cargo clippy --workspace --lib` | warns: pre-existing `expect()` in `dot/src/lib.rs:203`; otherwise clean |
| `cargo build --workspace --all-targets` | ✗ `strider-py` lib-test fails to link (Python symbols) — known limitation; `[lib] test = false` in `crates/strider-py/Cargo.toml:19` |
| `cd crates/strider-py && uv run pytest` | ✓ 766 passed, 11 skipped |

## Source enumeration (in scope)

| Crate | `*.rs` count | Notes |
|-------|-------------|-------|
| pattern | 74 | Largest crate; matcher + builders + ctors + commutativity |
| strider | 55 | Orchestrator + IR-strider + insn handlers + indirect_resolve + tests |
| ir | 54 | Graph + builder + validate (3 layers) + node_signature + walk |
| opt | 51 | 12+ passes + pipeline + worklist + sp_expr + indirect_branch_resolve |
| cfg | 38 | Region builder + indirect_resolve mini-graph + types + dot |
| strider-py | 14 | PyO3 bindings + GIL handling + typed exceptions |
| pcode-lift | 12 | vn_io register-aliasing + value lifters per op family |
| reader | 11 | ELF + relocations + autoload extension |
| graphwalk | 5 | Generic graph traversal |
| entity-utils | 3 | DenseEntitySet + Worklist |
| dot | 3 | DOT renderer |
| **Total** | **320** | |

## Test files

- Rust tests + benches: included in the 320 `.rs` files above (under each crate's `tests/` and `benches/`).
- Python tests: 46 files under `crates/strider-py/tests/`.

## Documentation

- Cargo manifests: 12 (`Cargo.toml`).
- Per-crate READMEs: 14 (root `README.md` + 13 crate READMEs).
- SKILL files: 19 (`crates/strider/.claude/skills/*/SKILL.md`).
- Workspace `CLAUDE.md`: 1.

## Subagent assignment (round 1)

| Agent | Crates | File budget |
|-------|--------|-------------|
| 1A | `ir` | 54 .rs |
| 1B | `pcode-lift` + `cfg` | 12 + 38 = 50 .rs |
| 1C | `opt` | 51 .rs |
| 1D | `pattern` | 74 .rs |
| 1E | `strider` + `target` + `reader` | 55 + ?(target — separate from strider) + 11 .rs |
| 1F | `strider-py` + `dot` + `graphwalk` + `entity-utils` | 14 + 3 + 5 + 3 = 25 .rs + 46 .py |

(`target` listed separately above — its files were counted under one of the parent buckets;
agent 1E covers the workspace's `target` crate explicitly.)

## Out of scope per round-10 prompt

- Editing source code.  Output is `reviews/round10-*.md` only.
- Authoring tests / skills / READMEs.  Diffs only.
- `../rsleigh` upstream.  Consult, don't audit.
- Generated artefacts under `target/`, `.git/`, `__pycache__/`.

## Round-9 deferred items (carry into round 10 as known-deferred)

These were explicitly deferred by the user during round 9 and remain
out of scope for round 10's correctness work unless newly surfaced by
this round's audit:

- IMP-1 AArch64-BE stack-array dispatch (`Or(SP,K)` + Truncate-wrapped
  labels).
- IMP-2 MIPS64 PIC GOT-indirect dispatch.
- IMP-3 PPC32/64 stack-array dispatch.
- IMP-5 ARM-BE VFP register aliasing.
- IMP-7 PPC32/64 `ret_val_regs_float: ["f1","f2"]` over-approximation.

Each has an `#[ignore]`d test in `crates/strider/tests/indirect_branch.rs`
documenting the specific lifter shape that fails.

## Coverage tick-list

Each round-1 subagent must, at the end of its report, include a
"Coverage" section listing every file in its assigned bucket as one of:

- **fully** — read end-to-end
- **partially** — read in part; remainder deferred
- **not at all** — skipped

Round 7 must verify the union of all subagents' "fully" tick-lists
covers the entire 320-file `.rs` set + 46-file `.py` set + 12 / 14 / 19
doc files above.
