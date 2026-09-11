# strider-orchestrator

Drives one function all the way from bytes to optimised IR: decode, lift,
optimise, resolve the indirect branches that the first decode could not see, and
re-run the whole thing until the control-flow graph stops changing.

## What's here

- `Strider<R>`, the per-binary handle: `new(arch, sleigh, rom)` over an
  `rsleigh::Sleigh` and an optional read-only image (what `LoadReadOnly` folds
  against and what the jump-table classifier reads tables from; `None` disables
  both).
- `Strider::analyze(entry, cc, lift_opts, opt_opts, pipeline)` -> `AnalyzeResult`:
  the loop below. `pipeline` defaults to `strider_opt::default_pipeline`, and
  `strider_opt::IndirectBranchClassify` is appended as a post-pass unless the
  supplied pipeline already runs it. `lift_opts.compact` applies once after the
  loop.
- `Strider::build_cfg(entry, cfg_opts)`: a structural decode only, no lift, no
  optimisation, no resolution.
- Accessors that let a caller run its own pipeline against the same state:
  `arch`, `sleigh`, `sleigh_regs`, `user_op_names`, `rom`.
- Re-exports, so a caller needs one dependency: `strider_opt` as `opt`, and
  `Lifter` / `LiftOptions` / `LiftOutcome` from `strider-lift`.

## One round

`build_lift` is the unit the loop repeats: build the CFG from the current
`known_targets`, lift it, run the pipeline. Every `BranchIndirect` the CFG could
not seat became an `IndirectBranch` placeholder anchored to its pcode address,
and every `Switch` the CFG did seat kept an anchor of its own so a table can
still widen. The `IndirectBranchClassify` post-pass walks the optimised IR and
answers each anchor with a `ResolvedTargets`, or with `None` for a site it
cannot derive.

The classifier only REPORTS targets. Nothing rewrites the graph in place: the
orchestrator folds the answers into `known_targets` and rebuilds the CFG from
them, so a resolved branch becomes a real CFG edge and the next round's IR is
lifted with that edge present. That is what makes a chain of trampolines
resolve: one round seats one level of discovery.

## The fixed-point loop

`apply_resolutions` folds a round's classifications into `known_targets` and
reports what changed. Two anchors sharing one pcode address merge within the
round; across rounds the staged set replaces the previous entry. The caller's
own seed is unioned back in by address every round, and the caller's map is
never mutated. On an arch with an ISA-mode variable a mode-less re-derivation
adopts the mode already proved for that address, rather than re-decoding proved
arms in whatever mode flows into the branch.

The loop converges when no anchor is left, or when a round changes no address's
successor set. It stops for three other reasons, none of them an error:

- A site that NARROWS twice is abandoned. One narrowing is a refinement (the
  classifier over-approximates an index bound and then proves the tighter
  answer); a second means the answer depends on what the previous round seated,
  so no member of the cycle is trustworthy. Its seat is dropped and its
  placeholder left live.
- A site naming a target the CFG could not decode is abandoned whole: the
  bound that produced it is what is wrong, so no arm of that answer stands. A
  site naming an address two edges reached in different ISA modes loses just
  that arm, since any two edges raise a clash, direct ones included. Either way
  the site is frozen, taking no further classification.
- `MAX_RESOLUTION_ITERATIONS` (256) caps the loop. Since one iteration seats one
  level of discovery, the cap is also the discovery-depth limit, and exhausting
  it while sites are still growing is that limit being hit, not an oscillation.
  Those sites are reported.

## Five channels, and all five must be read

A converged CFG may be incomplete, and `AnalyzeResult` says so five ways.
`is_complete()` is the question "may this be incomplete?" and needs all five;
none of them answers it alone.

- `unresolved_indirect_branches`: a live `IndirectBranch` placeholder, a seated
  `Switch` whose selector no longer derives, a site still growing when the cap
  ran out, and a site whose fold dropped a successor the classifier proved.
- `unverified_seeded_sites`: answers that are whole but that nothing verified. A
  `Switch` holding exactly the caller's seed and nothing derived; a site the CFG
  CONSUMED, where a `LinkRegister` answer became a `Return` and a lone
  out-of-function target became a `TailCall`, leaving no placeholder and no
  anchor to report through; and an arm seated on the mode its siblings proved,
  which nobody evaluated for that arm.
- `isa_mode_conflicts`, `interior_branch_targets` and
  `unmapped_branch_targets`: losses no indirect site owns, since a direct edge
  produces them too. An address two edges reached in different modes, a target
  off every instruction boundary, and a target the image had no bytes for,
  seated as an empty `TailCall` stub. All three are accumulated across rounds,
  not read off the final CFG: the round that decoded an address twice fed the
  classifier whether or not a later rebuild still carries that edge.

A caller seed asserts the site is settled, which suppresses the unclassifiable
seated `Switch` report until the site outgrows the seed. It suppresses nothing
else: not a live placeholder, and not a successor the classifier proved and the
fold dropped.

Depends on `strider-cfg`, `strider-ir`, `strider-lift`, `strider-opt`,
`strider-target`, `rsleigh`, `anyhow` and `rustc-hash`.
