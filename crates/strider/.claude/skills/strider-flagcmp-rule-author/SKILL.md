---
name: strider-flagcmp-rule-author
description: Add a new rule to opt::FlagCmpCanonicalize — covers the Rule struct (rhs_capture: Option<Capture>), binary-vs-unary helpers, RHS asm-fingerprint propagation, and arch filtering.
---

# strider-flagcmp-rule-author

## When to invoke

A new arch (or a new shape on an existing arch) emits flag-bit reads that survive past `ConstantFold` — typically appearing as a `BoolBinaryOp` over individual flag-bit reads feeding an `If`. Triggers include:

- "Add a `FlagCmpCanonicalize` rule for `<flag-tree shape>`."
- "PowerPC CR-bit conditional branches don't canonicalise to `IntCmpOp`."
- "Indirect dispatch fails on `<arch>` because the bound walker can't see the `IntCmp`."
- "Thumb `B<cond>` lifts as `IntEqual(flag, 0:1)` and survives optimisation."

Round 8 (`round8-correctness-cross-arch.md` §2) flagged the PPC CR canonicalisation gap as the canonical case for this skill.

## When NOT to invoke

- The shape is already an `IntCmpOp` — no canonicalisation needed.
- The shape is unique to one fixture and the rule wouldn't generalise — write a targeted fold in `ConstantFold` instead.
- The shape requires removing nodes — `FlagCmpCanonicalize` rewrites uses but does not detach. Use `RedundantPhis` or a new destructive pass.
- The transformation requires arch-specific register names (e.g. "look up `cr0`") — those belong in arch-specific lift code in `pcode-lift`, not in a canonicalisation rule.

## Files this skill operates on

- `crates/opt/src/flag_cmp_canonicalize/mod.rs` — single-file pass: `Rule` struct, `try_apply_rule`, RHS builders, the `RULES` static, `build_rules` factory.
- `crates/opt/src/flag_cmp_canonicalize/tests.rs` (or `mod tests` in `mod.rs`) — graphmock unit tests.
- A real-ELF fixture via `strider-fixture-author` if the shape exists in real binaries.
- `crates/strider/tests/<feature>.rs` for an end-to-end indirect-branch resolution regression test if the rule fixes a bound-walker gap.

## Procedure

1. **Identify the source shape.** Lift a small example (`strider-cli-runner` produces `graph-opt.html`), locate the flag-bit producers feeding the consumer of interest. Typical shapes:
   - x86: `BoolAnd(BoolNeg(IntLess(a, b)), BoolNeg(IntEqual(diff, 0)))` for `JA`.
   - AArch64: `IntEqual(Add(a, Neg(b)), 0)` for `BEQ` (the EQ/ZR identity).
   - ARM Thumb: `IntEqual(CastToInt(flag), 0)` for `BNE` / `BCC` / `BPL` / `BVC`.
   - PPC: `BoolAnd` over individual CR bits — currently no rule covers this.

2. **Read the existing 9 rules in `build_rules` (around `mod.rs:314`)** to understand the rule numbering and shape vocabulary. Rules 1-7 are AArch64-style (carry bit / sign / overflow trees); rules 8-9 are Thumb cast-to-int unaries.

3. **Decide binary or unary.** Two helpers exist:

   - **Binary** (most rules):
     ```rust
     fn rule(lhs_builder: impl FnOnce(Capture, Capture) -> Pat,
             build_rhs: fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId)
             -> Rule
     ```
     Allocates two captures, hands them to the LHS builder, sets `rhs_capture: Some(b)`. Use for any LHS that binds two distinct varnode operands.

   - **Unary** (Thumb-style "test bool against 0"):
     ```rust
     fn rule_unary(lhs_builder: impl FnOnce(Capture) -> Pat,
                   build_rhs: fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId)
                   -> Rule
     ```
     Allocates one capture, sets `rhs_capture: None`. The `build_rhs` MUST ignore its `_b` parameter (the caller passes `a` as a placeholder).

4. **Round-8 silent-failure call-out (H1).** The `Rule` struct's `rhs_capture` field is now `Option<Capture>`, not `Capture`. The pre-round-8 form had a load-bearing `unwrap_or(a)` fallback in `try_apply_rule` that hid binding-contract violations. The round-8 form is:

   ```rust
   let b = match rule.rhs_capture {
       Some(c) => m.output(c).expect("Capture b must bind to a value output"),
       None    => a,
   };
   ```

   When writing a new binary rule, the LHS pattern MUST capture `rhs_capture` at a value-producing position. If `match_at` succeeds and `rhs_capture.is_some()`, `m.output(rhs_capture)` MUST return `Some` — anything else is a structurally wrong rewrite and the `expect` correctly panics. Do NOT regress to `unwrap_or(a)` — it silently masks rule-author bugs.

5. **Write the LHS pattern.** Use `pattern` crate constructors. The shape is composed bottom-up. Example for the EQ/ZR identity (rule 1):

   ```rust
   |a, b| int_eq(add(var(a), neg(var(b))), int_const(0))
   ```

   `var(c)` introduces an open capture; the pattern matcher binds it on success. Multiple occurrences of the same `Capture` in one pattern must agree (cross-reference). Use `pattern::sub` / `pattern::int_le` / `pattern::int_sle` aliases when expressing lift-time-canonicalised shapes.

6. **Write the RHS builder.** The signature is `fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId` where the third `NodeId` argument is the original root for fingerprint absorption. The replacement subtree is constructed manually (NOT via `pattern::rewrite_rule`) so each new node can absorb the root's fingerprint via `extend_asm_fingerprint_from`. Helpers:

   - `build_int_cmp(graph, op, lhs, rhs, root) -> NodeOutputId` — builds an `IntCmpOp(op)` node and absorbs the root's fingerprint.
   - `build_bool_neg(graph, inner, root) -> NodeOutputId` — builds a `BoolUnaryOp::Neg` node and absorbs.

   For multi-node RHS shapes, every intermediate node must absorb. The validator's Layer-C `check_asm_fingerprints` requires every reachable non-exempt node to carry a non-empty fingerprint — see `strider-fingerprint-audit`. `pattern::rewrite_rule` only absorbs into the outermost node, which is why this pass uses manual construction.

7. **Register the rule in `build_rules`.** Append to the `vec!` returned by `build_rules` (`mod.rs:314`). Maintain numerical comments so downstream readers can cross-reference rule N to its semantics.

8. **Arch filtering.** Today the rule table is global — every rule fires on every arch. If a new rule would false-positive on an unrelated arch (e.g. a PPC CR rule that happens to alias an x86 shape), add an `arch_filter: ArchPreset` set to the `Rule` struct and gate `try_apply_rule` on it. This is a forward-extending change — flag it explicitly to the user before doing it.

9. **Test with a graphmock LHS.** Construct the exact shape in a unit test, run `FlagCmpCanonicalize::run`, assert the post-pass IR matches the expected RHS via `pattern::find_all`. Include a negative test: a shape that nearly-matches but should NOT fire (e.g. `IntEqual(Add(a, b), 0)` without the `Neg` should not collapse to `IntEqual(a, b)`).

10. **Real-ELF complement.** If the shape exists in a real binary (PPC CR canonicalisation does), add a fixture via `strider-fixture-author` and an end-to-end test that asserts the indirect-branch resolver succeeds after the rule fires. The bound walker in `IndirectBranchResolve` consumes the canonicalised `IntCmpOp` to compute jump-table sizes; the failure mode without the rule is "indirect branch unresolved on `<arch>`."

## Verification

- `cargo test --package opt flag_cmp_canonicalize` — graphmock unit tests.
- `cargo test --package opt validate_with_options` — Layer-C asm-fingerprint check (every reachable non-exempt node carries a fingerprint).
- `cargo test --package opt` — full opt crate.
- If real-ELF fixture exists: `cargo test --package strider <fixture>`.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- Rule fires on the target shape and only the target shape (negative tests confirm no false positives on adjacent shapes).
- Asm-fingerprint absorption verified: every new node in the RHS calls `extend_asm_fingerprint_from(new_node, root)`.
- `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` passes on a real-ELF lift through the rule.
- For binary rules, `rhs_capture: Some(b)` is set and the LHS captures `b` at a value-producing position. For unary rules, `rhs_capture: None` and `build_rhs` ignores `_b`.
- `IndirectBranchResolve`'s bound walker can compute the table size on an indirect dispatch using this canonicalisation (if the rule unlocks resolution).

## Pitfalls

- **Regressing to `unwrap_or(a)` on `rhs_capture`.** Round-8 silent-failure audit H1 flagged this as load-bearing — it hid rule-author bugs where the LHS forgot to capture `rhs_capture` at a value-producing position. The current code uses `.expect("Capture b must bind to a value output")`. Keep it.
- **Forgetting fingerprint absorption on intermediate RHS nodes.** `pattern::rewrite_rule` would absorb only into the outermost; this pass constructs nodes manually so every helper (`build_int_cmp`, `build_bool_neg`) MUST call `extend_asm_fingerprint_from`. Multi-node RHS shapes that skip an intermediate node will fail Layer-C `check_asm_fingerprints` validation on the next fixture run.
- **Using `pattern::rewrite_rule` instead of manual construction.** It works, but only the outermost node absorbs fingerprints — intermediate nodes have empty fingerprints and Layer-C will flag them.
- **LHS pattern that binds `rhs_capture` at a control-flow position.** `m.output(c)` returns `None` for control captures, and the `.expect` will fire. Always anchor `rhs_capture` on a value-producing leaf.
- **Forgetting commutativity.** `int_eq` and `bool_and` / `bool_or` are commutative; rules using them try both orderings automatically. Non-commutative ops (`int_lt`, `int_slt`) keep stated order. Use `.ordered()` only when you need to disambiguate.
- **Adding a rule that masks a real bug.** The bound walker exists to compute jump-table sizes. If a rule overgeneralises and rewrites a shape that the walker depends on for arithmetic, indirect-branch resolution breaks silently. Always include a negative test on a near-miss shape.

## Background: why this pass exists

ARM, AArch64, and PowerPC all encode comparison results as individual flag bits (NZCV / CR0..CR7 fields). The Sleigh lift produces explicit flag-bit reads — for AArch64, `IntEqual(Add(a, Neg(b)), 0)` writes the Z flag, `IntSless(Add(a, Neg(b)), 0)` writes N, `IntSborrow(a, b)` writes V, etc. Conditional branches then read individual bits or simple boolean expressions over them: `BEQ` reads `Z`, `BHI` reads `BoolAnd(BoolNeg(C), BoolNeg(Z))`, `BLE` reads `BoolOr(Z, BoolNeg(Equal(N, V)))`, etc.

`ConstantFold` and `KnownBits` cannot reduce these because the operands `a` and `b` are runtime values. `FlagCmpCanonicalize` recognises the structural shape of the flag-test tree and rewrites it into a single `IntCmpOp` over `a` and `b`. After canonicalisation, the conditional branch reads a single comparison, and `IndirectBranchResolve`'s bound walker can compute jump-table sizes using the bound's arithmetic.

The 9 existing rules cover AArch64 and ARM Thumb. PowerPC's CR-bit shape is the round-8 documented gap.

## Edge cases worth flagging

- **Commutativity matters for the LHS pattern.** `int_eq` and `bool_and` / `bool_or` automatically try both orderings. If your LHS is non-commutative (e.g. `IntLess`) and the source could emit either ordering, list both as separate rules — the matcher won't try the swap automatically on non-commutative ops.
- **`ConstantFold` runs before this pass.** Rules can assume `BoolNeg(BoolNeg(x)) → x` has already collapsed; rule 3 (LS) explicitly relies on this.
- **`IfCondInversion` runs after this pass.** Don't rely on `If(BoolNeg(...))` being canonicalised yet; that happens later in the stable pipeline.
- **PPC CR0 vs CR1..CR7.** PowerPC has 8 condition register fields; most lifted code uses CR0. A PPC rule that hard-codes CR0 names misses CR1..CR7 — capture the field index instead, or accept missing on CR1+ for now and document the gap.
- **Thumb size-1 immediates.** ARM Thumb's `B<cond>` lifts with `IntEqual(flag, 0:1)` where `0:1` is a size-1 immediate. The lifter inserts `CastToInt(flag, U8)` for the size mismatch. Rules 8 and 9 handle the cast-to-int wrapper.

## Related skills

- `strider-pattern-author` — for the LHS pattern construction.
- `strider-fingerprint-audit` — to confirm Layer-C `check_asm_fingerprints` still passes after the rule fires.
- `strider-fixture-author` — for the real-ELF complement when the shape exists in real binaries.
- `strider-debug-pattern` — when the LHS doesn't match.
- `strider-indirect-shape-author` — when the rule's purpose is to unlock a new indirect-branch resolution path.
