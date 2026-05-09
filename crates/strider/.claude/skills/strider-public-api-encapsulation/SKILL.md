---
name: strider-public-api-encapsulation
description: Tighten a `pub` field's visibility to `pub(crate)` (or add a builder/accessor pair) when the field carries an invariant that external mutation could silently violate.
---

# strider-public-api-encapsulation

## When to use

Triggers:
- "round 9 R9-2D flagged this `pub` field as carrying an unenforced invariant"
- "this struct's documentation says one thing but the type permits another"
- a reviewer suggests "should this be `pub(crate)` with an accessor?"
- a future-parallelism / future-mutation concern surfaces in a code review

## When NOT to use

- The field is `pub` because external code legitimately needs to construct or mutate the struct directly (e.g. a builder pattern's intermediate state).  Tightening would force a builder-only API and potentially break ergonomic constructors.
- The struct is `pub(crate)` already — no external surface to tighten.
- The "invariant" is a comment-only convention with no real enforcement value (e.g. "fields are sorted alphabetically for readability").

## Procedure

1. Identify the unenforced invariant.  Examples from round 9:
   - `BuiltCallingConvention.callee_saved_regs` and `arg_passing_regs` must be disjoint (V4 / R9-2D H3).
   - `cfg::PcodeInsnAddr.machine_addr` and `insn_index` must not be reordered (the `Ord` derive depends on field declaration order — V3 / R9-2D H2).
   - `IndirectBranchResolve.{unresolved_anchors, anchor_contexts}` must be in lockstep — every entry in one has a matching entry in the other (V5 / R9-2D H5).
   - `AnalyzeOptions.all_vns` must be sorted by `pcode_lift::vn_sort_key` (P3 / R9-2D M3).
2. Decide the migration shape:
   - **Tighten visibility + add accessor**: best when the field is already only-read externally.  Change `pub field` → `pub(crate) field`, add `pub fn field(&self) -> &Field` (or `&[T]` for slices, `T` for Copy scalars).
   - **Add validating constructor**: best when external code legitimately constructs the struct.  Keep field public, add `pub fn try_from_parts(...) -> Result<Self>` that validates invariants; document the unchecked `from_parts` as test-only.  Round 9 V4 (`BuiltCallingConvention::try_from_parts`) is the canonical example.
   - **Newtype with checked ctor**: best when the invariant is a Vec-property (sorted, non-empty, deduped).  Wrap the inner Vec in a newtype with `try_from_vec` and pattern-matchable accessor.  Round 9 P5 (`ResolvedTargets::multiple` validating constructor) is the additive variant; a newtype would be the structural variant.
   - **Sum type**: best when two fields are coupled (one ignored when the other is set).  Replace `(Option<T>, bool)` with `enum { Variant1, Variant2 }`.  Round 9 P2 (`FunctionBoundary { Unbounded, Bounded }`) was the proposed shape.
3. Map the blast radius:
   - For visibility tightening: `rg "\.field_name\b"` to find all readers.  Each must go through the new accessor.
   - For sum-type / newtype migration: every constructor and pattern-match site needs updating.
4. Apply the migration in a focused commit.  For visibility tightenings: usually a single PR with the accessor + N call-site changes.  For type-level refactors: the diff can be large; consider splitting if it crosses crate boundaries.
5. Verify with `cargo build --workspace` + `cargo test --workspace --exclude strider-py` + `cargo clippy --workspace -- -D warnings`.  Add at least one test that asserts the new constructor rejects an invalid input.

## Round 9 applications (canonical references)

- **V1**: `read_variable_optional` `pub` → `pub(super)` (Phase C).  Single sibling caller; pure visibility tightening.
- **V4**: `BuiltCallingConvention::try_from_parts` (Wave 11).  Validating-constructor pattern.
- **V6**: `cfg::Cfg::start_addr_to_region_id` `pub` → `pub(crate)` (Wave 3).  Visibility tightening, derived lookup index.
- **V9**: `opt::Optimizer` / `OptimizerOnBuilt` add `Send + Sync` bound (Phase C).  Trait-bound tightening, prep for parallelism.
- **P5**: `ResolvedTargets::multiple` validating constructor (Wave 12).  Additive validating ctor — variant tuple-construct retained for pattern matching.
- **D1**: `LiftAddrGuard` `pub` re-export deletion (Phase C).  Zero-callers — purely a deletion, but same shape (don't expose what isn't used).

Items round 9 considered and deferred (with documented rationale):

- **V2**: `BuiltFunctionGraph` CC fields `pub` → `pub(crate)`.  Big blast radius; deferred until a more careful refactor.
- **V3**: `cfg::PcodeInsnAddr` / `MachineInsnAddr` field tightening.  ~30 deep-field-access sites.
- **V5**: `IndirectBranchResolve` lockstep field refactor.  Cannot tighten because cross-crate population; needs type-level refactor.
- **V7**: `target::SleighArch` field tightening.  Many call sites.
- **P2**: `RunConfig::{fn_max_size, allow_code_before_start}` → `FunctionBoundary` sum type.  Subtle semantic change to OptionsBuilder ordering.
- **P3**: `AnalyzeOptions::all_vns` → `SortedVns` newtype.  No external surface today.

## Anti-patterns

- "Add a debug_assert in the consumer" — surfaces violations only in debug builds; release builds silently miscompile.  Use `Result` + a typed error.
- "Make the field `pub(crate)` and add a `pub fn raw_field_mut(&mut self) -> &mut Field`" — defeats the encapsulation.  Either expose only validated mutations, or leave as `pub`.
- Adding a builder method that's a single-shot constructor for one specific shape.  Builder is for incremental construction; if you only have one shape, use a free function.

## See also

- `reviews/round9-2D-types.md` — the original type-design audit that flagged these.
- `reviews/round9-simplifications.md` — visibility-tightening section (V1-V9, P1-P8).
- `reviews/round9-fix-verification.md` — verification methodology used to triage which findings are real bugs vs. doc-only concerns.
