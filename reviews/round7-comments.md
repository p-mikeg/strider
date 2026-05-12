# Round-7 sweep: comment staleness, accuracy, and tech-debt indicators

Scope: every `crates/*/src/**/*.rs` in the workspace.  Trust model: strict;
no `*-r6.md` reviews consulted.  All file:line citations are first-encounter
absolute paths under `/mnt/c/Users/mikeg/Documents/strider`.

----

## Section A — Comments referencing deleted symbols

### A1.  `crates/opt/src/lib.rs:149` and `crates/opt/src/lib.rs:182` — `CallOtherElide`
Both doc-strings on `destructive_default_pipeline` and `default_pipeline`
mention "the pre-existing `CallOtherElide` pass is gone."  These are
**intentional historical notes** explaining a behavioural change — they
*name* a deleted symbol but do so in a "what changed and why" sentence.
**Severity: low / informational.**  Recommendation: keep, but consider
trimming once a release boundary makes the historical breadcrumb
unnecessary.

### A2.  `crates/strider/src/strider/pipeline.rs:233` — `CallOtherElide` (implicit)
The doc-string for `build_destructive_optimizer_pipeline` ends with
"CallOther no-op handling is now done at construction time in
`target::call_other_abi::classify`."  Clean (no symbol name).  No action.

### A3.  `crates/ir/src/graph/mod.rs:96` — `IfCase` (HIGH)
The `asm_fingerprints` doc lists exempt `NodeKind` variants as:

```
Region nodes (`ControlState`, phis, `Entry`, `InitialMemory`,
`InitialVar`, `FunctionArg`, `IfCase`)
```

`IfCase` is **not a `NodeKind` variant** — `crates/ir/src/node/kind.rs`
declares only `If` (no `IfCase`).  The actual exempt list lives in
`crates/ir/src/validate/layer_c.rs:164-177` and reads:

```
Entry | InitialMemory | InitialVar(_) | FunctionArg { .. } |
ControlState | MemPhi | VarPhi(_) | ValuePhi | StackStorePhi { .. }
```

The doc both invents `IfCase` and **omits the actual exempt members**
`MemPhi`, `VarPhi(_)`, `ValuePhi`, `StackStorePhi`.  This is a `pub`
field's documented contract — high-severity drift.

(Also note: CLAUDE.md still claims `IfCase(bool)` is a `NodeKind` variant
in its IR Node Model section; same drift root cause.  Out of scope here
but worth a follow-up.)

### A4.  `crates/pcode-lift/src/value/misc_value.rs:5,8` — `MultiEqual`
The module-level comment correctly describes the live behaviour:
`MultiEqual` is bailed on as an error inside
`crates/strider/src/strider/insn/mod.rs:85-90`.  No staleness.

### A5.  `crates/pattern/src/matcher/bindings.rs:136` and `crates/pattern/src/pat/mod.rs:43`
"the old typed-Var getters" / "previous overloading on `Capture` vs
typed-Var is gone with the typed Vars themselves" — historical
breadcrumbs.  No action.

### A6.  `NO_OP_USER_OPS` — not found anywhere in `crates/*/src`.  Clean.

----

## Section B — TODO / FIXME / HACK / XXX / HMMMM markers

The grep finds **four** real markers (excluding `\uXXXX` literal escapes
in `crates/dot/src/lib.rs:178,556` which are not TODOs).

| File:line | Classification | Notes / suggestion |
|---|---|---|
| `crates/cfg/src/cfg/decode_cache.rs:35` | OPEN | `TODO(Task17): remove after incremental indirect-resolve lands — see docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`.  Plan file exists.  Marker is actionable, well-scoped, and links to a tracked plan.  **Already in `// TODO(crateuser/issue#NNN)`-style; keep as-is.** |
| `crates/strider/src/orchestrator.rs:251` | OPEN | Same `TODO(Task17)` reference for the cached-`unique-vns` field.  Actionable, scoped, linked.  Keep. |
| `crates/strider/src/strider/pipeline.rs:43` | OPEN | Same `TODO(Task17)` reference for the `exit_vn_to_value` `Arc`.  Actionable, scoped, linked.  Keep. |
| `crates/strider-py/src/pattern.rs:20` | OPEN | "(TODO: op-variant accessors are not yet exposed on the Python `Match`)" — concrete, narrow gap.  Recommendation: turn into `// TODO(strider-py): expose Match.{int,bool,float}_{binary,unary,cmp}_op` so it surfaces in a grep without needing a tracked-issue rewrite. |

No FIXME / HACK / HMMMM / standalone XXX markers anywhere in
`crates/*/src`.  Tech-debt-marker hygiene across the workspace is
exemplary.

----

## Section C — Comments contradicting the code as written

### C1.  `crates/cfg/src/cfg/types.rs:103-105` — stale "legacy mapping"
The `RegionTerminator::Return` doc says:

```
`Return` opcode (or, in the legacy mapping retained until the
indirect-branch resolver lands, a `BranchIndirect`).
```

The indirect-branch resolver **has landed** (`crates/opt/src/indirect_branch_resolve/`,
`crates/strider/src/indirect_resolve/`), and
`crates/cfg/src/cfg/builder/region_builder.rs:390-394` shows
`Opcode::Return` and `Opcode::BranchIndirect` are now dispatched
**separately** — `BranchIndirect` no longer falls through to
`RegionTerminator::Return`.  The parenthetical is obsolete and should
be deleted (or replaced with a one-line note that `BranchIndirect`
gets its own terminator path now).

### C2.  `crates/strider/src/strider/pipeline.rs:201-210` — stable-pipeline pass list out of date
The doc-string for `build_stable_optimizer_pipeline` says:

> Composed of passes whose rewrites survive a later iteration that
> adds new phi inputs: `ConstantFold`, `KnownBits`,
> `StackStoreDetect`, `StackLoadForward`, and the `FunctionArgDetect`
> post-pass.

But `opt::stable_default_pipeline()` (the call this method delegates to,
`crates/opt/src/lib.rs:106-126`) adds **four** passes — `ConstantFold`,
`KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion` — and the
delegating method tacks on `StackStoreDetect` + `StackLoadForward` +
`FunctionArgDetect`.  The doc-string omits `FlagCmpCanonicalize` and
`IfCondInversion`.  CLAUDE.md inherits the same gap (its description
of `Strider::build_stable_optimizer_pipeline` lists the same five
passes), but that is out of scope for the in-source audit.

### C3.  `crates/pattern/src/pat/ctor/control.rs:122-127` — `if_node()` symmetric-matching claim
The user-facing doc-comment on `pub fn if_node()` reads:

> **Symmetric matching.**  When `.cond(C)` is set, the matcher also
> tries the compiler-inverted layout: input `Not(C)` with branches
> swapped.

The implementation is **direct-only** — the file-level comment of
`crates/pattern/src/pat/builders/branch.rs:1-17` explicitly states
"Match layout (single, direct)" and explains that the IR is
canonicalised by `opt::IfCondInversion` so symmetric matching is no
longer needed.  The `IfPattern::try_match_at` body
(`branch.rs:89-99`) calls a single `try_layout`, no second layout
attempt.  Doc-string lies about behaviour — see Section D.

### C4.  `crates/pattern/src/pat/ctor/variant_agnostic.rs:197` — `float_cmp_any` mentions nonexistent `NotEqual`
The doc-string says:

> Commutative comparisons (`Equal`, `NotEqual`) try both operand
> orderings automatically.

`FloatCmpOp` (`crates/ir/src/ops/op_kinds.rs:152-157`) has only `Equal`
and `Less`.  `NotEqual` is **lowered at lift time** to
`BoolNeg(FloatEqual(..))` per the very file-level comment in
`op_kinds.rs:146-147`.  The runtime decider
`is_commutative_float_cmp_op` (`crates/pattern/src/matcher/commutativity.rs:34-36`)
only matches `FloatCmpOp::Equal`.  Doc-string lies — see Section D.

### C5.  `crates/pattern/src/pat/ctor/control.rs:39` — `phi()` claims "any phi node"
> Starts building a `VarPhi` pattern.  Matches any phi node.

`PhiPat`'s `From` impl (`crates/pattern/src/pat/builders/phi.rs:36-48`)
constructs only `KindSpec::variant(&NodeKind::VarPhi(_))`.  It does NOT
match `MemPhi`, `ValuePhi`, or `StackStorePhi`.  The "any phi node" claim
is wrong; the correct phrasing is "any `VarPhi` regardless of `Vn`".
See Section D.

----

## Section D — Doc-string lies on `pub` items

| Item | Location | Claim | Reality | Severity |
|---|---|---|---|---|
| `pub fn if_node()` | `crates/pattern/src/pat/ctor/control.rs:122-127` | "Symmetric matching … also tries the compiler-inverted layout" | Direct-layout-only.  `branch.rs:1-17` and `89-99` confirm the second-layout code was deleted in favour of `opt::IfCondInversion` upstream-canonicalisation. | HIGH (user-facing, semantic) |
| `pub fn phi()` | `crates/pattern/src/pat/ctor/control.rs:39-43` | "Matches any phi node" | Matches only `VarPhi`. Will not match `MemPhi` / `ValuePhi` / `StackStorePhi`. | HIGH |
| `pub fn float_cmp_any` | `crates/pattern/src/pat/ctor/variant_agnostic.rs:194-198` | "Commutative comparisons (`Equal`, `NotEqual`) try both operand orderings automatically" | `FloatCmpOp` has only `Equal` and `Less`; `NotEqual` is not a variant. The decider only treats `Equal` as commutative; `NotEqual` is lowered at lift time. | HIGH |
| `pub struct PyPhiPat` doc | `crates/strider-py/src/pattern.rs:550-553` | "Typed builder for `VarPhi` / `MemPhi` / `ValuePhi` patterns" | `finalise()` (`pattern.rs:567-577`) calls `pattern::phi()` / `pattern::phi_for(vn)` only — `VarPhi`-only.  The Python wrapper inherits the Rust limitation. | HIGH (user-facing) |
| `pub fn build_stable_optimizer_pipeline` | `crates/strider/src/strider/pipeline.rs:201-210` | Lists `ConstantFold, KnownBits, StackStoreDetect, StackLoadForward, FunctionArgDetect` | Actually adds those plus `FlagCmpCanonicalize` and `IfCondInversion` (via `opt::stable_default_pipeline()`). | MEDIUM (composition drift) |
| `Graph::asm_fingerprints` (field doc) | `crates/ir/src/graph/mod.rs:94-98` | Exempt set: "`ControlState`, phis, `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `IfCase`" | Actual exempt set in `validate/layer_c.rs:164-177` is `Entry, InitialMemory, InitialVar, FunctionArg, ControlState, MemPhi, VarPhi, ValuePhi, StackStorePhi` — i.e. doc invents `IfCase` (does not exist) and lumps "phis" without naming `StackStorePhi` / `ValuePhi`. | HIGH |
| `pub fn ordered` on `PyPat` | `crates/strider-py/src/pattern.rs:426-438` | Documented as a no-op on a finalised `Pat`; recommends typed builders instead. | Doc-string accurately describes the no-op.  **Not a lie** — kept here only because the user prompt called it out; included for completeness. | None — passes audit. |
| `// asserted by type_info_table_matches_variants` | `crates/ir/src/node/output_type.rs:50-51` | "Order MUST match the `NodeOutputType` enum declaration order" | The named test exists at `crates/ir/src/node/tests.rs:305-329`.  It does not literally assert "declaration order matches table order", but indexing relies on `self as usize` and the test enumerates every variant against `info().name`/`byte_size`/category, so any swap at either site would fail the test.  **Not a lie**; the comment is approximately correct.  Optional minor: rephrase to "names/sizes per variant verified by …". | None — passes audit. |

### Doc-strings naming nonexistent functions
Searched specifically for the canonical example
`type_info_table_matches_variants` — it exists at
`crates/ir/src/node/tests.rs:305`.  No other doc-comment in the workspace
names a function the codebase does not contain.

----

## Summary

| Category | Count |
|---|---|
| **A. Deleted-symbol references** | 1 high (`IfCase`), 2 informational historical notes (`CallOtherElide` x2), 2 historical-breadcrumb notes (`typed-Var`). |
| **B. TODO/FIXME/HACK markers** | 4 OPEN, all `TODO(Task17)`-style and well-scoped + linked.  0 OUTDATED, 0 VAGUE.  No FIXME / HACK / XXX / HMMMM markers. |
| **C. Comments contradicting code** | 5 (cfg/types.rs legacy-mapping; build_stable_optimizer_pipeline pass list; if_node symmetric claim; float_cmp_any NotEqual; phi() "any phi node"). |
| **D. Doc-string lies on `pub` items** | 6 confirmed (HIGH severity on `if_node`, `phi`, `float_cmp_any`, `PyPhiPat`, `Graph::asm_fingerprints`; MEDIUM on `build_stable_optimizer_pipeline`). |

### Cross-cutting observations
- The `pattern` crate's `pub` constructor doc-strings have systematically
  drifted from a previous symmetric-matching / multi-phi-shape design
  (the pre-`IfCondInversion` and pre-typed-`Capture` era).  A focused
  doc-only sweep over `crates/pattern/src/pat/ctor/*.rs` and
  `crates/pattern/src/pat/builders/*.rs` would close all the C/D issues
  in one pass.
- `Graph::asm_fingerprints`'s exempt-set documentation (graph/mod.rs:96)
  has drifted from the actual `validate/layer_c.rs:164` matcher.  Worth
  treating these as a paired source-of-truth: either lift the exempt set
  into a `const` named-array shared by both, or have one of the two cite
  the other ("see `validate::layer_c::asm_fingerprint_exempt`").
- The Task17 TODOs are healthy: they all link to the same plan file at
  `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`,
  which exists.
- One **stale legacy phrase** in `cfg/types.rs:103` is the only narrative
  drift in the cfg crate; the rest of cfg's doc-comments stayed
  current with the resolver landing.
