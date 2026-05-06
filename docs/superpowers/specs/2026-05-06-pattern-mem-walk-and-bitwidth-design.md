# Pattern surface: mem-chain walk + bit-width filter

Add three families of pattern-builder methods so callers can express
queries that follow the IR memory chain through Load / Store / CallOther
nodes and constrain Load / Store by their value bit-width.

## Motivation

After the v2.1 categorization (`docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`),
markers like `LOCK`, `UNLOCK`, `DataMemoryBarrier`, etc. are CallOther
nodes on the IR memory chain.  But the pattern surface today exposes
no concise way to walk that chain through Store / Load:

* `LoadPat` only has `.space(s)` and `.addr(p)`.
* `StorePat` only has `.space(s)`, `.addr(p)`, `.data(p)`.
* `CallOtherPat` has `.arg(i, p)` / `.ret(i, p)` (slot-indexed,
  backward-only) and aliases `.ctrl / .mem / .ctrl_out / .mem_out`.

To express "find a Store inside a `LOCK` … `UNLOCK` pair" today, the
caller has to capture the Store's mem input/output via `.arg(0, …)` /
`.mem_out(any().capture(c))` and then run a separate find-all on the
captured outputs.  Doable but indirect.

A second gap: callers cannot constrain Load / Store by the **value
bit-width**.  Bsdfinder offset patterns often want to disambiguate
"this is a 4-byte pointer load" vs "this is a 1-byte byte load" of the
same address — strider's IR carries the type
(`NodeOutputKind::OutputType(NodeOutputType)`) but the pattern surface
doesn't expose a width filter.

## Goals

1. **Backward mem-input constraints on Load / Store / CallOther.**
   `.mem_in(p)` constrains the memory input slot (input 0 for
   Load/Store, input 1 for CallOther) — the pattern walks back from
   the input edge to its producer in the standard data-flow direction.

2. **Forward mem-output / ctrl-output walk on Load / Store / CallOther.**
   `.next_mem(p)` matches `p` against the **unique consumer** of the
   memory output.  `.next_ctrl(p)` does the same for the control
   output.  Returns no match if the output has zero or multiple
   consumers (deterministic; no arbitrary pick).

3. **Bit-width filter on Load / Store.**
   `.bit_width(n)` constrains the matched node's value width:
   * Load: width of the value output (`outputs[1]`'s
     `NodeOutputType::bit_width()`).
   * Store: width of the data input's producer
     (`inputs[2]`'s `NodeOutputType::bit_width()`).
   Matches by width regardless of int/float distinction (so
   `.bit_width(32)` matches both `U32` and `F32` values).

4. **No breaking changes** to existing pattern surface.  All new
   methods are additive on the existing builders.  `.arg(i, p)` /
   `.ret(i, p)` semantics on `CallOtherPat` are preserved
   (backward-on-output).  `.mem_out(p)` on `CallOtherPat` keeps its
   current backward-on-output meaning; `.next_mem(p)` is the new
   forward-walk variant.

5. **Strider-py exposure.**  Every new method is mirrored on the
   Python wrappers (`LoadPat`, `StorePat`, `CallOtherPat`).

## Non-goals

* **Multi-consumer forward walk.**  `.next_mem(p)` returns no match
  when the output has multiple consumers (e.g. memory fans out at a
  join point).  A future `.any_mem_consumer(p)` could iterate; not in
  this spec.
* **Cross-region forward walks.**  `.next_mem(p)` only follows
  immediate consumers; it does not traverse `MemPhi` / `ControlState`
  joins by default.  (The existing
  `MatcherOptions::ignore_control_states` flag, used by IfPat for ctrl
  walks, is not extended to mem walks here.)
* **Bit-width on `CallOther`'s pcode-explicit value output.**  CallOther
  may have a value output but it's rarely the focus of width queries.
  Skip; revisit if a real consumer surfaces.
* **A new `Pure` enum variant** or any change to `target::call_other_abi`.
  This spec is purely pattern-surface.

## Architectural facts

* `walk::next_control_node(matcher, output) -> Option<NodeId>` already
  exists in `crates/pattern/src/matcher/walk.rs`.  Despite the name
  it's generic over output kind: it returns the unique consumer node
  of any output, or `None` if zero or multiple consumers.  Reuse for
  mem-output walks.
* `IfPat` already implements forward consumer walks via
  `match_branch_consumer` (in `crates/pattern/src/pat/builders/branch.rs`),
  which calls `next_control_node` then `match_consumer_node`.
  Pattern repeats for the new builders.
* `Load` node layout: inputs `[mem(0), addr(1)]`, outputs `[mem(0), value(1)]`.
* `Store` node layout: inputs `[mem(0), addr(1), data(2)]`, outputs `[mem(0)]`.
* `CallOther` node layout (post-v2): inputs `[ctrl(0), mem(1), *args, *implicit_reads]`,
  outputs `[ctrl(0), mem(1), value?, *clobbers]`.
* `NodeOutputType` enum (`Bool`, `U8`, `U16`, `U32`, `U64`, `U128`, `U256`,
  `F32`, `F64`) has no `bit_width()` method today; needs adding (one match
  arm per variant).
* `NodeOutputKind::as_value() -> Option<NodeOutputType>` already exists
  ([`crates/ir/src/node/`]) — used to extract the type from an output.

## Design

### `NodeOutputType::bit_width`

```rust
// In crates/ir/src/node/...
impl NodeOutputType {
    /// Returns the bit-width of this value type.
    /// `Bool=1`, `U8/F32/F64=8/32/64` etc.
    #[must_use]
    pub const fn bit_width(self) -> u32 {
        match self {
            Self::Bool => 1,
            Self::U8   => 8,
            Self::U16  => 16,
            Self::U32 | Self::F32 => 32,
            Self::U64 | Self::F64 => 64,
            Self::U128 => 128,
            Self::U256 => 256,
        }
    }
}
```

Used by every `.bit_width(n)` post-match check.

### Forward-walk helper (shared)

In `crates/pattern/src/pat/builders/walk_helpers.rs` (new file):

```rust
//! Shared one-step forward consumer walks for Load / Store / CallOther
//! pattern builders.  All three builders' `.next_mem(p)` /
//! `.next_ctrl(p)` methods route through these.

use crate::matcher::walk::next_control_node;
use crate::pat::node_pat::match_consumer_node;
use crate::pat::Pat;
use crate::matcher::bindings::Bindings;
use crate::pat::node_pat::MatchCtx;
use ir::node::NodeId;

/// Match `pat` against the unique consumer of `node`'s output at
/// `output_index`.  Returns `false` if the output has zero or multiple
/// consumers, or if `pat` doesn't match the consumer node.
pub(crate) fn match_unique_output_consumer(
    ctx: &MatchCtx,
    node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.graph.graph.node_outputs(node);
    let Some(out) = outputs.into_iter().nth(output_index) else { return false };
    let Some(consumer) = next_control_node(ctx.matcher, out) else { return false };
    match_consumer_node(ctx, consumer, pat, b)
}
```

Both `LoadPat` and `StorePat` call this with `output_index=0` (mem)
for `.next_mem`.  For `Load` there's no ctrl output → no `.next_ctrl`.
`StorePat` similarly.  `CallOtherPat` calls with `output_index=0`
(ctrl) and `output_index=1` (mem) for `.next_ctrl` / `.next_mem`.

### LoadPat additions

```rust
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    mem_in: Option<Pat>,         // NEW
    next_mem: Option<Pat>,       // NEW
    bit_width: Option<u32>,      // NEW
}

impl LoadPat {
    pub fn mem_in(mut self, p: impl Into<Pat>) -> Self { self.mem_in = Some(p.into()); self }
    pub fn next_mem(mut self, p: impl Into<Pat>) -> Self { self.next_mem = Some(p.into()); self }
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self { self.bit_width = Some(n); self }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr, mem_in, next_mem, bit_width } = b;
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(p) = mem_in { indexed.push((0, p)); }
        if let Some(p) = addr { indexed.push((1, p)); }
        let kind = /* …existing space-aware kind spec… */;
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));

        // Bit-width post-match: outputs[1] is the value.
        if let Some(want) = bit_width {
            pat = pat.with_post_match(Arc::new(move |ctx, node, _b| {
                let outs = ctx.graph.graph.node_outputs(node);
                let Some(value_out) = outs.into_iter().nth(1) else { return false; };
                let Some(ty) = ctx.graph.graph.output_kind(value_out).as_value() else { return false; };
                ty.bit_width() == want
            }));
        }

        // Forward mem walk: outputs[0] is the mem output.
        if let Some(p) = next_mem {
            pat = pat.with_post_match(Arc::new(move |ctx, node, b| {
                walk_helpers::match_unique_output_consumer(ctx, node, 0, &p, b)
            }));
        }

        pat.into_pat()
    }
}
```

(Stacking two `with_post_match` calls requires the matcher to chain
multiple post-match closures; if the existing API only allows one,
combine into a single closure that runs both checks.  Either is fine
mechanically; pick whichever the implementation surface already
supports.)

### StorePat additions

Symmetric to LoadPat: same three new methods (`.mem_in`, `.next_mem`,
`.bit_width`), with `.bit_width(n)` checking the producer of
`inputs[2]` (data).

### CallOtherPat additions

```rust
impl CallOtherPat {
    pub fn next_ctrl(mut self, p: impl Into<Pat>) -> Self { self.next_ctrl = Some(p.into()); self }
    pub fn next_mem (mut self, p: impl Into<Pat>) -> Self { self.next_mem  = Some(p.into()); self }
}
```

`.next_ctrl` walks the unique consumer of `outputs[0]` (ctrl);
`.next_mem` walks the unique consumer of `outputs[1]` (mem).

### Strider-py mirror

Add the same methods on `PyLoadPat`, `PyStorePat`, `PyCallOtherPat`.
Each forwards to the Rust builder; signature is identical.

## Test plan

**Unit (Rust, in `crates/pattern/tests/`):**

1. `pattern_mem_in_load.rs` — Build a graph with `Store(a) → Load(a)`,
   match `load(addr=a).mem_in(store(addr=a))`.  Asserts the Load
   matches and the mem_in's nested store matches.

2. `pattern_next_mem_store.rs` — Build a graph with `Store(a) → Load(b)`,
   match `store(addr=a).next_mem(load(addr=b))`.

3. `pattern_lock_store_unlock.rs` — Build the canonical pattern: a
   `LOCK` CallOther followed by a `Store(addr)` followed by `UNLOCK`,
   all chained via memory edges.  Match
   `store(addr=any()).mem_in(call_other().name("LOCK")).next_mem(call_other().name("UNLOCK"))`.

4. `pattern_bit_width_load.rs` — Two Loads at the same addr, one U32 and
   one U64.  `load(addr=a).bit_width(32)` matches only the U32 Load;
   `bit_width(64)` matches only the U64 Load.

5. `pattern_bit_width_store.rs` — Same shape with Stores.

6. `pattern_callother_next_mem.rs` — `LOCK.next_mem(store(addr=any()))`
   matches a LOCK whose immediate mem successor is a Store.

7. `pattern_next_mem_zero_when_multi_consumer.rs` — A Store whose mem
   output has TWO consumers (rare but constructible) doesn't match
   `.next_mem(any())` — returns deterministic no-match.

**Python (in `crates/strider-py/tests/python/test_pattern_full_builders.py`):**

Add smoke tests for each new method on each builder (just verify the
Python API accepts them and the resulting Pat compiles).

## Migration / rollout

Single PR.  No backward-incompatible changes; existing pattern
queries continue to work.  All three builders gain new methods
additively.

## Risks & open questions

* **Stacking post-match closures.**  If `NodePat::with_post_match`
  only allows one post-match callback, the implementation combines
  multiple checks (bit_width AND next_mem) into one closure that runs
  both.  Mechanical; no design risk.

* **Captures across forward walks.**  When `.next_mem(p)` matches and
  `p` contains captures, those captures bind to nodes in the
  consumer.  Existing `IfPat::true_branch` already exhibits this;
  reuse that machinery so semantics are identical.

* **Naming consistency.**  `next_mem` mirrors how `IfPat`'s
  `true_branch / false_branch` describe forward walks.  Alternative
  names considered (`mem_consumer`, `followed_by_mem`); `next_mem` is
  shortest and least ambiguous given the IR's chain orientation.
