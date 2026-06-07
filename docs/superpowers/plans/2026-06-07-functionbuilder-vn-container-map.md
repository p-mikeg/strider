# FunctionBuilder vn-container map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `FunctionBuilder::new` the single owner of varnode (vn) canonicalization — seed, dedup, deterministic sort, and a persisted `vn → largest-container` map on `Function` — and route every calling-convention-vs-tracked comparison through that map so an ABI register narrower than its tracked container (e.g. CC says `eax`, function tracks `rax`) resolves correctly.

**Architecture:** Today the lifter (`find_all_unique_vns`) sorts the vn list for deterministic `VarId`, `FunctionBuilder::new` seeds CC regs + dedups overlapping vns, and a builder-lifetime `OnceCell<HashMap<Vn,Vn>>` (`largest_container`) caches tracked-vn→container for the register-aliasing read/write paths. Two `Function` methods (`call_ret_vals_for`, `call_clobbered_for`) compare CC registers to tracked vns by **exact equality**, silently dropping/mis-filing a ret-val whose ABI width differs from the tracked container. This plan moves the sort into the builder, persists the container map on `Function` (computed once in `new`, keyed over the original pre-dedup set ∪ all CC registers), exposes a `Function::container_of` resolver, deletes the builder `OnceCell`, removes the lifter's sort, and fixes the two CC-derivation functions to resolve through `container_of`.

**Tech Stack:** Rust workspace (sea-of-nodes IR in `strider-ir`, lifter in `strider-lift`, target descriptions in `strider-target`, PyO3 bindings in `strider-py`). `rsleigh::Vn` is a varnode (`addr_space`, `addr_off: u64`, `size: u32`). `rustc_hash::FxHashMap`. Tests: `cargo test -p <crate>`, full gate `cargo test --workspace` + `cargo clippy --workspace --all-targets`, then rebuild `.so` + `uv run pytest`.

---

## File Structure

- **`crates/strider-ir/src/function/data.rs`** — add `vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>` field; add it as the 4th `Function::new` parameter; add the `container_of(&self, vn) -> rsleigh::Vn` resolver (map hit → on-the-fly containment scan of `all_vns` → self); route `call_ret_vals_for` / `call_clobbered_for` through `container_of`.
- **`crates/strider-ir/src/builder/mod.rs`** — in `new`: sort the deduped vn list deterministically (new private `vn_sort_key`), build the `vn_to_container` map over `(all_used_variables ∪ cc.callee_saved_regs)`, pass it to `Function::new`; delete the `largest_container` `OnceCell` field and `largest_container_for`.
- **`crates/strider-ir/src/builder/nodes.rs`** — `build_entry` must preserve+restore `vn_to_container` across the in-place `Function::new` reset (like `all_vns`).
- **`crates/strider-ir/src/builder/vn_io.rs`** — `find_largest_fitting_register` routes through `self.function.container_of` instead of the deleted `largest_container_for`.
- **`crates/strider-lift/src/lift/mod.rs`** — `find_all_unique_vns` stops sorting; drop the `pub use pcode_util::vn_sort_key` re-export.
- **`crates/strider-lift/src/lift/pcode_util.rs`** — delete `vn_sort_key` (no remaining caller).
- **`CLAUDE.md`** — update the `strider-ir` Function-state description, the `FunctionBuilder` contract, and the "Register Aliasing" section.

---

### Task 1: Deterministic vn sort owned by FunctionBuilder

Move ordering ownership from the lifter into the builder so `all_vns` is deterministic regardless of input order, then strip the lifter's sort.

**Files:**
- Modify: `crates/strider-ir/src/builder/mod.rs` (the `dedup_overlapping_largest` region ~63-81 and `new` ~224-236)
- Test: `crates/strider-ir/src/builder/tests.rs`
- Modify: `crates/strider-lift/src/lift/mod.rs:165-177`
- Modify: `crates/strider-lift/src/lift/pcode_util.rs:23-25` and `crates/strider-lift/src/lift/mod.rs:30`

- [ ] **Step 1: Write the failing test** (in `crates/strider-ir/src/builder/tests.rs`, near the other builder tests)

```rust
/// `FunctionBuilder::new` is the SSoT for vn ordering: the tracked
/// `all_vns` set must come out sorted by (space, offset, size)
/// regardless of the order the vns were handed in, so `VarId`
/// assignment (and every derived clobber-slot index) is deterministic.
#[test]
fn function_builder_sorts_all_vns_deterministically() -> Result<()> {
    // Three disjoint registers handed in OUT of sorted order.
    let r_hi = reg_vn(0x40, 8);
    let r_lo = reg_vn(0x10, 8);
    let r_mid = reg_vn(0x20, 8);
    let sp = reg_vn(0x7000, 8);
    let b = raw_builder(
        vec![r_hi, r_mid, r_lo],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let got: Vec<rsleigh::Vn> = b.function().all_vns().to_vec();
    let mut expected = got.clone();
    expected.sort_by_key(|v| (v.addr_space.shortcut_raw(), v.addr_off, v.size));
    assert_eq!(got, expected, "all_vns must be sorted by (space, off, size)");
    Ok(())
}
```

> Note: `all_vns()` is an existing public accessor on `Function`; if it is not present, add `pub fn all_vns(&self) -> &[rsleigh::Vn] { &self.all_vns }` to `crates/strider-ir/src/function/data.rs` in this step. `raw_builder` and `reg_vn` are the existing test helpers in `builder/tests.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir function_builder_sorts_all_vns_deterministically`
Expected: FAIL — `all_vns` currently preserves insertion order (the lifter sorted upstream; the builder does not), so the shuffled input stays shuffled.

- [ ] **Step 3: Add the sort in `FunctionBuilder::new`**

In `crates/strider-ir/src/builder/mod.rs`, add a private free function next to `dedup_overlapping_largest` (after line 81):

```rust
/// Deterministic ordering key for a tracked varnode: `(space, offset,
/// size)`.  Sorting `all_vns` by this in `FunctionBuilder::new` makes
/// `VarId` assignment — and every derived clobber-slot index — stable
/// regardless of the order varnodes were collected from the CFG.  The
/// builder owns this so the lifter need not pre-sort.
fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size)
}
```

Then in `FunctionBuilder::new`, change line 224 from:

```rust
        let all_variables = dedup_overlapping_largest(&all_used_variables);
```

to:

```rust
        let mut all_variables = dedup_overlapping_largest(&all_used_variables);
        // FunctionBuilder owns vn ordering: sort the deduped tracked set
        // by (space, offset, size) so VarId assignment is deterministic
        // independent of CFG-collection order.  The lifter no longer sorts.
        all_variables.sort_by_key(|v| vn_sort_key(v));
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-ir function_builder_sorts_all_vns_deterministically`
Expected: PASS

- [ ] **Step 5: Remove the lifter's now-redundant sort**

In `crates/strider-lift/src/lift/mod.rs`, the `find_all_unique_vns` body (lines 165-177) ends with:

```rust
    let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
    vns.sort_unstable_by_key(crate::lift::pcode_util::vn_sort_key);
    vns
```

Replace with (collection order is now irrelevant — the builder sorts):

```rust
    // Ordering is owned by `FunctionBuilder::new`, which sorts the tracked
    // set deterministically; the lifter only needs to hand over the unique
    // used-varnode set.
    all_vns.into_iter().collect()
```

Adjust the surrounding `let mut vns` / return so the function returns the `collect()` directly (drop the now-unused `mut`).

Then delete the re-export at `crates/strider-lift/src/lift/mod.rs:30`:

```rust
pub use pcode_util::vn_sort_key;
```

and delete the `vn_sort_key` function in `crates/strider-lift/src/lift/pcode_util.rs` (lines 23-25). Fix the two doc comments that reference `crate::lift::pcode_util::vn_sort_key` (`function_lifter.rs:36`, `lift/mod.rs:203`) to say "sorted deterministically by `FunctionBuilder::new`" instead.

- [ ] **Step 6: Run the affected crates' tests**

Run: `cargo test -p strider-ir -p strider-lift`
Expected: PASS (no failures). If a strider-lift test asserted on `find_all_unique_vns` ordering, it still holds because the builder re-sorts by the same key.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir/src/builder/mod.rs crates/strider-ir/src/builder/tests.rs \
        crates/strider-lift/src/lift/mod.rs crates/strider-lift/src/lift/pcode_util.rs \
        crates/strider-lift/src/lift/function_lifter.rs
git commit -m "refactor(ir): FunctionBuilder::new owns deterministic vn sort

Move the tracked-varnode ordering from the lifter's find_all_unique_vns
into FunctionBuilder::new (sort the deduped set by (space, off, size)).
The lifter no longer sorts; vn_sort_key is deleted from strider-lift.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Persist the `vn → largest-container` map on `Function`

Add the map field + `Function::container_of` resolver, build the map in `FunctionBuilder::new` over the original pre-dedup set plus every CC-referenced register, and preserve it across the `build_entry` reset.

**Files:**
- Modify: `crates/strider-ir/src/function/data.rs` (struct field ~82, `Function::new` ~198-209, new accessor)
- Modify: `crates/strider-ir/src/builder/mod.rs` (`new` ~242-243)
- Modify: `crates/strider-ir/src/builder/nodes.rs` (`build_entry` ~23-27)
- Test: `crates/strider-ir/src/builder/tests.rs`

- [ ] **Step 1: Write the failing test** (in `crates/strider-ir/src/builder/tests.rs`)

```rust
/// `Function::container_of` resolves a sub-register query to its tracked
/// largest container, so a calling convention that names `eax` (4 bytes)
/// while the function tracks `rax` (8 bytes) maps correctly.  A vn that
/// is its own container maps to itself; a vn with no tracked container
/// maps to itself.
#[test]
fn container_of_resolves_subregister_to_tracked_container() -> Result<()> {
    // x86-64-style: tracked rax at offset 0 size 8; eax is its low 4 bytes.
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    // Hand in BOTH rax and eax; dedup keeps rax (container), and the map
    // records eax -> rax.
    let b = raw_builder(
        vec![rax, eax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let f = b.function();
    assert_eq!(f.container_of(&eax), rax, "eax must resolve to its rax container");
    assert_eq!(f.container_of(&rax), rax, "rax is its own container");
    // An untracked register with no tracked container maps to itself.
    let r9 = reg_vn(0x90, 8);
    assert_eq!(f.container_of(&r9), r9, "untracked, uncontained -> self");
    Ok(())
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir container_of_resolves_subregister_to_tracked_container`
Expected: FAIL — `Function::container_of` does not exist yet (compile error: no method named `container_of`).

- [ ] **Step 3: Add the field + `Function::new` param + accessor**

In `crates/strider-ir/src/function/data.rs`, add a field after `all_vns` (line 82):

```rust
    /// `original vn → its largest tracked container` map. Domain: every
    /// varnode in the pre-dedup tracked set *plus* every register the
    /// calling convention names (arg / ret / float-ret / stack /
    /// callee-saved), so a CC register narrower than its tracked container
    /// (ABI says `eax`, function tracks `rax`) resolves to the container.
    /// Codomain: an element of `all_vns`, or the key itself when no wider
    /// tracked vn contains it. Computed once in `FunctionBuilder::new`.
    /// Plain `rsleigh::Vn` keys/values (no arena ids), so [`Self::compact`]
    /// leaves it untouched. Read through [`Self::container_of`].
    pub(crate) vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
```

Change `Function::new` (lines 198-209) to take the map:

```rust
    pub fn new(
        default_cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
        all_vns: Vec<rsleigh::Vn>,
        vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
    ) -> Self {
        Self {
            default_cc,
            endianness,
            all_vns,
            vn_to_container,
            ..Self::default()
        }
    }
```

Add the resolver as a method on `Function` (place it near `all_vns` accessors):

```rust
    /// Resolve `vn` to its largest tracked container.
    ///
    /// Fast path: the precomputed [`Self::vn_to_container`] map (covers
    /// every original tracked vn + every CC register). Fallback: an
    /// on-the-fly containment scan of `all_vns` for ad-hoc varnodes not in
    /// the map (synthetic/test vns). When nothing tracked contains `vn`,
    /// returns `vn` unchanged. Containment is offset-range inclusion in the
    /// same address space; non-REGISTER/UNIQUE spaces resolve to `vn`.
    pub fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        if let Some(c) = self.vn_to_container.get(vn) {
            return *c;
        }
        if vn.addr_space != rsleigh::VnSpace::REGISTER
            && vn.addr_space != rsleigh::VnSpace::UNIQUE
        {
            return *vn;
        }
        let start = vn.addr_off;
        let end = start.saturating_add(u64::from(vn.size));
        let mut best: Option<rsleigh::Vn> = None;
        for cand in &self.all_vns {
            if cand.addr_space != vn.addr_space {
                continue;
            }
            let cs = cand.addr_off;
            let ce = cs.saturating_add(u64::from(cand.size));
            if cs > start || ce < end {
                continue;
            }
            if best.is_none_or(|b| b.size < cand.size) {
                best = Some(*cand);
            }
        }
        best.unwrap_or(*vn)
    }
```

> `FxHashMap` is already imported in `data.rs` (used by `value_vn` etc.). `is_none_or` is the same idiom already used in `vn_io.rs:118`.

- [ ] **Step 4: Build the map in `FunctionBuilder::new` and pass it through**

In `crates/strider-ir/src/builder/mod.rs`, after `all_variables` is sorted (Task 1) and `all_vns` is snapshotted (line 236), build the map. Replace the `Function::new(cc.clone(), endianness, all_vns)` call (line 243) region with:

```rust
        // Build the vn->container map: domain is every original (pre-dedup)
        // tracked vn PLUS every CC-referenced register (arg / ret / float
        // ret / stack / callee-saved), each resolved to its largest
        // container within `all_vns` (or itself). This lets every CC-reg-vs
        // -tracked comparison and every register read/write resolve a
        // narrower ABI register (`eax`) onto its tracked container (`rax`).
        // Canonicalization is meaningful ONLY for REGISTER / UNIQUE space:
        // those behave like fixed-offset registers where containment-by-
        // offset applies. CONST is left to the graph's structural dedup
        // cache, and RAM (load/store) is deliberately not deduped. So the
        // domain is filtered to aliasable-space vns.
        let mut vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn> =
            FxHashMap::default();
        let domain = all_used_variables
            .iter()
            .chain(cc.callee_saved_regs.iter())
            .copied()
            .filter(|v| is_aliasable_space(v.addr_space));
        for vn in domain {
            let container = largest_container_in(&all_vns, &vn);
            vn_to_container.insert(vn, container);
        }
        // Every aliasable tracked container maps to itself (covers all_vns
        // reg/unique entries not already inserted via the original-set
        // domain). Non-aliasable (const / RAM) tracked vns stay out of the
        // map; `container_of` resolves them to themselves via its space gate.
        for vn in &all_vns {
            if is_aliasable_space(vn.addr_space) {
                vn_to_container.entry(*vn).or_insert(*vn);
            }
        }

        let mut fb = FunctionBuilder {
            function: Function::new(cc.clone(), endianness, all_vns, vn_to_container),
            var_table,
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
            lift_addr: None,
        };
```

(Note the removal of the `largest_container: std::cell::OnceCell::new(),` initializer — that field is deleted in Task 3. If implementing Task 2 before Task 3, keep the initializer for now and remove it in Task 3. To keep this task self-contained and compiling, leave the `largest_container` field+initializer in place here and delete it in Task 3.)

Add the shared containment helper next to `vn_sort_key` in `builder/mod.rs`:

```rust
/// Largest varnode in `tracked` (same space, offset-range inclusion) that
/// fully contains `vn`, or `vn` itself when none does. Shared by the
/// `vn_to_container` map construction in `FunctionBuilder::new`.
fn largest_container_in(tracked: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if vn.addr_space != rsleigh::VnSpace::REGISTER
        && vn.addr_space != rsleigh::VnSpace::UNIQUE
    {
        return *vn;
    }
    let start = vn.addr_off;
    let end = start.saturating_add(u64::from(vn.size));
    let mut best: Option<rsleigh::Vn> = None;
    for cand in tracked {
        if cand.addr_space != vn.addr_space {
            continue;
        }
        let cs = cand.addr_off;
        let ce = cs.saturating_add(u64::from(cand.size));
        if cs > start || ce < end {
            continue;
        }
        if best.is_none_or(|b| b.size < cand.size) {
            best = Some(*cand);
        }
    }
    best.unwrap_or(*vn)
}
```

> This duplicates the scan in `Function::container_of`'s fallback. That is intentional and acceptable: the builder helper operates on a `&[Vn]` slice during construction (no `Function` yet), and the `Function` method serves post-build callers. Keep both; they share the documented containment rule.

- [ ] **Step 5: Preserve the map across the `build_entry` reset**

In `crates/strider-ir/src/builder/nodes.rs`, `build_entry` (lines 23-27) currently takes `default_cc` / `all_vns` / `endianness` and rebuilds `Function`. Add the map to the preserve+restore:

```rust
        let default_cc = std::mem::take(&mut self.function.default_cc);
        let all_vns = std::mem::take(&mut self.function.all_vns);
        let vn_to_container = std::mem::take(&mut self.function.vn_to_container);
        let endianness = self.function.endianness;
        self.function =
            crate::function::Function::new(default_cc, endianness, all_vns, vn_to_container);
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p strider-ir container_of_resolves_subregister_to_tracked_container`
Expected: PASS. Also run `cargo test -p strider-ir` — the new `Function::new` 4th param compiles at both builder sites; `Function::default()` still yields an empty map.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir/src/function/data.rs crates/strider-ir/src/builder/mod.rs \
        crates/strider-ir/src/builder/nodes.rs crates/strider-ir/src/builder/tests.rs
git commit -m "feat(ir): persist vn->container map on Function

FunctionBuilder::new builds a vn->largest-container map over the original
pre-dedup tracked set plus every CC register, stored on Function and read
via Function::container_of (map hit, else all_vns containment scan, else
self). Resolves narrow ABI regs (eax) onto tracked containers (rax).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Route register aliasing through `Function::container_of`; delete the builder `OnceCell`

The persisted map supersedes the builder-lifetime `largest_container` cache.

**Files:**
- Modify: `crates/strider-ir/src/builder/vn_io.rs` (`find_largest_fitting_register` ~83-123)
- Modify: `crates/strider-ir/src/builder/mod.rs` (delete `largest_container` field ~114-119, delete `largest_container_for` ~327-413, delete its initializer in `new`)
- Test: `crates/strider-ir/src/builder/tests.rs` (existing read/write-reg tests are the regression net)

- [ ] **Step 1: Write the failing test** (in `crates/strider-ir/src/builder/tests.rs`)

```rust
/// Reading a sub-register when only the wider container is tracked routes
/// through `Function::container_of` (the persisted map), shifting/masking
/// out of the container. Pins that the read path no longer depends on the
/// deleted builder-lifetime `largest_container` cache.
#[test]
fn read_subregister_routes_through_container_map() -> Result<()> {
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    let mut b = raw_builder(
        vec![rax, eax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    // Reading eax must succeed and produce an I32-typed value (truncated
    // out of the I64 rax container) — i.e. container resolution worked.
    let v = b.read_reg_vn(&eax)?;
    assert_eq!(
        b.function().value_type(v).unwrap(),
        strider_ir::node::ValueType::I32,
        "eax read yields I32 sliced from the rax container",
    );
    Ok(())
}
```

> `value_type` is an existing `IRViewer` accessor. If the exact assertion API differs, assert via the existing helpers used by the current `read_reg_vn` tests in this file (mirror their shape).

- [ ] **Step 2: Run test to verify it fails (or passes spuriously) — confirm current behavior**

Run: `cargo test -p strider-ir read_subregister_routes_through_container_map`
Expected: PASS today (the OnceCell path already resolves this). This test is the **regression guard** for the refactor — keep it; it must stay green after the OnceCell is deleted.

- [ ] **Step 3: Re-point `find_largest_fitting_register` at the persisted map**

In `crates/strider-ir/src/builder/vn_io.rs`, replace the body of `find_largest_fitting_register` (lines 83-123) with:

```rust
    pub(crate) fn find_largest_fitting_register(
        &self,
        reg: &rsleigh::Vn,
    ) -> Result<rsleigh::Vn> {
        let space = reg.addr_space;
        if space != rsleigh::VnSpace::REGISTER && space != rsleigh::VnSpace::UNIQUE {
            bail!("unsupported varnode space {space:?}");
        }
        // The persisted `Function::container_of` covers every tracked vn +
        // CC register (fast map hit) and falls back to an `all_vns`
        // containment scan for ad-hoc vns. It returns `reg` unchanged when
        // nothing tracked contains it — which for this caller means the reg
        // is its own container (a legitimate full-width access).
        Ok(self.function.container_of(reg))
    }
```

> Behavior note: the old slow path returned an error ("no enclosing container") only when `best` was `None` *and* the reg wasn't tracked. With `container_of` returning `reg` itself in that case, the caller (`read_reg_vn`/`write_reg_vn`) treats `reg` as its own container — the same outcome as a tracked full-width register, which is correct (every vn contains itself). If any existing test asserts the *error* message for a genuinely-unmappable vn, locate it (grep `has no enclosing container`) and update it to assert the self-resolution instead; there are no production callers that depend on the error.

- [ ] **Step 4: Delete the builder `OnceCell` field and `largest_container_for`**

In `crates/strider-ir/src/builder/mod.rs`:
- Delete the `largest_container` field + its doc (lines ~114-119).
- Delete the `largest_container_for` method (lines ~327-413).
- Remove the `largest_container: std::cell::OnceCell::new(),` initializer from `FunctionBuilder::new` (if still present from Task 2).
- If `FxHashMap` / `std::cell::OnceCell` imports become unused, remove them (`FxHashMap` is still used by the Task-2 map construction, so keep it; `OnceCell` likely becomes unused — remove its `use`).

- [ ] **Step 5: Run tests**

Run: `cargo test -p strider-ir`
Expected: PASS, including `read_subregister_routes_through_container_map` and every existing `read_reg_vn`/`write_reg_vn`/aliasing test (`builder/tests.rs`, `builder/vn_io.rs` tests).

- [ ] **Step 6: Clippy the crate**

Run: `cargo clippy -p strider-ir --all-targets`
Expected: clean (no unused-import / dead-code warnings from the deletions).

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir/src/builder/vn_io.rs crates/strider-ir/src/builder/mod.rs \
        crates/strider-ir/src/builder/tests.rs
git commit -m "refactor(ir): aliasing reads use Function::container_of; drop builder OnceCell

find_largest_fitting_register now resolves through the persisted
Function::container_of map; the builder-lifetime largest_container OnceCell
and largest_container_for are deleted (single source of truth).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Fix `call_ret_vals_for` / `call_clobbered_for` to resolve CC regs through `container_of`

This closes the exact-match bug: a CC ret register narrower than its tracked container is currently dropped from the ret-val group and mis-filed as a clobber.

**Files:**
- Modify: `crates/strider-ir/src/function/data.rs` (`call_ret_vals_for` ~295-316, `call_clobbered_for` ~332-359)
- Test: `crates/strider-ir/src/function/` test module that already exercises these (grep `call_ret_vals_for` in tests; otherwise add to `crates/strider-ir/src/builder/tests.rs`)

- [ ] **Step 1: Write the failing test** (in `crates/strider-ir/src/builder/tests.rs`)

```rust
/// A calling convention whose ret-val register is a SUB-register (`eax`)
/// of a tracked container (`rax`) must still classify the container as the
/// return value — not silently drop it (call_ret_vals_for) or mis-file it
/// as a clobber (call_clobbered_for). Pins the container_of routing.
#[test]
fn cc_subregister_ret_reg_resolves_to_tracked_container() -> Result<()> {
    use strider_target::BuiltCallingConvention;
    let rax = reg_vn(0x0, 8);
    let eax = reg_vn(0x0, 4);
    let sp = reg_vn(0x7000, 8);
    // Build a CC whose ret reg is eax (sub-register), no args, nothing
    // callee-saved, stack = sp. Function tracks rax (container) + sp.
    let cc = BuiltCallingConvention::try_new(
        /* arg_passing_regs */ vec![],
        /* callee_saved_regs */ vec![],
        /* ret_val_regs */ vec![eax],
        /* ret_val_regs_float */ vec![],
        /* stack_vn */ sp,
        /* stack_arg_offsets */ vec![],
        /* ret_stack_pop */ 0,
        /* link_register_vn */ None,
        /* preserves_memory */ false,
    )?;
    // Track rax + sp (rax used by the body; eax is the CC view of it).
    let b = raw_builder(
        vec![rax],
        &[],
        &[],
        &[],
        Some(sp),
        0,
        strider_target::Endianness::Little,
    )?;
    // Re-seed: ensure the function's CC for the derivation is `cc`.
    let f = b.function();
    let ret_vals = f.call_ret_vals_for(&cc);
    assert_eq!(ret_vals, vec![rax], "eax ret reg resolves to its rax container");
    let clobbers = f.call_clobbered_for(&cc);
    assert!(
        !clobbers.contains(&rax),
        "the rax return register must not also appear as a clobber",
    );
    Ok(())
}
```

> The exact `BuiltCallingConvention::try_new` argument order is in `crates/strider-target/src/calling_convention/mod.rs:844-854` — match it precisely (verify before writing). If `raw_builder` does not let you set the derivation CC independently, build the function with `cc` as its default CC (mirror how other `builder/tests.rs` tests that call `call_ret_vals_for` set up their CC) so `f.all_vns()` contains `rax` and `sp`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-ir cc_subregister_ret_reg_resolves_to_tracked_container`
Expected: FAIL — `call_ret_vals_for` returns `[]` (because `tracked.contains(eax)` is false) and `call_clobbered_for` returns `[rax]` (because `ret_vars = {eax}` does not exclude `rax`).

- [ ] **Step 3: Resolve CC regs through `container_of` in both functions**

In `crates/strider-ir/src/function/data.rs`, change `call_ret_vals_for` (lines 310-315) from:

```rust
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .copied()
            .filter(|v| tracked.contains(v) && is_clobbered(v))
            .collect()
```

to (resolve each CC ret reg onto its tracked container, then test membership/clobber on the container, and emit the container):

```rust
        cc.ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| self.container_of(v))
            .filter(|c| tracked.contains(c) && is_clobbered(c))
            .collect()
```

> `is_clobbered` tests `!callee_saved.contains(v) && *v != stack_vn`. `callee_saved` and `stack_vn` are CC-side values; resolving the ret reg to its container `c` and testing `is_clobbered(c)` is correct because the container is the tracked vn the Call output represents. For the built-in presets `container_of(RAX) == RAX`, so the result is byte-identical to today.

Change `call_clobbered_for` (lines 346-358). The `ret_vars` exclusion set must hold *containers* so it matches the `all_vns` containers being iterated:

```rust
        // Resolve the CC ret regs onto their tracked containers so the
        // exclusion matches the containers iterated from `all_vns`
        // (a sub-register ret reg like `eax` excludes its container `rax`).
        let ret_vars: FxHashSet<rsleigh::Vn> = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .map(|v| self.container_of(v))
            .collect();
        self.all_vns
            .iter()
            .copied()
            .filter(|v| is_clobbered(v) && !ret_vars.contains(v))
            .collect()
```

> `is_clobbered` here also consults `callee_saved`. For full symmetry a CC callee-saved sub-register would need the same `container_of` treatment; the `callee_saved` set could be resolved through `container_of` the same way (`cc.callee_saved_regs.iter().map(|v| self.container_of(v)).collect()`). Apply that same mapping to the `callee_saved` set construction in BOTH `call_ret_vals_for` and `call_clobbered_for` so callee-saved sub-registers also resolve. Built-in presets are unaffected (their callee-saved lists are already full-width containers).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-ir cc_subregister_ret_reg_resolves_to_tracked_container`
Expected: PASS

- [ ] **Step 5: Run the broader IR + target + lift tests**

Run: `cargo test -p strider-ir -p strider-target -p strider-lift`
Expected: PASS — built-in presets produce identical clobber/ret partitions (`container_of` is identity on full-width regs).

- [ ] **Step 6: Commit**

```bash
git add crates/strider-ir/src/function/data.rs crates/strider-ir/src/builder/tests.rs
git commit -m "fix(ir): resolve CC regs through container_of in call derivations

call_ret_vals_for and call_clobbered_for now map each CC register onto its
tracked container before membership/exclusion, so a sub-register ABI reg
(eax) whose tracked container is wider (rax) is classified correctly
instead of being dropped from ret-vals and mis-filed as a clobber.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Docs + full workspace gate

**Files:**
- Modify: `CLAUDE.md`
- Verify: whole workspace + Python

- [ ] **Step 1: Update CLAUDE.md**

In the `strider-ir` Function-state paragraph (the part describing `default_cc` + `all_vns` as the convention SSoT), add the new map. Replace the sentence describing the side-table registry / convention SSoT to mention:

```
The convention SSoT is `default_cc` + `all_vns`, plus a `vn_to_container`
map (every original / CC-referenced varnode → its largest tracked
container) read via `Function::container_of`; clobber / ret-val reads go
through `default_cc` and resolve each CC register onto its container, so a
narrower ABI register (`eax`) maps onto the tracked container (`rax`).
```

In the **Register Aliasing** section, replace the description of the builder-cached `find_largest_fitting_register` with: ordering, dedup, and the `vn → container` map are all built once in `FunctionBuilder::new`; the read/write paths and the CC derivations resolve through the persisted `Function::container_of` (there is no builder-lifetime `largest_container` cache, and the lifter no longer sorts vns).

- [ ] **Step 2: Full Rust workspace test**

Run: `cargo test --workspace`
Expected: 102 suites pass, 0 failures (matches the pre-change baseline).

- [ ] **Step 3: Full workspace clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 4: Rebuild the Python extension and run pytest**

Run:
```bash
cargo build -p strider-py
cp target/debug/libstrider_py.so crates/strider-py/strider/strider.abi3.so
cd crates/strider-py && uv run pytest -q
```
Expected: all pytest pass (799 baseline, unless a test legitimately needs updating for the CC-derivation fix — none expected, since presets are unaffected).

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: FunctionBuilder owns vn canonicalization + container map

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- (a) seed ret/call vns → already present in `new` (lines 212-222); the map's domain explicitly includes all CC regs incl. callee-saved (Task 2). ✓
- (b) remove contained vns → existing `dedup_overlapping_largest`, unchanged. ✓
- (c) sort for deterministic VarId → moved into `new` (Task 1). ✓
- (d) `vn → container` map on `Function`, used whenever working with the CC → Tasks 2-4. ✓
- "lifter shouldn't know it needs to sort / SSoT / dedup / mapping" → Task 1 removes the lifter sort; all canonicalization now in `new`. ✓
- eax→rax everywhere the CC meets tracked vns → Task 4 routes both derivations + Task 3 routes read/write through `container_of`. ✓

**Placeholder scan:** no TBD/TODO; every code step shows full code. The two "verify before writing" notes (the `BuiltCallingConvention::try_new` arg order, and the `all_vns()` accessor existence) are explicit verification instructions, not deferred work.

**Type consistency:** `Function::new` gains a 4th param `vn_to_container: FxHashMap<rsleigh::Vn, rsleigh::Vn>` and both call sites (`builder/mod.rs:243`, `builder/nodes.rs` `build_entry`) are updated. `container_of(&self, &rsleigh::Vn) -> rsleigh::Vn` is referenced consistently in Tasks 3 and 4. `vn_sort_key`/`largest_container_in` are private free fns in `builder/mod.rs`. No method renamed mid-plan.

**Known cross-task ordering:** Task 2 leaves the `largest_container` `OnceCell` in place (so it compiles standalone) and Task 3 deletes it — explicitly called out in Task 2 Step 4.
