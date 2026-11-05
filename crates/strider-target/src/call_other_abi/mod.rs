use crate::calling_convention::regs_to_vns;

mod common;
#[cfg(test)]
mod tests;

/// The effects of a user-op beyond its p-code `CALLOTHER(id, args) -> output`,
/// with registers as names ([`CallOtherAbi`]) or varnodes
/// ([`BuiltCallOtherAbi`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpAbi<L> {
    /// Read beyond the p-code inputs.
    pub implicit_reads: L,

    /// Written beyond the p-code output.
    pub implicit_writes: L,

    /// Whether the op advances the memory chain.
    pub clobbers_memory: bool,

    /// Control does not pass this op.
    pub no_return: bool,
}

/// A user-op ABI as static register-name slices.
pub type CallOtherAbi = OpAbi<&'static [&'static str]>;

/// Vn-resolved [`CallOtherAbi`].
pub type BuiltCallOtherAbi = OpAbi<Vec<rsleigh::Vn>>;

impl CallOtherAbi {
    /// # Errors
    /// The first register name `sleigh_regs` does not resolve.
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
    /// No IR node; any p-code output is ignored.
    NoOp,

    Call(CallOtherAbi),
}

impl CallOtherClass {
    /// A bare trap.
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

    /// No implicit register traffic; clobbers memory.
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

    /// A footprint the caller resolved against a `SleighRegs` itself.
    Built(BuiltCallOtherAbi),
}

impl From<CallOtherClass> for CallOtherOverride {
    fn from(class: CallOtherClass) -> Self {
        Self::Class(class)
    }
}

/// Caller-supplied classifications, consulted before the built-in tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallOtherOverrides(rustc_hash::FxHashMap<String, CallOtherOverride>);

/// The empty override table.
pub static NO_OVERRIDES: CallOtherOverrides = CallOtherOverrides(
    rustc_hash::FxHashMap::with_hasher(rustc_hash::FxBuildHasher),
);

impl CallOtherOverrides {
    /// # Errors
    /// If a name appears twice.
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

    /// The resolved footprint, `None` for [`CallOtherClass::NoOp`].
    ///
    /// # Errors
    /// The first register name `sleigh_regs` does not resolve.
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

/// Classifies a user-op name: the family's exact rows, the arch-independent
/// rows, then the family's prefixes and fallback. `None` is a name no table
/// answers yet.
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

/// A PowerPC trap that fires on every relation (`BUG()`): its TO mask, the
/// op's first operand, covers signed `LT | GT | EQ` or unsigned
/// `LTU | GTU | EQ`.
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

/// Whether `name` is ARM `msrRegisterMode(new, current)`, which the lifter
/// lowers to `new`, or to `current` under `msr_register_keeps_mode`. A caller
/// override takes precedence.
#[must_use]
pub fn is_msr_register_mode(preset: crate::ArchPreset, name: &str) -> bool {
    preset.family().msr_mode_op == Some(name)
}

/// Whether `name` yields the rounding mode a conversion uses
/// (`mxcsrRoundingControl(rc)`, ...), which the lifter lowers to its input,
/// or to 0 (nearest) under `fp_rounding_mode_is_nearest`; an input-less one
/// reads [`rounding_mode_register`]. A caller override takes precedence.
#[must_use]
pub fn is_rounding_mode_control(preset: crate::ArchPreset, name: &str) -> bool {
    preset.family().rounding_ops.contains(&name)
}

/// The register an input-less rounding-mode op reads (AArch64 `fpcr`).
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

/// A family's name stems, sound only if every op the sla declares under them
/// shares the class.
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
