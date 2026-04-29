# crates/pattern review — round 6 (2026-04-29)

## Summary

- Files reviewed: 39 (all `.rs` under `crates/pattern/src`, `crates/pattern/tests`, `crates/pattern/examples`)
- Findings total: 50
  - Correctness: 12
  - Dead code: 9
  - Duplication & unification: 8
  - Simplification: 7
  - Readability: 11
  - Performance: 3
- High-confidence findings: 32
- Low-risk findings: 38

The crate is well-organised and the trait-based unification (single `Pattern` trait, single `NodePat` configured by spec enums) has paid off — most public ctors are one-liners. Standout themes:

- **Capture-rule contract is partly aspirational**: docs claim "control-flow builders never let you call `.capture(v)`" and "no builder exposes both `.capture` and `.capture_node`", but the blanket `IntoPat` impl gives every `Into<Pat>` value `.capture` / `.when` for free. Several control-flow builders have docstrings advertising `.capture()` in their ctor doc that contradicts the capture-rule.
- **Stale documentation post-anyhow / post-trait-rewrite**: `crate::build`, `crate::ErrorKind`, `Pat::when_impl` doclinks, references to a non-existent `pat::Contains` / `WithCapture` / `WithPredicate` set, BUG-19 codenames in three places, and CLAUDE.md-derived doc text that names features (`Matcher` pre-indexing Call/Return/If) which the code does not implement.
- **`anyhow::Error::new(crate::error::MissingBinding(_))` boilerplate is replicated 8 times** across ctor implementations (`pat/any.rs`, `pat/mod.rs`, `pat/ctor/consts.rs`, `pat/ctor/variant_agnostic.rs`); a one-line constructor would collapse all sites.
- **`ignore_control_states` walk-through is documented to apply to `if_node().true_branch(p)` and the forward-walk consumer path, but `ConsumersSpec` matching uses `match_node_id` which bypasses walk-through entirely.** Tests only exercise the backward-walk case (`ret().preceded_by(call())`).
- **`pub` items only used internally** are widespread: `RewriteSkip`, `is_skip`, `BuildCtx`, `BuildOutcome`, `PredicateFn`, `MatchPredicateFn`, `Match::{get_int_const, get_bool_const, get_float_bits, get_float_const, bindings_clone, get_vn}`, `FunctionArgHandle` and the entire `Matcher::function_arg*` API have zero external callers in the workspace.
- **The example (`examples/pattern_query.rs`) has a stale comment** that asserts a commutative-add pattern returns `0` when commutativity should clearly make it return `1` — either the example output is wrong, or commutativity has a regression in this specific scenario.
- **`int_cmp(op, l, r)` / `int_cmp_any` apply commutative-retry for `Equal`/`Carry`/`Scarry` automatically, but the return type is `Pat`, not a typed builder, so `.ordered()` opt-out is unavailable.** Asymmetric with `int_binary` which returns `IntBinaryOpPat` for the same commutative ops.
- **`float_cmp` does NOT apply commutative retry for `Equal`/`NotEqual`** even though they are mathematically commutative; `is_commutative_float_cmp_op` does not exist while `is_commutative_int_cmp_op` does.
- **`int_const` post-match closure runs `KindSpec::variant_with(&NodeKind::IntConst(0u128), |_| true)`** — the tautological closure is a code smell; `KindSpec::variant(...)` would do.
- **`pub enum KindSpec`/`InputsSpec`/`OutputsSpec`/`ConsumersSpec`/`BuildTy` are `pub` inside a `pub(crate) mod node_pat`** — visibility is `pub(crate)` in effect; the `pub` is misleading.

## Table of contents

- F-001 `IntoPat` blanket impl gives every builder a meaningless `.capture(v)` (Correctness, `pat/mod.rs:203-224`)
- F-002 `IntoPat::capture` doc claims control-flow builders "never match" — actually `Call`/`CallOther` may match a value ret slot (Correctness, `pat/mod.rs:204-211`)
- F-003 `if_node().true_branch(p)` does NOT walk through `ControlState` even with `ignore_control_states` set (Correctness, `pat/node_pat.rs:510-521`, `lib.rs:99-103`)
- F-004 `add(phi_for(rbx), any())` example expects 0 matches; commutativity should yield 1 (Correctness, `examples/pattern_query.rs:573-577`)
- F-005 Documented commutative ops list omits `int_cmp`/`int_cmp_any` (Correctness/Readability, `lib.rs:80-82`, `pat/ctor/int.rs:74-85`)
- F-006 `float_cmp(Equal, ...)` does NOT commute even though mathematically commutative (Correctness, `pat/ctor/float.rs:60-67`, `matcher/commutativity.rs`)
- F-007 `int_cmp(...)` returns bare `Pat` so `.ordered()` is unavailable (Correctness, `pat/ctor/int.rs:76-85`)
- F-008 `RetPat::ret_val` doc says "0-based after the ctrl input" but offset is `2 + i` (i.e. after ctrl AND mem) (Correctness/Readability, `pat/builders/ret.rs:30-31`)
- F-009 `RetPat::preceded_by` doc says single-step-direct, but `ret().preceded_by(call())` test in example expects multi-step walk (Correctness/Readability, `examples/pattern_query.rs:184-202`)
- F-010 `call_other` ctor doc mentions `.capture()` but `CallOtherPat` only exposes `.capture_node()` (Correctness, `pat/ctor/control.rs:107-112`)
- F-011 `try_walk_through_cast` returns immediately if cast has no inputs but the IR validator guarantees one — defensive but the comment claims "every cast has exactly one value input by signature" (Correctness, `matcher/walk_through.rs:48-54`)
- F-012 `rewrite_rule` documents that rooting on `Load`/`Store`/`Call` is an error but no test pins it (Correctness, `rewrite.rs:34-41`)
- F-013 `pub use error::{is_skip, ...}` — `is_skip` has zero external callers (Dead code, `lib.rs:127`)
- F-014 `pub use error::{... RewriteSkip}` — `RewriteSkip` has zero external callers (Dead code, `lib.rs:127`)
- F-015 `pub use pat::traits::{BuildCtx, BuildOutcome}` — neither type has external callers (Dead code, `lib.rs:136`)
- F-016 `pub PredicateFn` / `pub MatchPredicateFn` type aliases have zero external callers (Dead code, `lib.rs:143`, `pat/mod.rs:23-34`)
- F-017 `Match::{get_int_const, get_bool_const, get_float_bits, get_float_const, get_vn, bindings_clone}` — all six have zero external callers (Dead code, `matcher/match_result.rs:128-198`)
- F-018 `FunctionArgHandle` and `Matcher::{function_arg, function_args, function_arg_count, function_arg_len}` API has zero external callers (Dead code, `matcher/mod.rs:198-262`, `matcher/function_arg_handle.rs`)
- F-019 `pat::node_pat::{KindSpec, InputsSpec, OutputsSpec, ConsumersSpec, BuildTy, NodePat}` — `pub` inside `pub(crate) mod` is misleading; effective visibility is `pub(crate)` (Dead code/Readability, `pat/node_pat.rs:59,149,184,216,266,275`)
- F-020 `Pat::capture_impl` / `when_impl` are private but only called by the blanket `IntoPat::{capture, when}`; the indirection is dead (Dead code, `pat/mod.rs:157-191`)
- F-021 `predicate(f)` ctor has zero external callers (Dead code, `pat/ctor/wildcards.rs:119-124`)
- F-022 8 `anyhow::Error::new(crate::error::MissingBinding($name))` sites would collapse to one helper (Duplication & unification, multiple files)
- F-023 `KindSpec::variant_with`-with-payload-equality closure replicated 5 times (Duplication & unification, `pat/builders/{memory,phi,function_arg}.rs`)
- F-024 `IntBinaryOpPat` / `BoolBinaryOpPat` / `FloatBinaryOpPat` are mechanically identical (Duplication & unification, `pat/builders/binary_op.rs`)
- F-025 `impl_variant_any!` macro has 3 near-duplicate arms — unary/cmp/binary diverge only by arity & commutativity field (Duplication & unification, `pat/ctor/variant_agnostic.rs:29-119`)
- F-026 `pat/ctor/casts.rs::unary_node` and `pat/ctor/float.rs::unit_conv` are the same helper (Duplication & unification, `pat/ctor/casts.rs:13-20`, `pat/ctor/float.rs:82-89`)
- F-027 `decl_var!` / `decl_bind_get!` / `decl_pat_*_ops!` / `op_variant_contract!` / `simple_unary_cast!` — five separate macro families that all share "13 op-family kinds" boilerplate (Duplication & unification, multiple files)
- F-028 `cast_mask_of` exhaustive match overlaps with the cast list in `try_walk_through_cast`; the match-only enumerated set is the spec, but it's not a single shared list (Duplication & unification, `matcher/cast_mask.rs:40-91`)
- F-029 `int_const` ctor uses tautological `variant_with(_, |_| true)` instead of `variant(_)` (Simplification, `pat/ctor/wildcards.rs:51-54`)
- F-030 `Pat::capture_impl` / `when_impl` add an indirection via `self.into().capture_impl(v)` from the blanket impl — could be inlined (Simplification, `pat/mod.rs:157-220`)
- F-031 `Match::root` is `pub` (a `NodeId`) but every other Match accessor uses methods; mixing field/method API style (Simplification/Readability, `matcher/match_result.rs:21-25`)
- F-032 `try_once`'s `swap` defensive guard is documented as redundant — the caller already enforces `pats.len() == 2` (Simplification, `pat/node_pat.rs:464-470`)
- F-033 `CallOtherPat::From<...>::for Pat` constructs a `KindSpec::variant(&CallOther { user_op_id: 0 })` for the no-filter case but `KindSpec::Exact(...)` for the filter case (Simplification, `pat/builders/call.rs:122-125`)
- F-034 `ConsumersSpec` matching uses `match_node_id` (no walk-through), `OutputsSpec` matching uses `match_one` (with walk-through) — inconsistent dispatch (Simplification, `pat/node_pat.rs:494-521`)
- F-035 `bool_unary` lists `Neg` as the only variant — the bool unary family has only one variant, so `decl_pat_unary_ops!(bool_unary, ...)` adds a single wrapper around a single-variant op (Simplification/Readability, `pat/ctor/bool_.rs:31-44`)
- F-036 `crate::build::rewrite_rule` doclinks reference a path that does not exist (Readability, `pat/traits.rs:11`, `matcher/mod.rs:187`, `macros.rs:22`)
- F-037 `crate::ErrorKind::MissingBinding` doc reference points to a path that does not exist (Readability, `pat/ctor/consts.rs:113`)
- F-038 `Pat::when_impl` doclink in PredicateFn doc — `when_impl` is a private name (Readability, `pat/mod.rs:22`)
- F-039 `BUG-19` codename references in three places (Readability, `matcher/mod.rs:51`, tests)
- F-040 `function_arg_index()` builds the index lazily but the doc claims "single preorder walk per find_all"; description doesn't mention the lazy-OnceCell behavior (Readability, `matcher/mod.rs:62-68, 136-148`)
- F-041 `Bindings::default` / `mark` / `restore` are `pub(crate)` but `Bindings` itself is `pub` — the only way for an external caller to actually use the type is through `Match::bindings_clone` and `MatchPredicateFn` callbacks (Readability, `matcher/bindings.rs:30-73`)
- F-042 The `// `\<sample\>` payload-don't-care exemplar` pattern is unidiomatic — exemplar_vn / IntConst(0u128) / BoolConst(false) etc. have no docs explaining the pattern (Readability, multiple files)
- F-043 `decl_var!` doc on `Var` says "Each `Var::new()` call produces a globally unique id" but the inline doc on the macro itself says ids "are unique across both kinds and all types" — neither actually explains _why_ that matters (Readability, `var.rs:11-65`)
- F-044 Match::get_vn doc has a "well-defined only for a handful of producer kinds" caveat that lists 2 cases; future producers will silently return None without a debug assert (Readability, `matcher/match_result.rs:160-198`)
- F-045 `pat/mod.rs` `decl_any_const!` macro has an "unreachable" comment in the post_match arm but the unreachable case is functionally suppressed — the comment doesn't note it's defensive against later kind-relaxation (Readability, `pat/mod.rs:78-87`)
- F-046 `Bindings` linear-scan lookup design doc explains why HashMap was rejected, but the cost analysis cites "≤10 bindings" without measurement; on a complex `*_const_with!` rule chain the count can be larger (Readability, `matcher/bindings.rs:25-29`)
- F-047 `Matcher::find_all` allocates a fresh `Bindings` per candidate and clones it into the `Match` — could reuse via mark/restore for failed matches (Performance, `matcher/mod.rs:163-180`)
- F-048 `KindSpec::clone()` for `VariantWith` clones an `Arc` but `kind_spec()` is called every `find_all` invocation (Performance, `pat/node_pat.rs:286-288`, `matcher/mod.rs:163-167`)
- F-049 `ConsumersSpec`-with-walk_through omitted (also under F-034) — when `ignore_control_states` is set, the user expectation is that `if_node().true_branch(p)` works, but the implementation skips it (Performance/Correctness, `pat/node_pat.rs:510-521`)
- F-050 `Bindings::bind_*` does an O(n) linear scan on every bind to detect conflict; on commutative-retry-heavy patterns this is repeated work (Performance, `matcher/bindings.rs:79-107`)

## Findings

### Correctness

### F-001 `IntoPat` blanket impl gives every builder a meaningless `.capture(v)`

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/mod.rs:203-224`
- **What:**
  ```rust
  pub trait IntoPat: Into<Pat> + Sized {
      /// After matching, bind the matched **value** output to `v`.
      ///
      /// **Only meaningful on value-producing patterns.**  Control-flow
      /// builders (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`) expose
      /// `.capture_node(nv)` instead — calling `.capture(v)` on one of those
      /// compiles (via this blanket impl) but the resulting pattern never
      /// matches, because `Var` refers to a data edge and those nodes have
      /// no value output to bind.
      fn capture(self, v: Var) -> Pat { self.into().capture_impl(v) }
      // ...
  }
  impl<T: Into<Pat>> IntoPat for T {}
  ```
- **Why:** The doc of `pat/builders/mod.rs:11-25` (the "Capture rule" doc block) says: "No builder exposes both. A user never has to choose between them on a given builder." That promise is false at the type level — `CallPat`, `IfPat`, `RetPat`, `CallOtherPat` all expose **both** `.capture(v)` (via the blanket impl) and `.capture_node(nv)`. There's no compile-time enforcement of the "value-producing only" restriction. The runtime behavior is salvaged by the value-kind check inside `CapturePat::try_match` (`pat/any.rs:77`) — which silently returns `false` rather than producing a useful diagnostic. CLAUDE.md says: "No builder exposes both" — that statement is also false.
- **Proposed change:** Either (a) move `.capture` / `.when` from the blanket trait to a specific `ValueProducing` marker trait that only data-producing builders implement, or (b) keep the blanket impl but fail loudly (e.g. via `unreachable!()` in a build-only context, or a `#[deprecated]` annotation on the unsupported pattern). The current "compiles fine, never matches" footgun is invisible to users who don't read the doc.
- **Confidence:** high
- **Risk if applied:** medium (touches the public trait surface)

### F-002 `IntoPat::capture` doc claims control-flow builders "never match" — `Call`/`CallOther` actually do

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/mod.rs:204-211`
- **What:**
  ```rust
  /// **Only meaningful on value-producing patterns.**  Control-flow
  /// builders (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`) expose
  /// `.capture_node(nv)` instead — calling `.capture(v)` on one of those
  /// compiles (via this blanket impl) but the resulting pattern never
  /// matches, because `Var` refers to a data edge and those nodes have
  /// no value output to bind.
  ```
- **Why:** `Call`'s `expected_signature` outputs are `[Control, Memory, retval0, retval1, ...]`. Several of those slots ARE value outputs. `CapturePat::try_match` iterates outputs (via `try_match_node`) and binds the first value output it finds — which IS the first ret-val slot. So `call().capture(v)` on a Call with ret-val outputs *does* match and binds `v` to the first ret-val output. Same for `CallOther` with a `ret_ty: Some(...)`. The doc's "never matches" claim is true only for `If` and `Return` (zero-output / control-only), not for `Call`/`CallOther`. Users who naively trust the doc will silently miss a footgun (binding the first ret-val slot when they meant something else).
- **Proposed change:** Either fix the doc to say "may bind the first value output as a side-effect; you almost certainly want `.capture_node(nv)` instead" or actually disable `.capture(v)` on those builders.
- **Confidence:** high
- **Risk if applied:** low

### F-003 `if_node().true_branch(p)` does NOT walk through `ControlState` even with `ignore_control_states` set

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/node_pat.rs:510-521`, `crates/pattern/src/lib.rs:99-103`
- **What:**
  ```rust
  // pat/node_pat.rs (ConsumersSpec arm of try_once)
  if let (ConsumersSpec::Indexed(items), Some(outputs)) = (&pat.consumers, &outputs) {
      for (i, p) in items {
          let Some(&out) = outputs.get(*i) else { return false; };
          let Some(consumer) = walk::next_control_node(ctx.matcher, out) else {
              return false;
          };
          if !ctx.matcher.match_node_id(consumer, p, b) {
              return false;
          }
      }
  }
  ```
  vs. the doc:
  ```rust
  // lib.rs
  //! * [`Matcher::ignore_control_states`] — walk through `ControlState`
  //!   (region-join) nodes when traversing control chains.  Lets
  //!   `ret(call(...))` cross region joins between the Return and the Call.
  ```
  And `MatcherOptions::ignore_control_states`'s doc explicitly says "Lets `ret(call(...))`, `if_node().true_branch(p)` etc. cross region joins". (`crates/pattern/src/matcher/mod.rs:54-56`)
- **Why:** `ConsumersSpec`-driven matching uses `match_node_id`, which short-circuits to `try_match_node` and never invokes `match_output_with_walk_through`. For the `ret(call(...))` case the walk-through fires (because `RetPat`'s `preceded_by` populates an `InputsSpec::Indexed` slot, which goes through `match_one` → `match_output_with_walk_through`). But `IfPat::true_branch(p)` populates `ConsumersSpec::Indexed`, which uses the no-walk-through dispatch path. The doc claim about `if_node().true_branch(p)` is unsupported by the implementation. No test pins this.
- **Proposed change:** Either (a) change the `ConsumersSpec` matching to honor `ignore_control_states` by routing through `match_output_with_walk_through` (but this is the **forward** direction, so a separate "skip ControlState forward" helper is needed), or (b) update the docs to drop `if_node().true_branch(p)` from the supported list.
- **Confidence:** high
- **Risk if applied:** medium (option (a) is a real feature change)

### F-004 Example: `add(phi_for(rbx), any())` expects 0 matches; commutativity should yield 1

- **Category:** Correctness
- **Location:** `crates/pattern/examples/pattern_query.rs:573-577`
- **What:**
  ```rust
  // Build: rax + rbx, return result   (rax_val = lhs, rbx_val = rhs)
  let sum = b.build_int_binary_operation(rax_val, rbx_val, IntBinaryOp::Add, NodeOutputType::U64)...

  let hits = m.find_all(&add(phi_for(rax_vn), any()).into());
  println!("add(rax_phi, _): {}", hits.len()); // 1

  let hits_wrong = m.find_all(&add(phi_for(rbx_vn), any()).into());
  println!("add(rbx_phi, _): {}", hits_wrong.len()); // 0 — rax is lhs, rbx is rhs
  ```
- **Why:** `add(...)` returns an `IntBinaryOpPat` whose `From<...>::for Pat` impl uses `InputsSpec::fixed_commutative` for `IntBinaryOp::Add` (`pat/builders/binary_op.rs:53-57`, `matcher/commutativity.rs:5-10`). With commutative retry on, the matcher tries operands in both orders — so `add(phi_for(rbx), any())` should match the swapped order and return 1, not 0. Either: (a) the example's comment is stale (likely — the comment predates commutative-retry being default), or (b) commutative retry is buggy on this specific path. Examples are documentation; a stale expected-output comment misleads new users about how the pattern engine behaves.
- **Proposed change:** Re-run the example, verify the printed value, and update the comment to match. Add `.ordered()` to demonstrate the original intent if the author wanted strict ordering.
- **Confidence:** high
- **Risk if applied:** low

### F-005 Documented commutative ops list omits `int_cmp`/`int_cmp_any`

- **Category:** Correctness / Readability
- **Location:** `crates/pattern/src/lib.rs:80-82`, `crates/pattern/src/pat/ctor/int.rs:74-85`
- **What:**
  ```rust
  // lib.rs (the public-facing crate-level summary):
  //! Binary operations that are mathematically commutative (`add`, `mul`, `and`,
  //! `or`, `xor`, `bool_and`, `bool_or`, `bool_xor`) automatically try both
  //! operand orderings.  Call `.ordered()` on the returned builder to opt out.

  // pat/ctor/int.rs::int_cmp:
  pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
      let inputs = if is_commutative_int_cmp_op(op) {
          InputsSpec::fixed_commutative(lhs.into(), rhs.into())
      } else {
          InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()])
      };
      ...
  }
  ```
- **Why:** `int_cmp(IntCmpOp::Equal, _, _)` does try commutative retry (verified by `int_cmp_any_retries_swap_only_for_commutative_cmp` in `tests/matching/variant_agnostic.rs:104`). The lib-level summary list mentions only the seven binary ops, omitting `int_eq`/`int_carry`/`int_scarry` (and the variant-agnostic `int_cmp_any`). Users reading lib.rs's summary won't know that swapping operands of `int_eq(5, 3)` matches; they'll think they need `.ordered()`.
- **Proposed change:** Add `int_eq`, `int_carry`, `int_scarry`, plus their `*_any` counterparts to the lib.rs commutativity bullet. Also clarify that `float_cmp_any` does **not** commute (matches the implementation — see F-006 for the related correctness gap on `float_cmp` itself).
- **Confidence:** high
- **Risk if applied:** low

### F-006 `float_cmp(Equal, _, _)` does NOT commute even though mathematically commutative

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/ctor/float.rs:60-67`, `crates/pattern/src/matcher/commutativity.rs`
- **What:**
  ```rust
  // float.rs::float_cmp:
  pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
      NodePat::matcher(
          KindSpec::Exact(NodeKind::FloatCmpOp(op)),
          InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()]),  // ← always ordered
      )
      ...
  }
  ```
  And `matcher/commutativity.rs` exports `is_commutative_int_cmp_op` but no `is_commutative_float_cmp_op`. The variant-agnostic `float_cmp_any` (`pat/ctor/variant_agnostic.rs:174-179`) explicitly uses the `cmp` macro arm without commutativity, and its docstring says "No float comparison operators are currently treated as commutative".
- **Why:** `FloatCmpOp::Equal` and `FloatCmpOp::NotEqual` are mathematically commutative (NaN aside, but neither side of the comparison's order changes IEEE-754 NaN behavior). The asymmetry with `int_eq` is unmotivated. A user pattern `float_cmp(Equal, var(x), int_const(0))` won't match `FloatCmpOp::Equal(0, x)`. CLAUDE.md says "and `bool_xor` and the `BoolBinaryOp` equivalents" are commutative but says nothing about cmp ops being expected to commute on the float side.
- **Proposed change:** Add `is_commutative_float_cmp_op(op) -> bool` matching `Equal | NotEqual`, and route `float_cmp` / `float_cmp_any` through `fixed_maybe_commutative` like `int_cmp` does. Update `float_cmp_any`'s docstring.
- **Confidence:** medium (NaN semantics under reordering of float comparisons is a question; but `FloatCmpOp::Equal` is symmetric for non-NaN inputs and IEEE-754 doesn't define a "side", so commutativity is sound at the IR level)
- **Risk if applied:** low

### F-007 `int_cmp(...)` returns bare `Pat` so `.ordered()` is unavailable

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/ctor/int.rs:76-85`
- **What:**
  ```rust
  pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
      let inputs = if is_commutative_int_cmp_op(op) {
          InputsSpec::fixed_commutative(lhs.into(), rhs.into())
      } else {
          InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()])
      };
      NodePat::matcher(KindSpec::Exact(NodeKind::IntCmpOp(op)), inputs)
          .with_build_exact(NodeKind::IntCmpOp(op), BuildTy::Fixed(...))
          .into_pat()
  }
  ```
- **Why:** `int_binary` returns `IntBinaryOpPat` (a typed builder), so `int_binary(Add, l, r).ordered()` works. `int_cmp(Equal, l, r)` returns a plain `Pat`; it tries commutative retry by default (since `Equal` is commutative) but the user has no way to opt out. To force `.ordered()` you'd need to drop down to `int_binary_any` or hand-roll a `NodePat`. Asymmetry with `int_binary`. CLAUDE.md doesn't mention this.
- **Proposed change:** Introduce `IntCmpOpPat` mirroring `IntBinaryOpPat`, supporting `.ordered()` and `.capture(v)` and `.when(f)` by default via `IntoPat`.
- **Confidence:** high
- **Risk if applied:** medium (introduces a new public type — but it's a strict superset)

### F-008 `RetPat::ret_val` doc says "0-based after the ctrl input" but offset is `2 + i`

- **Category:** Correctness / Readability
- **Location:** `crates/pattern/src/pat/builders/ret.rs:30-31`, `crates/pattern/src/pat/builders/ret.rs:54-56`
- **What:**
  ```rust
  /// Constrain return value at position `idx` (0-based after the ctrl input).
  pub fn ret_val(mut self, idx: usize, p: impl Into<Pat>) -> Self {
      self.ret_vals.push((idx, p.into()));
      self
  }

  // From-impl:
  // Return inputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
  for (i, p) in ret_vals {
      indexed_inputs.push((2 + i, p));
  }
  ```
- **Why:** "0-based after the ctrl input" suggests `idx=0 → input slot 1`. But the implementation maps `idx=0 → input slot 2` (skipping mem too). The implementation is correct; the doc is misleading. Code at line 47-48 says: "Return inputs: [ctrl(0), mem(1), retval0(2), ...]." — 2 doesn't reduce to "after ctrl"; it's "after ctrl AND mem".
- **Proposed change:** Change the doc to "0-based after the ctrl and mem inputs (slot `2 + idx`)" — matches `CallPat::arg`'s wording at builders/call.rs:30 ("0-based, after ctrl and mem inputs"). Same fix applies to consistency.
- **Confidence:** high
- **Risk if applied:** low

### F-009 Example: `ret().preceded_by(call(0x1000))` claims 1 match; the documented behavior is direct-step

- **Category:** Correctness / Readability
- **Location:** `crates/pattern/examples/pattern_query.rs:184-202`, `crates/pattern/src/pat/builders/ret.rs:21-29`
- **What:**
  ```rust
  // examples/pattern_query.rs (graph: call(0x1000); call(0x2000); return):
  // ── preceded_by: find a return that follows a specific call ───────────────
  println!(
      "ret preceded by call(0x2000): {}",
      m.find_all(&ret().preceded_by(call().at(0x2000)).into()).len()
  ); // 1

  // preceded_by walks the full backward chain, so earlier calls are found too
  println!(
      "ret preceded by call(0x1000): {}",
      m.find_all(&ret().preceded_by(call().at(0x1000)).into()).len()
  ); // 1
  ```
  vs.
  ```rust
  // pat/builders/ret.rs:
  /// Match `p` against the Return's **direct** ctrl predecessor (the node
  /// producing input slot 0 — typically a `ControlState` at a region
  /// header).  This is a single-step match, not a backward walk through the
  /// CFG; to reach a non-adjacent ancestor the caller must structure `p`
  /// accordingly (e.g. `.preceded_by(cs().preceded_by(call()))`).
  pub fn preceded_by(mut self, p: impl Into<Pat>) -> Self { ... }
  ```
- **Why:** The example's comment ("preceded_by walks the full backward chain") is the opposite of the doc. If the example is trustworthy and `// 1` is the actual printed value, then `preceded_by(call(0x1000))` finds the call somehow — perhaps via the default `try_match` calling `get_node_from_output(ctrl_input)` then iterating outputs, but that wouldn't reach a call two control-flow steps back. If the doc is right and the example output is wrong, that's a bug. Either way the example doesn't agree with the doc.
- **Proposed change:** Update the example's comment to match the doc, or add `.ignore_control_states()` and verify the output. The example's "Will print IntConst(0x2000) since 0x2000 is immediately before return" later (line 219-220) is consistent with direct-step; the inconsistent comment seems hand-edited.
- **Confidence:** medium (haven't run the example to confirm)
- **Risk if applied:** low

### F-010 `call_other` ctor doc mentions `.capture()` but `CallOtherPat` only exposes `.capture_node()`

- **Category:** Correctness
- **Location:** `crates/pattern/src/pat/ctor/control.rs:107-112`
- **What:**
  ```rust
  /// Starts building a `CallOther` (user-defined op) pattern.  Chain
  /// `.user_op_id()`, `.arg()`, `.capture()` to add constraints.
  #[must_use]
  pub fn call_other() -> CallOtherPat { CallOtherPat::new() }
  ```
- **Why:** `CallOtherPat` (per `pat/builders/call.rs:93-114`) exposes `.user_op_id()`, `.arg()`, `.capture_node()` — not `.capture()`. The doc is wrong. Per the capture rule, control-flow builders use `.capture_node(nv: NodeVar)`. This isn't merely a typo — it propagates the F-001 confusion (since `.capture(v)` does compile via the blanket trait, but it's the wrong handle).
- **Proposed change:** `Chain `.user_op_id()`, `.arg()`, `.capture_node()` to add constraints.` And add a similar fix to `call_other` in the lib-level docs if any exist.
- **Confidence:** high
- **Risk if applied:** low

### F-011 `try_walk_through_cast`: defensive comment says "every cast has exactly one value input by signature"

- **Category:** Correctness
- **Location:** `crates/pattern/src/matcher/walk_through.rs:48-54`
- **What:**
  ```rust
  // Casts have exactly one input; treat any unexpected shape as
  // "can't walk through" rather than panicking.
  let inputs = ctx.graph.graph.node_inputs(producer);
  let Some(value_input) = inputs.into_iter().next() else {
      return false;
  };
  ```
- **Why:** The check is correct — `Truncate`, `Extend`, `CastToInt/Bool/Float`, `IntBitsToFloat`, `FloatBitsToInt` all have exactly one value input per `expected_signature`. The defensive `else { return false; }` is fine. But the comment glosses over a corner case: if the IR's `node_inputs` for a cast somehow returned ≥ 2 elements (validator regression), the helper would silently use input[0] and walk through to it. The validator should catch invalid IR before pattern matching, but the comment frames this as "treat any unexpected shape as 'can't walk through'" — yet the implementation only handles the zero-input case, not the multi-input case (where the silent walk-through is exactly what we want to AVOID).
- **Proposed change:** Either (a) assert input count == 1 and document that the validator guarantees it, or (b) treat input count != 1 as "can't walk through" by checking `inputs.len() == 1` explicitly before unwrapping. The current code is sound but the comment overpromises.
- **Confidence:** medium
- **Risk if applied:** low

### F-012 `rewrite_rule` documents that rooting on `Load`/`Store`/`Call` is an error but no test pins it

- **Category:** Correctness
- **Location:** `crates/pattern/src/rewrite.rs:34-41`
- **What:**
  ```rust
  /// # Single-value-output constraint
  ///
  /// The LHS root must have exactly one value output — the rule redirects that
  /// output's uses to the RHS-built output.  Rooting a rule on a multi-output
  /// node (any `Call`, `Load`, `Store`, control-flow node) returns an
  /// `IrError` from `node_outputs_exact::<1>`.  If you need to rewrite a
  /// multi-slot producer, operate on the value slot explicitly (e.g. match
  /// the slot-consumer rather than the producer).
  ```
- **Why:** The constraint is real (`fg.graph.node_outputs_exact::<1>(node)?` at rewrite.rs:60). No test in `crates/pattern/tests/matching/rewrite.rs` exercises this path — i.e. there's no `rewrite_rule(load(), ...)` or `rewrite_rule(call(), ...)` test that asserts the error fires. Tests pin all the "user-side errors" (NotBuildable, MissingBinding, RewriteSkip) but not this graph-side error. A regression in `node_outputs_exact` arity (e.g. relaxing to "first value output") would silently change behavior. The doc claim "returns an `IrError`" is therefore unverified.
- **Proposed change:** Add a test that constructs a `Load` graph, builds `rewrite_rule(load(), int_const(0))` (or similar), and asserts the call to the rule on the `Load` node returns `Err(_)` with a specific downcast.
- **Confidence:** high
- **Risk if applied:** low

### Dead code

### F-013 `pub use error::is_skip` — zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/lib.rs:127`
- **What:**
  ```rust
  pub use error::{is_skip, skip, MissingBinding, NotBuildable, Result, RewriteSkip};
  ```
- **Why:** `is_skip(err: &anyhow::Error) -> bool` (`error.rs:48-50`) is the test-the-sentinel half of the skip API. Its only call site is `crate::error::is_skip(&e)` inside `rewrite.rs:76` — internal. `grep -rn 'is_skip' crates/` outside `crates/pattern/` returns zero hits. External code uses `pattern::skip()` to produce the sentinel, but doesn't need `is_skip` to test for it (the rewrite-rule interpreter handles that internally). Could be `pub(crate)`.
- **Proposed change:** Drop `is_skip` from the lib re-export. Keep the function `pub(crate)` for the interpreter's use.
- **Confidence:** high
- **Risk if applied:** low

### F-014 `pub use error::RewriteSkip` — zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/lib.rs:127`
- **What:** Same line as F-013. `RewriteSkip` is the type carried by the skip sentinel. External callers use `pattern::skip()` to produce it — they don't downcast to `RewriteSkip`. Tests in `crates/pattern/tests/matching/rewrite.rs:198-247` downcast only to `pattern::NotBuildable` and `pattern::MissingBinding`, never to `RewriteSkip`. Grep confirms.
- **Why:** `RewriteSkip` is purely an internal sentinel type. `pub` re-export is unnecessary.
- **Proposed change:** Drop `RewriteSkip` from the lib re-export; keep it as `pub` inside `error` for the interpreter.
- **Confidence:** high
- **Risk if applied:** low

### F-015 `pub use pat::traits::{BuildCtx, BuildOutcome}` — zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/lib.rs:136`
- **What:**
  ```rust
  pub use pat::traits::{BuildCtx, BuildOutcome};
  ```
- **Why:** `BuildCtx` is the closure parameter type for `int_const_with_fn` / `bool_const_with_fn` / `float_const_with_fn`. The `*_const_with!` macros expand to closures of shape `move |__strider_ctx: &$crate::BuildCtx<'_>| { ... }`, so the macros need the path. But external callers always use the `*_const_with!` macros, never naming `BuildCtx` directly. `BuildOutcome` is even more internal — the rewrite interpreter uses it; nothing else does. Grep confirms zero external usage.
- **Proposed change:** Either keep both in scope but note that the macros are the user-facing surface, OR (better) demote them via `pub(crate)` and have the macros qualify with the full path. Today the re-export is documentation noise.
- **Confidence:** high
- **Risk if applied:** low (BuildCtx must remain pub for macro expansion, but BuildOutcome could go pub(crate))

### F-016 `pub PredicateFn` / `pub MatchPredicateFn` type aliases have zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/lib.rs:143`, `crates/pattern/src/pat/mod.rs:23-34`
- **What:**
  ```rust
  // pat/mod.rs:
  pub type PredicateFn =
      Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync>;
  pub type MatchPredicateFn = Arc<
      dyn Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool + Send + Sync,
  >;

  // lib.rs:
  pub use pat::{IntoPat, MatchPredicateFn, Pat, PredicateFn};
  ```
- **Why:** Grep across the workspace finds zero external uses of either name. These aliases exist as helpful documentation for the closure shape, but the API surface they back (`Pat::when` / `Pat::when_match`) takes generic `F: Fn(...) -> bool`, not `Arc<dyn Fn(...)>`. So neither type alias is part of the user surface. They're internal storage types for `GuardPat`.
- **Proposed change:** Demote to `pub(crate)` and remove from `lib.rs` re-exports.
- **Confidence:** high
- **Risk if applied:** low

### F-017 `Match::{get_int_const, get_bool_const, get_float_bits, get_float_const, get_vn, bindings_clone}` have zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/matcher/match_result.rs:128-198`
- **What:**
  ```rust
  pub fn get_int_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u128> { ... }
  pub fn get_bool_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<bool> { ... }
  pub fn get_float_bits(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> { ... }
  pub fn get_float_const(&self, fv: FloatVar) -> Option<u64> { ... }
  pub fn get_vn(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<rsleigh::Vn> { ... }
  pub fn bindings_clone(&self) -> Bindings { ... }
  ```
- **Why:** Grep shows external callers use `m.get(v)`, `m.get_int(iv)`, `m.get_bool(bv)`, `m.get_node(nv)` — the typed accessors. The `*_const` and `*_bits` helpers (which take `Var` + `&Graph` and resolve through the producer's NodeKind) and `get_vn` and `bindings_clone` have zero hits in `crates/strider/`, `crates/opt/`, or anywhere else. Tests in `crates/pattern/tests/` exercise them, but only as internal contract checks. `bindings_clone` is used internally by `rewrite_rule`. So six accessors are dead from an external perspective.
- **Proposed change:** Demote to `pub(crate)` (for `bindings_clone`, since it's used internally) or remove. The `get_*_const` helpers may become useful for the planned `strider-py` surface, but they aren't today; mark them `#[allow(dead_code)]` with a TODO or drop them.
- **Confidence:** high
- **Risk if applied:** low (`bindings_clone` is reachable only through `Match`, so demoting it is cheap)

### F-018 `FunctionArgHandle` and `Matcher::function_arg*` API has zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/matcher/mod.rs:198-262`, `crates/pattern/src/matcher/function_arg_handle.rs`
- **What:**
  ```rust
  // matcher/mod.rs:
  pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'g>> { ... }
  pub fn function_arg_count(&self) -> usize { ... }
  pub fn function_arg_len(&self) -> usize { ... }
  pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)> + '_ { ... }
  ```
- **Why:** Grep across the workspace: `\.function_arg(` / `\.function_args(` / `\.function_arg_count(` / `\.function_arg_len(` — zero hits outside `crates/pattern/`. The only mentions in other crates are doc-comments. `FunctionArgHandle` itself: zero external uses. Tests in `tests/matching/ssa.rs:155-179` exercise the API as a public-API contract check. So the entire API surface is internal-only / future-API. The implementation also costs an `OnceCell<HashMap>` per `Matcher`, which is paid even when no caller uses the API.
- **Proposed change:** Demote the entire `function_arg*` API and `FunctionArgHandle` to `pub(crate)`, OR keep public for the strider-py future but mark dead-code in CI.
- **Confidence:** high
- **Risk if applied:** low

### F-019 `pat::node_pat::{KindSpec, InputsSpec, OutputsSpec, ConsumersSpec, BuildTy, NodePat}` are `pub` inside `pub(crate) mod`

- **Category:** Dead code / Readability
- **Location:** `crates/pattern/src/pat/node_pat.rs:59,149,184,216,266,275`
- **What:**
  ```rust
  // pat/mod.rs:
  pub(crate) mod node_pat;

  // pat/node_pat.rs:
  pub enum KindSpec { ... }
  pub enum BuildTy { ... }
  pub struct NodePat { ... }
  pub enum InputsSpec { ... }
  pub enum OutputsSpec { ... }
  pub enum ConsumersSpec { ... }
  ```
- **Why:** `pat::node_pat` is declared `pub(crate)`, so `pub` items inside it are at most `pub(crate)` from the outside-crate perspective. The `pub` annotations are technically inert. They don't broadcast intent. A reader scanning for the public surface sees these "pub" items and may wonder why they're not in `lib.rs`'s re-export list.
- **Proposed change:** Change to `pub(crate)` to honestly reflect their visibility. Or, if the intent is that these become public for advanced consumers, expose them in `lib.rs` (but I see no driver for that — the spec enums are infrastructure, not user-facing).
- **Confidence:** high
- **Risk if applied:** low

### F-020 `Pat::capture_impl` / `Pat::when_impl` are private but only called by the blanket `IntoPat::{capture, when}`

- **Category:** Dead code / Simplification
- **Location:** `crates/pattern/src/pat/mod.rs:157-220`
- **What:**
  ```rust
  impl Pat {
      fn capture_impl(self, v: Var) -> Pat { ... }  // private, called only from below
      fn when_impl<F>(self, f: F) -> Pat ...        // private, called only from below
  }

  pub trait IntoPat: Into<Pat> + Sized {
      fn capture(self, v: Var) -> Pat { self.into().capture_impl(v) }
      fn when<F>(self, f: F) -> Pat ... { self.into().when_impl(f) }
  }
  ```
- **Why:** The `*_impl` private methods exist solely so the blanket impl can dispatch. They're a thin wrapper. Inlining their bodies into `IntoPat::capture` / `IntoPat::when` and dropping `Pat::capture_impl` / `Pat::when_impl` saves two methods. Since `Pat: Into<Pat>` (via `From`-reflexive), `Pat` itself benefits from the blanket impl directly.
- **Proposed change:** Inline:
  ```rust
  fn capture(self, v: Var) -> Pat {
      let inner: Pat = self.into();
      Pat::from_dyn(Arc::new(crate::pat::any::CapturePat { inner, var: v }))
  }
  ```
  and remove `Pat::capture_impl` / `Pat::when_impl`. Doc references to `Pat::when_impl` (e.g. `pat/mod.rs:22`) become stale anyway — see F-038.
- **Confidence:** high
- **Risk if applied:** low

### F-021 `predicate(f)` ctor has zero external callers

- **Category:** Dead code
- **Location:** `crates/pattern/src/pat/ctor/wildcards.rs:119-124`
- **What:**
  ```rust
  pub fn predicate<F>(f: F) -> Pat
  where
      F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
  {
      any().when(f)
  }
  ```
- **Why:** Grep across `crates/{strider,opt,cfg}/` finds zero calls. Internal tests use `predicate(...)` — that's not a workspace-callers count. The doc says "Equivalent to `any().when(f)`". The function is just an alias.
- **Proposed change:** Either demote to `pub(crate)` (it's tested internally, so it stays accessible inside the crate), or leave it for the future `strider-py` surface. Lean toward demotion now.
- **Confidence:** high
- **Risk if applied:** low

### Duplication & unification

### F-022 8 sites of `anyhow::Error::new(crate::error::MissingBinding($name))`

- **Category:** Duplication & unification
- **Location:** Multiple files (8 occurrences)
  - `crates/pattern/src/pat/any.rs:41` — `"Var"`
  - `crates/pattern/src/pat/any.rs:96` — `"Var"`
  - `crates/pattern/src/pat/mod.rs:93` — uses macro variable `$missing`
  - `crates/pattern/src/pat/ctor/consts.rs:126` — uses macro variable `$kind_name`
  - `crates/pattern/src/pat/ctor/variant_agnostic.rs:50,78,106` — uses `$missing`
  - `crates/pattern/src/pat/traits.rs:98` — `NotBuildable` (related)
  - `crates/pattern/src/pat/node_pat.rs:310,330` — `NotBuildable`
- **What:**
  ```rust
  ctx.bindings
      .get(self.var)
      .ok_or_else(|| anyhow::Error::new(crate::error::MissingBinding("Var")))?;
  ```
- **Why:** Identical wrapping pattern. A `MissingBinding::for_kind(name) -> anyhow::Error` constructor would collapse all sites:
  ```rust
  pub fn missing(kind: &'static str) -> anyhow::Error {
      anyhow::Error::new(MissingBinding(kind))
  }
  ```
  Same applies to `NotBuildable` (3 sites). Today the boilerplate is repeated and easy to typo (e.g. mistyping the kind string).
- **Proposed change:** Add `pub fn missing(kind: &'static str) -> anyhow::Error` and `pub fn not_buildable(name: &'static str) -> anyhow::Error` to `error.rs`. Replace each `anyhow::Error::new(...)` site with these helpers. Maybe also `is_missing(&err) -> bool` to mirror `is_skip`.
- **Confidence:** high
- **Risk if applied:** low

### F-023 `KindSpec::variant_with`-with-payload-equality closure replicated 5 times

- **Category:** Duplication & unification
- **Location:** `crates/pattern/src/pat/builders/{memory.rs,phi.rs,function_arg.rs}`
- **What:** Five sites all have the same shape:
  ```rust
  // phi.rs:
  Some(expected) => KindSpec::variant_with(
      &NodeKind::ControlPhi(exemplar_vn()),
      move |k| matches!(k, NodeKind::ControlPhi(actual) if *actual == expected),
  ),
  // memory.rs (Load):
  Some(s) => KindSpec::variant_with(
      &NodeKind::Load(rsleigh::VnSpace::RAM),
      move |k| matches!(k, NodeKind::Load(actual) if *actual == s),
  ),
  // ... StackStorePhi space, Store space, etc.
  ```
- **Why:** The "discriminant + single-payload-field equality" check is a recurring pattern. A small helper macro
  ```rust
  macro_rules! kind_variant_eq {
      ($exemplar:expr, $variant:ident, $field:expr) => {{
          let expected = $field;
          KindSpec::variant_with($exemplar, move |k| matches!(k, NodeKind::$variant(actual) if *actual == expected))
      }};
  }
  ```
  would collapse the boilerplate. The current sites are short but mechanically duplicated.
- **Proposed change:** Add a `match_payload!` (or `kind_variant_eq!`) macro and use it.
- **Confidence:** medium
- **Risk if applied:** low

### F-024 `IntBinaryOpPat` / `BoolBinaryOpPat` / `FloatBinaryOpPat` are mechanically identical

- **Category:** Duplication & unification
- **Location:** `crates/pattern/src/pat/builders/binary_op.rs`
- **What:** All three structs share the same fields (`op`, `lhs`, `rhs`, `ordered`), the same constructor, the same `.ordered()` method, and the same `From<...>::for Pat` impl that picks `fixed_commutative` vs `fixed_ordered` based on a per-family `is_commutative_*_op` predicate.
- **Why:** Three near-identical 30-line structs; a generic `BinaryOpPat<O>` parameterised by op enum + commutativity decider + result type would replace them. The current shape is verbose and any addition (e.g. a new `.with_kind(...)` method) needs three edits.
- **Proposed change:** Define a generic builder:
  ```rust
  pub struct BinaryOpPat<O: Copy> {
      op: O, lhs: Pat, rhs: Pat, ordered: bool,
      result_type_for_op: fn(O) -> NodeOutputType,
      kind_for_op: fn(O) -> NodeKind,
      is_commutative: fn(O) -> bool,
  }
  ```
  with type aliases `pub type IntBinaryOpPat = BinaryOpPat<IntBinaryOp>` etc. Or a macro that generates each struct from a single source.
- **Confidence:** medium
- **Risk if applied:** medium (touches the public API surface; type aliases preserve existing user code)

### F-025 `impl_variant_any!` macro has 3 near-duplicate arms

- **Category:** Duplication & unification
- **Location:** `crates/pattern/src/pat/ctor/variant_agnostic.rs:29-119`
- **What:** Three arms (`binary`, `cmp`, `unary`) of the macro share most of their body — they only diverge in arity (1 vs 2 inputs), commutativity decider (provided vs not), and the `lhs/rhs/operand` parameter list. The three arms are mechanically copy-pasted with small differences.
- **Why:** A nested macro-of-macros is overkill, but the duplication is mostly avoidable with a more parametric arm structure.
- **Proposed change:** Either accept the repetition (it's localised in one file) or factor the post-match closure into a helper function and parameterise the inputs spec via a callback. The benefit is mostly maintainability — adding a new field (e.g. type-restriction post-match) needs three edits today.
- **Confidence:** medium
- **Risk if applied:** medium

### F-026 `pat/ctor/casts.rs::unary_node` and `pat/ctor/float.rs::unit_conv` are the same helper

- **Category:** Duplication & unification
- **Location:** `crates/pattern/src/pat/ctor/casts.rs:13-20`, `crates/pattern/src/pat/ctor/float.rs:82-89`
- **What:**
  ```rust
  // casts.rs:
  fn unary_node(build_kind: NodeKind, build_ty: BuildTy, operand: impl Into<Pat>) -> Pat {
      NodePat::matcher(KindSpec::Exact(build_kind), InputsSpec::fixed_ordered(vec![operand.into()]))
          .with_build_exact(build_kind, build_ty)
          .into_pat()
  }

  // float.rs::unit_conv (note: drops `build_ty`, hardcodes InheritRoot):
  fn unit_conv(build_kind: NodeKind, operand: impl Into<Pat>) -> Pat {
      NodePat::matcher(KindSpec::Exact(build_kind), InputsSpec::fixed_ordered(vec![operand.into()]))
          .with_build_exact(build_kind, BuildTy::InheritRoot)
          .into_pat()
  }
  ```
- **Why:** `unit_conv` is a special-case of `unary_node` with `BuildTy::InheritRoot` baked in. Two functions, one in `casts.rs` and one in `float.rs`, with identical structure.
- **Proposed change:** Move `unary_node` to a shared place (e.g. `pat/ctor/mod.rs` as `pub(super)`) and replace `unit_conv` calls with `unary_node(NodeKind::IntToFloat, BuildTy::InheritRoot, operand)`.
- **Confidence:** high
- **Risk if applied:** low

### F-027 Five separate macro families that all share "13 op-family kinds" boilerplate

- **Category:** Duplication & unification
- **Location:** Multiple files
  - `crates/pattern/src/var.rs::decl_var!` (13 var types)
  - `crates/pattern/src/matcher/bindings.rs::decl_bind_get!` (13 bind/get pairs)
  - `crates/pattern/src/macros.rs::decl_pat_{binary,unary,cmp}_ops!` (op constructors)
  - `crates/pattern/tests/matching/bindings.rs::op_variant_contract!` (13 test contracts)
  - `crates/pattern/src/pat/ctor/casts.rs::simple_unary_cast!` (cast op constructors)
- **What:** Each macro family invokes a per-op-family pattern. The 13 capture-var types (`Var`, `NodeVar`, `IntVar`, `BoolVar`, `FloatVar`, plus the 8 op-variant types) are the recurring axis. Adding a new op family today requires editing FOUR files and FOUR macro invocations: declare a new `decl_var!`, a new `decl_bind_get!`, a new ctor wrapper macro family, and a new test contract.
- **Why:** Each macro is well-scoped, but the cross-cutting "13 op kinds" knowledge is duplicated across 5 macro families. A code-gen build script (or `proc_macro` + a single source-of-truth list) would reduce the surface to one source.
- **Proposed change:** Long-term: derive macro or build script. Short-term: a single shared source of truth (e.g. an `OP_FAMILIES` const-list with the metadata) consumed by all five macros.
- **Confidence:** medium
- **Risk if applied:** high (toolchain-level change for a derive macro)

### F-028 `cast_mask_of` exhaustive match overlaps with the cast list in `try_walk_through_cast`

- **Category:** Duplication & unification
- **Location:** `crates/pattern/src/matcher/cast_mask.rs:40-91`
- **What:** `cast_mask_of` is a 50-line exhaustive `match` over every `NodeKind` variant — the cast variants map to a single bit, the rest map to `CastMask::empty()`. The "every other NodeKind" arm enumerates 33 kinds explicitly to force a compile error if `NodeKind` grows. This is **the** spec for "what is a cast".
- **Why:** The exhaustive match is correct and well-justified. But duplicating the cast list in `tests::cast_mask_of_returns_non_empty_for_all_cast_kinds` (test file) and again in `MatcherOptions` doc and `lib.rs` doc creates 3-4 places where the cast list lives. If a new cast variant is added (and assigned a flag), all four sites need updating.
- **Proposed change:** Make `cast_mask_of`'s arm-list (cast → mask) the single canonical source by exposing a `ALL_CASTS: &[NodeKind]` constant. Tests iterate it; docs reference it. Today the test re-encodes the list inline.
- **Confidence:** medium
- **Risk if applied:** low

### Simplification

### F-029 `int_const` ctor uses tautological `variant_with(_, |_| true)`

- **Category:** Simplification
- **Location:** `crates/pattern/src/pat/ctor/wildcards.rs:51-54`
- **What:**
  ```rust
  // Use VariantWith to prefilter to IntConst discriminant (no payload check);
  // the width-aware equality is done in post_match where we have the output type.
  NodePat::matcher(
      KindSpec::variant_with(&NodeKind::IntConst(0u128), |_| true),
      InputsSpec::None,
  )
  .with_post_match(...)
  ```
- **Why:** `KindSpec::variant_with(_, |_| true)` has the same effect as `KindSpec::variant(_)`: discriminant-only check. The `|_| true` closure is a no-op tautology. The only argument for keeping it is "the codepath through `VariantWith` runs the closure"; but it's just slower than `Variant` for no reason. Compare `pat/builders/phi.rs:39-42`:
  ```rust
  let kind = match vn {
      None => KindSpec::variant(&NodeKind::ControlPhi(exemplar_vn())),
      Some(expected) => KindSpec::variant_with(...),
  };
  ```
  — phi correctly uses `variant` for the no-filter case.
- **Proposed change:** Replace `KindSpec::variant_with(&NodeKind::IntConst(0u128), |_| true)` with `KindSpec::variant(&NodeKind::IntConst(0u128))`.
- **Confidence:** high
- **Risk if applied:** low

### F-030 `Pat::capture_impl` / `when_impl` indirection (cross-ref F-020)

- **Category:** Simplification
- **Location:** `crates/pattern/src/pat/mod.rs:157-220`
- See F-020 for details. The blanket `IntoPat::capture` calls `self.into().capture_impl(v)`; both `Pat` and the builder types satisfy `Into<Pat>`. Inlining `capture_impl` into the trait method removes the indirection.
- **Proposed change:** See F-020.
- **Confidence:** high
- **Risk if applied:** low
- **Cross-refs:** F-020.

### F-031 `Match::root` is `pub` field but every other accessor is a method

- **Category:** Simplification / Readability
- **Location:** `crates/pattern/src/matcher/match_result.rs:21-25`
- **What:**
  ```rust
  pub struct Match {
      /// The root node where the top-level pattern matched.
      pub root: NodeId,
      pub(super) bindings: Bindings,
  }

  impl Match {
      pub fn get(&self, v: Var) -> Option<NodeOutputId> { ... }
      pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> { ... }
      // ... all other accessors are methods
  }
  ```
- **Why:** Mixing field access (`m.root`) with method access (`m.get(v)`) is inconsistent. Most users would write `m.root()` if it were a method. Tests do use `m.root` directly (e.g. `match_at_hits_correct_node` at `tests/matching/matcher_api.rs:56`).
- **Proposed change:** Either make `root` a method (`pub fn root(&self) -> NodeId`) for consistency, or consider also making the `bindings` field accessible directly via `pub fn bindings(&self) -> &Bindings`. Not a bug — just stylistic mismatch.
- **Confidence:** medium
- **Risk if applied:** low

### F-032 `try_once`'s `swap` defensive guard is documented as redundant

- **Category:** Simplification
- **Location:** `crates/pattern/src/pat/node_pat.rs:464-470`
- **What:**
  ```rust
  // `swap` is only ever true when `pats.len() == 2` (enforced by
  // caller) — but re-assert defensively: the swapped arm would
  // otherwise read stale pat indices.
  if swap && pats.len() != 2 {
      return false;
  }
  ```
- **Why:** The check is defensive; the comment notes the caller (`try_match_common`, line 435-443) only sets `swap = true` when `pats.len() == 2`. So the guard is dead in practice. The cost is one condition per call. Either remove it (with the assumption pinned by a `debug_assert!`) or trust the caller and let the `inputs.get(inp_idx)` guard at line 473 catch the stale index. Today both happen.
- **Proposed change:** Remove the `if swap && pats.len() != 2 { return false; }` guard; replace with `debug_assert!(!swap || pats.len() == 2)`. (Per the user's "no debug_assert!" memory rule, this may not be acceptable; in that case use `if cfg!(debug_assertions) && swap { assert_eq!(pats.len(), 2); }`. Or just trust the caller and remove.)
- **Confidence:** medium
- **Risk if applied:** low

### F-033 `CallOtherPat::From<...>::for Pat` mixes `KindSpec::variant` and `Exact` for the same field

- **Category:** Simplification
- **Location:** `crates/pattern/src/pat/builders/call.rs:122-125`
- **What:**
  ```rust
  let kind = match user_op_id {
      None => KindSpec::variant(&NodeKind::CallOther { user_op_id: 0 }),
      Some(expected) => KindSpec::Exact(NodeKind::CallOther { user_op_id: expected }),
  };
  ```
- **Why:** Two different `KindSpec` variants for what is conceptually one constraint ("CallOther kind, optionally with specific user_op_id"). Could be unified via `variant_with`:
  ```rust
  let kind = match user_op_id {
      None => KindSpec::variant(&NodeKind::CallOther { user_op_id: 0 }),
      Some(expected) => KindSpec::variant_with(
          &NodeKind::CallOther { user_op_id: 0 },
          move |k| matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected),
      ),
  };
  ```
  matching the `Load`/`Store`/`StackStore` pattern in `memory.rs`. (Or: keep the `Exact` style and propagate it to the others — choose one style.)
- **Proposed change:** Pick a consistent KindSpec idiom across builders.
- **Confidence:** medium
- **Risk if applied:** low

### F-034 `ConsumersSpec` matching uses `match_node_id` (no walk-through), `OutputsSpec` uses `match_one` (with walk-through) — inconsistent

- **Category:** Simplification (cross-ref F-003)
- **Location:** `crates/pattern/src/pat/node_pat.rs:494-521`
- **What:**
  ```rust
  // OutputsSpec arm (line 499-508): goes through match_one (walk-through aware).
  if let (OutputsSpec::Indexed(items), Some(outputs)) = (&pat.outputs, &outputs) {
      for (i, p) in items {
          let Some(&out) = outputs.get(*i) else { return false; };
          if !match_one(ctx, out, p, b) { return false; }
      }
  }
  // ConsumersSpec arm (line 510-521): match_node_id, no walk-through.
  if let (ConsumersSpec::Indexed(items), Some(outputs)) = (&pat.consumers, &outputs) {
      for (i, p) in items {
          ...
          let Some(consumer) = walk::next_control_node(ctx.matcher, out) else {
              return false;
          };
          if !ctx.matcher.match_node_id(consumer, p, b) { ... }
      }
  }
  ```
- **Why:** The forward-step `consumer` lookup (used by `IfPat::true_branch`/`false_branch`) goes through `match_node_id` directly, never `match_output_with_walk_through`. This is a missed dispatch site for the `ignore_control_states` flag — which is documented to apply here. See F-003.
- **Proposed change:** Either extract a shared `match_node_with_walk_through(node, pat, b)` helper that mirrors `match_output_with_walk_through` for the node-rooted case, or thread `ignore_control_states` through manually.
- **Confidence:** high
- **Risk if applied:** medium (touches a behavior tests don't verify)
- **Cross-refs:** F-003.

### F-035 `bool_unary` has only `Neg` — the macro family is overkill for one variant

- **Category:** Simplification / Readability
- **Location:** `crates/pattern/src/pat/ctor/bool_.rs:31-44`
- **What:**
  ```rust
  pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat { ... }

  decl_pat_unary_ops!(bool_unary, BoolUnaryOp, Pat, [
      /// Matches a boolean NOT node.
      (bool_not, Neg),
  ]);
  ```
- **Why:** `BoolUnaryOp` has only `Neg` (per `ir/src/ops/`). The macro family ceremony is unnecessary for a single variant — a hand-written 3-line `pub fn bool_not` would be simpler than the macro invocation. This is a macro-overhead issue, not a bug.
- **Proposed change:** Either inline `bool_not` or document why the macro is preferred (e.g. "future-proofing: when more variants are added, drop them in here"). The `bool_unary_op_var_bind_and_idempotent` test even has a special-case for "no conflict variant" — see `tests/matching/bindings.rs:225`. So the cost is paid in three places. Worth considering.
- **Confidence:** medium
- **Risk if applied:** low

### Readability

### F-036 `crate::build::rewrite_rule` doclinks reference a path that does not exist

- **Category:** Readability
- **Location:** `crates/pattern/src/pat/traits.rs:11`, `crates/pattern/src/matcher/mod.rs:187`, `crates/pattern/src/macros.rs:22`
- **What:**
  ```rust
  // pat/traits.rs:11:
  //! [`crate::build::rewrite_rule`]: materialize this pattern into fresh IR

  // matcher/mod.rs:187:
  /// Used by [`crate::build::rewrite_rule`] and other callers
  ```
- **Why:** There is no `crate::build` module. The actual path is `crate::rewrite::rewrite_rule`. Stale doclinks date from a pre-migration phase. `cargo doc` will produce broken links.
- **Proposed change:** Find-and-replace `crate::build::` → `crate::rewrite::` (or just `crate::rewrite_rule` since it's re-exported).
- **Confidence:** high
- **Risk if applied:** low

### F-037 `crate::ErrorKind::MissingBinding` doc reference points to a non-existent path

- **Category:** Readability
- **Location:** `crates/pattern/src/pat/ctor/consts.rs:113`
- **What:**
  ```rust
  /// # Errors
  ///
  /// Returns [`crate::ErrorKind::MissingBinding`] when the capture variable
  /// is not present in `ctx.bindings` ...
  ```
- **Why:** No `crate::ErrorKind` exists post-anyhow migration. The actual error is wrapped in `anyhow::Error::new(crate::error::MissingBinding(_))`. Stale doc.
- **Proposed change:** `Returns an [`anyhow::Error`] wrapping [`crate::error::MissingBinding`] when ...`.
- **Confidence:** high
- **Risk if applied:** low

### F-038 `Pat::when_impl` doclink in PredicateFn doc — `when_impl` is private

- **Category:** Readability
- **Location:** `crates/pattern/src/pat/mod.rs:22`
- **What:**
  ```rust
  /// Predicate function type used by the `WhenPat` combinator (produced by
  /// [`IntoPat::when`] / [`Pat::when_impl`]).
  pub type PredicateFn = ...
  ```
- **Why:** `Pat::when_impl` is a private method. `cargo doc` may render this as a broken link (or as private-item-link). Also, the doc references `WhenPat` — but the actual struct is `GuardPat` with `GuardFn::Output`. Triple staleness.
- **Proposed change:** Update doc:
  ```rust
  /// Predicate function type used by the [`GuardPat`] combinator (produced by
  /// [`IntoPat::when`]).
  ```
- **Confidence:** high
- **Risk if applied:** low

### F-039 `BUG-19` codename references in three places

- **Category:** Readability
- **Location:** `crates/pattern/src/matcher/mod.rs:51`, `crates/pattern/tests/matching/matcher_api.rs:321,355`
- **What:**
  ```rust
  // matcher/mod.rs:51:
  /// Use case: x86 / x64 register-merge chains and width casts cause
  /// patterns like `add(mul(_,_), _)` to find `Add(Extend(Mul), arg)`
  /// without re-shaping the source.  See BUG-19.

  // tests:
  // canonical BUG-19 problem.  Without `ignore_casts` the strict matcher
  // ... This is the BUG-19 fix in miniature.
  ```
- **Why:** The user noted that `BUG-19` and similar codenames were stripped from `opt`. Three remain in `pattern`.
- **Proposed change:** Replace `See BUG-19.` with a short description of the actual problem (e.g. "See the `add_mul_pattern_does_not_match_through_extend_by_default` test for the canonical case.") and similar in the test file.
- **Confidence:** high
- **Risk if applied:** low

### F-040 `Matcher::function_arg_index` doc claim "single preorder walk per find_all" is silently lazy

- **Category:** Readability
- **Location:** `crates/pattern/src/matcher/mod.rs:62-68,136-148`
- **What:**
  ```rust
  /// Construction is O(1); the `FunctionArg` index is built lazily on first
  /// use of a `function_arg*` query.  `find_all` does a single preorder walk
  /// of the graph each call and tries the pattern against every node.
  pub struct Matcher<'g> {
      ...
      function_arg_index: std::cell::OnceCell<FunctionArgIndex>,
  }
  ```
- **Why:** The doc says "single preorder walk of the graph each call" — but actually `find_all` does one preorder walk per call AND `function_arg_index()` does another preorder walk on its first call. The lazy build of the index is intentional (callers who don't use the function-arg API don't pay), but the docstring undersells the cost (one extra preorder for the first `function_arg*` user). Not a bug; readability.
- **Proposed change:** Document explicitly that the index is lazily-built via OnceCell, which costs a one-time preorder walk on first `function_arg*` query. Document that `find_all`/`match_at` paths don't trigger the build.
- **Confidence:** medium
- **Risk if applied:** low

### F-041 `Bindings` API shape — `default`/`mark`/`restore` are `pub(crate)` but type is `pub`

- **Category:** Readability
- **Location:** `crates/pattern/src/matcher/bindings.rs:30-73`
- **What:**
  ```rust
  pub struct Bindings { entries: Vec<BindingEntry> }

  impl Bindings {
      pub(crate) fn mark(&self) -> BindingsMark { ... }
      pub(crate) fn restore(&mut self, mark: BindingsMark) { ... }
  }
  // bind/get methods (decl_bind_get!) are pub.
  // Default::default is derived (pub).
  ```
- **Why:** `Bindings` is `pub` but its construction surface is `Default::default()`. External callers can construct one and call `bind_*`/`get_*` methods — but not `mark`/`restore`. Effectively a half-public API. The `MatchPredicateFn` callback receives `&Bindings` and can call `get_*`. Tests in `tests/matching/bindings.rs` construct `Bindings::default()` and call public bind methods directly. So the API is intentionally read-only-from-outside, but the public/private split isn't documented.
- **Proposed change:** Document the intent: "`Bindings` is publicly readable (via `get_*`) but only the matcher mutates it via the journal API (`mark`/`restore`)". Or restrict construction further (`pub(crate) fn default()` — but Rust doesn't let you `pub(crate)` an auto-derived Default).
- **Confidence:** medium
- **Risk if applied:** low

### F-042 `exemplar_vn` / `IntConst(0u128)` / `BoolConst(false)` payload-don't-care exemplars are unidiomatic

- **Category:** Readability
- **Location:** Multiple files (`pat/builders/{phi,memory}.rs`, `pat/ctor/wildcards.rs`, `pat/mod.rs`)
- **What:**
  ```rust
  // pat/node_pat.rs:124-133:
  /// Arbitrary `rsleigh::Vn` used as a payload-don't-care exemplar when
  /// constructing `NodeKind` variants for discriminant-only purposes
  /// (e.g. `KindSpec::variant(&NodeKind::ControlPhi(exemplar_vn()))`).
  pub(crate) fn exemplar_vn() -> rsleigh::Vn { ... }

  // pat/builders/phi.rs:40:
  None => KindSpec::variant(&NodeKind::ControlPhi(exemplar_vn())),

  // pat/ctor/wildcards.rs:51-53:
  KindSpec::variant_with(&NodeKind::IntConst(0u128), |_| true),
  ```
- **Why:** Constructing a sample value just to extract its discriminant is awkward. `KindSpec::variant_for_kind<K>(...)` would be clearer if it took a discriminant directly. Today, every builder constructs a "throwaway" exemplar. The pattern is documented for `exemplar_vn` but not for the literal `0u128` / `false` / `0u64` exemplars used elsewhere.
- **Proposed change:** Either add a constant per kind (`const INT_CONST_DISCRIMINANT: ... = ...`) or accept the current ergonomics with a small documentation-friendly helper. Less of a finding, more an aesthetic note.
- **Confidence:** low
- **Risk if applied:** low

### F-043 `decl_var!` / `Var` doc — globally-unique-id rationale not explained

- **Category:** Readability
- **Location:** `crates/pattern/src/var.rs:11-65`
- **What:**
  ```rust
  decl_var!(Var,
      "A capture variable that binds a [`ir::node::NodeOutputId`] (data value edge).\n\nEach `Var::new()` call produces a globally unique id.  The same `Var` can appear in multiple positions of a pattern; the matcher requires all occurrences to bind to the **same** output.");
  ```
- **Why:** "Globally unique" — globally across what? All `Var` instances in the program, or across the matcher? The atomic counter is process-wide. A user composing patterns in multiple threads gets unique ids. The docstring doesn't say *why* uniqueness matters: because the bindings are stored in a flat `Vec<BindingEntry>` keyed by id, and a clash would mean a stale binding from another pattern. The rationale is somewhat hidden in `Bindings`'s docstring, but the `Var` doc itself doesn't link it.
- **Proposed change:** Add a sentence: "Uniqueness is enforced via a process-wide atomic counter so that the matcher's `Bindings` storage (a flat append-only `Vec`) can identify entries unambiguously without per-pattern bookkeeping." See `Bindings` for details.
- **Confidence:** low
- **Risk if applied:** low

### F-044 `Match::get_vn` lists 2 producer kinds that map; future producers silently return None

- **Category:** Readability
- **Location:** `crates/pattern/src/matcher/match_result.rs:160-198`
- **What:**
  ```rust
  /// Returns the [`rsleigh::Vn`] associated with the `NodeOutputId` bound to
  /// `v`, if one can be determined.  The output-to-varnode mapping is
  /// well-defined only for a handful of producer kinds:
  ///
  /// * `InitialVar(vn)` — ...
  /// * `Call` outputs at slot `2 + i` — ...
  ///
  /// For any other producer (`IntConst`, `Load`, `Phi`, binary ops, …), the
  /// notion of "the varnode this value represents" is not meaningful and the
  /// function returns `None`.
  ```
- **Why:** Adding a new producer kind that has a well-defined output→varnode mapping (e.g. `FunctionArg { source: Register(vn), ... }` — in fact `FunctionArg` could conceivably map back to its source register) would silently return `None` from `get_vn` until someone updates this match. The current implementation has an explicit `_ => None` arm. A user looking at the pattern crate's API surface would not know to extend `get_vn` when introducing a new node kind.
- **Proposed change:** Either (a) add `FunctionArg` to the supported set explicitly (so `function_arg(0)` captures map to a Vn via `get_vn`), or (b) add a comment in `NodeKind` (out-of-scope, in `ir`) noting "if you add a kind with a well-defined source varnode, update `pattern::Match::get_vn`."
- **Confidence:** medium
- **Risk if applied:** low

### F-045 `decl_any_const!` macro's `unreachable` arm comment glosses over the contract

- **Category:** Readability
- **Location:** `crates/pattern/src/pat/mod.rs:78-87`
- **What:**
  ```rust
  // Kind spec already enforces the variant, so the match arm
  // below is unreachable in the `_` case — but keeping it
  // lets the closure body stay a single `match`.  Binding
  // happens in `post_match` per the NodePat kind-purity rule.
  .with_post_match(Arc::new(move |ctx, node, b| {
      match ctx.graph.graph.node_kind(node) {
          NodeKind::$variant(v) => b.$bind(tv, *v),
          _ => false,
      }
  }))
  ```
- **Why:** The "unreachable" claim only holds if the kind spec strictly equals `Variant(discriminant)`. If a future change relaxes the kind spec (e.g. allowing `Any` for `*_const_with_fn` patterns), the `_ => false` arm fires legitimately. The comment makes the future maintainer think they can remove the arm — but they shouldn't.
- **Proposed change:** Soften the comment: "The `_` arm is defensive — the kind spec normally restricts to this variant, but we don't depend on that."
- **Confidence:** low
- **Risk if applied:** low

### F-046 `Bindings` "≤10 bindings" cost analysis is unmeasured

- **Category:** Readability
- **Location:** `crates/pattern/src/matcher/bindings.rs:25-29`
- **What:**
  ```rust
  /// Lookups (`get_*`) are linear scans filtered by entry variant.  Typical
  /// matches produce ≤10 bindings so scan cost is in-cache and beats HashMap
  /// on cold-path inserts.  If a future pattern produces dozens of bindings,
  /// a lazy overlay index can be added without changing the public API.
  ```
- **Why:** The "≤10 bindings" heuristic isn't measured against the actual `opt::constant_fold/rules.rs` patterns or real-world strider workloads. Some `*_const_with!` macros bind up to 4-5 captures; chained match-with-multiple-bindings can grow. The "in-cache" claim is plausible but unverified. Not a bug — just an unverifiable claim that a reader might trust without context.
- **Proposed change:** Either measure (a benchmark) or soften the claim: "Bindings are typically small (single-digit to low-double-digit); a hash-overlay can be added if profiling shows the linear scan is hot."
- **Confidence:** low
- **Risk if applied:** low

### Performance

### F-047 `Matcher::find_all` allocates a fresh `Bindings` per candidate node

- **Category:** Performance
- **Location:** `crates/pattern/src/matcher/mod.rs:163-180`
- **What:**
  ```rust
  pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
      let kind = pat.as_dyn().kind_spec();
      self.fn_graph
          .preorder()
          .filter(|&node| kind.accepts_discriminant(...))
          .filter_map(|node| {
              let mut bindings = Bindings::default();
              if self.match_node_id(node, pat, &mut bindings) { ... }
              else { None }
          })
          .collect()
  }
  ```
- **Why:** Each candidate root allocates a fresh `Bindings` (a `Vec<BindingEntry>`) for its match attempt. Failed matches discard the allocation. With N candidate roots and a kind-prefiltered set, this is N allocations even if most fail before binding anything. A reused `Bindings` (with `mark()`/`restore()` between roots) would let a single allocation serve all attempts; only successful matches would clone-out their Bindings into the `Match` result.
- **Proposed change:** Reuse a single `Bindings` across candidates:
  ```rust
  let mut bindings = Bindings::default();
  for node in self.fn_graph.preorder().filter(...) {
      let mark = bindings.mark();
      if self.match_node_id(node, pat, &mut bindings) {
          let m = Match { root: node, bindings: bindings.clone() };
          bindings.restore(mark);
          results.push(m);
      } else {
          bindings.restore(mark);
      }
  }
  ```
  Allocates once per `find_all`, plus once per successful match. Match count is typically much smaller than candidate count.
- **Confidence:** high
- **Risk if applied:** low

### F-048 `KindSpec::clone()` Arc-clones for `VariantWith`; `kind_spec()` called every `find_all`

- **Category:** Performance
- **Location:** `crates/pattern/src/pat/node_pat.rs:286-288`, `crates/pattern/src/matcher/mod.rs:163-167`
- **What:**
  ```rust
  // pat/node_pat.rs:
  fn kind_spec(&self) -> KindSpec {
      self.kind.clone()
  }

  // KindSpec::VariantWith stores Arc<dyn Fn(&NodeKind) -> bool>; clone bumps refcount.

  // matcher/mod.rs::find_all:
  let kind = pat.as_dyn().kind_spec();  // <- one Arc clone per find_all
  ```
- **Why:** Each `find_all` invocation Arc-clones the kind spec once. Cheap (atomic increment). But the API would let callers stash the spec for repeated queries: `let spec = pat.kind_spec(); for graph in graphs { matcher(graph).find_all_with_spec(&pat, &spec) }`. Currently each call repeats the clone.
- **Proposed change:** Either accept the cost (it's a single atomic increment) or expose a borrow: `fn kind_spec_ref(&self) -> &KindSpec` that returns a reference. The current Vec-of-Patterns design owns the spec inside the pattern, so a borrow is cleaner.
- **Confidence:** medium
- **Risk if applied:** low

### F-049 (See F-034 — same finding from the perf angle)

### F-050 `Bindings::bind_*` does O(n) linear scan on every bind

- **Category:** Performance
- **Location:** `crates/pattern/src/matcher/bindings.rs:79-107`
- **What:**
  ```rust
  pub fn $bind_name(&mut self, v: $var, val: $val) -> bool {
      for entry in &self.entries {
          if let BindingEntry::$variant(k, existing) = entry
              && *k == v
          {
              return *existing == val;
          }
      }
      self.entries.push(BindingEntry::$variant(v, val));
      true
  }
  ```
- **Why:** O(n) scan to detect conflict. With commutative retry, a partial match path may try bind-x → fail → swap → bind-x; each `bind_x` does the scan. With deeply nested patterns and many capture vars, the scan adds up. The crate's own doc (line 25-29) acknowledges this and claims it beats HashMap for ≤10 bindings — but per F-046, that's unmeasured.
- **Proposed change:** Either accept the cost (matches the documented "small N" assumption), or add an optional overlay HashMap when entry count exceeds a threshold. Today the linear scan is stable and predictable.
- **Confidence:** medium
- **Risk if applied:** low

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| crates/pattern/Cargo.toml | full | |
| crates/pattern/src/lib.rs | full | re-export audit; doc claims about commutativity / Matcher pre-indexing |
| crates/pattern/src/error.rs | full | `is_skip` / `RewriteSkip` re-export check |
| crates/pattern/src/var.rs | full | 13 capture-var types via `decl_var!` |
| crates/pattern/src/macros.rs | full | `decl_pat_*_ops!` + `*_const_with!` family |
| crates/pattern/src/rewrite.rs | full | `rewrite_rule` invariants and skip-handling |
| crates/pattern/src/matcher/mod.rs | full | `Matcher`, `find_all`/`match_at`, `function_arg*` API |
| crates/pattern/src/matcher/bindings.rs | full | journal-based bindings, 13 op-var bind/get |
| crates/pattern/src/matcher/cast_mask.rs | full | exhaustive cast→mask classifier |
| crates/pattern/src/matcher/cast_mask/tests.rs | full | bit-distinctness, exhaustive coverage |
| crates/pattern/src/matcher/commutativity.rs | full | 4 commutativity classifiers |
| crates/pattern/src/matcher/function_arg_handle.rs | full | dead-code candidate |
| crates/pattern/src/matcher/match_result.rs | full | accessor surface; many unused helpers |
| crates/pattern/src/matcher/walk.rs | full | one helper + tests |
| crates/pattern/src/matcher/walk_through.rs | full | cast / ControlState walk-through, tests inline |
| crates/pattern/src/pat/mod.rs | full | `Pat` / `IntoPat` / blanket impl / decl_any_const! macro |
| crates/pattern/src/pat/traits.rs | full | `Pattern` trait, `MatchCtx`, `BuildCtx`, `BuildOutcome` |
| crates/pattern/src/pat/node_pat.rs | full | `NodePat`, `KindSpec`, `InputsSpec`, `OutputsSpec`, `ConsumersSpec`, `try_once` |
| crates/pattern/src/pat/any.rs | full | `AnyPat`, `VarPat`, `CapturePat` |
| crates/pattern/src/pat/guards.rs | full | `GuardPat`, `GuardFn` |
| crates/pattern/src/pat/builders/mod.rs | full | re-export shape |
| crates/pattern/src/pat/builders/binary_op.rs | full | three near-duplicate typed builders |
| crates/pattern/src/pat/builders/branch.rs | full | `IfPat` |
| crates/pattern/src/pat/builders/call.rs | full | `CallPat`, `CallOtherPat`, mixed KindSpec usage |
| crates/pattern/src/pat/builders/function_arg.rs | full | |
| crates/pattern/src/pat/builders/memory.rs | full | `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat` |
| crates/pattern/src/pat/builders/phi.rs | full | `PhiPat` |
| crates/pattern/src/pat/builders/ret.rs | full | `RetPat`, ret_val doc inconsistency |
| crates/pattern/src/pat/ctor/mod.rs | full | re-export skeleton |
| crates/pattern/src/pat/ctor/bool_.rs | full | `bool_binary` / `bool_unary` (single-variant family) |
| crates/pattern/src/pat/ctor/casts.rs | full | unary cast helpers |
| crates/pattern/src/pat/ctor/consts.rs | full | `*_const_with_fn`, `FromCtx`, `first_value_input_type` |
| crates/pattern/src/pat/ctor/control.rs | full | memory/phi/control ctors |
| crates/pattern/src/pat/ctor/float.rs | full | float family — no commutativity |
| crates/pattern/src/pat/ctor/int.rs | full | int binary/unary/cmp ctors |
| crates/pattern/src/pat/ctor/variant_agnostic.rs | full | `*_any` macro |
| crates/pattern/src/pat/ctor/wildcards.rs | full | `any`, `var`, `int_const`, `bool_const`, `float_const`, `predicate` |
| crates/pattern/tests/cast_mask_walk.rs | full | walk-through behaviour table |
| crates/pattern/tests/matching.rs | full | top-level mod hub |
| crates/pattern/tests/matching/arithmetic.rs | full | int binary/unary/cmp |
| crates/pattern/tests/matching/bindings.rs | full | 13 family contracts |
| crates/pattern/tests/matching/captures_and_predicates.rs | full | `var`, `when`, `predicate`, `get_int_const` |
| crates/pattern/tests/matching/casts_and_conversions.rs | full | cast pattern matching |
| crates/pattern/tests/matching/commutativity.rs | full | commutative + .ordered() |
| crates/pattern/tests/matching/control_flow.rs | full | Call / Return / If / CallOther |
| crates/pattern/tests/matching/int_const_width_aware.rs | full | width-aware int_const |
| crates/pattern/tests/matching/matcher_api.rs | full | find_all / match_at / function_arg / MatcherOptions |
| crates/pattern/tests/matching/memory.rs | full | Load / Store |
| crates/pattern/tests/matching/rewrite.rs | full | `rewrite_rule`, error paths |
| crates/pattern/tests/matching/ssa.rs | full | InitialVar / ControlPhi / FunctionArg |
| crates/pattern/tests/matching/stack.rs | full | StackStore / StackStorePhi |
| crates/pattern/tests/matching/variant_agnostic.rs | full | `*_any` ctors |
| crates/pattern/tests/matching/wildcards_and_consts.rs | full | any, var, constants |
| crates/pattern/tests/matching/support/assertions.rs | full | matches/unique/none/first/find_node |
| crates/pattern/tests/matching/support/graph.rs | full | `Tb` test builder |
| crates/pattern/tests/matching/support/mod.rs | full | re-export |
| crates/pattern/tests/matching/support/shapes.rs | full | reusable graph fixtures |
| crates/pattern/examples/pattern_query.rs | full | 6 examples; comment-vs-implementation drift on commutativity |

## Out-of-scope items observed

The following items were noted but are out of scope per the review brief:

- **CLAUDE.md-vs-code drift in NodeKind list** (extra-crate): CLAUDE.md lists `Extract`, `Insert`, `Piece`, `FloatIsNan`, `Contains`, `WithCapture`, `WithPredicate`. None of these exist in `crates/ir/src/node/kind.rs` or `crates/pattern/src/`. CLAUDE.md is stale; updating it is out-of-scope, but the drift is worth noting.
- **`ir::NodeKind::CastToInt` doc says "Reinterpret an integer value as `Bool`"** — that's the wrong direction (CastToInt is bool→int per `node_signature.rs`). Pattern's `cast_to_int` doc gets it right ("`bool` → `0` or `1`"). Cross-crate (`crates/ir/src/node/kind.rs:121`) — out of scope.
- **The `ir-macros::match_value!` macro is referenced in the user prompt but is not used in the pattern crate.** Could be — maybe — useful for replacing some `match` arms in Bindings or guards. Cross-crate.
- **The user noted that the planned `strider-py` crate will be the primary user-facing surface.** Many of the F-013…F-021 dead-code findings are dead today but may become live once `strider-py` lands. Per the review brief, I report them; the user can choose to keep them.
- **Anyhow shape**: per the brief, the post-anyhow design is not subject to revision. F-022 proposes a small constructor helper that's still anyhow-idiomatic.

## Notes

- **The `pat/` subtree's split** (`builders/`, `ctor/`, `any.rs`, `guards.rs`, `traits.rs`, `node_pat.rs`, `mod.rs`) is well-organized: each file has a single clear responsibility. The unusual nested layout (vs. flat) seems to be paying off — files are mostly under 200 lines, and the per-builder style is consistent. Worth preserving.
- **The `decl_var!` / `decl_bind_get!` / `decl_pat_*_ops!` / `op_variant_contract!` macro families collectively encode "13 op-family kinds" four times** (F-027). A future maintenance pass might consolidate via a build script or `proc_macro`, but the current state is functional.
