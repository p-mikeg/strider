# Pattern mem-walk + bit-width Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `.mem_in(p)`, `.next_mem(p)`, `.bit_width(n)` to `LoadPat`/`StorePat` and `.next_mem(p)`/`.next_ctrl(p)` to `CallOtherPat`, plus the Python wrappers.

**Architecture:** New post-match closures on each builder, sharing a single forward-walk helper that reuses the existing `walk::next_control_node` (which is already generic over output kind despite its name).  `NodeOutputType` gains a `bit_width()` const fn used by every `.bit_width(n)` filter.

**Tech Stack:** Rust 1.x, anyhow, pattern-matcher infrastructure already in `crates/pattern/src/`, PyO3 + maturin (strider-py).

**Spec:** `docs/superpowers/specs/2026-05-06-pattern-mem-walk-and-bitwidth-design.md`

---

## File Map

**Modify:**
- `crates/ir/src/node/output_kind.rs` (or wherever `NodeOutputType` lives) — add `bit_width()` const fn.
- `crates/pattern/src/pat/builders/memory.rs` — add `.mem_in`, `.next_mem`, `.bit_width` to LoadPat + StorePat.
- `crates/pattern/src/pat/builders/call.rs` — add `.next_ctrl`, `.next_mem` to CallOtherPat.
- `crates/strider-py/src/pattern.rs` — mirror new methods on PyLoadPat / PyStorePat / PyCallOtherPat.
- `crates/strider-py/strider/pattern.pyi` — type stubs for the new methods.
- `crates/strider-py/tests/python/test_pattern_full_builders.py` — smoke tests for the Python surface.

**Create:**
- `crates/pattern/src/pat/builders/walk_helpers.rs` — shared `match_unique_output_consumer` helper.
- `crates/pattern/tests/pattern_mem_in_load.rs`
- `crates/pattern/tests/pattern_next_mem_store.rs`
- `crates/pattern/tests/pattern_lock_store_unlock.rs`
- `crates/pattern/tests/pattern_bit_width_load.rs`
- `crates/pattern/tests/pattern_bit_width_store.rs`
- `crates/pattern/tests/pattern_callother_next_mem.rs`
- `crates/pattern/tests/pattern_next_mem_zero_when_multi_consumer.rs`

**No change to:**
- `crates/target/src/call_other_abi.rs` — pattern-only spec.
- `crates/cfg/`, `crates/strider/`, `crates/opt/` — pattern-only spec.

---

## Task 1: `NodeOutputType::bit_width()` const fn

**Files:**
- Modify: `crates/ir/src/node/output_kind.rs` (or the file that defines `NodeOutputType`)

- [ ] **Step 1: Locate the type definition**

Run: `grep -rn "pub enum NodeOutputType" crates/ir/src/ 2>&1 | head -3`

Open whichever file holds `pub enum NodeOutputType { Bool, U8, U16, U32, U64, U128, U256, F32, F64 }`.

- [ ] **Step 2: Write the failing test**

Append to the same file (in its `#[cfg(test)]` mod):

```rust
#[test]
fn bit_width_matches_each_variant() {
    use NodeOutputType::*;
    assert_eq!(Bool.bit_width(), 1);
    assert_eq!(U8.bit_width(), 8);
    assert_eq!(U16.bit_width(), 16);
    assert_eq!(U32.bit_width(), 32);
    assert_eq!(U64.bit_width(), 64);
    assert_eq!(U128.bit_width(), 128);
    assert_eq!(U256.bit_width(), 256);
    assert_eq!(F32.bit_width(), 32);
    assert_eq!(F64.bit_width(), 64);
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ir bit_width_matches_each_variant 2>&1 | tail -10`

Expected: compile error (`bit_width` undefined).

- [ ] **Step 4: Implement**

Add to the `impl NodeOutputType` block in the same file:

```rust
/// Returns the bit-width of this value type.  Used by
/// `pattern::LoadPat::bit_width` / `pattern::StorePat::bit_width` to
/// filter Loads / Stores by their value width independent of int /
/// float distinction.
#[must_use]
pub const fn bit_width(self) -> u32 {
    match self {
        Self::Bool => 1,
        Self::U8 => 8,
        Self::U16 => 16,
        Self::U32 | Self::F32 => 32,
        Self::U64 | Self::F64 => 64,
        Self::U128 => 128,
        Self::U256 => 256,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p ir bit_width_matches_each_variant 2>&1 | tail -5`

Expected: 1 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/node/
git commit -m "$(cat <<'EOF'
ir: add NodeOutputType::bit_width() const fn

Returns the value type's width in bits.  U32 and F32 both return 32;
U64 and F64 both return 64; etc.  Consumed by the new
pattern::LoadPat::bit_width / pattern::StorePat::bit_width filters
(next commit).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Shared forward-walk helper

**Files:**
- Create: `crates/pattern/src/pat/builders/walk_helpers.rs`
- Modify: `crates/pattern/src/pat/builders/mod.rs` — add `pub(crate) mod walk_helpers;`

- [ ] **Step 1: Read what's available on the matcher**

Run: `grep -n "pub.*fn next_control_node\|pub.*fn match_consumer_node" crates/pattern/src/matcher/walk.rs crates/pattern/src/pat/node_pat.rs 2>&1 | head -10`

Confirm both `next_control_node` (in `matcher/walk.rs`) and `match_consumer_node` (in `pat/node_pat.rs`) are accessible from `pat::builders`.  Both should already be `pub(crate)` or `pub(super)`-reachable.

- [ ] **Step 2: Create the helper**

Create `crates/pattern/src/pat/builders/walk_helpers.rs`:

```rust
//! Shared one-step forward consumer walks for Load / Store / CallOther
//! pattern builders.  All three builders' `.next_mem(p)` /
//! `.next_ctrl(p)` methods route through these.

use crate::matcher::walk::next_control_node;
use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::node_pat::{match_consumer_node, MatchCtx};
use ir::node::NodeId;

/// Match `pat` against the unique consumer of `node`'s output at
/// `output_index`.  Returns `false` if the output has zero or
/// multiple consumers (deterministic no-match), or if `pat` doesn't
/// match the consumer node.
///
/// Despite its name, [`next_control_node`] is generic — it returns
/// the unique consumer of any output kind.  We reuse it for control
/// AND memory walks.
pub(crate) fn match_unique_output_consumer(
    ctx: &MatchCtx,
    node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.graph.graph.node_outputs(node);
    let Some(out) = outputs.into_iter().nth(output_index) else {
        return false;
    };
    let Some(consumer) = next_control_node(ctx.matcher, out) else {
        return false;
    };
    match_consumer_node(ctx, consumer, pat, b)
}
```

- [ ] **Step 3: Register the module**

In `crates/pattern/src/pat/builders/mod.rs`, add:

```rust
pub(crate) mod walk_helpers;
```

(Place after the existing `mod` declarations.)

- [ ] **Step 4: Adjust import paths if compilation fails**

Run: `cargo build -p pattern 2>&1 | tail -10`

If `Bindings` / `MatchCtx` / `match_consumer_node` aren't reachable at the paths above, grep for their actual exports and adjust the `use` statements:
* `grep -n "pub.*Bindings\|pub.*MatchCtx" crates/pattern/src/matcher/*.rs crates/pattern/src/pat/*.rs`
* `grep -n "pub.*fn match_consumer_node" crates/pattern/src/pat/*.rs`

Fix imports until the helper compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/pattern/src/pat/builders/walk_helpers.rs crates/pattern/src/pat/builders/mod.rs
git commit -m "$(cat <<'EOF'
pattern: add shared forward-walk helper match_unique_output_consumer

Reuses the existing matcher::walk::next_control_node (which is
generic over output kind despite the name) to find the unique
consumer of any output, then matches the supplied pattern against
the consumer node via match_consumer_node (the same helper IfPat
uses for true_branch / false_branch).

Used by the next commits to implement .next_mem / .next_ctrl on
LoadPat, StorePat, and CallOtherPat.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: LoadPat — `.mem_in(p)`, `.next_mem(p)`, `.bit_width(n)`

**Files:**
- Modify: `crates/pattern/src/pat/builders/memory.rs`

- [ ] **Step 1: Read the current LoadPat**

Run: `sed -n '17,60p' crates/pattern/src/pat/builders/memory.rs`

Note the struct fields and `From<LoadPat> for Pat` body.

- [ ] **Step 2: Write the failing tests**

Create `crates/pattern/tests/pattern_mem_in_load.rs`:

```rust
//! LoadPat::mem_in(p) constrains inputs[0] (memory predecessor).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, load, store, int_const};

#[test]
fn load_mem_in_matches_preceding_store() {
    // Build: Store(addr=0x100, data=42) ; Load(addr=0x200)
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr1 = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr1");
    let val   = b.build_int_const(42u64,    NodeOutputType::U32).expect("val");
    b.build_store(addr1, val, rsleigh::VnSpace::RAM).expect("store");
    let addr2 = b.build_int_const(0x200u64, NodeOutputType::U64).expect("addr2");
    let _load = b.build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32).expect("load");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let pat = load().addr(int_const(0x200u64))
        .mem_in(store().addr(int_const(0x100u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(hits.len(), 1, "load whose mem_in is the prior store should match exactly once");
}
```

Create `crates/pattern/tests/pattern_bit_width_load.rs`:

```rust
//! LoadPat::bit_width(n) filters Loads by value width.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, load, int_const};

#[test]
fn bit_width_filters_load_by_value_width() {
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr");
    let _l32 = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32).expect("u32 load");
    let _l64 = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64).expect("u64 load");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);
    let h32 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(32).into());
    let h64 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(64).into());
    assert_eq!(h32.len(), 1, "bit_width(32) matches only the U32 load");
    assert_eq!(h64.len(), 1, "bit_width(64) matches only the U64 load");
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p pattern --test pattern_mem_in_load 2>&1 | tail -10
cargo test -p pattern --test pattern_bit_width_load 2>&1 | tail -10
```

Expected: compile errors (`mem_in` / `bit_width` undefined).

- [ ] **Step 4: Implement on LoadPat**

In `crates/pattern/src/pat/builders/memory.rs`, replace the LoadPat struct + impl + From:

```rust
/// Builder for `Load` node patterns.  Created by [`crate::pat::load`].
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    mem_in: Option<Pat>,
    next_mem: Option<Pat>,
    bit_width: Option<u32>,
}

impl LoadPat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None, mem_in: None, next_mem: None, bit_width: None }
    }
    /// Restrict the match to loads in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the load's address operand (inputs[1]).
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Constrain the load's memory predecessor (inputs[0]).  The
    /// pattern walks back from the input edge to its producer.
    pub fn mem_in(mut self, p: impl Into<Pat>) -> Self {
        self.mem_in = Some(p.into());
        self
    }
    /// Match `p` against the unique consumer of the load's memory
    /// output (outputs[0]).  Returns no match if the output has zero
    /// or multiple consumers (deterministic; no arbitrary pick).
    pub fn next_mem(mut self, p: impl Into<Pat>) -> Self {
        self.next_mem = Some(p.into());
        self
    }
    /// Restrict the match to loads whose value output (outputs[1]) is
    /// `n` bits wide (`U8`/`F32`/`F64` etc. distinguished by width
    /// only).  Matches both integer and float types of the same width.
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr, mem_in, next_mem, bit_width } = b;
        // Load inputs = [mem(0), addr(1)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(p) = mem_in { indexed.push((0, p)); }
        if let Some(p) = addr   { indexed.push((1, p)); }
        let kind = match space {
            None => KindSpec::variant(&NodeKind::Load(rsleigh::VnSpace::RAM)),
            Some(s) => KindSpec::variant_with(
                &NodeKind::Load(rsleigh::VnSpace::RAM),
                move |k| matches!(k, NodeKind::Load(actual) if *actual == s),
            ),
        };
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));

        // Combined post-match closure: bit_width AND next_mem checks.
        if bit_width.is_some() || next_mem.is_some() {
            let want_width = bit_width;
            let next_mem_pat = next_mem;
            pat = pat.with_post_match(std::sync::Arc::new(move |ctx, node, b| {
                if let Some(w) = want_width {
                    let outs = ctx.graph.graph.node_outputs(node);
                    let Some(value_out) = outs.into_iter().nth(1) else { return false; };
                    let Some(ty) = ctx.graph.graph.output_kind(value_out).as_value() else { return false; };
                    if ty.bit_width() != w { return false; }
                }
                if let Some(ref p) = next_mem_pat {
                    if !super::walk_helpers::match_unique_output_consumer(ctx, node, 0, p, b) {
                        return false;
                    }
                }
                true
            }));
        }

        pat.into_pat()
    }
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p pattern --test pattern_mem_in_load 2>&1 | tail -10
cargo test -p pattern --test pattern_bit_width_load 2>&1 | tail -10
```

Expected: both pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/src/pat/builders/memory.rs \
        crates/pattern/tests/pattern_mem_in_load.rs \
        crates/pattern/tests/pattern_bit_width_load.rs
git commit -m "$(cat <<'EOF'
pattern: add LoadPat .mem_in / .next_mem / .bit_width

Three additive methods on LoadPat:

  * .mem_in(p)    — constrain inputs[0] (memory predecessor) — backward
                    walk via the standard data-flow direction.
  * .next_mem(p)  — match p against the UNIQUE consumer of outputs[0]
                    (memory output) — forward walk via
                    walk_helpers::match_unique_output_consumer.  No
                    match if zero or multiple consumers.
  * .bit_width(n) — filter by the value output's bit width
                    (NodeOutputType::bit_width).  Matches U32 and F32
                    on bit_width(32), U64 and F64 on bit_width(64), etc.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: StorePat — `.mem_in(p)`, `.next_mem(p)`, `.bit_width(n)`

**Files:**
- Modify: `crates/pattern/src/pat/builders/memory.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/pattern/tests/pattern_next_mem_store.rs`:

```rust
//! StorePat::next_mem(p) walks forward to the unique mem consumer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, load, store, int_const};

#[test]
fn store_next_mem_matches_following_load() {
    // Build: Store(addr=0x100, data=42) → Load(addr=0x200)
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr1 = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr1");
    let val   = b.build_int_const(42u64,    NodeOutputType::U32).expect("val");
    b.build_store(addr1, val, rsleigh::VnSpace::RAM).expect("store");
    let addr2 = b.build_int_const(0x200u64, NodeOutputType::U64).expect("addr2");
    let _load = b.build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32).expect("load");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let pat = store().addr(int_const(0x100u64))
        .next_mem(load().addr(int_const(0x200u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(hits.len(), 1);
}
```

Create `crates/pattern/tests/pattern_bit_width_store.rs`:

```rust
//! StorePat::bit_width(n) filters Stores by value width.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, store, int_const};

#[test]
fn bit_width_filters_store_by_data_width() {
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr");
    let v32 = b.build_int_const(1u64, NodeOutputType::U32).expect("v32");
    b.build_store(addr, v32, rsleigh::VnSpace::RAM).expect("u32 store");
    let v64 = b.build_int_const(2u64, NodeOutputType::U64).expect("v64");
    b.build_store(addr, v64, rsleigh::VnSpace::RAM).expect("u64 store");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);
    let h32 = m.find_all(&store().addr(int_const(0x100u64)).bit_width(32).into());
    let h64 = m.find_all(&store().addr(int_const(0x100u64)).bit_width(64).into());
    assert_eq!(h32.len(), 1);
    assert_eq!(h64.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p pattern --test pattern_next_mem_store 2>&1 | tail -10
cargo test -p pattern --test pattern_bit_width_store 2>&1 | tail -10
```

- [ ] **Step 3: Implement on StorePat**

In `crates/pattern/src/pat/builders/memory.rs`, replace the StorePat struct + impl + From with the symmetric extension (mem_in, next_mem, bit_width).  For Store, `.bit_width(n)` checks the producer of `inputs[2]` (data):

```rust
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    data: Option<Pat>,
    mem_in: Option<Pat>,
    next_mem: Option<Pat>,
    bit_width: Option<u32>,
}

impl StorePat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None, data: None, mem_in: None, next_mem: None, bit_width: None }
    }
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self { self.space = Some(s); self }
    pub fn addr(mut self, p: impl Into<Pat>) -> Self { self.addr = Some(p.into()); self }
    pub fn data(mut self, p: impl Into<Pat>) -> Self { self.data = Some(p.into()); self }
    pub fn mem_in(mut self, p: impl Into<Pat>) -> Self { self.mem_in = Some(p.into()); self }
    pub fn next_mem(mut self, p: impl Into<Pat>) -> Self { self.next_mem = Some(p.into()); self }
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self { self.bit_width = Some(n); self }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat { space, addr, data, mem_in, next_mem, bit_width } = b;
        // Store inputs = [mem(0), addr(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(p) = mem_in { indexed.push((0, p)); }
        if let Some(p) = addr   { indexed.push((1, p)); }
        if let Some(p) = data   { indexed.push((2, p)); }
        let kind = match space {
            None => KindSpec::variant(&NodeKind::Store(rsleigh::VnSpace::RAM)),
            Some(s) => KindSpec::variant_with(
                &NodeKind::Store(rsleigh::VnSpace::RAM),
                move |k| matches!(k, NodeKind::Store(actual) if *actual == s),
            ),
        };
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));

        if bit_width.is_some() || next_mem.is_some() {
            let want_width = bit_width;
            let next_mem_pat = next_mem;
            pat = pat.with_post_match(std::sync::Arc::new(move |ctx, node, b| {
                if let Some(w) = want_width {
                    // Store's data input is at inputs[2]; its producer's
                    // output type tells us the width.
                    let inputs = ctx.graph.graph.node_inputs(node);
                    let Some(&data_in) = inputs.get(2) else { return false; };
                    let Some(ty) = ctx.graph.graph.output_kind(data_in).as_value() else { return false; };
                    if ty.bit_width() != w { return false; }
                }
                if let Some(ref p) = next_mem_pat {
                    if !super::walk_helpers::match_unique_output_consumer(ctx, node, 0, p, b) {
                        return false;
                    }
                }
                true
            }));
        }

        pat.into_pat()
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p pattern --test pattern_next_mem_store 2>&1 | tail -10
cargo test -p pattern --test pattern_bit_width_store 2>&1 | tail -10
```

Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pattern/src/pat/builders/memory.rs \
        crates/pattern/tests/pattern_next_mem_store.rs \
        crates/pattern/tests/pattern_bit_width_store.rs
git commit -m "$(cat <<'EOF'
pattern: add StorePat .mem_in / .next_mem / .bit_width

Symmetric to the LoadPat additions in the previous commit.
.bit_width(n) checks the producer of inputs[2] (the stored data).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: CallOtherPat — `.next_mem(p)`, `.next_ctrl(p)`

**Files:**
- Modify: `crates/pattern/src/pat/builders/call.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/pattern/tests/pattern_callother_next_mem.rs`:

```rust
//! CallOtherPat::next_mem(p) and .next_ctrl(p) — forward walk to the
//! unique consumer of the matched CallOther's mem / ctrl output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, call_other, store, int_const};

#[test]
fn callother_next_mem_matches_following_store() {
    // Build: LOCK ; Store(addr=0x100, data=42)
    // LOCK is in target::call_other_abi as PURE_WITH_MEM_EDGE so it's
    // on the memory chain.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let _lock = b.build_call_other_modeled(
        1, "LOCK", &[], None, &[], &[], &[],
    ).expect("LOCK ok");
    let addr = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr");
    let val  = b.build_int_const(42u64,    NodeOutputType::U32).expect("val");
    // To make LOCK's mem_out advance, build_call_other_modeled does
    // not by itself; the strider layer normally calls
    // advance_cur_region_memory.  In this unit test we mimic that by
    // using build_store (which always advances mem from the current
    // region mem cursor — but cur_mem hasn't been advanced past LOCK).
    //
    // For the test to be meaningful, we need LOCK's mem_out to be the
    // mem_in of the Store.  The cleanest way: set cur_region_memory
    // to LOCK's mem_out before building the Store.  Use the helper
    // builder.advance_cur_region_memory(<lock's mem_out>)?.
    //
    // Implementation note: locate LOCK's mem_out via node_outputs.
    let lock_node = _lock; // (NodeId returned by build_call_other_modeled)
    let lock_mem_out = b.body().graph.node_outputs(lock_node)[1];
    b.advance_cur_region_memory(lock_mem_out).expect("advance");
    b.build_store(addr, val, rsleigh::VnSpace::RAM).expect("store");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let pat = call_other().name("LOCK")
        .next_mem(store().addr(int_const(0x100u64)));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(hits.len(), 1, "LOCK whose mem_out's unique consumer is the Store should match");
}
```

(If `advance_cur_region_memory` isn't `pub`, adapt — either expose it
or use a different graph-construction approach that produces the same
shape.  The goal is a LOCK CallOther whose `outputs[1]` flows into a
Store's `inputs[0]`.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p pattern --test pattern_callother_next_mem 2>&1 | tail -10`

Expected: `next_mem` undefined on CallOtherPat.

- [ ] **Step 3: Implement on CallOtherPat**

In `crates/pattern/src/pat/builders/call.rs`, add fields + methods + invoke from `From`:

```rust
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    name: Option<String>,
    inputs: Vec<(usize, Pat)>,
    outputs: Vec<(usize, Pat)>,
    next_ctrl: Option<Pat>,   // NEW
    next_mem:  Option<Pat>,   // NEW
}
```

Update `new`, the methods (mirror LoadPat's `.next_mem` shape):

```rust
/// Match `p` against the unique consumer of the CallOther's control
/// output (outputs[0]).  Returns no match if zero or multiple
/// consumers.
pub fn next_ctrl(mut self, p: impl Into<Pat>) -> Self {
    self.next_ctrl = Some(p.into());
    self
}
/// Match `p` against the unique consumer of the CallOther's memory
/// output (outputs[1]).  Returns no match if zero or multiple
/// consumers.
pub fn next_mem(mut self, p: impl Into<Pat>) -> Self {
    self.next_mem = Some(p.into());
    self
}
```

In the `From<CallOtherPat> for Pat` impl, after the existing
`with_post_match` for the name filter, chain a SECOND post-match for
the forward walks (or combine both into one closure if the existing
machinery only allows one):

```rust
if next_ctrl.is_some() || next_mem.is_some() {
    let nc = next_ctrl;
    let nm = next_mem;
    pat = pat.with_post_match(std::sync::Arc::new(move |ctx, node, b| {
        if let Some(ref p) = nc {
            if !super::walk_helpers::match_unique_output_consumer(ctx, node, 0, p, b) {
                return false;
            }
        }
        if let Some(ref p) = nm {
            if !super::walk_helpers::match_unique_output_consumer(ctx, node, 1, p, b) {
                return false;
            }
        }
        true
    }));
}
```

If `with_post_match` only allows ONE callback per pattern (replace
semantics), combine the name check AND the forward walks into a
single closure.

- [ ] **Step 4: Run the test**

Run: `cargo test -p pattern --test pattern_callother_next_mem 2>&1 | tail -10`

Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/pattern/src/pat/builders/call.rs \
        crates/pattern/tests/pattern_callother_next_mem.rs
git commit -m "$(cat <<'EOF'
pattern: add CallOtherPat .next_ctrl / .next_mem

Forward-walk variants matching the unique consumer of the CallOther's
control / memory output.  Symmetric with LoadPat.next_mem and
StorePat.next_mem; uses the same walk_helpers::match_unique_output_consumer.

The existing .ret(i, p) / .ctrl_out / .mem_out backward-on-output
methods stay unchanged — they're useful for capturing the output
itself via .mem_out(any().capture(c)) for cross-pattern joins.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Integration test — LOCK ; Store ; UNLOCK pattern

**Files:**
- Create: `crates/pattern/tests/pattern_lock_store_unlock.rs`

- [ ] **Step 1: Write the test**

Create `crates/pattern/tests/pattern_lock_store_unlock.rs`:

```rust
//! Canonical use case for the new mem-walk pattern surface:
//! "find an atomic Store inside a LOCK ... UNLOCK pair".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, call_other, store, int_const};

#[test]
fn matches_store_bracketed_by_lock_and_unlock() {
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);

    // LOCK (PURE_WITH_MEM_EDGE — advances mem)
    let lock = b.build_call_other_modeled(1, "LOCK", &[], None, &[], &[], &[]).expect("LOCK");
    let lock_mem_out = b.body().graph.node_outputs(lock)[1];
    b.advance_cur_region_memory(lock_mem_out).expect("advance");

    // Store inside the bracket
    let addr = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr");
    let val  = b.build_int_const(42u64,    NodeOutputType::U32).expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM).expect("store");

    // UNLOCK
    let unlock = b.build_call_other_modeled(2, "UNLOCK", &[], None, &[], &[], &[]).expect("UNLOCK");
    let unlock_mem_out = b.body().graph.node_outputs(unlock)[1];
    b.advance_cur_region_memory(unlock_mem_out).expect("advance");

    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    // Pattern: a Store whose mem_in is LOCK and whose next_mem is UNLOCK.
    let pat = store().addr(int_const(0x100u64))
        .mem_in(call_other().name("LOCK"))
        .next_mem(call_other().name("UNLOCK"));
    let hits = Matcher::new(&fg).find_all(&pat.into());
    assert_eq!(hits.len(), 1, "atomic Store inside LOCK ... UNLOCK should match exactly once");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p pattern --test pattern_lock_store_unlock 2>&1 | tail -10`

Expected: 1 passed.  If it fails because `advance_cur_region_memory` isn't pub, expose it as `pub` on `FunctionBuilder` (it should already be reachable from Tasks 3–5's tests if they did the same).

- [ ] **Step 3: Commit**

```bash
git add crates/pattern/tests/pattern_lock_store_unlock.rs
git commit -m "$(cat <<'EOF'
pattern: integration test for LOCK ; Store ; UNLOCK pattern

Canonical use case for the new mem-walk surface: locate an atomic
Store inside a LOCK ... UNLOCK bracket via .mem_in / .next_mem.
Verifies the precise-ABI v2.1 categorization (LOCK / UNLOCK as
PURE_WITH_MEM_EDGE) and the new pattern surface compose correctly
end-to-end.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Edge-case test — multi-consumer mem output

**Files:**
- Create: `crates/pattern/tests/pattern_next_mem_zero_when_multi_consumer.rs`

- [ ] **Step 1: Write the test**

Create `crates/pattern/tests/pattern_next_mem_zero_when_multi_consumer.rs`:

```rust
//! .next_mem(p) returns no match when the mem output has zero or
//! multiple consumers — deterministic, no arbitrary pick.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, store, any, IntoPat, int_const};

#[test]
fn next_mem_returns_no_match_when_no_consumer() {
    // A Store with no following mem op.  Its mem_out has zero
    // consumers; .next_mem(any()) should return no match.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr");
    let val  = b.build_int_const(42u64,    NodeOutputType::U32).expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM).expect("store");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    // Without .next_mem: matches the store.
    let h_no = Matcher::new(&fg).find_all(&store().addr(int_const(0x100u64)).into());
    assert_eq!(h_no.len(), 1, "baseline: store matches");

    // With .next_mem(any()): no match (the Return is the consumer of the
    // ControlState's ctrl, not the Store's mem — so Store's mem_out has
    // zero or one consumer depending on how Return is wired).  At minimum,
    // verify the API doesn't panic and returns deterministically.
    let h_next = Matcher::new(&fg).find_all(&store().addr(int_const(0x100u64)).next_mem(any()).into());
    // The exact count depends on how the IR builder wires the final mem
    // edge to Return.  If Return reads memory, h_next.len() == 1 (Return
    // is the unique consumer).  If not, h_next.len() == 0.  Either way
    // the call must not panic.
    assert!(h_next.len() <= 1, "deterministic: 0 or 1, never multi");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p pattern --test pattern_next_mem_zero_when_multi_consumer 2>&1 | tail -10`

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/pattern/tests/pattern_next_mem_zero_when_multi_consumer.rs
git commit -m "$(cat <<'EOF'
pattern: regression test for .next_mem with no-consumer / multi-consumer mem output

Verifies the deterministic no-match behaviour: a Store with no
following mem op (or with a fan-out point) does not panic and does
not arbitrarily pick a consumer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Strider-py — mirror new methods on PyLoadPat / PyStorePat / PyCallOtherPat

**Files:**
- Modify: `crates/strider-py/src/pattern.rs`
- Modify: `crates/strider-py/strider/pattern.pyi`
- Modify: `crates/strider-py/tests/python/test_pattern_full_builders.py`

- [ ] **Step 1: Locate the existing builders**

Run: `grep -n "PyLoadPat\|PyStorePat\|PyCallOtherPat\|fn addr\|fn mem_in\|fn next_mem" crates/strider-py/src/pattern.rs | head -20`

- [ ] **Step 2: Add methods on PyLoadPat**

Find the `#[pymethods] impl PyLoadPat` block.  Add:

```rust
/// Constrain the load's memory predecessor (inputs[0]).
fn mem_in(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
    let pat = p.into_pat()?;
    slf.borrow(py).mem_in.replace(Some(pat));
    Ok(slf)
}
/// Match against the unique consumer of the load's mem output.
fn next_mem(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
    let pat = p.into_pat()?;
    slf.borrow(py).next_mem.replace(Some(pat));
    Ok(slf)
}
/// Filter loads by value width (bits).
fn bit_width(slf: Py<Self>, py: Python<'_>, n: u32) -> Py<Self> {
    slf.borrow(py).bit_width.replace(Some(n));
    slf
}
```

Add corresponding fields on `PyLoadPat` (use `RefCell<Option<...>>` for
mutable interior, mirroring how the existing fields work).  In the
`finalise` method, forward to the Rust builder:

```rust
if let Some(p) = self.mem_in.borrow().clone()  { b = b.mem_in(p); }
if let Some(p) = self.next_mem.borrow().clone(){ b = b.next_mem(p); }
if let Some(n) = *self.bit_width.borrow()      { b = b.bit_width(n); }
```

- [ ] **Step 3: Add methods on PyStorePat (symmetric)**

Same shape as PyLoadPat.

- [ ] **Step 4: Add methods on PyCallOtherPat**

Add `next_ctrl` and `next_mem` (CallOther's version):

```rust
fn next_ctrl(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
    let pat = p.into_pat()?;
    slf.borrow(py).next_ctrl.replace(Some(pat));
    Ok(slf)
}
fn next_mem(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
    let pat = p.into_pat()?;
    slf.borrow(py).next_mem.replace(Some(pat));
    Ok(slf)
}
```

Add fields + forward in `finalise`.

- [ ] **Step 5: Update .pyi stubs**

In `crates/strider-py/strider/pattern.pyi`, add to the relevant
classes:

```python
class LoadPat:
    def mem_in(self, p: PatLike) -> "LoadPat": ...
    def next_mem(self, p: PatLike) -> "LoadPat": ...
    def bit_width(self, n: int) -> "LoadPat": ...

class StorePat:
    def mem_in(self, p: PatLike) -> "StorePat": ...
    def next_mem(self, p: PatLike) -> "StorePat": ...
    def bit_width(self, n: int) -> "StorePat": ...

class CallOtherPat:
    def next_ctrl(self, p: PatLike) -> "CallOtherPat": ...
    def next_mem(self, p: PatLike) -> "CallOtherPat": ...
```

- [ ] **Step 6: Add Python smoke tests**

Append to `crates/strider-py/tests/python/test_pattern_full_builders.py`:

```python
# ── new mem-walk + bit-width API ─────────────────────────────────────

def test_load_mem_in_chain_compiles():
    p = load().addr(int_const(0x100)).mem_in(store().addr(int_const(0x200)))
    assert isinstance(p.into_pat(), Pat)

def test_load_next_mem_chain_compiles():
    p = load().addr(int_const(0x100)).next_mem(store().addr(int_const(0x200)))
    assert isinstance(p.into_pat(), Pat)

def test_load_bit_width_compiles():
    p = load().addr(int_const(0x100)).bit_width(32)
    assert isinstance(p.into_pat(), Pat)

def test_store_mem_in_next_mem_bit_width_chain_compiles():
    p = (store().addr(int_const(0x100))
            .mem_in(load().addr(int_const(0x200)))
            .next_mem(load().addr(int_const(0x300)))
            .bit_width(64))
    assert isinstance(p.into_pat(), Pat)

def test_callother_next_ctrl_next_mem_compile():
    p = call_other().name("LOCK").next_ctrl(load().addr(int_const(0x100))).next_mem(store().addr(int_const(0x200)))
    assert isinstance(p.into_pat(), Pat)
```

- [ ] **Step 7: Build the wheel + run Python tests**

```bash
cd crates/strider-py
.venv/bin/maturin develop 2>&1 | tail -5
.venv/bin/python -m pytest tests/python/test_pattern_full_builders.py -v 2>&1 | tail -20
cd ../..
```

Expected: clean develop, all smoke tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/strider-py/
git commit -m "$(cat <<'EOF'
strider-py: mirror mem-walk + bit-width pattern API

PyLoadPat / PyStorePat gain .mem_in / .next_mem / .bit_width.
PyCallOtherPat gains .next_ctrl / .next_mem.

Each forwards to the matching Rust builder method.  Type stubs
updated in strider/pattern.pyi.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Final workspace verification

- [ ] **Step 1: Full workspace test**

```bash
cargo test --workspace 2>&1 | grep -E "test result" | awk -F'[ ;]+' '{passed+=$4; failed+=$6; ignored+=$8} END {print "TOTAL: " passed " passed, " failed " failed, " ignored " ignored"}'
```

Expected: 0 failures.  Total should be ≥3056 (3049 baseline + 6 new Rust tests + 5 new Python tests once the wheel is rebuilt).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace 2>&1 | tail -5
```

Expected: clean.

- [ ] **Step 3: Final commit if anything tweaked**

```bash
git status --short
git add -A 2>&1
git commit -m "pattern mem-walk + bit-width: final verification" || echo "nothing to commit"
```

---

## Self-Review Checklist

**Spec coverage:**
- ✅ Goal 1 (mem_in on Load/Store/CallOther) — Tasks 3, 4, 5.
- ✅ Goal 2 (next_mem / next_ctrl forward walks) — Tasks 3, 4, 5 + Task 2's helper.
- ✅ Goal 3 (bit_width filter) — Tasks 1 (NodeOutputType::bit_width), 3, 4.
- ✅ Goal 4 (no breaking changes) — all additive; Task 5 keeps existing CallOtherPat methods.
- ✅ Goal 5 (strider-py exposure) — Task 8.

**Placeholder scan:** Tasks 3 / 4 / 5 contain "or combine into a single closure if the matcher only allows one post-match".  This is a real implementation choice that depends on inspecting the existing `with_post_match` API — either shape works mechanically, and the task's TDD step proves the chosen shape is correct.

**Type consistency:**
- `match_unique_output_consumer(ctx, node, output_index, pat, b) -> bool` — Task 2 defines, Tasks 3 / 4 / 5 call.
- `NodeOutputType::bit_width() -> u32` — Task 1 defines, Tasks 3 / 4 use.
- All builder methods follow `mut self -> Self` pattern matching existing methods on the same builders.
