use crate::calling_convention::regs_to_vns;

mod common;
#[cfg(test)]
mod tests;

/// Per-user-op ABI covering the ISA-fixed effects Sleigh's pcode does NOT
/// encode, carrying each register list as `L`.  Sleigh emits
/// `CALLOTHER(user_op_id, args)` with an optional `output`; this fills in the
/// implicit channel around it.
///
/// The two instantiations are [`CallOtherAbi`], which names registers as
/// static Sleigh strings, and [`BuiltCallOtherAbi`], the same footprint with
/// those names resolved to varnodes.  [`CallOtherAbi::build`] is that
/// substitution and nothing else.
///
/// `Copy` only when `L` is, which keeps [`CallOtherAbi`] usable in the `const`
/// table rows while the `Vec`-carrying [`BuiltCallOtherAbi`] stays
/// move-or-clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpAbi<L> {
    /// Read beyond Sleigh's pcode-explicit `inputs[1..]`.  Named form: exact
    /// Sleigh register names, case-sensitive.
    pub implicit_reads: L,

    /// Written or scratch-clobbered beyond Sleigh's pcode-explicit `output`.
    pub implicit_writes: L,

    /// Whether the op advances the IR's memory edge: `false` for pure compute
    /// (rdtsc, NEON/SVE), `true` for anything touching memory (atomics,
    /// barriers, port I/O, syscalls, kernel entries).
    pub clobbers_memory: bool,

    /// `true` when control does not pass this op: a `BUG_ON`-class trap,
    /// `sysret`, or a modeled call known never to return.  A no-return op
    /// still carries its full register / memory footprint; a bare trap is the
    /// empty-footprint case.
    pub no_return: bool,
}

/// A user-op ABI as static register-name slices.
pub type CallOtherAbi = OpAbi<&'static [&'static str]>;

/// Vn-resolved [`CallOtherAbi`].
pub type BuiltCallOtherAbi = OpAbi<Vec<rsleigh::Vn>>;

impl CallOtherAbi {
    /// Resolves the name-based footprint against `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Short-circuits on the first name in `implicit_reads` or
    /// `implicit_writes` that does not resolve.
    pub fn build(&self, sleigh_regs: &rsleigh::SleighRegs) -> crate::Result<BuiltCallOtherAbi> {
        Ok(BuiltCallOtherAbi {
            implicit_reads: regs_to_vns(sleigh_regs, self.implicit_reads)?,
            implicit_writes: regs_to_vns(sleigh_regs, self.implicit_writes)?,
            clobbers_memory: self.clobbers_memory,
            no_return: self.no_return,
        })
    }
}

/// How a user-op name is lifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOtherClass {
    /// No IR node emitted, control and memory unchanged, and any
    /// pcode-explicit output ignored.
    NoOp,

    /// Register footprint, memory effect, and whether control returns, beyond
    /// what Sleigh's pcode encodes.
    Call(CallOtherAbi),
}

impl CallOtherClass {
    /// Bare trap: an empty-footprint, non-returning `Call` (`BUG_ON`,
    /// `sysret`, `UndefinedInstructionException`).
    pub const NO_RETURN: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: true,
    });

    /// No implicit register traffic and no memory effect.
    pub const PURE: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    });

    /// No implicit register traffic, but memory is conservatively clobbered.
    pub const MEM_CLOBBER: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: true,
        no_return: false,
    });

    #[must_use]
    pub fn is_no_return(&self) -> bool {
        matches!(self, CallOtherClass::Call(abi) if abi.no_return)
    }
}

/// One caller answer for a user-op name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallOtherOverride {
    /// A class shaped like a table row, its register names resolved at use.
    Class(CallOtherClass),

    /// A footprint the caller resolved against a `SleighRegs` itself.  Names
    /// outside this crate cannot be `&'static str`, so a caller-supplied
    /// footprint arrives already resolved.
    Built(BuiltCallOtherAbi),
}

impl From<CallOtherClass> for CallOtherOverride {
    fn from(class: CallOtherClass) -> Self {
        Self::Class(class)
    }
}

/// Caller-supplied classifications, consulted before the built-in tables.
///
/// Per-analysis, the way a calling convention is: it states what one binary's
/// build of the op does. The tables hold what is true of the ARCHITECTURE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallOtherOverrides(rustc_hash::FxHashMap<String, CallOtherOverride>);

/// No overrides, for a borrower with none to lend.
pub static NO_OVERRIDES: CallOtherOverrides = CallOtherOverrides(
    rustc_hash::FxHashMap::with_hasher(rustc_hash::FxBuildHasher),
);

impl CallOtherOverrides {
    /// # Errors
    /// If a name appears twice. The first wins, so a duplicate would silently
    /// drop the later correction.
    pub fn new(entries: Vec<(String, CallOtherOverride)>) -> Result<Self, String> {
        let mut map = rustc_hash::FxHashMap::with_capacity_and_hasher(
            entries.len(),
            rustc_hash::FxBuildHasher,
        );
        for (name, over) in entries {
            if map.contains_key(&name) {
                return Err(format!("duplicate call-other override for {name:?}"));
            }
            map.insert(name, over);
        }
        Ok(Self(map))
    }

    /// Keyed: `get` runs once per `CALLOTHER` occurrence from four lifter
    /// stages, and the set is sized by the caller's `call_other_abis`, which a
    /// sweep can seed in bulk. Both factors are the caller's, so neither is a
    /// handful by construction.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<CallOtherLookup<'_>> {
        self.0.get(name).map(|o| match o {
            CallOtherOverride::Class(c) => CallOtherLookup::Class(*c),
            CallOtherOverride::Built(abi) => CallOtherLookup::Built(abi),
        })
    }
}

/// One classification, borrowing the footprint when a caller supplied one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOtherLookup<'a> {
    Class(CallOtherClass),
    Built(&'a BuiltCallOtherAbi),
}

impl<'a> CallOtherLookup<'a> {
    #[must_use]
    pub fn is_no_return(self) -> bool {
        match self {
            Self::Class(c) => c.is_no_return(),
            Self::Built(abi) => abi.no_return,
        }
    }

    /// The Vn-resolved footprint, `None` for [`CallOtherClass::NoOp`].
    /// Borrowed when the caller pre-resolved it, so only the name-based
    /// classes pay for resolution.
    ///
    /// # Errors
    ///
    /// Short-circuits on the first name a class carries that `sleigh_regs`
    /// does not resolve.
    pub fn built(
        self,
        sleigh_regs: &rsleigh::SleighRegs,
    ) -> crate::Result<Option<std::borrow::Cow<'a, BuiltCallOtherAbi>>> {
        Ok(match self {
            Self::Class(CallOtherClass::NoOp) => None,
            Self::Class(CallOtherClass::Call(abi)) => {
                Some(std::borrow::Cow::Owned(abi.build(sleigh_regs)?))
            }
            Self::Built(abi) => Some(std::borrow::Cow::Borrowed(abi)),
        })
    }
}

/// [`classify`], with `overrides` winning over every built-in table.
#[must_use]
pub fn classify_with<'a>(
    overrides: &'a CallOtherOverrides,
    preset: crate::ArchPreset,
    name: &str,
) -> Option<CallOtherLookup<'a>> {
    overrides
        .get(name)
        .or_else(|| classify(preset, name).map(CallOtherLookup::Class))
}

/// Classifies a user-op name for the given architecture: the family's exact
/// rows first, then the arch-independent rows, then the family's prefix
/// families and its fallback.
///
/// A missing entry is intentional: entries are added on demand as real
/// binaries surface them.
pub fn classify(preset: crate::ArchPreset, name: &str) -> Option<CallOtherClass> {
    let family = preset.family();
    family
        .call_other
        .iter()
        .find(|row| row.matches(preset, name))
        .map(|row| row.class)
        .or_else(|| {
            common::ROWS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
        })
        .or_else(|| {
            family
                .call_other_prefixes
                .iter()
                .find(|p| p.prefixes.iter().any(|s| name.starts_with(s)))
                .map(|p| p.class)
        })
        .or_else(|| family.call_other_fallback.and_then(|f| f(name)))
}

/// A PowerPC trap that fires on every relation, i.e. `BUG()`.
///
/// `tw` / `twi` / `td` / `tdi` carry a 5-bit TO mask of the comparisons that
/// trap, and the sla passes it as the op's first operand. Signed `LT | GT | EQ`
/// and unsigned `LTU | GTU | EQ` are each a trichotomy, so a mask covering
/// either one always fires and the fall-through is dead. Seven of the 32 masks
/// qualify, not just all-bits-set; the rest are conditional checks whose
/// fall-through is live, which is why the table classes the family as
/// `MEM_CLOBBER` rather than no-return.
#[must_use]
pub fn trap_is_unconditional(name: &str, to_mask: Option<u128>) -> bool {
    const SIGNED_TRICHOTOMY: u128 = 0x1c;
    const UNSIGNED_TRICHOTOMY: u128 = 0x07;
    matches!(
        name,
        "trapWord" | "trapDoubleWord" | "trapDoubleWordImmediate"
    ) && to_mask.is_some_and(|to| {
        to & SIGNED_TRICHOTOMY == SIGNED_TRICHOTOMY
            || to & UNSIGNED_TRICHOTOMY == UNSIGNED_TRICHOTOMY
    })
}

/// Whether `name` is the ARM op through which a register-form `msr` to the
/// CPSR control field yields the mode it writes: `msrRegisterMode(new,
/// current)` is `new`, or `current` under the claim that the mode such a write
/// names is the one in force. The lifter lowers it to one of its arguments, so
/// it has no ABI row, unless a caller override names it, which then classifies
/// it as any user op and neither claim value applies.
#[must_use]
pub fn is_msr_register_mode(preset: crate::ArchPreset, name: &str) -> bool {
    preset.family().msr_mode_op == Some(name)
}

/// Whether `name` is the op through which a conversion reads the rounding
/// mode it rounds by: x86 `mxcsrRoundingControl(rc)` (MXCSR.RC) and
/// `x87RoundingControl(rc)` (the x87 control word's RC), PowerPC
/// `fpscrRoundingControl(rn)` (FPSCR.RN), ARM `fpscrRoundingControl(rmode)`
/// (FPSCR.RMode) and MIPS `fcsrRoundingControl(rm)` (FCSR.RM) are their
/// input, the live mode, or 0 under the claim that the mode is round to
/// nearest, which 0 is in each encoding. AArch64 `fpcrRoundingControl()`
/// takes no input and is FPCR ([`rounding_mode_register`]), or 0 under the
/// claim. The lifter lowers it to one of the two, so it has no ABI row,
/// unless a caller override names it, which then classifies it as any user
/// op and neither claim value applies.
#[must_use]
pub fn is_rounding_mode_control(preset: crate::ArchPreset, name: &str) -> bool {
    preset.family().rounding_ops.contains(&name)
}

/// The register a rounding-mode op with no input is, read only when the
/// claim is off: AArch64 `fpcrRoundingControl()` is `fpcr`, whose reads the
/// pspec marks volatile, so a lift under the claim reads none.
#[must_use]
pub fn rounding_mode_register(preset: crate::ArchPreset, name: &str) -> Option<&'static str> {
    preset
        .family()
        .rounding_register
        .and_then(|(op, register)| (op == name).then_some(register))
}

pub(crate) const PURE: CallOtherClass = CallOtherClass::PURE;
pub(crate) const MEM_CLOBBER: CallOtherClass = CallOtherClass::MEM_CLOBBER;

/// One exact-name row of a family's table.
pub(crate) struct Row {
    /// A slice so one row covers related ops: `mwait` + `mwaitx`, or the
    /// SMCCC pair `CallHyperVisor` + `CallSecureMonitor`.
    pub(crate) names: &'static [&'static str],
    /// `None`: every preset of the family; `Some`: a width split within it.
    pub(crate) only: Option<&'static [crate::ArchPreset]>,
    pub(crate) class: CallOtherClass,
}

impl Row {
    /// A one-name row for every preset of the family.
    pub(crate) const fn new(names: &'static [&'static str], class: CallOtherClass) -> Self {
        Self {
            names,
            only: None,
            class,
        }
    }

    fn matches(&self, preset: crate::ArchPreset, name: &str) -> bool {
        self.only.is_none_or(|only| only.contains(&preset)) && self.names.contains(&name)
    }
}

/// Arch-scoped prefix families, each covering dozens of GHIDRA user-ops (one
/// per named system register, cache-maintenance target, or SIMD op).  Scoped
/// to the arches that define the family so a same-named user-op elsewhere
/// cannot inherit the classification; anything outside a known family returns
/// `None`.
///
/// **Homogeneity was verified against the GHIDRA `.sinc` `define pcodeop`
/// lists**, since a substring rule is only sound if EVERY member shares the
/// classification; each family file records the verification beside its list.
pub(crate) struct Prefix {
    pub(crate) prefixes: &'static [&'static str],
    pub(crate) class: CallOtherClass,
}

#[cfg(test)]
mod trap_tests {
    /// TO names the comparisons that trap. Bits are LT|GT|EQ|LTU|GTU from
    /// 0x10 down, so a mask covering either trichotomy always fires.
    #[test]
    fn a_mask_covering_either_trichotomy_is_unconditional() {
        use super::trap_is_unconditional as t;
        let unconditional: Vec<u128> = (0..32).filter(|&to| t("trapWord", Some(to))).collect();
        assert_eq!(unconditional, [0x07, 0x0f, 0x17, 0x1c, 0x1d, 0x1e, 0x1f]);
        // 0x0c is `twgei` (GT|EQ), a live bounds check; 0x1c adds LT and
        // completes signed LT|GT|EQ, which cannot all be false.
        assert!(!t("trapWord", Some(0x0c)));
        assert!(t("trapWord", Some(0x1c)));
        for name in ["trapWord", "trapDoubleWord", "trapDoubleWordImmediate"] {
            assert!(t(name, Some(0x1f)));
        }
        // A non-constant operand cannot be proven unconditional.
        assert!(!t("trapWord", None));
        // Not every op named like a trap is one of these.
        assert!(!t("trap", Some(0x1f)));
        assert!(!t("syscall", Some(0x1f)));
    }
}
