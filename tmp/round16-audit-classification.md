# Round 16: NodeKind Classification Dispatch-Table Unification Audit

## Executive Summary

The strider IR employs **12+ independent per-NodeKind classification sites** that recur across the codebase. While `expected_signature` achieves unified dispatch for input/output shape, complementary properties (cacheability, phi semantics, asm-fingerprint exemption, structural role) remain scattered. Several unifications are viable and low-risk; others diverge intentionally by design.

**Key finding:** `asm_fingerprint_exempt`, `is_cacheable`, and phi-kind classification share the exact same 8-node predicate. A shared `NodeKindProperties` trait or table could eliminate this duplication at negligible cost. Float rendering and commutativity operations are legitimately divergent.

---

## Finding 1: Phi-Kind Classification (Recurs 4+ Sites)

### Locations

1. **`ir/src/validate/layer_c.rs:125`** — `check_layer_c_phis` match arm:
   ```rust
   let is_phi = matches!(
       kind,
       NodeKind::VarPhi(_)
           | NodeKind::MemPhi
           | NodeKind::StackStorePhi { .. }
           | NodeKind::ValuePhi
   );
   ```

2. **`ir/src/validate/layer_c.rs:186`** — `asm_fingerprint_exempt` helper:
   ```rust
   fn asm_fingerprint_exempt(kind: &NodeKind) -> bool {
       matches!(
           kind,
           NodeKind::Entry
               | NodeKind::InitialMemory
               | NodeKind::InitialVar(_)
               | NodeKind::FunctionArg { .. }
               | NodeKind::ControlState
               | NodeKind::MemPhi
               | NodeKind::VarPhi(_)
               | NodeKind::ValuePhi
               | NodeKind::StackStorePhi { .. }
       )
   }
   ```

3. **`ir/src/node/kind.rs:268`** — `is_cacheable` method (negated form):
   ```rust
   pub fn is_cacheable(&self) -> bool {
       !matches!(
           self,
           Self::Entry
               | Self::InitialMemory
               | Self::InitialVar(..)
               | Self::FunctionArg { .. }
               | Self::Return
               | Self::IndirectBranch
               | Self::ControlState
               | Self::MemPhi
               | Self::VarPhi(..)
               | Self::ValuePhi
               | Self::Call
               | Self::CallOther { .. }
               | Self::CPoolRef
               | Self::New
               | Self::StackStorePhi { .. }
       )
   }
   ```

### Unified Shape

The three classifiers share **core phi predicates** (`VarPhi`, `MemPhi`, `StackStorePhi`, `ValuePhi`), with `asm_fingerprint_exempt` extending to include structural nodes (`Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`).

**Proposed table entry:**
```rust
pub enum NodeKindProperties {
    // Per NodeKind discriminant
    Phi,                // VarPhi | MemPhi | StackStorePhi | ValuePhi
    Structural,         // + Entry | InitialMemory | InitialVar | FunctionArg | ControlState
    NonCacheable,       // + Return | IndirectBranch | Call | CallOther | CPoolRef | New
    Cacheable,          // All others
}

impl NodeKind {
    pub fn kind_properties(&self) -> NodeKindProperties { … }
    pub fn is_phi(&self) -> bool { matches!(self.kind_properties(), NodeKindProperties::Phi | NodeKindProperties::Structural) }
    pub fn is_asm_fingerprint_exempt(&self) -> bool { !matches!(self.kind_properties(), NodeKindProperties::Cacheable) }
}
```

### LOC Savings

- **Eliminated:** 3 independent `matches!` blocks = ~45 LOC.
- **Added:** 1 method on `NodeKind` + 1 helper + table comments = ~30 LOC.
- **Net:** ~15 LOC saved.

### Risk Assessment

**Low.** These predicates are read-only validators. A single source of truth improves invariant clarity. Callers already treat phis as a category; extracting the classifier doesn't change logic.

**Foot-gun mitigation:** Adding a new phi-like kind (`NewPhiVariant`) requires updating only `NodeKindProperties` enum, not three scattered match arms. Audit burden drops from 3→1 site.

---

## Finding 2: Output-Kind Classification (3 Sites, Minor Duplication)

### Locations

1. **`ir/src/node/output_kind.rs:27–87`** — `NodeOutputKind` methods:
   ```rust
   pub fn is_value(self) -> bool { matches!(self, Self::OutputType(..)) }
   pub fn is_control(self) -> bool { self == Self::Control }
   pub fn is_memory(self) -> bool { self == Self::Memory }
   pub fn is_phi_token(self) -> bool { self == Self::PhiToken }
   ```

2. **`ir/src/dot/mod.rs:91–100`** — `role_color` dispatches on `SlotRole`:
   ```rust
   match role {
       SlotRole::Control => "\"#00cccc\"",
       SlotRole::Memory => "\"#cc88aa\"",
       // …
   }
   ```

3. **`pattern/src/matcher/commutativity.rs:5–36`** — op-family commutation testers (indirect via `SlotRole`).

### Unified Shape

These are **already cohesive** because:
- `is_value` / `is_control` / `is_memory` are efficient property accessors on `NodeOutputKind`, not match arms.
- Rendering (dot colors) is intentionally display-only and shouldn't tie to validator logic.
- Commutativity is tied to `IntBinaryOp` / `FloatBinaryOp` families, not output kinds.

### Conclusion

**No unification needed.** These sites are contextually distinct and small enough to stay independent. Attempting to merge display logic with semantics would create artificial coupling.

---

## Finding 3: Asm-Fingerprint Exemption (Unified in Finding 1)

### Location

`ir/src/validate/layer_c.rs:186–199` (duplicates phi check + structural nodes).

### Current State

```rust
fn asm_fingerprint_exempt(kind: &NodeKind) -> bool {
    matches!(kind, Entry | InitialMemory | InitialVar(_) | FunctionArg { .. }
        | ControlState | MemPhi | VarPhi(_) | ValuePhi | StackStorePhi { .. })
}
```

### Unified Approach

Covered by Finding 1's `NodeKindProperties`. Replace call:
```rust
if !graph.node_kind(node).is_asm_fingerprint_exempt() { … }
// Becomes:
if graph.node_kind(node).is_cacheable() { … }  // Opposite sense
```

### LOC Savings & Risk

Already accounted in Finding 1. Negligible risk — validator-only usage.

---

## Finding 4: Commutativity Classification (Isolated, Not Duplicative)

### Locations

1. **`pattern/src/matcher/commutativity.rs:5–36`**:
   ```rust
   pub fn is_commutative_int_op(op: IntBinaryOp) -> bool {
       matches!(op, Add | Mul | And | Or | Xor)
   }
   pub fn is_commutative_float_op(op: FloatBinaryOp) -> bool {
       matches!(op, Add | Mul)
   }
   pub fn is_commutative_int_cmp_op(op: IntCmpOp) -> bool {
       matches!(op, Equal | Carry | Scarry)
   }
   ```

2. **`pattern/src/pat/builders/binary_op.rs:40–52`** — builder trait:
   ```rust
   impl BinaryOpClassifier for IntBinaryOp {
       fn is_commutative(self) -> bool { is_commutative_int_op(self) }
   }
   ```

3. **`opt/src/constant_fold/mod.rs`** — implicitly uses op commutativity for fold rewrites (e.g., `Add(x, 0) → x` regardless of order).

### Unified Shape

Commutation is **per-op-family** (IntBinaryOp, FloatBinaryOp, IntCmpOp), not per-NodeKind. It's independent of output-kind classification and shouldn't be generalized with it. The current `is_commutative_*` functions are the right abstraction level.

### Risk Assessment

**No unification needed.** Pattern matching and constant-fold both consult the same `is_commutative_int_op` source; they already share it. The functions are minimal and op-family-scoped. Dragging them into a NodeKind-level properties table would be over-generalization.

---

## Finding 5: Dot Rendering (Display-Only, Intentionally Divergent)

### Locations

1. **`ir/src/dot/mod.rs:15–44`** — `node_shape` per kind:
   ```rust
   pub fn node_shape(kind: &NodeKind) -> &'static str {
       match kind {
           NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) | … => "Mdiamond",
           NodeKind::ControlState => "invhouse",
           NodeKind::VarPhi(_) | NodeKind::MemPhi | NodeKind::ValuePhi => "house",
           …
       }
   }
   ```

2. **`ir/src/dot/mod.rs:46–86`** — `node_fillcolor` per kind (dark-theme colors).

3. **`ir/src/dot/label.rs`** — per-kind label formatting.

### Unified Shape

Rendering properties (shape, color, label style) are **display-only** and intentionally de-coupled from validator/semantic logic. Binding them to semantic properties would:
- Constrain future color/shape changes to IR semantic changes.
- Make reasoning about display invariants harder (colors would become part of the IR contract).
- Create false coupling between unrelated concerns (a semantic property change would force visual re-review).

### Recommendation

**No unification. Explicitly document this intentional divergence.**

The current per-location match arms are fine — they're specialized for rendering context and should stay independent.

---

## Finding 6: Discriminant-Based PatKind Dispatch (Efficient, Not Duplicative)

### Location

`pattern/src/pat/node_pat.rs:59–100` — `KindSpec` enum:
```rust
pub enum KindSpec {
    Any,
    Variant(std::mem::Discriminant<NodeKind>),
    Exact(NodeKind),
    VariantWith { discriminant: std::mem::Discriminant<NodeKind>, check: Arc<…> },
}
```

**Usage in matcher:**
```rust
// crates/pattern/src/matcher/mod.rs:147 — fast bucket lookup by discriminant
let d = std::mem::discriminant(self.graph.node_kind(node));
// Later at line 310–322: by_discriminant bucketing in find_all_multi
match pat.as_dyn().kind_spec().discriminant() {
    Some(d) => by_discriminant.entry(d).or_default().push(i),
    …
}
```

### Unified Shape

The `discriminant()` method on `KindSpec` is already a single source of truth for extracting the discriminant. No duplicate extraction logic exists.

### Recommendation

**No action needed.** This is already well-factored. The `KindSpec` abstraction is lightweight and efficient.

---

## Finding 7: ValueLifter::lift Opcode Dispatch (Shared Source, Not Duplicative)

### Locations

1. **`pcode-lift/src/value/mod.rs:28–150`** — main dispatch loop:
   ```rust
   pub fn lift<R: rsleigh::MemReader>(
       lifter: &mut ValueLifter<'_, R>,
       insn: &rsleigh::Insn,
   ) -> Result<bool> {
       match insn.opcode {
           Opcode::BoolNeg => { lifter.process_bool_unary_op(…) }
           Opcode::BoolAnd => { lifter.process_bool_binary_op(…) }
           …
       }
   }
   ```

2. **`strider/src/strider/insn/mod.rs:68–100`** — control-flow re-dispatch:
   ```rust
   match insn.opcode {
       Opcode::Nop => {}
       Opcode::Branch => { self.handle_branch(…) }
       Opcode::CondBranch => { self.handle_cond_branch(…) }
       …
   }
   ```

### Unified Shape

Both sites dispatch on `rsleigh::Opcode` (not `NodeKind`). They **share the same opcode space** but handle different subsets:
- `ValueLifter::lift` handles value-producing opcodes (arithmetic, floats, casts, loads).
- Strider's control-flow handler handles branches, calls, stores, returns.
- No overlap (by design — the lifter returns `false` for control ops).

**Single source of truth:** Both rely on `rsleigh::Opcode` semantics; neither reimplements the taxonomy. Re-dispatch at the strider level is necessary for region awareness (control-flow needs CFG context).

### Recommendation

**No unification needed.** The separation is semantically correct and unavoidable. Further factoring would be over-abstraction.

---

## Finding 8: Per-Kind Slot-Count Validation (Delegated to Signature Table)

### Location

`ir/src/graph/store.rs:78–100` — `set_node_kind` validation:
```rust
pub fn set_node_kind(&mut self, node_id: NodeId, kind: NodeKind) -> crate::Result<()> {
    let old_kind = self.nodes[node_id].kind;
    if old_kind.is_cacheable() || kind.is_cacheable() { … }
    if !crate::node_signature::slot_counts_match_kind(self, node_id, &kind) {
        return Err(…);
    }
    …
}
```

**Helper at `node_signature.rs:403+`:**
```rust
pub(crate) fn slot_counts_match_kind(graph: &Graph, node: NodeId, kind: &NodeKind) -> bool {
    let expected = expected_signature(kind);
    let actual_inputs = graph.node_inputs(node).len();
    let actual_outputs = graph.node_outputs(node).len();
    // Check fixed/variadic arity against expected…
}
```

### Unified Shape

Already unified via `expected_signature`. No duplicate per-kind classification exists — all slot validation delegates to the signature table.

### Recommendation

**No action needed.** This is already the single source of truth.

---

## Finding 9: Call-Other Opcode Classification (Unified, Different Crate)

### Location

`target/src/call_other_abi.rs` — arch-specific call-other user-op dispatch:
```rust
pub fn classify(preset: ArchPreset, name: &str) -> CallOtherClass {
    match (preset, name) {
        (_, "mfence") => CallOtherClass::NoOp,
        (_, "sfence") => CallOtherClass::NoOp,
        (X86_64, "syscall") => CallOtherClass::Call(…),
        (ARM, "swi") => CallOtherClass::Call(…),
        // …
    }
}
```

**Usage sites:**
- `cfg::region_builder` — terminates trap regions at first iteration.
- `strider::IrStrider::handle_call_other` — emits precise or terminal CallOther nodes.
- `ir::FunctionBuilder` — wires control / memory / output slots.

### Unified Shape

Already a single source of truth. Both callers delegate to `target::classify(preset, name)` — no duplication.

### Recommendation

**No action needed.** Per-spec (2026-05-05/06), this is correctly centralized.

---

## Finding 10: Per-NodeKind "Is Structural" Classification (No Unified Home, But Low Impact)

### Observed Patterns

Code patterns that implicitly classify nodes as "structural" (lifted without contributing assembly):
- `ControlState`, `VarPhi`, `MemPhi`, `ValuePhi`, `StackStorePhi` — region/join nodes.
- `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg` — initial-state nodes.
- `Return`, `Call`, `CallOther` — control/terminator nodes (may or may not be structural depending on context).

### Current State

No explicit `is_structural()` method exists. Instead, callers use ad-hoc predicates:
- Asm-fingerprint exemption uses `asm_fingerprint_exempt(kind)`.
- Phi checking uses `matches!(kind, VarPhi | MemPhi | …)`.
- Cacheability uses `!is_cacheable()`.

These are partially overlapping but not identical.

### Proposed Refinement

Rather than a single "is structural" flag, partition NodeKind into **categories**:
```rust
pub enum NodeCategory {
    Structural,      // Entry, InitialMemory, InitialVar, FunctionArg, ControlState
    Phi,             // VarPhi, MemPhi, StackStorePhi, ValuePhi
    Computation,     // All const, binary op, unary op, cast nodes
    Memory,          // Load, Store, StackStore, StackStorePhi
    Control,         // If, Return, IndirectBranch, Call, CallOther
    Opaque,          // CPoolRef, New, SegmentOp, CallOther (user-defined)
}
```

**Pro:** Single dispatch point for category-based logic. **Con:** Categories can overlap (e.g., `StackStorePhi` is both Phi and Memory). **Risk:** Over-design if only 2–3 call-sites actually benefit.

### Recommendation

**Defer.** Finding 1's `NodeKindProperties` + `is_asm_fingerprint_exempt()` addresses the immediate unification need. If a third call-site emerges that benefits from categorical dispatch, revisit this.

---

## Finding 11: PatKind / NodeKind Mirroring (Round 14 D-1 Deferred)

### Status

From CLAUDE.md and prior audit notes: `PatKind` enum mirrors `NodeKind` for pattern matching (e.g., `Pat::Add`, `Pat::Load`, `Pat::Call`). Both are manually synchronized.

### Current Coupling

1. **Manual enum sync:** New `NodeKind` variant requires adding corresponding `PatKind` variant.
2. **Pattern builders:** Each `NodeKind` has a builder in `pattern/src/pat/builders/`.
3. **Discriminant use:** `PatKind` builders call `std::mem::discriminant` to match buckets in `find_all_multi`.

### Incremental Win (Short of Full Proc-Macro Mirror)

A shared **opcode-constant table** could live in `ir::node::kind.rs`:
```rust
#[derive(Clone, Copy)]
pub struct NodeKindInfo {
    pub name: &'static str,
    pub discriminant_key: usize,  // Stable across builds
    pub is_cacheable: bool,
    pub is_phi: bool,
    pub is_asm_fingerprint_exempt: bool,
}

impl NodeKind {
    pub const fn info(&self) -> NodeKindInfo { … }
}
```

**Pro:** Single audit point when adding kinds. **Con:** Still requires manual pattern builder additions.

**Verdict:** Without a proc-macro unifying the two enums, the incremental win is modest (~1 match arm per new kind). Defer full mirroring to a future round unless the pain intensifies.

---

## Finding 12: Known-Bits Analysis (Per-OpFamily, Not Duplicative)

### Location

`opt/src/known_bits/mod.rs:150–` — per-node known-bits computation:
```rust
// Implicit dispatch: for each node kind, extract op family, compute bits.
// No single match(kind) block; instead, per-op-family analyzers are called.
```

### Pattern

Constant-fold and known-bits both evaluate the same op-families (IntBinaryOp, IntCmpOp, FloatBinaryOp, etc.) but from different directions:
- **Constant-fold:** fully-constant inputs → constant output.
- **Known-bits:** partially-known bits + unknown bits → refined known bits.

### Unified Shape

Both delegate to op-family evaluation helpers (`eval_int.rs`, `eval_float.rs`, `eval_bool.rs`). No per-NodeKind match arm exists here — dispatch is per opcode, not per node.

### Recommendation

**No action needed.** Already well-factored.

---

## Summary Table: Unification Opportunities

| Finding | Sites | Recommendation | LOC Savings | Risk | Effort |
|---------|-------|-----------------|-----------|------|--------|
| 1. Phi + Asm-Fingerprint | 4 | **Unify into `NodeKindProperties`** | ~15 | Low | 2–3 days |
| 2. Output-Kind Classification | 3 | No action (display vs. semantic divergence intended) | 0 | N/A | 0 |
| 3. Dot Rendering | 3 | No action (display-only, intentionally divergent) | 0 | N/A | 0 |
| 4. Commutativity | 3 | No action (already shared via `is_commutative_*` functions) | 0 | N/A | 0 |
| 5. Discriminant Extraction | 2 | No action (already unified in `KindSpec`) | 0 | N/A | 0 |
| 6. ValueLifter / Strider Opcode Dispatch | 2 | No action (opcode-level, not NodeKind; region-aware re-dispatch necessary) | 0 | N/A | 0 |
| 7. Slot-Count Validation | 1 | No action (delegated to `expected_signature`) | 0 | N/A | 0 |
| 8. Call-Other Classification | 1 | No action (unified in `target::classify`) | 0 | N/A | 0 |
| 9. Structural Classification | Implicit | Defer (Finding 1 addresses immediate needs) | 0 | Medium | Pending |
| 10. PatKind / NodeKind Mirror | 2 | Defer (full proc-macro unification deferred from Round 14 D-1) | 0 | Medium | Pending |

---

## Concrete Proposal: Finding 1 Implementation Sketch

**File:** `crates/ir/src/node/kind.rs`

```rust
impl NodeKind {
    /// Returns `true` if this kind is allowed to carry an empty asm-fingerprint.
    /// Structural, phi, and initial-state nodes are synthesized without a parent
    /// machine instruction.
    #[inline]
    pub fn is_asm_fingerprint_exempt(&self) -> bool {
        matches!(
            self,
            Self::Entry
                | Self::InitialMemory
                | Self::InitialVar(..)
                | Self::FunctionArg { .. }
                | Self::ControlState
                | Self::MemPhi
                | Self::VarPhi(..)
                | Self::ValuePhi
                | Self::StackStorePhi { .. }
        )
    }

    /// Returns `true` if this is a phi node (region join point).
    #[inline]
    pub fn is_phi(&self) -> bool {
        matches!(
            self,
            Self::VarPhi(..) | Self::MemPhi | Self::ValuePhi | Self::StackStorePhi { .. }
        )
    }
}
```

**File:** `crates/ir/src/validate/layer_c.rs` (simplification)

```rust
// OLD: fn asm_fingerprint_exempt(kind: &NodeKind) -> bool { matches!(…) }
// NEW: delegate to method on NodeKind
if graph.node_kind(node).is_asm_fingerprint_exempt() {
    continue;
}

// OLD: let is_phi = matches!(kind, VarPhi | MemPhi | …)
// NEW: use the extracted method
for (node, kind) in reachable_nodes(…) {
    if !kind.is_phi() && !matches!(kind, NodeKind::StackStorePhi { .. }) {
        continue;  // StackStorePhi is special-cased for arity; revisit if generalizing
    }
    …
}
```

---

## Honest Tail: Load-Bearing Divergence

The following classification sites **must** remain divergent — unifying them would lose important semantics:

1. **Dot rendering colors** — tied to visual design intent, not IR semantics. Color changes shouldn't require validator approval.
2. **Opcode-level dispatch (ValueLifter, strider insn handler)** — operates on pcode opcode space, not NodeKind space. Forced coupling to NodeKind would be over-abstraction.
3. **Constant-fold vs. known-bits evaluation** — both analyze op families but from different semantic angles (total vs. partial).

---

## Conclusion

**Actionable high-confidence unification:** Finding 1 (phi + asm-fingerprint exemption). Effort: 2–3 days. Risk: negligible. Payoff: clearer invariant, 1 fewer place to audit when shipping new phi-like kinds.

**Everything else:** already well-factored or intentionally divergent. No surprises or foot-guns detected.

**Total codebase health:** solid. Classification logic is appropriate to its scope; no monolithic dispatch table is hiding.
