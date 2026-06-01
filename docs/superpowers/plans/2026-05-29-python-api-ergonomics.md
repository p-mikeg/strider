# Python API ergonomics — 7 improvements

> Pre-release branch `rewrite/python-api-ergonomics`. Mostly ADDITIVE; the two
> breaking bits (#5 Sleigh behavior, #6 rename) are fine here. Each group =
> one gated implementer commit-set. Spike-confirmed feasibility noted inline.

## Group A — graph traversal + a `Node` handle (#3 + #4)
The IR `Graph` already exposes `node_inputs` / `node_outputs` / `output_definition`
/ `node_kind` / `int_const_val` / `bool_const_val`. Surface them.
- New Rust pyclass **`Node`** (holds a cloned `Function` handle + a `NodeId`):
  `.id -> int`, `.kind -> str`, `.inputs() -> list[Node]`, `.outputs_consumers()`
  / or `.uses()`, `.const_int() -> int | None`, `.const_bool() -> bool | None`,
  `.fingerprint() -> list[int]`, `.wide_const_bytes() -> bytes | None`,
  `.call_other_name() -> str | None`, `__repr__` (kind + id + fingerprint head).
- `Function.node(id) -> Node` and `Match.node(key) -> Node | None` (key = Capture|str)
  return a `Node` — the discoverable handle instead of raw `u32` + a dozen getters.
- Keep all existing `Match`/`Function` getters (additive; don't break callers).
- `.pyi` + docstrings + tests.

## Group B — disassembly / provenance text (#1)
`rsleigh::Insn: Display` is the instruction text.
- `Program.disasm(addr, count=1) -> list[(int, str)]` — lift `count` instructions
  from `addr` via the program's Sleigh+reader and format each `Insn`.
- `Analysis.fingerprint_text(node) -> list[(int, str)]` — the node's fingerprint
  addresses each disassembled (closes the "audit trail" loop).
- `Node.disasm()` (uses its function's fingerprint → text) optional.
- Needs a Sleigh built from the program's arch + reader; reuse `Analysis.sleigh`
  where possible, else build one in `Program`. `.pyi` + docstrings + tests.

## Group C — query conveniences (#2 + #6)
- **#2** `Analysis.find_one(pat, **opts) -> Match | None` and
  `Function.find_one(...)`. Trivial (first of `find_all`, or None).
- **#6** rename `find_all_requirements` → **`find_joined`** (it's a cross-pattern
  join on shared captures). Rename the Rust `Matcher::find_all_requirements` +
  the strider-py `Function`/`Analysis` methods + `Graph.find_all_requirements`
  Rust API + `.pyi` + tests + README/CLAUDE. (No back-compat alias — pre-release.)
  **DECISION TO CONFIRM:** the name `find_joined` (alternatives: `find_matching_set`,
  `find_all_joined`).

## Group D — remove the Sleigh-consumption footgun (#5)
`build_cfg(sleigh)` empties the caller's `PySleigh` and parks the Sleigh in the
returned `PyCfg`; reusing the original `Sleigh` then raises. Fix:
- After `Builder::build()` returns `(cfg, sleigh)`, **put the Sleigh back into the
  original `PySleigh`**, and have `PyCfg` hold a `Py<PySleigh>` reference (share the
  one handle) so `analyze_cfg` / dot dumping borrow it from there.
- Net: the caller's `Sleigh` is usable after `build_cfg`; drop the README warning
  note. (Moderate refactor of `cfg.rs` + `PyCfg` + the `analyze_cfg`/dot consumers.)
- Tests: a `build_cfg` then reuse-the-same-Sleigh test that previously raised.

## Group E — minor inconsistencies (#7)
- `bool_binary(op, l, r)` returns a finalised `Pat` while `int_binary`/`float_binary`
  return chainable typed builders → make `bool_binary` return a `BoolBinaryPat`
  (chainable `.ordered()`/`.capture()`/…) for symmetry.
- `VnSpace.const_()` → expose as `VnSpace.const_()` keep? `const` is a valid Python
  name; add `VnSpace.const()` (keep `const_` as a hidden alias to avoid a hard break,
  or just rename — pre-release). **Low priority; confirm if worth the churn.**
- `CallingConvention.custom(...)` 10-arg call → leave (advanced); optionally a
  small note. (Lowest priority — likely SKIP.)

## Sequence
A → B → C → D → E, each gated (`cargo test --workspace` + clippy + maturin + pytest,
incl. the snapshot + docstring tests). Then docs (README + CLAUDE.md), final full
gate, final code-review, PR → `rewrite/strider`.

## Open decisions (proceeding with my picks; redirect anytime)
1. `find_all_requirements` → `find_joined`.
2. `Node` as a Rust pyclass (not a Python wrapper) for consistency with `Match`/`Function`.
3. #5 via put-back + `PyCfg` sharing `Py<PySleigh>`.
4. #7: do the `bool_binary` symmetry; treat `const_`/custom-CC as optional/skip unless you want them.
