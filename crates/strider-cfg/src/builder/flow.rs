//! Flowing decode context: as in GHIDRA's `ContextDatabase`, each flowing
//! var holds a value committed per address.
//!
//! A committed (`globalset`) var is re-derived by
//! `RegionBuilder::rederive_flow`, not pinned: a pin's forward paint stops at
//! the next change point, where an earlier build's commit sits.
//!
//! A cold entry is pinned to the pspec defaults and each region to the
//! context that flowed to it, so a resolved target or a backward edge decodes
//! in it.

/// The context vars the sla marks as flowing
/// ([`strider_target::DecodeTables::flow_vars`]).
#[derive(Clone, Copy)]
pub(crate) struct FlowVars {
    vars: &'static [String],
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
    /// The flowing context vars of `tables`.
    pub fn of(tables: &'static strider_target::DecodeTables) -> Self {
        Self {
            vars: &tables.flow_vars,
        }
    }

    /// The value of every flow var at `addr`. Panics on a read failure: every
    /// name is sla-declared, and Sleigh throws only on an unregistered one.
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

    /// Commits `want` at `addr`, painting forward to the next change point.
    /// Writes only the disagreeing vars: a `set_context_at` flushes Sleigh's
    /// whole parse cache.
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

    /// Sets the flow var `var` in `ctx` to `value`; a no-op when `var` is not
    /// a flow var or `ctx` is not one of these vars' contexts.
    pub fn set_var(&self, ctx: &mut FlowContext, var: &str, value: u32) {
        if let Some(i) = self.vars.iter().position(|name| name == var)
            && let Some(slot) = ctx.0.get_mut(i)
        {
            *slot = value;
        }
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

/// The value of each flow var at a program point, in [`FlowVars`] order.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct FlowContext(Vec<u32>);

impl From<&[u32]> for FlowContext {
    fn from(values: &[u32]) -> Self {
        Self(values.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm_like() -> FlowVars {
        FlowVars {
            vars: vec!["TMode".into(), "itmode".into()].leak(),
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
    fn set_var_sets_only_the_named_var() {
        let vars = arm_like();
        let base = FlowContext(vec![0, 5]);
        // TMode -> 1, itmode untouched; an unknown var leaves it unchanged.
        let mut ctx = base.clone();
        vars.set_var(&mut ctx, "TMode", 1);
        assert_eq!(ctx, FlowContext(vec![1, 5]));
        let mut ctx = base.clone();
        vars.set_var(&mut ctx, "nope", 1);
        assert_eq!(ctx, base);
    }

    /// The setter is not a bit-setter: a flowing committed var wider than one
    /// bit carries its whole value, and two writes in sequence both land.
    #[test]
    fn set_var_sets_the_whole_value() {
        let vars = arm_like();
        let mut ctx = FlowContext(vec![0, 5]);
        vars.set_var(&mut ctx, "itmode", 9);
        assert_eq!(ctx, FlowContext(vec![0, 9]));
        vars.set_var(&mut ctx, "TMode", 1);
        assert_eq!(ctx, FlowContext(vec![1, 9]));
    }

    /// A context built for another `FlowVars` (an empty one, say) must not
    /// index out of bounds.
    #[test]
    fn a_context_shorter_than_the_var_list_does_not_index_out_of_bounds() {
        let vars = arm_like();
        let foreign = FlowContext::default();
        assert_eq!(vars.value_of(&foreign, "TMode"), None);
        let mut ctx = foreign.clone();
        vars.set_var(&mut ctx, "TMode", 1);
        assert_eq!(ctx, foreign);
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
        let flow = FlowVars::of(strider_target::SleighArch::arm().decode_tables());
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
            vars: vec!["itmode".into(), "cond_mask".into()].leak(),
        };
        for name in vars.vars {
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

    /// ARM declares four flow vars at bit `(0,0)` (`TMode`, `T`,
    /// `LowBitCodeMode`, `ISA_MODE`). A Thumb-to-ARM target sets only `TMode`
    /// in the carried context, and `pin_at` may write the aliases after it;
    /// the carried context must still name ARM for the repair.
    #[test]
    fn a_carried_context_survives_pin_at_still_naming_the_arm_mode() {
        use rsleigh::mem_readers::BufMemReader;
        use strider_target::SleighArch;
        let arch = SleighArch::arm();
        let reader = BufMemReader::new(vec![0u8; 0x100], 0x1000);
        let mut sleigh = arch.sleigh(reader).expect("create Sleigh");
        let flow = FlowVars::of(arch.decode_tables());
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
        let mut carried = function_mode.clone();
        flow.set_var(&mut carried, "TMode", 0);

        // The target's live context is ARM-default (not yet Thumb-painted): all
        // four bit-(0,0) aliases read 0 there, so pinning the carried context
        // sees the three aliases DIFFER (0 vs the Thumb 1) and writes them.
        sleigh
            .set_context_at(0x2000, "TMode", 0)
            .expect("arm-default target");
        flow.pin_at(&mut sleigh, 0x2000, &carried).expect("pin_at");
        // `RegionBuilder::hold_isa_mode` repairs a flipped mode on the next
        // `lift_one`; end to end in
        // `strider-orchestrator/tests/isa_mode/isa_mode_resolved_branch.rs`.
        assert_eq!(
            flow.value_of(&carried, "TMode"),
            Some(0),
            "the carried context still names the ARM mode the repair re-imposes",
        );
    }
}
