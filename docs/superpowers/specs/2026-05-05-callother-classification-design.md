# Construction-time `CallOther` classification

Replace strider's two-stage handling of Sleigh `CallOther` user-ops
("emit an opaque node, then maybe optimise it away") with a single
construction-time classification.  When the IR builder encounters a
CallOther it consults a per-name table in `target::user_ops` and
dispatches one of three outcomes: skip the node entirely (no-op),
emit a CallOther and signal that the region terminates here
(noreturn), or emit today's opaque CallOther (everything else).  Any
user-op name actually emitted by a lift that has no entry in the
table fails the lift with a typed error.

This subsumes the existing [`opt::CallOtherElide`] pass + the
`NO_OP_USER_OPS` constant + the `is_known_no_op` shortcut in
`strider::insn::handle_call_other`, AND fixes the BUG_ON / trap
unresolved-indirect-branch crash on real Linux kernels (`commit_creds`,
`do_exit`, `do_task_dead`, `__schedule`, `__alloc_pages_nodemask`,
`kmem_cache_free`, `vfree`, … on x86_64 and aarch64; same root cause
for the FreeBSD trap path).

## Motivation

Two existing problems converge on one solution:

1. **The `setISAMode` / `setEndianState` / `CallOtherElide` cycle.**
   Today the IR lifter emits an opaque `CallOther` for every Sleigh
   user-op, then the optimiser removes the no-op ones in a separate
   pass.  Two parallel name-lookups (one in `handle_call_other` to
   suppress conservative-clobber for known no-ops, one in
   `CallOtherElide` to drop the node).  Construction-time
   classification collapses both into one decision.

2. **BUG_ON / trap instructions crash the lift.**  Sleigh lifts `ud2`
   (x86) and `brk #0x800` (aarch64) — Linux's `BUG()` / `BUG_ON()`
   trap — to two pcode ops:

   ```
   pcode[0]: CallOther [user_op = invalidInstructionException | SoftwareBreakpoint]
   pcode[1]: BranchIndirect
   ```

   The trailing `BranchIndirect` is unresolvable (it represents the
   trap handler taking control), but the orchestrator treats it as
   a real indirect branch and surfaces
   `UnresolvedIndirectBranchError`.  The trap is noreturn — control
   never resumes at the next pcode op — so the region should
   terminate at the CallOther without emitting the trailing
   BranchIndirect into the IR at all.

Both shapes are construction-time questions about a single CallOther
node.  Doing them at construction puts all CallOther knowledge in one
place and removes a downstream optimisation pass.

## Goals

1. **One classification *table*.**
   `target::user_ops::classify(name) -> Option<UserOpClass>` is the
   single source of truth for what a CallOther means.  Two callers
   read it: `cfg::region_builder` (to terminate trap regions on the
   first pass) and `ir::FunctionBuilder::build_call_other` (to
   dispatch IR shape).  Both are pure consumers — the *logic* lives
   only in the table.

2. **One IR node kind for all CallOthers.**
   `NodeKind::CallOther { user_op_id }` stays uniform.  NoReturn-ness
   is a property of the *name* (table lookup), not the node kind.
   Pattern queries continue to use `pattern::call_other()` and gain
   no new constructor.

3. **Strict on emission.**
   Any user-op name actually emitted by a lift that has no entry in
   the table fails the lift with a typed
   `UnknownUserOpError { name, addr }`.  The 1756-name x86_64 user-op
   universe is irrelevant; the table grows incrementally with what
   the corpus actually emits.

4. **`opt::CallOtherElide` is deleted.**
   Construction-time NoOp handling subsumes it.  The `NO_OP_USER_OPS`
   constant and the `is_known_no_op` shortcut in
   `strider::insn::handle_call_other` go too.  Strider's pipeline
   builders stop adding `CallOtherElide` to the destructive pipeline.

5. **`commit_creds` / `do_exit` / `do_task_dead` / `__schedule` /
   `__alloc_pages_nodemask` / `kmem_cache_free` / `vfree` / etc.
   lift cleanly** on x86_64 and aarch64 4.19, plus the corresponding
   6.x kernels and any FreeBSD trap-using paths.

6. **The cfg emits the truthful terminator on iteration 0.**  A new
   `cfg::RegionTerminator::NoReturn` variant is introduced; the cfg's
   `process_new_insn` consults `target::user_ops::classify(name)` for
   every CallOther it encounters, and finishes the region as
   `NoReturn` immediately when the classification is `NoReturn` —
   skipping the trailing `BranchIndirect` that today's cfg routes
   into `process_branch_indirect`.  No post-IR rewrite, no per-IR
   `terminated_early` flag, no `analyze_cfg` signature change.

7. **No new orchestrator iteration, no new `ResolvedTargets`
   variant, no tier-2 classifier rule.**  The trap fix is local to a
   small `CallOther` arm in `cfg::region_builder` plus the IR
   builder's classification dispatch.

8. **`pattern::call_other()` gains a `.name(s)` constraint.**  Today
   the builder offers `.user_op_id(u64)`; numerically opaque and
   per-Sleigh-spec.  Adding `.name(&str)` lets pattern queries match
   directly on the user-op name from `Graph::call_other_names`,
   making queries like `pattern::call_other().name("cpuid")` natural.

## Non-goals

* **Per-arch classification keying.**  The initial table is keyed by
  name only.  Known names of interest (`setISAMode`,
  `setEndianState`, `invalidInstructionException`,
  `SoftwareBreakpoint`) do not collide across the arches we currently
  support.  If a future op name collides with different semantics on
  two arches, promote the table to `(arch, name) -> UserOpClass` —
  out of scope here.

* **Tuning the Opaque clobber set per user-op.**  Today's Opaque
  default is "every tracked variable except SP".  This spec keeps
  that.  Per-user-op clobber overrides (analogous to
  `RunConfig::per_address_ccs` for direct calls) are a separate
  follow-up that rides on top of the table introduced here.

* **Cleaning up the wait_consider_task empty-region bug surfaced
  during root-cause investigation.**  That is a separate cfg-side
  edge case (regions that start at a zero-pcode `nop` whose first
  fall-through machine instruction is already a known region start)
  and gets its own spec.

## Architectural facts

* `ir::Graph` holds one `NodeKind::CallOther { user_op_id }` variant.
  The user-op *name* lives on the side table
  `Graph::call_other_names: SecondaryMap<NodeId, Option<String>>`,
  populated by the analyser at IR construction time.  Pattern
  matchers and the existing optimiser pass both consult this side
  table, not the CallOther kind itself.

* `ir::FunctionBuilder::build_call_other` ([`crates/ir/src/builder/call.rs`])
  has two entry points today: `build_call_other(user_op_id, args,
  output_ty)` (conservative-clobber default) and
  `build_call_other_with_clobbers(user_op_id, args, output_ty,
  clobber_vars)` (caller-supplied clobber set, used by the
  `is_known_no_op` shortcut to pass an empty slice).

* `strider::insn::handle_call_other`
  ([`crates/strider/src/strider/insn/mod.rs:109`]) resolves the
  user-op id to a name via `self.cfg.sleigh.user_op_name(id)`,
  consults `opt::NO_OP_USER_OPS`, picks the with-clobbers or
  default-clobbers builder, and afterwards stamps the resolved name
  onto `Graph::call_other_names` for the optimiser to see.

* `opt::CallOtherElide` ([`crates/opt/src/call_other_elide/mod.rs`])
  is in the destructive optimiser pipeline.  It walks
  `NodeKind::CallOther` candidates, checks the side-table name
  against `NO_OP_USER_OPS`, rewires control-out → control-in and
  memory-out → memory-in, then detaches the node's inputs so it
  becomes a zombie that subsequent passes ignore.

* The `cfg::Builder` walks pcode and today routes `Opcode::CallOther`
  through the catch-all `_ => DidntFinishProcessing` arm in
  [`crates/cfg/src/cfg/builder/region_builder.rs:395`], appending the
  CallOther insn to `self.insns` and continuing.  When the trailing
  `Opcode::BranchIndirect` arrives one pcode index later, it routes
  to `process_branch_indirect`, which fails to resolve the target
  and emits `RegionTerminator::UnresolvedIndirectBranch { target_vn,
  addr }`.  This spec replaces the catch-all with a typed
  `Opcode::CallOther` arm that resolves the user-op name (via
  `self.builder.sleigh.user_op_name(id)`) and consults
  `target::user_ops::classify` so trap regions terminate cleanly on
  the first pass.

* `cfg`'s current Cargo deps are `rsleigh` + `petgraph` + workspace
  utilities; it transitively depends on `target` via `pcode-lift →
  ir → target`.  Adding a direct `cfg → target` dep is one line in
  `crates/cfg/Cargo.toml` and creates no cycle.

* The IR per-region driver in
  [`crates/strider/src/strider/pipeline.rs:305`] walks each cfg
  region's pcode insns, skipping the trailing terminator opcode via
  `SpecialTerm::skips_opcode`, then dispatches a post-loop handler
  (`handle_unresolved_indirect_branch` /
  `handle_switch` / `handle_tail_call`) based on the cfg's
  `RegionTerminator`.

* `target` is leaf in the workspace dep graph (no IR, no cfg, no
  rsleigh-state).  Adding a `target::user_ops` module keeps it leaf.

* `ir` already depends on `target` (it consumes `BuiltCallingConvention`
  in `FunctionBuilder::new`), so importing `target::user_ops::classify`
  from `ir::FunctionBuilder::build_call_other` adds no new dep edge.

## Design

### Module layout

```
crates/target/
└── src/
    ├── lib.rs              (exports user_ops module)
    ├── arch.rs             (existing)
    ├── cc.rs               (existing)
    └── user_ops.rs         (NEW)

crates/ir/
└── src/builder/
    └── call.rs             (build_call_other re-shaped)

crates/strider/
└── src/
    ├── strider/insn/mod.rs (handle_call_other simplified)
    ├── strider/pipeline.rs (per-region driver gains terminated_early)
    └── errors.rs           (UnknownUserOpError added)

crates/opt/
└── src/
    └── call_other_elide/   (DELETED)
```

### `target::user_ops` (new module)

```rust
//! Sleigh user-op (CallOther) classification table.  Single source of
//! truth for how the IR builder handles each user-op encountered
//! during a lift.

/// What `ir::FunctionBuilder::build_call_other` does for a given
/// user-op name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOpClass {
    /// True no-op in the IR's data-flow / control-flow / memory model.
    /// `build_call_other` skips the node entirely; control and memory
    /// pass through unchanged; the pcode insn's output varnode (if any)
    /// is ignored.
    NoOp,

    /// Trap instruction whose semantic effect is "execution does not
    /// continue past this point" (Linux `BUG()` / `BUG_ON()` /
    /// `WARN()`-class).  `build_call_other` emits a CallOther node
    /// (with control + memory inputs only — no clobber outputs, no
    /// value output, dangling control + memory outputs) and returns
    /// `CallOtherOutcome::NoReturn`.  The IR per-region driver, on
    /// receiving NoReturn, sets `terminated_early` and stops
    /// processing the region.
    NoReturn,

    /// Known opaque user-op (cpuid, syscall, lock-prefix, …).
    /// `build_call_other` emits today's CallOther shape:
    /// `[ctrl_in, mem_in, args…]` → `[ctrl_out, mem_out, value?,
    /// clobbers…]` with the conservative "every tracked variable
    /// except SP" clobber set.
    Opaque,
}

/// Look up a user-op name in the classification table.  Returns
/// `None` for unknown names; the IR builder converts that into
/// `UnknownUserOpError`.
#[must_use]
pub fn classify(name: &str) -> Option<UserOpClass> {
    TABLE.get(name).copied()
}

/// The classification table.  Kept tiny and ASCII-sorted within
/// each group for diffability.  Grows incrementally as new names
/// are encountered in real lifts.
static TABLE: phf::Map<&'static str, UserOpClass> = phf_map! {
    // ── No-ops (decoder context bits; no IR-visible effect) ──
    "setEndianState" => UserOpClass::NoOp,
    "setISAMode"     => UserOpClass::NoOp,

    // ── Noreturn traps (Linux BUG_ON / WARN-class) ──
    "invalidInstructionException" => UserOpClass::NoReturn, // x86 / x86_64 ud2
    "SoftwareBreakpoint"          => UserOpClass::NoReturn, // aarch64 brk #imm

    // ── Opaque (test-required + initial real-world set) ──
    // Tests in opt/, pattern/, ir/ that previously used synthetic
    // user-op ids migrate to these real names.
    "cpuid"   => UserOpClass::Opaque,   // x86 / x86_64 CPUID
    "syscall" => UserOpClass::Opaque,   // x86_64 SYSCALL
    "rdtsc"   => UserOpClass::Opaque,   // x86 / x86_64 RDTSC
    // (more added incrementally as lifts surface them)
};
```

The table can be implemented as either a `phf::Map` (perfect-hash,
compile-time) or a `match` expression — both yield compile-time
constant lookup with the same semantics.  This spec does not commit
a choice; the implementation plan will pick whichever fits the
existing dep graph more cleanly.

### `CallOtherOutcome` (new return type)

```rust
// In crates/ir/src/builder/call.rs
pub enum CallOtherOutcome {
    /// Classification was `NoOp`.  No IR node emitted.  Caller's
    /// region walk continues with control / memory unchanged.
    NoOp,

    /// Classification was `NoReturn`.  The IR builder emitted a
    /// `NodeKind::CallOther` node anchoring control + memory (so its
    /// asm fingerprint preserves the trap address) but rebound
    /// nothing.  The caller's region walk MUST treat the region as
    /// terminated: stop processing the rest of the region's pcode
    /// AND skip the post-loop terminator dispatcher.
    NoReturn,

    /// Classification was `Opaque`.  Today's behaviour: a full
    /// CallOther with conservative clobbers + optional value output.
    Built {
        node: NodeId,
        value: Option<NodeOutputId>,
    },
}
```

### `ir::FunctionBuilder::build_call_other` (re-shaped)

```rust
// In crates/ir/src/builder/call.rs

/// Build a CallOther according to its classification in
/// `target::user_ops::classify(name)`.
///
/// # Errors
/// Returns [`UnknownUserOpError`] if `name` has no entry in the
/// classification table.  Forwarded as `anyhow::Error` for
/// downcast.
pub fn build_call_other(
    &mut self,
    name: &str,
    user_op_id: u64,
    args: &[NodeOutputId],
    output_ty: Option<NodeOutputType>,
) -> Result<CallOtherOutcome> {
    let class = target::user_ops::classify(name).ok_or_else(|| {
        UnknownUserOpError { name: name.to_string() }
    })?;
    match class {
        UserOpClass::NoOp => {
            // No node, no output rebinds.  Ctrl/mem unchanged.
            Ok(CallOtherOutcome::NoOp)
        }
        UserOpClass::NoReturn => {
            // Emit a CallOther with [ctrl, mem] inputs and
            // [ctrl, mem] outputs but no clobbers and no value.
            // Outputs dangle (region terminates here, no consumers).
            // Stamp the user-op name on the side table for patterns.
            let node = self.build_call_other_terminal(user_op_id)?;
            self.set_call_other_name(node, name);
            Ok(CallOtherOutcome::NoReturn)
        }
        UserOpClass::Opaque => {
            // Today's behaviour: conservative clobbers, optional value.
            let (node, value) = self.build_call_other_opaque(
                user_op_id, args, output_ty,
            )?;
            self.set_call_other_name(node, name);
            Ok(CallOtherOutcome::Built { node, value })
        }
    }
}
```

`build_call_other_opaque` is the renamed body of today's
`build_call_other` (the conservative-clobber path).
`build_call_other_terminal` is a thin variant that emits no clobbers
and no value, sized only for ctrl + mem inputs / outputs.  Both are
`pub(crate)`; only the high-level `build_call_other` is `pub`.

The existing `build_call_other_with_clobbers(user_op_id, args,
output_ty, clobber_vars)` is **demoted to `pub(crate)`** and used
internally by `build_call_other_opaque`.  No external caller is
expected to hand-pick clobber sets after this change; all such
decisions live in the table.

### `strider::insn::handle_call_other` (simplified)

```rust
fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
    let id_vn = pcode_lift::first_input_or_err(insn)?;
    if id_vn.addr_space != rsleigh::VnSpace::CONST {
        bail!("opcode {:?} expects a CONST input at position 0", insn.opcode);
    }
    let user_op_id = id_vn.addr_off;
    let user_op_id_u32 = u32::try_from(user_op_id)
        .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
    let name = self.cfg.sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
        anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
    })?;
    let args: Vec<ir::Value> = if insn.inputs.len() > 1 {
        insn.inputs[1..].iter().map(|vn| self.read_vn(vn)).collect::<Result<_>>()?
    } else {
        Vec::new()
    };
    let output_ty: Option<NodeOutputType> = match insn.output.as_ref() {
        Some(out_vn) => Some(out_vn.size.try_into()?),
        None => None,
    };
    match self.builder.build_call_other(name, user_op_id, &args, output_ty)? {
        CallOtherOutcome::NoOp | CallOtherOutcome::NoReturn => Ok(()),
        CallOtherOutcome::Built { value, .. } => {
            if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
                self.write_vn(out_vn, val)?;
            }
            Ok(())
        }
    }
}
```

`is_known_no_op` lookup gone.  Two-builder dispatch gone.  Trailing
`set_call_other_name` gone (the IR builder does it now, where the
node is created).  Return type stays `Result<()>` — the cfg has
already terminated trap regions, so the IR walker has nothing
extra to signal.

### Per-region IR driver — unchanged

The cfg now emits `RegionTerminator::NoReturn` for trap regions on
iteration 0.  Such a region's pcode list is `[..., CallOther(noreturn)]`
— the trailing `BranchIndirect` is never decoded.  The IR per-region
walk processes the CallOther via the normal pcode loop:
`build_call_other(name, …)` returns `CallOtherOutcome::NoReturn`,
which `handle_call_other` interprets by writing nothing (no value
output to bind) and returning `Ok(())`.  The post-loop dispatcher
sees `SpecialTerm::from_terminator(NoReturn) == None` and does
nothing.  Region's IR ends with the terminal CallOther node; no
successors; no `IndirectBranch` placeholder.

No `RegionStatus` enum, no `terminated_early` flag, no
`analyze_cfg(&mut Cfg)` signature change.

### `cfg::RegionTerminator::NoReturn` (new variant)

```rust
// In crates/cfg/src/cfg/types.rs
pub enum RegionTerminator {
    Branch,
    CondBranch,
    Return,
    NoReturn,                          // NEW
    Fallthrough,
    TailCall { target: u64 },
    Switch { target_vn, targets, target_value },
    UnresolvedIndirectBranch { target_vn, addr },
}
```

Emitted by `cfg::region_builder::process_new_insn` when a CallOther's
classification is `UserOpClass::NoReturn`.  Consumer impact:
* `dot::cfg_dot` rendering: add a NoReturn label.
* Any `match terminator` with a wildcard arm continues to compile.
* `SpecialTerm::from_terminator` returns `None` for `NoReturn` (no
  post-loop handler — the IR per-region walk processes the CallOther
  via the normal pcode loop, which builds a terminal CallOther node).

### `cfg::region_builder::process_new_insn` — new `CallOther` arm

```rust
// In crates/cfg/src/cfg/builder/region_builder.rs::process_new_insn
//
// Inserted before the final `_ => Ok(ProcessInsnRes::DidntFinishProcessing)`
// arm.  Today's catch-all kept the CallOther in self.insns and
// continued — the same default behaviour for NoOp / Opaque / unknown
// classifications.  Only NoReturn terminates the region.
rsleigh::Opcode::CallOther => {
    // Resolve the user-op id from the CONST input at position 0.
    let id_vn = insn.inputs.first().ok_or_else(|| {
        anyhow!("CallOther at {addr:?} has no user-op id input")
    })?;
    if id_vn.addr_space != rsleigh::VnSpace::CONST {
        return Ok(ProcessInsnRes::DidntFinishProcessing);
    }
    let id_u32 = u32::try_from(id_vn.addr_off).unwrap_or(u32::MAX);
    let name = self.builder.sleigh.user_op_name(id_u32);
    let class = name.and_then(target::user_ops::classify);
    if matches!(class, Some(target::user_ops::UserOpClass::NoReturn)) {
        // Terminate the region BEFORE Sleigh's trailing
        // BranchIndirect can be processed.  The CallOther is
        // already in self.insns from process_new_insn's prologue
        // push; finish_current_region carries it.
        self.finish_current_region(RegionTerminator::NoReturn)?;
        return Ok(ProcessInsnRes::FinishedProcessing);
    }
    // NoOp / Opaque / unknown: today's catch-all (insn stays in
    // self.insns; loop continues to the next pcode op).
    Ok(ProcessInsnRes::DidntFinishProcessing)
}
```

**The cfg never errors on unknown user-ops** — that is the IR
builder's strict-on-emission gate.  The cfg only needs to know
which CallOthers terminate a region; everything else falls through
to today's behaviour.

### `pattern::CallOtherPat::name`

```rust
// In crates/pattern/src/pat/builders/call.rs

pub struct CallOtherPat {
    user_op_id: Option<u64>,
    name: Option<String>,        // NEW
    args: Vec<(usize, Pat)>,
}

impl CallOtherPat {
    /// Constrain the matched node's user-op name (read from
    /// `Graph::call_other_names`) to equal `n`.  Combinable with
    /// `.user_op_id(...)` (both must match) and `.arg(...)`.
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
}

impl From<CallOtherPat> for Pat {
    fn from(b: CallOtherPat) -> Pat {
        let CallOtherPat { user_op_id, name, args } = b;
        // ...build base pattern as today...
        let pat: Pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed_inputs)).into_pat();
        match name {
            None => pat,
            Some(want) => pat.when(move |m, g| {
                g.graph.call_other_name(m.root())
                    .map_or(false, |s| s == want.as_str())
            }),
        }
    }
}
```

The strider-py wrapper exposes the same builder method (`pat.name(s)`).

### Removals

* `crates/opt/src/call_other_elide/` — entire module deleted.
  Module declaration removed from `crates/opt/src/lib.rs`.
* `opt::NO_OP_USER_OPS` — removed alongside the module.
* `is_known_no_op` shortcut + `build_call_other_with_clobbers`
  selection logic in `strider::insn::handle_call_other` — removed.
* `Strider::build_destructive_optimizer_pipeline` — drops
  `CallOtherElide` from the pass list
  ([`crates/strider/src/strider/pipeline.rs:184`]).
* `build_call_other_with_clobbers`'s `pub` visibility — demoted to
  `pub(crate)`.

### Error type

```rust
// In crates/strider/src/errors.rs (or crates/ir/src/error.rs —
// see Open Questions)
#[derive(Debug, thiserror::Error)]
#[error("unknown CallOther user-op name {name:?}; \
         add an entry to target::user_ops::TABLE")]
pub struct UnknownUserOpError {
    pub name: String,
}
```

Surfaced via `anyhow::Error::downcast_ref::<UnknownUserOpError>()`
matching the existing convention for `UnresolvedIndirectBranch`.

### Pattern surface

`pattern::call_other()` continues to match `NodeKind::CallOther`.
The new `.name(s)` builder method (above) covers the common case
of "match a specific user-op name."  Patterns wanting "is this a
NoReturn user-op" combine the new `.name(...)` with the
`UserOpClass` table:

```rust
// Match exactly the x86 trap CallOther.
pattern::call_other().name("invalidInstructionException")

// Or, generically, match any noreturn CallOther:
pattern::call_other().when(|m, g| {
    g.graph.call_other_name(m.root())
        .and_then(target::user_ops::classify)
        == Some(target::user_ops::UserOpClass::NoReturn)
})
```

## Test migration

Per Q5 = B (migrate tests to real user-op names): every existing
test that calls `build_call_other(<id>, …)` with a synthetic id
becomes `build_call_other("<real-name>", <id>, …)` where the real
name is in the `Opaque` group of the table.  Affected files:

* [`crates/opt/src/call_other_elide/tests.rs`] — DELETED with the module.
* [`crates/pattern/tests/get_vn_with_callother_clobber.rs`] —
  3 sites, all using id `7`.  Replace with `"cpuid"` (or another
  Opaque entry).
* [`crates/ir/src/builder/tests.rs`] —
  3 sites (`build_call_other_without_output_advances_ctrl_and_memory`,
  `build_call_other_with_output_returns_typed_value`,
  `build_call_other_rejects_non_value_arg`).  Same replacement.
* [`crates/opt/src/dead_branch/tests.rs`] — 1 site using id `0`.
  Replace.
* [`crates/pattern/tests/matching/support/graph.rs`] — test helper;
  takes user_op_id today, will need to take a name argument too.

A new test file `crates/ir/tests/call_other_classification.rs`
covers each outcome explicitly:
* `build_call_other("setISAMode", …)` → `Ok(NoOp)`, no node added,
  ctrl/mem unchanged.
* `build_call_other("invalidInstructionException", …)` → `Ok(NoReturn)`,
  CallOther node present, no clobber outputs, outputs dangle.
* `build_call_other("cpuid", …)` → `Ok(Built { … })`, conservative
  clobbers present (matches today's behaviour).
* `build_call_other("nonexistent_op_zzzz", …)` →
  `Err(UnknownUserOpError { … })`.

Integration coverage in `crates/strider/tests/`:
* `bug_on_lifts_cleanly_x86_64.rs` — minimal x86_64 fixture
  containing `ud2`, asserts `strider::run` returns `Ok` and the
  function graph terminates without an `IndirectBranch` node.
* `bug_on_lifts_cleanly_aarch64.rs` — same with `brk #0x800`.

## Strider-py exposure

`UnknownUserOpError` becomes `strider.errors.UnknownUserOpError`
(subclass of `strider.errors.StriderError`).  No new public surface
on `strider.Graph` / `strider.pattern` — the table is internal
data, queried via the existing `Match.captured_name` style accessors.

## Migration / rollout

1. Land `target::user_ops` + the new `build_call_other` API behind
   a parallel name (`build_call_other_classified`?), with the old
   `build_call_other(user_op_id, …)` retained as a thin wrapper that
   passes the empty string for `name` and shortcuts to Opaque.
2. Migrate `strider::insn::handle_call_other` to the new entry.
   At this point the trap fix is live but the optimiser still has
   `CallOtherElide`.
3. Migrate all test sites to real names.  Delete `CallOtherElide`
   and `NO_OP_USER_OPS`.  Delete the old `build_call_other` thin
   wrapper.  Demote `build_call_other_with_clobbers`.
4. Run the bsdfinder corpus end-to-end; harvest new "unknown user-op"
   errors and add to the table as Opaque entries.

Steps 1–2 are net-additive and can ship before 3–4 if a smaller PR
is preferred.  Steps 3–4 are the breaking-cleanup pass.

## Risks & open questions

* **Pattern queries that asserted "no CallOther" will now match BUG
  sites.**  Previously those regions crashed the lift, so no
  pattern could observe them.  Audit during step 4 of rollout.
  Likely affects `bsdfinder/`'s pattern queries that walk every
  `Call` / `CallOther`; mitigation is a `.when(name != "BUG_op")`
  predicate.

* **Where does `UnknownUserOpError` live?**  Two candidates:
  1. `ir::error` — the error originates in `ir::FunctionBuilder`.
  2. `strider::errors` — siblings of `UnresolvedIndirectBranch`,
     uniform downcast pattern for callers.

  Recommendation: `ir::error::UnknownUserOpError` (originates in
  `ir`, propagated via `anyhow::Error`), with `strider::errors`
  re-exporting for convenience.  Open for review.

* **`phf` dep on `target`.**  If undesirable, the table can be a
  `match` expression — same compile-time-constant semantics, ugly
  diff.  Either is acceptable.

* **Per-arch keying** if a future name collides with different
  semantics.  Documented as out of scope; the `classify(name)` API
  can be widened to `classify(arch, name)` later without ABI
  breakage on the `UserOpClass` enum itself.

* **cfg → target dep.**  cfg gains a direct dep on `target` (one
  line in `crates/cfg/Cargo.toml`).  No cycle: target is a leaf,
  and cfg already transitively depends on it via `pcode-lift → ir
  → target`.

* **Sleigh user-op resolution failures in cfg.**  If
  `sleigh.user_op_name(id)` returns `None` for a CallOther's id
  (id out of range), the cfg arm falls through to today's
  catch-all (no termination).  The IR builder's strict-on-emission
  check then surfaces the problem with full context.  Surfacing
  unknown user-op-id at the cfg layer too would be redundant.
