//! Flowing decode context, GHIDRA-style: each flowing var is a per-address
//! fact.
//!
//! A sla marks some context variables as *flowing*: the ISA mode (ARM
//! `TMode`) plus per-instruction decode internals (condition eval, IT-block
//! state, register-list iteration, x86 operand/address size) Sleigh flows
//! along straight-line code. Only some are ever `globalset`, and a committed
//! value is a per-address fact this build must re-derive rather than read.
//! `SleighArch::flow_commit_vars` names those vars and
//! `RegionBuilder::rederive_flow` re-derives them, erasing each to what its
//! sla flow class says. A pin cannot stand in for that on a FLOWING var: a
//! forward paint stops at the next change point of that var, which is where
//! an earlier build's commit sits. x86 declares dozens flowing but commits
//! none, so snapshotting and pinning there only read; PowerPC's `linkreg` IS
//! committed to a differing value by `mtlr`, but every constructor it selects
//! emits the same p-code as its `linkreg=0` twin, so a stale value cannot
//! change the lift.
//!
//! Like GHIDRA's `ContextDatabase`, the context is a value committed per
//! address. Pin a cold entry back to the pspec defaults (undo a prior
//! function's leak on a reused engine), then before decoding a region pin every
//! var to the value that flowed to it (its *captured* context), so a region
//! Sleigh never straight-line-flowed to (a strider-resolved indirect target, a
//! backward edge) still decodes in it. An interworking resolved branch bakes
//! its own ISA-mode bit into the captured context it hands the target (see
//! [`FlowVars::with_mode_bit`]).

/// The context vars the sla marks as flowing. Constant per sla, so the
/// [`DecodeEngine`](crate::DecodeEngine) reads it once.
pub(crate) struct FlowVars {
    vars: Vec<String>,
}

#[cfg(test)]
thread_local! {
    /// Context reads issued since a test last cleared it.
    pub(crate) static CONTEXT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One context read, counted under `cfg(test)`.
fn get_context_at<R: rsleigh::MemReader>(
    sleigh: &rsleigh::Sleigh<R>,
    addr: u64,
    name: &str,
) -> crate::Result<u32> {
    #[cfg(test)]
    CONTEXT_READS.with(|n| n.set(n.get() + 1));
    Ok(sleigh.get_context_at(addr, name)?)
}

impl FlowVars {
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The sla's flowing context vars.
    pub fn discover<R: rsleigh::MemReader>(sleigh: &rsleigh::Sleigh<R>) -> crate::Result<Self> {
        Ok(Self {
            vars: sleigh.flow_context_vars()?,
        })
    }

    /// The value of every flow var resolved at `addr`.
    ///
    /// Every name is one the sla declared flowing (via [`Self::discover`]), so
    /// `get_context_at` cannot miss it (Sleigh throws only on an unregistered
    /// name); a read failure is a broken rsleigh invariant, not a runtime case.
    pub fn snapshot<R: rsleigh::MemReader>(
        &self,
        sleigh: &rsleigh::Sleigh<R>,
        addr: u64,
    ) -> FlowContext {
        FlowContext(
            self.vars
                .iter()
                .map(|name| {
                    get_context_at(sleigh, addr, name).unwrap_or_else(|e| {
                        panic!("sla-declared flow var {name:?} failed to read: {e:?}")
                    })
                })
                .collect(),
        )
    }

    /// `ctx`'s value for the flow var named `var`, or `None` if not a flow var
    /// or `ctx` was not snapshotted by these vars.
    #[must_use]
    pub fn value_of(&self, ctx: &FlowContext, var: &str) -> Option<u32> {
        self.vars
            .iter()
            .position(|name| name == var)
            .and_then(|i| ctx.0.get(i).copied())
    }

    /// Whether `name` is one of the sla's flowing context vars.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.vars.iter().any(|n| n == name)
    }

    /// `name`'s class: flowing exactly when the sla declares it so. `name`
    /// must be a context var the sla declares, or it reads `NoFlow`;
    /// [`DecodeEngine::new`](crate::DecodeEngine::new) rejects an undeclared
    /// one by reading its default.
    #[must_use]
    pub fn commit_class(&self, name: &str) -> FlowCommit {
        if self.contains(name) {
            FlowCommit::Flowing
        } else {
            FlowCommit::NoFlow
        }
    }

    /// Commit `want` at `addr`, overwriting whatever flowed or forward-held
    /// there.
    ///
    /// Only the vars where `addr`'s current context disagrees are written.  A
    /// `set_context_at` flushes Sleigh's whole parse cache, so an all-agreeing
    /// context must cost none.
    ///
    /// Writes are FLOWING, so anything painted here holds until the next explicit
    /// change point. ARM's IT-block vars (`itmode` `(5,5)`, `cond_mask`
    /// `(10,13)`, `cond_shft` `(9,13)`, ...) flow in their own right; `condit`,
    /// the `(5,13)` field spanning them, is declared `noflow` and is never in
    /// the set. An `IT` does `globalset(inst_next, condit)`, committing all nine
    /// bits at once, so several of the overlapping views do enter one diff. That
    /// is safe: `want` for this group is always a whole snapshot, so the
    /// overlapping views write the same value to every bit they share.
    pub fn pin_at<R: rsleigh::MemReader>(
        &self,
        sleigh: &mut rsleigh::Sleigh<R>,
        addr: u64,
        want: &FlowContext,
    ) -> crate::Result<()> {
        let current = self.snapshot(sleigh, addr);
        for (name, value) in self.diff(&current, want) {
            sleigh.set_context_at(addr, name, value)?;
        }
        Ok(())
    }

    /// `base` with the flow var named `var` set to `value`, used to carry a
    /// live committed value onto an edge Sleigh never flowed along. Returns a
    /// clone of `base` unchanged when `var` is not a flow var of this sla, or
    /// `base` is not one of our contexts.
    #[must_use]
    pub fn with_var(&self, base: &FlowContext, var: &str, value: u32) -> FlowContext {
        let mut ctx = base.clone();
        if let Some(i) = self.vars.iter().position(|name| name == var)
            && let Some(slot) = ctx.0.get_mut(i)
        {
            *slot = value;
        }
        ctx
    }

    /// [`Self::with_var`] for a one-bit var, which bakes a resolved
    /// interworking target's ISA mode into the context it decodes in.
    #[must_use]
    pub fn with_mode_bit(&self, base: &FlowContext, var: &str, bit: bool) -> FlowContext {
        self.with_var(base, var, u32::from(bit))
    }

    /// The vars where `have` disagrees with `want`, as `(name, want value)`.
    fn diff<'a>(
        &'a self,
        have: &'a FlowContext,
        want: &'a FlowContext,
    ) -> impl Iterator<Item = (&'a str, u32)> {
        self.vars
            .iter()
            .zip(have.0.iter().zip(&want.0))
            .filter_map(|(name, (&h, &w))| (h != w).then_some((name.as_str(), w)))
    }
}

/// Writes `want` to the context var `name` at `addr` unless it already reads
/// so, returning whether it wrote: a `set_context_at` flushes Sleigh's whole
/// parse cache.
pub(crate) fn hold_var<R: rsleigh::MemReader>(
    sleigh: &mut rsleigh::Sleigh<R>,
    addr: u64,
    name: &str,
    want: u32,
) -> crate::Result<bool> {
    if sleigh.get_context_at(addr, name)? == want {
        return Ok(false);
    }
    sleigh.set_context_at(addr, name, want)?;
    Ok(true)
}

/// The value of each flow var at a program point, positionally aligned with the
/// [`FlowVars`] that produced it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct FlowContext(Vec<u32>);

/// Whether the sla declares a committed context var flowing, which decides
/// what `RegionBuilder::rederive_flow` erases it to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FlowCommit {
    /// Confined to one address, so the pspec default is the erase target.
    NoFlow,
    /// Painted forward to persist, so the predecessor's value is.
    Flowing,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm_like() -> FlowVars {
        FlowVars {
            vars: vec!["TMode".into(), "itmode".into()],
        }
    }

    #[test]
    fn diff_reports_all_disagreeing_vars() {
        let vars = arm_like();
        let have = FlowContext(vec![1, 1]);
        let want = FlowContext(vec![0, 0]);
        let changes: Vec<_> = vars.diff(&have, &want).collect();
        assert_eq!(changes, vec![("TMode", 0), ("itmode", 0)]);
    }

    #[test]
    fn matching_contexts_diff_to_nothing_and_compare_equal() {
        let vars = arm_like();
        let a = FlowContext(vec![1, 0]);
        let b = FlowContext(vec![1, 0]);
        assert_eq!(a, b);
        assert_eq!(vars.diff(&a, &b).count(), 0);
    }

    #[test]
    fn with_mode_bit_sets_only_the_named_var() {
        let vars = arm_like();
        let base = FlowContext(vec![0, 5]);
        // TMode -> 1, itmode untouched; an unknown var leaves it unchanged.
        assert_eq!(
            vars.with_mode_bit(&base, "TMode", true),
            FlowContext(vec![1, 5])
        );
        assert_eq!(vars.with_mode_bit(&base, "nope", true), base);
    }

    /// The general setter is not a bit-setter: a flowing committed var wider
    /// than one bit carries its whole value.
    #[test]
    fn with_var_sets_the_whole_value() {
        let vars = arm_like();
        let base = FlowContext(vec![0, 5]);
        assert_eq!(vars.with_var(&base, "itmode", 9), FlowContext(vec![0, 9]));
    }

    /// A context built for another `FlowVars` (an empty one, say) must not
    /// index out of bounds.
    #[test]
    fn a_context_shorter_than_the_var_list_does_not_index_out_of_bounds() {
        let vars = arm_like();
        let foreign = FlowContext::default();
        assert_eq!(vars.value_of(&foreign, "TMode"), None);
        assert_eq!(vars.with_mode_bit(&foreign, "TMode", true), foreign);
    }

    /// Counts every memory read the Sleigh issues, so a test can tell a parse
    /// cache hit from a re-decode without a timer.
    #[derive(Clone)]
    struct CountingReader {
        inner: rsleigh::mem_readers::BufMemReader<Vec<u8>>,
        reads: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl rsleigh::MemReader for CountingReader {
        type Err = <rsleigh::mem_readers::BufMemReader<Vec<u8>> as rsleigh::MemReader>::Err;

        fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
            self.reads.set(self.reads.get() + 1);
            self.inner.read(addr, out_buf)
        }
    }

    fn counting_arm_sleigh() -> (
        rsleigh::Sleigh<CountingReader>,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let arch = strider_target::SleighArch::arm();
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = CountingReader {
            inner: rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x100], 0x1000),
            reads: std::rc::Rc::clone(&reads),
        };
        let sleigh = arch.sleigh(reader).expect("create Sleigh");
        (sleigh, reads)
    }

    /// `set_context_at` flushes Sleigh's whole parse cache
    /// (`bindings.cpp`'s `invalidateDisassembly`), so re-imposing the ISA-mode
    /// var when nothing was written costs a re-decode of every region.
    #[test]
    fn pin_at_over_a_matching_context_does_not_flush_the_parse_cache() {
        let (mut sleigh, reads) = counting_arm_sleigh();
        let flow = FlowVars::discover(&sleigh).expect("discover flow vars");
        let want = flow.snapshot(&sleigh, 0x1000);

        sleigh.lift_one(0x1000).expect("decode once");
        let before = reads.get();
        sleigh.lift_one(0x1000).expect("re-lift");
        let cached_lift = reads.get() - before;

        flow.pin_at(&mut sleigh, 0x1000, &want).expect("pin_at");
        let before = reads.get();
        sleigh.lift_one(0x1000).expect("lift after pin");
        assert_eq!(
            reads.get() - before,
            cached_lift,
            "pinning a context that already matches re-decoded the instruction",
        );
    }

    /// One `IT` commits every `condit`-group bit at once, so re-imposing a
    /// context over an address it reached diffs several of them together. The
    /// group is written from one snapshot, so the shared bits agree and the
    /// values land.
    #[test]
    fn a_diff_touching_two_condit_group_vars_is_applied() {
        let (mut sleigh, _reads) = counting_arm_sleigh();
        let vars = FlowVars {
            vars: vec!["itmode".into(), "cond_mask".into()],
        };
        for name in &vars.vars {
            sleigh.set_context_at(0x1000, name, 0).expect("clear");
        }
        let want = FlowContext(vec![1, 1]);
        vars.pin_at(&mut sleigh, 0x1000, &want).expect("pin");
        assert_eq!(
            vars.snapshot(&sleigh, 0x1000),
            want,
            "a differing context must be written"
        );
    }

    /// The real ARM sla declares four flow vars at the same bit `(0,0)`:
    /// `TMode`, `T`, `LowBitCodeMode`, `ISA_MODE`. A resolved interworking target
    /// that switches a Thumb function to ARM sets only `TMode` to 0 in the
    /// carried context; the three aliases stay at the Thumb value, and
    /// `pin_at`'s diff writes them last, clobbering the intended mode. Pinned
    /// here is that the carried context survives `pin_at` still naming ARM,
    /// which is what the repair re-imposes from.
    #[test]
    fn a_carried_context_survives_pin_at_still_naming_the_arm_mode() {
        use rsleigh::mem_readers::BufMemReader;
        use strider_target::SleighArch;
        let arch = SleighArch::arm();
        let reader = BufMemReader::new(vec![0u8; 0x100], 0x1000);
        let mut sleigh = arch.sleigh(reader).expect("create Sleigh");
        let flow = FlowVars::discover(&sleigh).expect("discover flow vars");
        assert!(
            flow.vars
                .iter()
                .filter(|v| ["TMode", "T", "LowBitCodeMode", "ISA_MODE"].contains(&v.as_str()))
                .count()
                >= 2,
            "this test is only meaningful when the sla has ISA-mode alias vars",
        );

        // Thumb `function_mode`: pin TMode=1, snapshot (all bit-(0,0) aliases = 1).
        sleigh
            .set_context_at(0x1000, "TMode", 1)
            .expect("set TMode");
        let function_mode = flow.snapshot(&sleigh, 0x1000);

        // A resolved interworking target switching to ARM (bit 0 clear).
        let carried = flow.with_mode_bit(&function_mode, "TMode", false);

        // The target's live context is ARM-default (not yet Thumb-painted): all
        // four bit-(0,0) aliases read 0 there, so pinning the carried context
        // sees the three aliases DIFFER (0 vs the Thumb 1) and writes them.
        sleigh
            .set_context_at(0x2000, "TMode", 0)
            .expect("arm-default target");
        flow.pin_at(&mut sleigh, 0x2000, &carried).expect("pin_at");
        // `pin_at`'s diff writes the bit-(0,0) aliases, which can land after
        // `TMode` and flip it. `pin_at` does not repair that;
        // `RegionBuilder::hold_isa_mode` does, unconditionally, on the
        // `lift_one` that `explore` always follows this call with. The
        // end-to-end guarantee is pinned by
        // `strider-orchestrator/tests/isa_mode_resolved_branch.rs`.
        assert_eq!(
            flow.value_of(&carried, "TMode"),
            Some(0),
            "the carried context still names the ARM mode the repair re-imposes",
        );
    }

    /// What a re-derive erases a committed var to is the sla's own `noflow`
    /// declaration: ARM's IT-block `condit` is `noflow` and ThumbEE's `TEEMode`
    /// flows, on the big-endian sla as on the little-endian one.
    #[test]
    fn commit_class_is_the_slas_flow_declaration() {
        let want = [
            ("condit", FlowCommit::NoFlow),
            ("TEEMode", FlowCommit::Flowing),
        ];
        let mut checked = 0;
        for &preset in strider_target::ArchPreset::ALL {
            let arch = preset.arch();
            if arch.flow_commit_vars().is_empty() {
                continue;
            }
            let sleigh = arch
                .sleigh(rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0))
                .expect("create Sleigh");
            let flow = FlowVars::discover(&sleigh).expect("flow vars");
            for &name in arch.flow_commit_vars() {
                let (_, class) = want
                    .iter()
                    .find(|(n, _)| *n == name)
                    .unwrap_or_else(|| panic!("{preset:?}: no expected class for `{name}`"));
                assert_eq!(flow.commit_class(name), *class, "{preset:?}: `{name}`");
                checked += 1;
            }
        }
        assert_eq!(
            checked, 8,
            "the four ARM32 presets re-derive condit and TEEMode"
        );
    }
}
