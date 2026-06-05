# Collapse IntConst/IntConstWide into one `IntConst(IntPayload)` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Shrink the IR's per-node footprint (`Node<NodeKind>` 48→32 bytes, `NodeKind` 32→24) **and** unify the two integer-constant node kinds into a single `NodeKind::IntConst(IntPayload)` where `IntPayload = { Small(u64), Wide(WideConstId) }`, so "an integer constant" is one concept with an encapsulated small/wide representation.

**Architecture:** The `u128` payload of `IntConst` forces 16-byte alignment on every node. We replace it with a `Copy` enum that holds ≤64-bit values inline (`Small(u64)`) and routes I80/I128/I256/I512 through the existing wide-const interner (`Wide(WideConstId)`). The split is **keyed on the value's type** (I1…I64 ⇒ Small, I80+ ⇒ Wide), which makes the dedup-canonicalisation invariant a one-liner at each constructor and needs no interner access in `canonicalize`. `rsleigh::Vn` (16 bytes, align 8) becomes the new size floor, giving `NodeKind` = 24.

**Tech stack:** Rust workspace; `cargo test`/`clippy`; the dedup cache hashes via derived `Hash`/`PartialEq` (so a `Copy` enum payload Just Works); the wide interner lives on `Function`.

**Working rules:** Branch `develop`. One commit per task; `git push origin develop` after each. End commit messages with the `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` trailer. **Never** mention plan/task/step identifiers in code or commit messages. Prompt the user before merging `develop`→`master`. Full-workspace gate (`cargo test --workspace` + `cargo clippy --workspace --all-targets` + `uv run pytest`) before the merge prompt.

**Sequencing rationale:** Tasks 1–2 are behavior-preserving and keep `IntConst(u128)`/`IntConstWide` intact, but funnel reads through one accessor and move I80/I128 into the interner. That shrinks Task 3 (the atomic, cross-crate type switch) to roughly the constructors, the funnel body, the variant-list sites, the pattern DSL leaves, dot/validate/gc, and tests.

## Guiding principle: read constants through accessors, never match the payload manually

The whole point of `IntPayload` is that the small/wide representation is an **encapsulated implementation detail**. So the standing rule for this work — and for the codebase afterwards — is: **read a constant's value through a reader (`int_const_u128` / `int_const_val` / `int_const_i128` / the pattern bindings' `get_uint`/`get_int`/`get_bool`), never by pattern-matching `NodeKind::IntConst(..)` to pull the value out.**

Direct `NodeKind::IntConst(..)` matching is permitted ONLY in this closed set (everything else uses a reader):
1. the reader implementations themselves (`int_const_u128`/`int_const_i128` in `viewer.rs`);
2. the constructors (`build_int_const`/`build_int_const_wide`/`build_boolean_const`);
3. `canonicalize` (graph/cache.rs) and `gc_wide_consts` (data.rs) — they rewrite the payload structurally;
4. **kind-only** checks that don't read the value: `matches!(k, NodeKind::IntConst(_))`;
5. pattern-DSL `KindSpec::Exact(..)` leaf *construction* (which builds an exemplar `NodeKind`).

Anywhere a site currently does `let NodeKind::IntConst(v) = … else …` or `match … { IntConst(v) => use v }` to read the value, convert it to a reader. Task 1 does this conversion exhaustively (not just the hot sites), which is also what minimises Task 3's atomic diff. A `simplify`-style pass at the end (Task 3 Step 8) re-greps to confirm no stray value-binding matches remain outside the closed set.

Spike + design context: this conversation's discussion; the `value_vn`/sizes spike. Key measured facts: `IntPayload{Small(u64),Wide(WideConstId)}` = 16 bytes align 8; `rsleigh::Vn` = 16 bytes align 8; no other `NodeKind` variant exceeds 16 bytes.

---

## Task 1: Read-accessor funnel + `get_uint`/`get_int` viewer signature

**Goal:** Route every integer-constant *value read* through one accessor, and change the pattern bindings' `get_uint`/`get_int`/`get_bool` from `&Graph` to a `Function`-aware reader (the wide interner lives on `Function`, not `Graph`, so this is required before Wide values can be read). Pure refactor — behavior identical, compiles green.

**Files:**
- Modify: `crates/strider-ir/src/viewer.rs` (add `int_const_u128`; route existing readers through it)
- Modify: `crates/strider-pattern/src/bindings.rs` (`get_uint`/`get_int`/`get_bool` take `&Function`)
- Modify callers of `get_uint`/`get_int`: `crates/strider-opt/src/constant_fold/rules.rs`, `crates/strider-opt/src/indirect_branch_resolve/jump_table.rs` (and any others surfaced by grep)
- Modify value-read sites to use the funnel: `crates/strider-opt/src/known_bits/mod.rs`, `crates/strider-opt/src/sp_expr/walk.rs`, `crates/strider-opt/src/indirect_branch_resolve/{classify.rs,stack_array.rs}`, `crates/strider-orchestrator/src/{indirect_resolver.rs,strider/insn/control.rs}`

- [ ] **Step 1: Add the funnel accessor on `IRViewer`.**

In `crates/strider-ir/src/viewer.rs`, add (near `int_const_val`):
```rust
    /// The integer-constant value carried by `value`, masked to its declared
    /// type and widened to `u128`, or `None` if `value` is not an integer
    /// constant. Single read SSoT for constant values — every consumer reads
    /// constants through this (or its `u64`/`i64` projections) so the storage
    /// representation stays encapsulated.
    fn int_const_u128(&self, value: ValueId) -> Option<u128> {
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        match *self.kind_of_value(value) {
            NodeKind::IntConst(v) => ty.get_unsigned_int(v),
            // IntConstWide is not value-foldable today (I256/I512 only); a later
            // change moves I80/I128 here and this arm will read the interner.
            _ => None,
        }
    }
```
Then rewrite `int_const_val`, `get_as_unsigned_int`, `get_as_signed_int`, `const_value` to delegate:
```rust
    fn int_const_val(&self, value: ValueId) -> Option<u64> {
        self.int_const_u128(value).and_then(|v| u64::try_from(v).ok())
    }
```
For `get_as_signed_int`, add the signed projection (sign-extend via the type):
```rust
    fn int_const_i128(&self, value: ValueId) -> Option<i128> {
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() { return None; }
        match *self.kind_of_value(value) {
            NodeKind::IntConst(v) => ty.get_signed_int(v),
            _ => None,
        }
    }
```
Keep the public return types of `get_as_unsigned_int`/`get_as_signed_int`/`const_value` unchanged; only their bodies delegate to `int_const_u128`/`int_const_i128`. (`const_value` still needs the type, so it composes `int_const_u128` + `value_type`.)

- [ ] **Step 2: Verify the funnel is behavior-identical.**

Run: `cargo test -p strider-ir`
Expected: PASS (existing const-read tests unchanged).

- [ ] **Step 3: Move `get_uint`/`get_int`/`get_bool` to a `Function` reader.**

In `crates/strider-pattern/src/bindings.rs`, change the three signatures from `graph: &Graph` to `function: &strider_ir::Function` and delegate to the funnel:
```rust
    pub fn get_uint(&self, c: Capture, function: &strider_ir::Function) -> Option<u128> {
        use strider_ir::IRViewer;
        function.int_const_u128(self.get_value(c)?)
    }
    pub fn get_int(&self, c: Capture, function: &strider_ir::Function) -> Option<i128> {
        use strider_ir::IRViewer;
        function.int_const_i128(self.get_value(c)?)
    }
    pub fn get_bool(&self, c: Capture, function: &strider_ir::Function) -> Option<bool> {
        use strider_ir::IRViewer;
        let v = self.get_value(c)?;
        if !function.value_kind(v).is_bool() { return None; }
        function.int_const_u128(v).map(|x| x != 0)
    }
```
(Confirm `Function: IRViewer` is in scope; it is — `impl IRViewer for Function`.)

- [ ] **Step 4: Update `get_uint`/`get_int` callers.**

Grep first: `grep -rn "\.get_uint(\|\.get_int(\|\.get_bool(" crates/ --include=*.rs | grep -v "fn get_"`.
Each caller currently passes a `&Graph` (e.g. `ctx.graph()`); change to the function (`ctx.function()`). Known sites: `constant_fold/rules.rs` (lines ~164-166, 307, 339, 391, 408), `jump_table.rs` (~242, 245, 270). In pattern crate internal tests, pass the function.

- [ ] **Step 5: Migrate EVERY direct value-read match to the funnel (exhaustive).**

Per the guiding principle, convert *every* site that pattern-matches `NodeKind::IntConst(..)` to **read the value** into a reader call. Enumerate them first:
```bash
grep -rn "NodeKind::IntConst(" crates/*/src --include=*.rs | grep -v "/tests\|tests.rs" \
  | grep -vE "IntConst\((_| ?\.\.)?\)"   # drop kind-only matches
```
For each, decide: is it (a) reading the value, (b) a constructor, (c) canonicalize/gc, or (d) a pattern `KindSpec` leaf? Convert (a) to `int_const_u128`/`int_const_i128` (or `get_uint`/`get_int` in pattern code); leave (b)/(c)/(d) per the closed set. Known value-read sites to convert: `known_bits/mod.rs:121` (`IntConst(v) => from_const(v, ty)` → `int_const_u128` then `from_const`), `sp_expr/walk.rs:75` (`IntConst(c) => Constant{addr: c as i64}`), `indirect_branch_resolve/classify.rs` (90, 119), `stack_array.rs:170`, `indirect_resolver.rs:145`, `strider/insn/control.rs:394`, `strider-pattern/typed/consts.rs` (36, 89 — read `stored` for a value predicate → `get_uint`), and `builder_ext.rs:484` (the `IntBitsToFloat` immediate-fold read). Leave `matches!(kind, NodeKind::IntConst(_))` kind-only checks untouched.

- [ ] **Step 6: Verify the funnel is exhaustive, then build, test, commit.**

Re-grep the value-binding matches; everything remaining must be in the closed set (readers / constructors / canonicalize / gc / pattern `KindSpec` leaves):
```bash
grep -rn "NodeKind::IntConst([a-z]" crates/*/src --include=*.rs | grep -v "/tests\|tests.rs"
```
Eyeball the result — each hit must be justifiable under the guiding principle's closed set. If a value-read site slipped through, convert it.
Run: `cargo test -p strider-ir -p strider-opt -p strider-pattern -p strider-orchestrator` → PASS.
Run: `cargo clippy -p strider-ir -p strider-opt -p strider-pattern -p strider-orchestrator` → zero warnings.
```bash
git add crates/
git commit -m "refactor: funnel integer-constant value reads through int_const_u128

Route every constant value read through one accessor and move the pattern
bindings' get_uint/get_int/get_bool onto a Function reader (the wide interner
lives on Function). Behaviour-preserving prep.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 2: Route I80/I128 constants through the wide interner (behavior-preserving)

**Goal:** Move I80 and I128 constants from inline `IntConst(u128)` into `IntConstWide`/the interner, and teach `int_const_u128`/`int_const_i128` to read them back. After this, `IntConst(u128)` only ever holds ≤I64 values — even though the field type is still `u128`. Folding of I80/I128 continues to work, now through the interner.

**Files:**
- Modify: `crates/strider-ir/src/wide_const.rs` (extend `WideConstStorage` with I80/I128)
- Modify: `crates/strider-ir/src/builder/builder_ext.rs` (`build_int_const` routes I80/I128 to wide; `build_int_const_wide` accepts them)
- Modify: `crates/strider-ir/src/viewer.rs` (`int_const_u128`/`int_const_i128` read I80/I128 from the interner)
- Modify: `crates/strider-ir/src/validate/mod.rs` (wide byte-size/type checks accept I80/I128)
- Modify: any fold WRITE path that builds I80/I128 via `build_int_const` (it already routes through the constructor — verify)
- Test: `crates/strider-opt/src/constant_fold/tests.rs` (I128 fold round-trip)

- [ ] **Step 1: Read `wide_const.rs` to learn the exact `WideConstStorage` shape.**

Run: read `crates/strider-ir/src/wide_const.rs`. Note the variant set (today `I256([u64;4])`, `I512([u64;8])`), the `byte_size()` method, and any limbs/value accessor.

- [ ] **Step 2: Extend `WideConstStorage` with u128-backed I80/I128.**

Add variants (adapt to the real enum):
```rust
    /// 80-bit (x87 extended) value, low 80 bits significant.
    I80(u128),
    /// 128-bit value.
    I128(u128),
```
Extend `byte_size()` (`I80 => 10`, `I128 => 16`) and any value accessor. Add a helper to read a u128 view:
```rust
    /// The value as a `u128` if it fits (I80/I128), else `None` (I256/I512).
    pub fn as_u128(&self) -> Option<u128> {
        match self {
            Self::I80(v) | Self::I128(v) => Some(*v),
            Self::I256(_) | Self::I512(_) => None,
        }
    }
```
Add a constructor from a `(u128, ValueType)` if the existing `WideConstStorage` API expects byte arrays — mirror however I256/I512 are built. Keep dedup-by-value (the interner already value-dedups).

- [ ] **Step 3: Route I80/I128 construction to wide.**

In `builder_ext.rs` `build_int_const`: after the `is_integer` check, route I80/I128 (and reject only as needed) to the wide path. Cleanest: detect `matches!(output_type, I80 | I128 | I256 | I512)` and delegate to `build_int_const_wide` with the appropriate `WideConstStorage`; keep inline `IntConst` only for ≤I64.
```rust
        if matches!(output_type, ValueType::I80 | ValueType::I128
                    | ValueType::I256 | ValueType::I512) {
            let masked = val.into() & output_type.bit_mask_u128();
            let storage = WideConstStorage::for_type(masked, output_type)?; // I80/I128 → u128; I256/I512 unchanged path
            return self.build_int_const_wide(storage, output_type);
        }
        let masked = val.into() & output_type.bit_mask_u128();
        Ok(self.build_single_output_pure(NodeKind::IntConst(masked), [], output_type))
```
Update `build_int_const_wide` to accept I80/I128 (it currently expects I256/I512 byte sizes). Adjust its `expected` byte-size match to include `I80 => 10`, `I128 => 16`.

(`WideConstStorage::for_type` is a small helper you add: maps `(u128, I80|I128)` to the new variants and `(value, I256|I512)` to the existing array variants — reuse whatever path `build_int_const_wide` callers use today for I256/I512.)

- [ ] **Step 4: Teach the funnel to read I80/I128 from the interner.**

In `viewer.rs`, extend `int_const_u128`'s `_` arm:
```rust
            NodeKind::IntConstWide(id) => {
                self.function().wide_const_opt(id)?.as_u128().map(|v| v & ty.bit_mask_u128())
            }
            _ => None,
```
Same for `int_const_i128` (read u128 then sign-extend via `ty.get_signed_int`). Now I80/I128 fold reads resolve through the interner; I256/I512 still return `None` (not foldable), as before.

- [ ] **Step 5: Validator accepts I80/I128 wide.**

In `validate/mod.rs`, the `IntConstWide` checks currently require an I256/I512 declared type + matching byte size. Broaden to accept I80/I128 too (byte sizes 10/16). Keep the "declared type must be a wide type that matches the stored byte size" shape.

- [ ] **Step 6: Add an I128 fold round-trip test.**

In `constant_fold/tests.rs`, add a test that builds two I128 constants (now wide-backed), folds `add`, and asserts the I128 result is correct (read via `int_const_u128`). Run it; expect PASS (proves I80/I128 still fold through the interner).

- [ ] **Step 7: Build, test, commit.**

Run: `cargo test -p strider-ir -p strider-opt` → PASS.
Run: `cargo clippy -p strider-ir -p strider-opt` → zero warnings.
```bash
git add crates/
git commit -m "refactor(strider-ir): store I80/I128 constants in the wide interner

IntConst now only carries <=64-bit values inline; I80/I128 join I256/I512 in
the wide-const interner, and the value funnel reads them back so folding is
unchanged. Prep for shrinking IntConst's inline payload.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 3: The type switch — `IntConst(IntPayload)`, remove `IntConstWide`

**Goal:** Replace `IntConst(u128)` with `IntConst(IntPayload)` where `IntPayload { Small(u64), Wide(WideConstId) }`, and fold the `IntConstWide(WideConstId)` variant into `IntConst(IntPayload::Wide(id))` — removing `IntConstWide` entirely. Atomic cross-crate commit. After this: `NodeKind` = 24, `Node` = 32.

**Files (strider-ir):** `node/kind.rs` (define `IntPayload`; change `IntConst`; delete `IntConstWide`; fix `is_cacheable`/`is_commutative`/etc. arms), `node_signature.rs`, `builder/builder_ext.rs` (constructors), `graph/cache.rs` (canonicalize), `viewer.rs` (funnel bodies), `validate/{mod.rs,graph_invariants.rs}`, `function/data.rs` (`gc_wide_consts`), `function/dot/{raw.rs,label.rs}`, `walk/cast/mod.rs`
**Files (downstream):** `strider-opt` construct sites (`indirect_branch_resolve/{inplace.rs,stack_array.rs}`, `load_forward/mod.rs`), `strider-pattern` (`typed/consts.rs`, `bindings.rs`, `matcher`/`template` leaf builders), `strider-py` (`pattern.rs` PatRepr + bindings)
**Tests:** every inline-test `NodeKind::IntConst(<lit>)` construct site across the touched crates.

- [ ] **Step 1: Define `IntPayload` and switch the variant.**

In `crates/strider-ir/src/node/kind.rs`, add above `NodeKind`:
```rust
/// The payload of an [`NodeKind::IntConst`]: a small value held inline, or a
/// `WideConstId` into the function's wide-const interner for values wider than
/// 64 bits (I80/I128/I256/I512). The split is keyed on the constant's TYPE
/// (I1..I64 ⇒ `Small`, I80+ ⇒ `Wide`), so a given typed value has exactly one
/// representation and the dedup cache stays sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntPayload {
    /// A constant of type I1..I64, masked to its width.
    Small(u64),
    /// A constant of type I80/I128/I256/I512, interned in
    /// [`crate::Function`]'s wide-const table.
    Wide(crate::wide_const::WideConstId),
}
```
Change the variant:
```rust
    /// A compile-time integer constant. ≤64-bit values are held inline
    /// (`IntPayload::Small`); wider values (I80/I128/I256/I512) carry a
    /// `WideConstId` (`IntPayload::Wide`) into the function's interner.
    IntConst(IntPayload),
```
Delete the `IntConstWide(WideConstId)` variant. In `is_cacheable` (and `is_commutative`, and any other matcher that lists `| Self::IntConstWide(..)`), remove the `IntConstWide` arm — `IntConst(..)` already covers it.

Add a permanent size guard at the bottom of `kind.rs`:
```rust
const _: () = assert!(std::mem::size_of::<NodeKind>() <= 24,
    "NodeKind must stay <= 24 bytes (IntConst payload must not exceed rsleigh::Vn)");
```

- [ ] **Step 2: Update constructors.**

In `builder_ext.rs`:
- `build_boolean_const`: `NodeKind::IntConst(IntPayload::Small(u64::from(val)))`.
- `build_int_const` (≤I64 inline arm): `NodeKind::IntConst(IntPayload::Small((val.into() & mask) as u64))` (the mask guarantees it fits u64 for ≤I64; `#[allow(clippy::cast_possible_truncation)]` with a comment that the type bound makes it lossless).
- `build_int_const_wide`: returns `NodeKind::IntConst(IntPayload::Wide(id))` (was `IntConstWide(id)`).
- `build_int_bits_to_float`'s `if let NodeKind::IntConst(bits) = …` (reads the immediate-fold value): change to read via the funnel `self.int_const_u128(value)` instead of matching the payload directly.

- [ ] **Step 3: Update `canonicalize` (graph/cache.rs).**

```rust
    fn canonicalize(kind: NodeKind, _inputs: &[ValueId], outputs: &[ValueKind]) -> NodeKind {
        match (kind, outputs) {
            (NodeKind::IntConst(IntPayload::Small(v)), [ValueKind::Typed(ty)]) if ty.is_integer() => {
                NodeKind::IntConst(IntPayload::Small((v as u128 & ty.bit_mask_u128()) as u64))
            }
            (kind, _) => kind,
        }
    }
```
(`Wide` needs no canonicalisation — the interner already value-dedups, and the type is fixed.)

- [ ] **Step 4: Update the funnel bodies (viewer.rs).**

```rust
        match *self.kind_of_value(value) {
            NodeKind::IntConst(IntPayload::Small(v)) => ty.get_unsigned_int(u128::from(v)),
            NodeKind::IntConst(IntPayload::Wide(id)) =>
                self.function().wide_const_opt(id)?.as_u128().map(|v| v & ty.bit_mask_u128()),
            _ => None,
        }
```
Same shape for `int_const_i128` (sign-extend via `ty.get_signed_int`).

- [ ] **Step 5: Update validate, gc, dot, cast (strider-ir).**

- `validate/{mod.rs,graph_invariants.rs}`: the wide-const checks now match `NodeKind::IntConst(IntPayload::Wide(id))` (was `IntConstWide(id)`); the local-typing signature is one `IntConst` arm.
- `node_signature.rs`: single `NodeKind::IntConst(_) => INT_VAL` output; remove the `IntConstWide` arm.
- `function/data.rs` `gc_wide_consts`: scan for `NodeKind::IntConst(IntPayload::Wide(id))` (read) and rewrite the id in place via `IntConst(IntPayload::Wide(new_id))` (was `IntConstWide`).
- `function/dot/{raw.rs,label.rs}`: fold the two render arms into `IntConst(IntPayload::Small(v))` (show value) and `IntConst(IntPayload::Wide(id))` (show wide value via `wide_const_opt`).
- `walk/cast/mod.rs:66`: `IntConst(..)` covers it; remove the `IntConstWide` arm.

- [ ] **Step 6: Update downstream construct/match sites.**

- `strider-opt`: `inplace.rs` (`IntConst(masked_target)`/`IntConst(value)` → `IntConst(IntPayload::Small(... as u64))`), `stack_array.rs` (construct `IntConst(IntPayload::Small(..))`; the read at :170 already funneled in Task 1), `load_forward/mod.rs:215` (construct Small).
- `strider-pattern` `typed/consts.rs`: the `KindSpec::Exact(NodeKind::IntConst(v))` leaves and `matches!(k, NodeKind::IntConst(v) if set.contains(v))` predicates. Add a small constructor helper `fn small_const(v: u128) -> NodeKind { NodeKind::IntConst(IntPayload::Small(v as u64)) }` in the pattern crate and route the leaves through it; for value predicates, compare via the funnel/`as u64`. `int_const_any_of` (set of `u64`) matches `Small` values. `bindings.rs` already funnels (Task 1).
- `strider-py` `pattern.rs`: `PatRepr::IntConst(u128)` lowering builds `IntPayload::Small`; the wide path (if any) builds `Wide`. Match the Rust shape.

- [ ] **Step 7: Update test construct sites.**

Sweep every inline-test `NodeKind::IntConst(<literal>)` (the ~30 sites in `function/data.rs`, `walk/mod.rs`, `node_signature.rs`, `build_trait.rs`, `rewrite/mod.rs`, `template/builder.rs`, `matcher/builder.rs`, `typed/consts.rs`, `pipeline.rs`, etc.) to `NodeKind::IntConst(IntPayload::Small(<literal>))` (literals are all ≤u64). For test assertions matching a value (`matches!(k, IntConst(7))`), use `IntConst(IntPayload::Small(7))`.

- [ ] **Step 8: Build the whole workspace, fix residual sites, test.**

Run: `cargo build --workspace 2>&1 | tail -40` — iterate until clean (the compiler enumerates every missed site).
Run: `cargo test --workspace` → no new failures.
Run: `cargo clippy --workspace --all-targets` → zero warnings.

- [ ] **Step 9: Confirm the size win.**

Add a temporary probe (export `strider_graph::Node` via a one-line `pub use storage::Node;` in `strider-graph/src/lib.rs`, write `crates/strider-ir/tests/zz_size_probe.rs` asserting `size_of::<NodeKind>() == 24` and `size_of::<strider_graph::Node<NodeKind>>() == 32`), run it, then **revert both** the export and the probe file. (The permanent `const _: () = assert!` guard from Step 1 stays.)

- [ ] **Step 10: Commit + push.**

```bash
git add crates/
git commit -m "refactor(strider-ir): collapse IntConst/IntConstWide into IntConst(IntPayload)

IntConst now carries a Copy IntPayload { Small(u64), Wide(WideConstId) }: <=64-bit
values inline, wider values via the interner. IntConstWide is removed — one node
kind for every integer constant. NodeKind 32->24 bytes, Node<NodeKind> 48->32.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Task 4: Sync docs

**Files:** `CLAUDE.md`, `README.md`

- [ ] **Step 1: Update CLAUDE.md.**

Update the IR node-model section: `IntConst(IntPayload{Small(u64),Wide(WideConstId)})` is the single integer-constant kind (no `IntConstWide`); I80/I128/I256/I512 live in the wide interner via `IntPayload::Wide`; constant values are read through `int_const_u128`/`int_const_val`. Fix any `IntConstWide` mention and the `IntConst(u128)` description.

- [ ] **Step 2: Update README.md.**

Same: the optimizer/IR sections that mention `IntConst`/`IntConstWide` and constant reads.

- [ ] **Step 3: Commit + push.**

```bash
git add CLAUDE.md README.md
git commit -m "docs: sync IntConst(IntPayload) collapse and value-read funnel

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin develop
```

---

## Final verification gate (before merge prompt)

- [ ] `cargo test --workspace` → no new failures vs baseline.
- [ ] `cargo clippy --workspace --all-targets` → zero warnings.
- [ ] `cd crates/strider-py && uv run maturin develop && uv run pytest` → all pass.
- [ ] **Prompt the user** to fast-forward `develop` → `master` (do not merge unprompted).

---

## Self-review notes

- **Spec coverage:** size shrink → Task 3 + size guard/probe; variant collapse → Task 3; I80/I128-to-wide → Task 2; read funnel + `get_uint` API → Task 1; docs → Task 4. The `get_uint &Graph→&Function` friction is handled up front (Task 1). The dedup invariant is made trivial by the **type-keyed** Small/Wide split (Task 2/3), so `canonicalize` needs no interner access.
- **Type consistency:** new names used uniformly — `IntPayload::{Small,Wide}`, `int_const_u128`/`int_const_i128`, `WideConstStorage::{I80,I128}` + `as_u128`/`for_type`.
- **Compile-green staging:** Tasks 1–2 keep `IntConst(u128)`/`IntConstWide` and only add/funnel, so each commits green; Task 3 is the single atomic cross-crate switch, minimized by the prep.
- **Risk note:** Task 3 is large and cross-crate by nature (the type change can't be split into separately-compiling per-crate commits). The compiler is the checklist — Step 8 iterates `cargo build --workspace` until clean. The permanent `const _: () = assert!` guard locks the size win against regressions.
