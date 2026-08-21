//! Decode-mode handling, GHIDRA-style: the ISA mode is a per-address fact.
//!
//! A sla marks some context variables as *flowing*: the ISA mode (ARM `TMode`,
//! ppc `vle`) plus per-instruction decode internals (condition eval, IT-block
//! state, register-list iteration) Sleigh flows along straight-line code.
//!
//! Like GHIDRA's `ContextDatabase`, the context is a value committed per
//! address. Reset every flowing var at a cold entry (undo a prior function's
//! leak on a reused engine), then before decoding a region restore every var to
//! the value that flowed to it (its *captured* context), so a region Sleigh
//! never straight-line-flowed to (a strider-resolved indirect target, a backward
//! edge) still decodes in it. An interworking resolved branch bakes its own
//! ISA-mode bit into the captured context it hands the target (see
//! [`FlowVars::with_mode_bit`]).

/// The context vars the sla marks as flowing. Constant per sla, so the lift
/// engine reads it once and lends it to every [`Builder`](super::Builder).
#[derive(Default)]
pub struct FlowVars {
    vars: Vec<String>,
}

/// Shared empty set, so a builder with no flowing context borrows rather than
/// allocates.
pub(crate) static NO_FLOW_VARS: FlowVars = FlowVars { vars: Vec::new() };

/// The ARM flow vars overlapping `condit`'s `(5,13)` field (`ARM.sinc`), which
/// share bits with each other as well.
const CONDIT_GROUP: [&str; 6] = [
    "itmode",
    "cond_base",
    "cond_full",
    "cond_true",
    "cond_shft",
    "cond_mask",
];

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

    /// Pin every flow var at `addr` back to `defaults`, undoing any commit that
    /// forward-held into this cold entry from a prior function on a reused
    /// engine.  Only leaked vars are re-pinned, so a clean entry costs no
    /// parse-cache invalidation.
    pub fn reset_at<R: rsleigh::MemReader>(
        &self,
        sleigh: &mut rsleigh::Sleigh<R>,
        addr: u64,
        defaults: &FlowContext,
    ) -> crate::Result<()> {
        self.pin_at(sleigh, addr, defaults)?;
        Ok(())
    }

    /// Restore `addr`'s context to `want`, the context captured for the edge
    /// reaching this region, undoing a sibling region's forward-hold clobber
    /// since. `isa_var` names the authoritative ISA-mode var, rewritten last
    /// and unconditionally.
    pub fn restore_at<R: rsleigh::MemReader>(
        &self,
        sleigh: &mut rsleigh::Sleigh<R>,
        addr: u64,
        want: &FlowContext,
        isa_var: Option<&str>,
    ) -> crate::Result<()> {
        if !self.pin_at(sleigh, addr, want)? {
            return Ok(());
        }
        // Several sla context vars can alias the ISA-mode bit (ARM `TMode`, `T`,
        // `LowBitCodeMode`, `ISA_MODE` all at bit (0,0)); `pin_at`'s diff may
        // write an alias last and flip the intended mode.  Read the bit back and
        // re-impose only when it actually landed wrong: every write costs a full
        // parse-cache flush, a read costs none.
        let Some((var, value)) = isa_var.and_then(|v| self.value_of(want, v).map(|x| (v, x)))
        else {
            return Ok(());
        };
        if sleigh.get_context_at(addr, var)? != value {
            sleigh.set_context_at(addr, var, value)?;
        }
        Ok(())
    }

    /// `ctx`'s value for the flow var named `var`, or `None` if not a flow var
    /// or `ctx` is not one of ours (a default-constructed `function_mode`).
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

    /// Set only the vars where `addr`'s current context disagrees with `want`,
    /// returning whether anything was written.  A `set_context_at` flushes
    /// Sleigh's whole parse cache, so an all-agreeing context must cost none.
    ///
    /// Writes are FLOWING, so anything painted here holds until the next explicit
    /// change point. ARM's IT-block vars (`itmode`, `cond_mask`, `cond_shft`, ...)
    /// are in the flow set only because `condit`'s `(5,13)` field overlaps them;
    /// Sleigh commits `condit` `noflow`, over `[addr, addr + 1)` alone, so a
    /// snapshot at `addr` reads back what it captured and they never enter the
    /// diff.  Two of them in one diff would clobber each other's bits, so the
    /// `debug_assert` surfaces a spec change instead of a corrupted context.
    fn pin_at<R: rsleigh::MemReader>(
        &self,
        sleigh: &mut rsleigh::Sleigh<R>,
        addr: u64,
        want: &FlowContext,
    ) -> crate::Result<bool> {
        let current = self.snapshot(sleigh, addr);
        debug_assert!(
            self.diff(&current, want)
                .filter(|(name, _)| CONDIT_GROUP.contains(name))
                .count()
                <= 1,
            "diff at {addr:#x} writes two condit-group vars: {:?}",
            self.diff(&current, want).collect::<Vec<_>>(),
        );
        let mut wrote = false;
        for (name, value) in self.diff(&current, want) {
            sleigh.set_context_at(addr, name, value)?;
            wrote = true;
        }
        Ok(wrote)
    }

    /// `base` with the flow var named `var` set to `bit`, used to bake a
    /// resolved interworking target's ISA mode into the context it decodes in.
    /// Returns a clone of `base` unchanged when `var` is not a flow var of this
    /// sla, or `base` is not one of our contexts.
    #[must_use]
    pub fn with_mode_bit(&self, base: &FlowContext, var: &str, bit: bool) -> FlowContext {
        let mut ctx = base.clone();
        if let Some(i) = self.vars.iter().position(|name| name == var)
            && let Some(slot) = ctx.0.get_mut(i)
        {
            *slot = u32::from(bit);
        }
        ctx
    }

    /// Overwrite `ctx`'s flow var `var` with the value committed at `addr`.
    /// Pins the ISA mode from the parent region onto a snapshot whose `var`
    /// value may be the pspec default: a target the mode has not
    /// straight-line-flowed to yet (a forward branch).
    ///
    /// No-op when `var` is not a flow var of this sla, `ctx` is not one of ours
    /// (a default-constructed `function_mode`), or the read fails.
    pub fn take_var_at<R: rsleigh::MemReader>(
        &self,
        ctx: &mut FlowContext,
        sleigh: &rsleigh::Sleigh<R>,
        addr: u64,
        var: &str,
    ) {
        let Some(i) = self.vars.iter().position(|name| name == var) else {
            return;
        };
        if i >= ctx.0.len() {
            return;
        }
        if let Ok(value) = get_context_at(sleigh, addr, var) {
            ctx.0[i] = value;
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

/// The value of each flow var at a program point, positionally aligned with the
/// [`FlowVars`] that produced it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct FlowContext(Vec<u32>);

#[cfg(test)]
mod tests {
    use super::*;

    fn arm_like() -> FlowVars {
        FlowVars {
            vars: vec!["TMode".into(), "itmode".into()],
        }
    }

    #[test]
    fn reset_diff_reports_all_disagreeing_vars() {
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

    /// `Builder::with_flow_vars` and `with_function_mode` are independent
    /// setters and `function_mode` defaults to an empty `FlowContext`, so a
    /// `FlowVars` can be handed a context it did not produce.
    #[test]
    fn a_context_shorter_than_the_var_list_does_not_index_out_of_bounds() {
        let vars = arm_like();
        let foreign = FlowContext::default();
        assert_eq!(vars.value_of(&foreign, "TMode"), None);
        assert_eq!(vars.with_mode_bit(&foreign, "TMode", true), foreign);

        let (sleigh, _reads) = counting_arm_sleigh();
        let mut short = foreign.clone();
        vars.take_var_at(&mut short, &sleigh, 0x1000, "TMode");
        assert_eq!(short, foreign);
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
        let sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
        (sleigh, reads)
    }

    /// `set_context_at` flushes Sleigh's whole parse cache
    /// (`bindings.cpp`'s `invalidateDisassembly`), so re-imposing the ISA-mode
    /// var when nothing was written costs a re-decode of every region.
    #[test]
    fn restore_at_over_a_matching_context_does_not_flush_the_parse_cache() {
        let (mut sleigh, reads) = counting_arm_sleigh();
        let flow = FlowVars::discover(&sleigh).expect("discover flow vars");
        let want = flow.snapshot(&sleigh, 0x1000);

        sleigh.lift_one(0x1000).expect("decode once");
        let before = reads.get();
        sleigh.lift_one(0x1000).expect("re-lift");
        let cached_lift = reads.get() - before;

        flow.restore_at(&mut sleigh, 0x1000, &want, Some("TMode"))
            .expect("restore_at");
        let before = reads.get();
        sleigh.lift_one(0x1000).expect("lift after restore");
        assert_eq!(
            reads.get() - before,
            cached_lift,
            "restoring a context that already matches re-decoded the instruction",
        );
    }

    /// Two `condit`-group vars in one diff clobber each other's bits, and
    /// nothing repairs the result the way `restore_at` repairs the ISA-mode bit.
    #[test]
    #[should_panic(expected = "condit")]
    fn a_diff_touching_two_condit_group_vars_is_rejected() {
        let (mut sleigh, _reads) = counting_arm_sleigh();
        let vars = FlowVars {
            vars: vec!["itmode".into(), "cond_mask".into()],
        };
        for name in &vars.vars {
            sleigh.set_context_at(0x1000, name, 0).expect("clear");
        }
        let _ = vars.pin_at(&mut sleigh, 0x1000, &FlowContext(vec![1, 1]));
    }

    /// The real ARM sla declares four flow vars at the same bit `(0,0)`:
    /// `TMode`, `T`, `LowBitCodeMode`, `ISA_MODE`. A resolved interworking target
    /// that switches a Thumb function to ARM sets only `TMode` to 0 in the
    /// carried context; the three aliases stay at the Thumb value, and
    /// `restore_at`'s diff writes them last, clobbering the intended mode. The
    /// ISA-mode var must be re-imposed last so the target decodes as ARM.
    #[test]
    fn restore_at_reimposes_isa_mode_var_over_its_sla_aliases() {
        use rsleigh::mem_readers::BufMemReader;
        use strider_target::SleighArch;
        let arch = SleighArch::arm();
        let reader = BufMemReader::new(vec![0u8; 0x100], 0x1000);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
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
        // four bit-(0,0) aliases read 0 there, so restoring the carried context
        // sees the three aliases DIFFER (0 vs the Thumb 1) and writes them.
        sleigh
            .set_context_at(0x2000, "TMode", 0)
            .expect("arm-default target");
        flow.restore_at(&mut sleigh, 0x2000, &carried, Some("TMode"))
            .expect("restore_at");
        assert_eq!(
            sleigh.get_context_at(0x2000, "TMode").expect("read TMode"),
            0,
            "resolved ARM target must decode as ARM; the sla aliases must not clobber TMode",
        );
    }
}
