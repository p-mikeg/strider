# Int-type rename, Bool→I1 collapse, lifter-owned casts — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename `U8…U512`→`I8…I512`, collapse `Bool` into a 1-bit integer `I1` (removing the Bool category and its node kinds), make node-construction builders strict so the lifter owns all truncate/extend/bitcast fixups, remove `CastToBool`/`CastToInt`/`CastToFloat`, and let pattern queries select "booleans" by output width.

**Architecture:** Incremental phases, each keeping `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and the relevant test suites green before commit+push. The type model change (`output_type.rs`) is the foundation; node-kind removals follow; strict builders come after the conversion vocabulary is finalized; pattern/python and the #7 doc fix come last.

**Tech Stack:** Rust workspace (cranelift-entity, anyhow, smallvec), PyO3/maturin (`strider-py`), rsleigh (Sleigh p-code lifter at `../rsleigh`).

**Verification commands (per phase):**
- `cargo build --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p strider-ir` / `-p strider-lift` / `-p strider-analyze` (per-crate, avoid `--workspace` hang)
- Python phase: `cd crates/strider-py && uv run maturin develop && uv run pytest`

**Branch:** `rewrite/int-types` (already created). Push after every task: `git push origin rewrite/int-types`.

---

## Task 1: Rename integer variants `U8…U512` → `I8…I512`

`Bool` stays untouched in this task. Only the integer variant identifiers + their display strings + `WideConstStorage` change.

**Files:**
- Modify: `crates/strider-ir/src/node/output_type.rs` (enum, `TYPE_INFO` names `"u8"→"i8"`, `info()` arms, doc comments, tests)
- Modify: `crates/strider-ir/src/wide_const.rs` (`WideConstStorage::U256/U512`→`I256/I512`)
- Modify: every site referencing `NodeOutputType::U*` or `WideConstStorage::U*` (use the rename command below)
- Modify: `crates/strider-ir/src/function_dot/label.rs:145` (`:u{bits}`→`:i{bits}`)
- Modify: `crates/strider-ir/src/function_dot/tests.rs:651` (`"const 0x10:u64"`→`:i64`)
- Modify: doc strings in `crates/strider-py/src/{matcher,function,pattern}.rs` ("matches U32 and F32"→"I32"), `crates/strider-analyze/src/pattern/{mod,macros}.rs`
- Modify: `crates/strider-ir/src/lib.rs:43` and `node_signature.rs:39` doc lines

- [ ] **Step 1: Rename the variant identifiers (scripted, capital-U only)**

The Rust primitive `u8`/`u128` is lowercase; the variants are capital `U8…U512`. Word-boundary regex on capital U is safe. Run from repo root:

```bash
grep -rlZ --include='*.rs' -E '\b(U8|U16|U32|U64|U80|U128|U256|U512)\b' crates/ \
  | xargs -0 sed -i -E 's/\b(U)(8|16|32|64|80|128|256|512)\b/I\2/g'
```

- [ ] **Step 2: Rename the display strings and `:u` dot suffix**

```bash
sed -i -E 's/name: "u(8|16|32|64|80|128|256|512)"/name: "i\1"/' crates/strider-ir/src/node/output_type.rs
```

In `crates/strider-ir/src/function_dot/label.rs` find the `IntConstWide` label (`format!(...:u{bits}...)`) and change `u` to `i`. In `function_dot/tests.rs` change the `:u64` literal assertion to `:i64`. Grep for remaining lowercase doc/test occurrences and fix: `grep -rn '"u8"\|:u64\|:u32\|:u128' crates/`.

- [ ] **Step 3: Review the diff for false hits**

Run `git diff` and confirm no lowercase primitive (`u8`/`u128`) or unrelated capital-U identifier was changed. Pay attention to `wide_const.rs`, `arithmetic.rs`, `output_type.rs`.

- [ ] **Step 4: Build + clippy + test**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p strider-ir`
Expected: PASS. Fix any `as_str`/Display test that still expects `"u…"`.

- [ ] **Step 5: Commit + push**

```bash
git add -A && git commit -m "change: rename integer output types U8..U512 to I8..I512"
git push origin rewrite/int-types
```

---

## Task 2: byte-size→type function (mission 2)

**Files:**
- Modify: `crates/strider-ir/src/node/output_type.rs` (remove `impl TryFrom<u32>`, add `int_for_byte_size`)
- Modify: call sites `crates/strider-lift/src/pcode_lift/vn_io.rs:277,349` and anywhere using `.try_into()`/`NodeOutputType::try_from` for a byte size (grep: `try_into\(\)\?.*ValueType\|NodeOutputType::try_from`)

- [ ] **Step 1: Write failing test** in `output_type.rs` tests:

```rust
#[test]
fn int_for_byte_size_maps_widths() {
    use super::NodeOutputType as T;
    assert_eq!(T::int_for_byte_size(1).unwrap(), T::I8);
    assert_eq!(T::int_for_byte_size(8).unwrap(), T::I64);
    assert_eq!(T::int_for_byte_size(10).unwrap(), T::I80);
    assert_eq!(T::int_for_byte_size(64).unwrap(), T::I512);
    assert!(T::int_for_byte_size(3).is_err());
}
```

- [ ] **Step 2: Run** `cargo test -p strider-ir int_for_byte_size` — Expected: FAIL (no method).

- [ ] **Step 3: Implement** — replace the `impl TryFrom<u32>` block with:

```rust
impl NodeOutputType {
    /// Maps a varnode byte size to the corresponding **integer** output
    /// type: `1→I8, 2→I16, 4→I32, 8→I64, 10→I80, 16→I128, 32→I256, 64→I512`.
    /// Byte size 1 maps to `I8`, never `I1` — `I1` is produced only by
    /// comparisons, not by varnode widths.
    pub fn int_for_byte_size(n: u32) -> crate::error::Result<Self> {
        match n {
            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            4 => Ok(Self::I32),
            8 => Ok(Self::I64),
            10 => Ok(Self::I80),
            16 => Ok(Self::I128),
            32 => Ok(Self::I256),
            64 => Ok(Self::I512),
            n => Err(anyhow::anyhow!("unsupported node output size: {n} bytes")),
        }
    }
}
```

Update the old `try_from_u32_10_is_u80` test to call `int_for_byte_size(10)`.

- [ ] **Step 4: Update call sites** — replace `let t: ValueType = vn.size.try_into()?;` style with `let t = NodeOutputType::int_for_byte_size(vn.size)?;`. Grep `try_into` and `try_from` in `strider-lift` and fix each. Build to find the rest.

- [ ] **Step 5: Build + clippy + test**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p strider-ir && cargo test -p strider-lift`
Expected: PASS.

- [ ] **Step 6: Commit + push**

```bash
git add -A && git commit -m "change: replace TryFrom<u32> for NodeOutputType with int_for_byte_size"
git push origin rewrite/int-types
```

---

## Task 3: Type model — add `I1`, make it integer, first-class `bit_width`

This makes `Bool` an alias-in-spirit for a 1-bit integer at the *type* level first, before touching node kinds. Strategy: rename `Bool`→`I1`, move it into the `Int` category, give `TYPE_INFO` a real `bit_width` column.

**Files:**
- Modify: `crates/strider-ir/src/node/output_type.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn i1_is_a_1bit_integer() {
    use super::NodeOutputType as T;
    assert!(T::I1.is_integer());
    assert!(!T::I1.is_float());
    assert_eq!(T::I1.bit_width(), 1);
    assert_eq!(T::I1.byte_size(), 1);
    assert_eq!(T::I1.bit_mask_u128(), 1);
    assert_eq!(T::I1.get_unsigned_int(0xFF), Some(1));
    assert_eq!(T::I1.as_str(), "i1");
}
```

- [ ] **Step 2: Run** — Expected: FAIL (no `I1`).

- [ ] **Step 3: Implement.** In `output_type.rs`:
  - Rename enum variant `Bool` → `I1` (keep it first).
  - `NodeOutputTypeCategory`: remove `Bool`; now `{ Int, Float }`.
  - Add `bit_width: u16` to `TypeInfo`; set `I1→1, I8→8, …, I512→512, F32→32, F64→64, F80→80`. `TYPE_INFO[0]` becomes `{ name:"i1", byte_size:1, bit_width:1, category:Int }`.
  - `info()` arm `Self::Bool => &TYPE_INFO[0]` becomes `Self::I1 => &TYPE_INFO[0]`.
  - `bit_width()` returns `self.info().bit_width as usize` (no longer `byte_size*8`).
  - `is_integer()` returns true for `I1`. Remove `is_bool()`'s category check; redefine `is_bool()` as `self == Self::I1`.
  - `bit_mask_u128`: delete the `if self.is_bool() { return 1 }` special-case — `(1<<1)-1=1` now falls out.
  - `get_unsigned_int`: delete the Bool exclusion note; `I1` is an integer so it masks normally.
  - `to_natural_int_type`: `I1=>I1`.

- [ ] **Step 4: Run** `cargo test -p strider-ir output_type` — Expected: PASS. Update any test still naming `Bool`.

- [ ] **Step 5: Build the rest of the workspace** — `cargo build --workspace` will now surface every `NodeOutputType::Bool` / `is_bool()` / category-`Bool` use site. Do NOT fix node-kind logic yet; only fix type-level references (rename `Bool`→`I1`). Node-kind collapse is Task 4. Expected: many errors in `node/kind.rs`, `node_signature.rs`, `coerce.rs`, lifter, opt, pattern — these get resolved in Task 4. **If the workspace can't compile cleanly with only type-renames, fold Step 5 into Task 4 (they are one logical change).**

- [ ] **Step 6: Commit + push** (only if green on its own; otherwise commit together with Task 4)

```bash
git add -A && git commit -m "change: model Bool as I1, a 1-bit integer type with first-class bit_width"
git push origin rewrite/int-types
```

> **Note:** Tasks 3 and 4 are likely a single atomic commit because removing the Bool *category* and the Bool *node kinds* must land together to compile. Treat the Task-3 tests as the spec; commit when the whole workspace is green after Task 4.

---

## Task 4: Collapse Bool node kinds into integer ops (mission 4)

Remove `BoolConst`, `BoolBinaryOp`, `BoolUnaryOp`, `CastToBool`, `CastToInt`. Comparisons output `I1`. Logical ops become integer ops at `I1`.

**Files:**
- Modify: `crates/strider-ir/src/node/kind.rs` (remove the 5 variants from `NodeKind` and from every exhaustive match: `is_const`, `is_cacheable`, `asm_fingerprint_exempt`, `is_commutative`)
- Modify: `crates/strider-ir/src/node_signature.rs` (drop `BoolConst/BoolBinaryOp/BoolUnaryOp/CastToBool/CastToInt` signature arms; `IntCmpOp`/`FloatCmpOp` output `I1`)
- Modify: `crates/strider-ir/src/builder/coerce.rs` (DELETE `convert_to_bool_if_needed` and `get_as_bool` entirely — verified that Sleigh never feeds a raw int into a bool context, so no int→bool fold is needed; `convert_to_int_if_needed` drops the CastToInt branch and keys on **bit width** so `I1`→`I8` extends; remove `ConstValue::Bool`, fold to `Int`)
- Modify: `crates/strider-ir/src/builder/nodes.rs` (`build_int_cmp_operation`/`build_float_cmp_op` output `I1`; remove `build_boolean_*`, `build_cast_*`; `build_boolean_const` → `build_int_const(b as u128, I1)`)
- Modify: `crates/strider-lift/src/pcode_lift/value/boolean.rs` (BOOL_AND/OR/XOR → `IntBinaryOp::{And,Or,Xor}` at I1; BOOL_NEGATE → `IntUnaryOp::BitNot` at I1)
- Modify: `crates/strider-lift/src/pcode_lift/value/arithmetic.rs` & `float.rs` (the lowered `BoolNeg(...)` forms → `BitNot(...)` at I1; cmp outputs I1)
- Modify: `crates/strider-analyze/src/strider/insn/control.rs:201` (If condition: pass the value directly — it is already `I1`; `build_if` strict-requires `I1`)
- Modify: `crates/strider-analyze/src/opt/constant_fold/{mod,rules}.rs` (`bool_const_with!`→`int_const_with!` at I1; remove CastToBool/CastToInt round-trip folds at rules.rs:539-562; cmp folds produce `IntConst` at I1)
- Modify: `crates/strider-analyze/src/opt/flag_cmp_canonicalize/mod.rs` (boolean-tree rules match `IntBinaryOp(And/Or/Xor)` + `IntUnaryOp(BitNot)` at I1 instead of bool kinds)
- Modify: `crates/strider-ir/src/function_dot/label.rs` (remove `CastToBool`/`CastToInt` arms; cmp `"→ bool"`→`"→ i1"`; bool-op arms → int-op arms; the hard-coded `bool` labels)
- Modify: `crates/strider-ir/src/walk/cast/mod.rs` (drop `CAST_TO_BOOL`/`CAST_TO_INT` bits + arms)

- [ ] **Step 1: Write failing tests** (new, in `crates/strider-ir/src/builder/tests.rs`):

```rust
#[test]
fn int_cmp_outputs_i1() {
    let mut b = make_empty_fn();
    let a = b.build_int_const(1, NodeOutputType::I32).unwrap();
    let c = b.build_int_const(2, NodeOutputType::I32).unwrap();
    let cmp = b.build_int_cmp_operation(a, c, IntCmpOp::Equal).unwrap();
    assert_eq!(b.get_output_type(cmp).unwrap(), NodeOutputType::I1);
}

#[test]
fn bool_to_int_widening_is_zero_extend_keyed_on_bit_width() {
    // setcc-then-arithmetic: an I1 used at I8 must ZeroExtend, even though
    // I1 and I8 share byte size 1 (helpers must key on bit width).
    let mut b = make_empty_fn();
    let a = b.build_int_const(1, NodeOutputType::I32).unwrap();
    let c = b.build_int_const(2, NodeOutputType::I32).unwrap();
    let flag = b.build_int_cmp_operation(a, c, IntCmpOp::Equal).unwrap(); // I1
    let widened = b.convert_to_int_if_needed(flag, NodeOutputType::I8).unwrap();
    assert_eq!(b.get_output_type(widened).unwrap(), NodeOutputType::I8);
}
```

- [ ] **Step 2: Run** — Expected: FAIL (compile errors; I1/strict not yet in place).

- [ ] **Step 3: Implement** the removals + mappings listed in Files. DELETE
`convert_to_bool_if_needed` and `get_as_bool` (verified unnecessary — Sleigh
never feeds a raw int into a bool context). The `If` condition and logical
ops strict-require `I1`, supplied directly by the producing comparison.
`convert_to_int_if_needed` keys on bit width so `I1`→`I8` widening is a real
`ZeroExtend`:

```rust
// coerce.rs — convert_to_int_if_needed (bit-width keyed; no CastToInt branch)
pub fn convert_to_int_if_needed(
    &mut self, output_id: NodeOutputId, output_type: NodeOutputType,
) -> Result<NodeOutputId> {
    let curr = self.get_output_type(output_id)?;
    // everything is an integer now; compare BIT WIDTH (I1=1 vs I8=8 differ
    // though both are byte_size 1).
    if curr.bit_width() > output_type.bit_width() {
        return self.truncate_if_needed(output_id, output_type);
    }
    self.extend_if_needed(output_id, output_type, ExtendOp::ZeroExtend)
}
```

`truncate_if_needed` / `extend_if_needed` likewise compare `bit_width()`
instead of `byte_size()`.

`convert_to_int_if_needed` loses its trailing `CastToInt` branch (everything is now an integer): truncate-then-zero-extend only. `ConstValue::Bool` is removed; bool constants are `Int { val: 0|1, ty: I1 }`.

- [ ] **Step 4: Run targeted tests** then full per-crate suites:

`cargo test -p strider-ir && cargo test -p strider-lift && cargo test -p strider-analyze`
Expected: PASS. Update/replace tests asserting old Bool kinds (the coercion tests in `builder/tests.rs` listed in the spec; `casts_and_conversions.rs` cast-to-bool/int cases).

- [ ] **Step 5: Build + clippy**

`cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS (exhaustive matches in `kind.rs` force every site to be handled — good).

- [ ] **Step 6: Commit + push**

```bash
git add -A && git commit -m "change: collapse Bool into 1-bit integer ops; remove Bool node kinds and CastToBool/CastToInt"
git push origin rewrite/int-types
```

---

## Task 5: Remove `CastToFloat` → `IntBitsToFloat` (mission 5)

**Files:**
- Modify: `crates/strider-ir/src/node/kind.rs` (remove `CastToFloat` from enum + all exhaustive matches)
- Modify: `crates/strider-ir/src/builder/{nodes.rs,coerce.rs}` (remove `build_cast_to_float`/`cast_to_float_if_needed`; lifter float ops call `build_int_bits_to_float` at the matching width)
- Modify: `crates/strider-lift/src/pcode_lift/value/float.rs` (`process_float_binary_op`/`unary`/`cmp`, `handle_float_float_to_float`, `handle_float_trunc`: read each operand at its own varnode width and `IntBitsToFloat` it; `build_float_cmp_op` casts each operand per-operand, not lhs-inferred)
- Modify: `crates/strider-analyze/src/opt/constant_fold/mod.rs` (remove `try_lower_cast_to_float` and its rule)
- Modify: `crates/strider-ir/src/function_dot/{label.rs:253,mod.rs:78}` (remove `CastToFloat` arms)
- Modify: `crates/strider-ir/src/walk/cast/mod.rs` (drop `CAST_TO_FLOAT` bit + arm)

- [ ] **Step 1: Write failing test** in `crates/strider-lift` or `builder/tests.rs`:

```rust
#[test]
fn float_add_operands_are_int_bits_to_float_not_cast() {
    // build a F32 add from two I32 reads; operands must be IntBitsToFloat
    let mut b = make_empty_fn();
    let x = b.build_int_const(0x3f800000, NodeOutputType::I32).unwrap();
    let y = b.build_int_const(0x40000000, NodeOutputType::I32).unwrap();
    let xf = b.build_int_bits_to_float(x, NodeOutputType::F32).unwrap();
    let yf = b.build_int_bits_to_float(y, NodeOutputType::F32).unwrap();
    let add = b.build_float_binary_op(xf, yf, FloatBinaryOp::Add, NodeOutputType::F32).unwrap();
    assert_eq!(b.get_output_type(add).unwrap(), NodeOutputType::F32);
}
```

- [ ] **Step 2: Run** — Expected: FAIL if `build_float_binary_op` still requires/inserts CastToFloat or after CastToFloat removal compile error.

- [ ] **Step 3: Implement** — in `float.rs`, each `cast_to_float_if_needed(raw, ty)` call becomes `self.builder.build_int_bits_to_float(raw_at_width, ty)` where `raw_at_width` is the int read of the operand's own varnode size (already the case). For `process_float_cmp_op`, bitcast lhs and rhs independently to the float type matching each one's width. Remove the CastToFloat node kind and its lowering rule.

- [ ] **Step 4: Test** — `cargo test -p strider-ir && cargo test -p strider-lift && cargo test -p strider-analyze`. Regenerate the cross-arch snapshot if a float function's histogram changed (`cargo insta review` if available, else `cargo test ... -- --nocapture` and update the `.snap`).

- [ ] **Step 5: Build + clippy** — `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 6: Commit + push**

```bash
git add -A && git commit -m "change: remove CastToFloat; lifter uses IntBitsToFloat for float operand casts"
git push origin rewrite/int-types
```

---

## Task 6: Strict node-construction builders (mission 3)

Make `build_int_binary_operation`, `build_int_unary_operation`, `build_int_cmp_operation`, `build_popcount`, `build_lzcount`, `build_float_binary_op`, `build_float_unary_op`, `build_float_cmp_op` **require** correctly-typed inputs (error otherwise) instead of implicitly coercing. The lifter must already pass correct types (Tasks 4–5 moved the conversions into the lifter); this task removes the safety-net coercion and adds the strict checks.

**Files:**
- Modify: `crates/strider-ir/src/builder/nodes.rs` (replace implicit `convert_to_int_if_needed`/`cast_*` calls with `require_*` type checks)
- Modify: `crates/strider-lift/src/pcode_lift/value/{arithmetic,float,integer,boolean}.rs` and `vn_io.rs` (insert explicit `truncate_if_needed`/`extend_if_needed`/`build_int_bits_to_float` before each strict `build_*` call where operand widths may mismatch)
- Modify: `crates/strider-ir/src/builder/tests.rs` (the `*_coerces_*` / `*_auto_casts` tests become "errors on mismatched type" tests)

- [ ] **Step 1: Write failing test** (strict behavior):

```rust
#[test]
fn build_int_binary_errors_on_mismatched_operand_width() {
    let mut b = make_empty_fn();
    let a = b.build_int_const(1, NodeOutputType::I8).unwrap();
    let c = b.build_int_const(2, NodeOutputType::I64).unwrap();
    // strict: operands must already match output_type
    assert!(b.build_int_binary_operation(a, c, IntBinaryOp::Add, NodeOutputType::I64).is_err());
}
```

- [ ] **Step 2: Run** — Expected: FAIL (currently coerces, returns Ok).

- [ ] **Step 3: Implement** — in each `build_*`, replace coercion with a `require_value_type(input, output_type)` check that errors on mismatch. Then make the lifter explicitly fix up operand widths before calling (it reads each varnode at a known width and inserts `truncate_if_needed`/`extend_if_needed` to reach `output_type`). Build will point at every lifter site that relied on implicit coercion.

- [ ] **Step 4: Test** — `cargo test -p strider-ir && cargo test -p strider-lift && cargo test -p strider-analyze`. The always-on validator + cross-arch shape test guard soundness: any missed fixup now errors loudly.

- [ ] **Step 5: Build + clippy.**

- [ ] **Step 6: Commit + push**

```bash
git add -A && git commit -m "change: strict node builders; lifter inserts all width fixups explicitly"
git push origin rewrite/int-types
```

---

## Task 7: Pattern DSL — boolean queries by output width (mission 6)

**Files:**
- Modify: `crates/strider-analyze/src/pattern/pat/builders/` (generalize the `bit_width` post-match closure from `memory.rs` into an output-bit-width constraint on value `NodePat`)
- Modify: `crates/strider-analyze/src/pattern/pat/ctor/` (remove `bool_*` ctors in `bool_.rs`; remove `cast_to_bool/int/float` in `casts.rs`; keep `int_cmp`/`float_cmp` producing I1)
- Modify: `crates/strider-analyze/src/pattern/matcher/cast_mask.rs` & `crates/strider-ir/src/walk/cast/mod.rs` (already trimmed in Task 4/5; confirm no dangling bits)
- Modify: `crates/strider-py/src/pattern.rs` (remove `bool_const/any_bool_const/bool_not/bool_binary/bool_bin_any/bool_un_any`, `cast_to_*` `unary!` lines, `PyBoolBinaryPat`, `PyCastMask::cast_to_*`; add an `output_bit_width`/`.bool()` method mirror); `matcher.rs` `Match.bool` accessor → reads I1 value
- Modify: `crates/strider-analyze/tests/pattern_matching/casts_and_conversions.rs` (rewrite cast tests; add a width==1 query test)

- [ ] **Step 1: Write failing test** (Rust pattern):

```rust
#[test]
fn output_width_one_matches_comparison_result() {
    // build an int_cmp (I1) and a plain I32 add; the width==1 filter
    // matches only the comparison.
    // ... build graph, then:
    let pat = any_value().output_bit_width(1); // new API
    let matches = Matcher::new(&pat).find_all(&func, entry);
    // exactly the cmp node matches
}
```

- [ ] **Step 2: Run** — Expected: FAIL (no `output_bit_width`).

- [ ] **Step 3: Implement** the generalized width filter + remove the bool/cast ctors. Mirror in `strider-pattern-macros`/`strider-py`.

- [ ] **Step 4: Test** — `cargo test -p strider-analyze`, then Python: `cd crates/strider-py && uv sync --group dev && uv run maturin develop && uv run pytest`. Update Python tests that used `bool_*`/`cast_to_*`.

- [ ] **Step 5: Build + clippy.**

- [ ] **Step 6: Commit + push**

```bash
git add -A && git commit -m "change: query booleans via output-width filter; drop bool/cast pattern builders"
git push origin rewrite/int-types
```

---

## Task 8: Fix the AArch64 FP soundness note + test (mission 7)

**Files:**
- Modify: `crates/strider-lift/src/pcode_lift/vn_io.rs:288-298` (rewrite comment)
- Modify: `crates/strider-lift/src/pcode_lift/vn_io.rs:604-617` (convert ignored test to positive)

- [ ] **Step 1: Rewrite the comment** to explain it's sound because Sleigh emits explicit upper-bits zeroing (`Copy #0` on AArch64, `IntZext` on x86 VEX) as separate pcode ops that the lifter processes as ordinary sub-register writes.

- [ ] **Step 2: Convert the ignored test** into a positive test: lift `fmov s0, w0` (bytes `00 00 27 1e`) on `SleighArch::aarch64()` through the value lifter and assert the resulting `s0` container value has its upper bytes zeroed (the lifted IR contains the zero-writes from the `register(...) = Copy(#0)` ops). Remove `#[ignore]`.

- [ ] **Step 3: Test** — `cargo test -p strider-lift aarch64_scalar_fp`. Expected: PASS.

- [ ] **Step 4: Commit + push**

```bash
git add -A && git commit -m "fix: correct obsolete AArch64 scalar-FP soundness note; assert Sleigh's explicit zeroing"
git push origin rewrite/int-types
```

---

## Task 9: Docs + final sweep

**Files:**
- Modify: `CLAUDE.md` (NodeOutputType list: `I1…I512`; remove `Bool`, `CastToInt`/`CastToBool`/`CastToFloat` from the node-kind inventory; update the "Boolean" and "Bitcasts"/"Generic float cast" sections; update lift-time canonicalisation notes referencing `BoolNeg`)
- Modify: `README` if it mentions these types
- Modify: design/plan docs cross-links if needed

- [ ] **Step 1: Update CLAUDE.md** node-kind and type sections to match the new model.

- [ ] **Step 2: Final full verification**

`cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p strider-ir && cargo test -p strider-lift && cargo test -p strider-analyze && cargo test -p strider-target && cargo test -p strider-reader`
Then `cd crates/strider-py && uv run maturin develop && uv run pytest`.
Expected: all green; no NEW failures vs the documented baseline.

- [ ] **Step 3: Commit + push**

```bash
git add -A && git commit -m "docs: update CLAUDE.md for I-typed ints, I1 booleans, removed cast kinds"
git push origin rewrite/int-types
```

---

## Self-review notes

- **Spec coverage:** mission 1→Task1, 2→Task2, 4→Task3+4, 5→Task5, 3→Task6, 6→Task7, 7→Task8, docs→Task9. All covered.
- **Ordering rationale:** type model (3) before node-kind collapse (4); cast removals (4,5) before strict builders (6) so the lifter's final conversion vocabulary exists before the safety net is removed; pattern/py (7) after the IR is final; #7 note (8) independent.
- **Soundness guards:** the always-on asm-fingerprint + local-typing validator and the cross-arch shape snapshot run every phase; strict builders convert silent miscoercion into hard errors.
- **Risk:** Task 4 is the largest (atomic with Task 3). FlagCmpCanonicalize rule rewrite is the trickiest sub-part — verify its tests pass before moving on.
