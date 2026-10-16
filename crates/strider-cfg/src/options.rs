use rustc_hash::{FxHashMap, FxHashSet};

use crate::indirect_resolver::ResolvedTargets;
use crate::types::PcodeInsnAddr;

/// What `Default` lends: it hands out a `&'a`, which it cannot borrow from a
/// temporary.
static NO_KNOWN_TARGETS: FxHashMap<PcodeInsnAddr, ResolvedTargets> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);
static NO_NO_RETURN_TARGETS: FxHashSet<u64> = FxHashSet::with_hasher(rustc_hash::FxBuildHasher);

/// The extent of the function being built, which decides when a branch leaves
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionBound {
    /// `[start, start + n)`: a branch outside it is a tail call.
    Size(std::num::NonZeroU64),
    /// No extent: a branch below the start is a tail call unless
    /// `code_before_start`.
    Open { code_before_start: bool },
}

impl Default for FunctionBound {
    fn default() -> Self {
        Self::Open {
            code_before_start: false,
        }
    }
}

impl FunctionBound {
    /// [`Self::Size`].
    ///
    /// # Panics
    ///
    /// If `n` is 0.
    #[must_use]
    pub const fn size(n: u64) -> Self {
        match std::num::NonZeroU64::new(n) {
            Some(n) => Self::Size(n),
            None => panic!("a function size bound must be > 0"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CfgOptions<'a> {
    /// How far the function extends; a branch leaving it is a tail call.
    pub bound: FunctionBound,
    /// A `BranchIndirect` listed here seats its cached terminator directly;
    /// every other site defers via `UnresolvedIndirectBranch`.  Read through
    /// [`CfgOptions::seated`], never indexed directly.
    ///
    /// Borrowed: a caller seeds a binary-wide table once and analyses one
    /// function per call, so owning it is the whole table per call.
    pub known_targets: &'a FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// Direct-call targets that never return (`exit`, `abort`); a call to one
    /// ends its region `NoReturn`. Borrowed, as `known_targets` is.
    pub no_return_targets: &'a FxHashSet<u64>,
    /// Caller classifications for user-op names, consulted before the built-in
    /// tables.  One override governs both the region terminator and the
    /// emitted node.
    pub call_other_overrides: strider_target::call_other_abi::CallOtherOverrides,
    /// Bytes the image marks as data (ELF `$d` mapping symbols), which the
    /// builder decodes only for the instruction at the start address; see
    /// [`crate::Reason::UndecodableTarget`].
    pub data_ranges: DataRanges,
}

/// Sorted, merged, half-open address ranges. Cloning shares one table.
#[derive(Clone, Default, Debug)]
pub struct DataRanges(std::sync::Arc<[std::ops::Range<u64>]>);

impl DataRanges {
    /// Any order, overlapping or adjacent; empty ranges are dropped.
    pub fn new(ranges: impl IntoIterator<Item = std::ops::Range<u64>>) -> Self {
        let mut sorted: Vec<std::ops::Range<u64>> =
            ranges.into_iter().filter(|r| r.start < r.end).collect();
        sorted.sort_unstable_by_key(|r| r.start);
        let mut merged: Vec<std::ops::Range<u64>> = Vec::with_capacity(sorted.len());
        for r in sorted {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        Self(merged.into())
    }

    pub fn ranges(&self) -> &[std::ops::Range<u64>] {
        &self.0
    }

    /// Whether any byte of `[start, end)` is data.
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        // The first range ending past `start` is the only candidate.
        let i = self.0.partition_point(|r| r.end <= start);
        self.0.get(i).is_some_and(|r| r.start < end)
    }
}

impl Default for CfgOptions<'_> {
    fn default() -> Self {
        Self {
            bound: FunctionBound::default(),
            known_targets: &NO_KNOWN_TARGETS,
            no_return_targets: &NO_NO_RETURN_TARGETS,
            call_other_overrides: Default::default(),
            data_ranges: DataRanges::default(),
        }
    }
}

impl CfgOptions<'_> {
    /// [`seated_in`] over `known_targets`.
    pub fn seated(&self, addr: PcodeInsnAddr) -> Option<&ResolvedTargets> {
        seated_in(self.known_targets, addr)
    }
}

/// The entry for `addr` in `map`: its exact key, else the key of its
/// instruction's p-code index 0.
///
/// An entry at p-code index 0 seats any `BranchIndirect` in that machine
/// instruction, so a caller holding only a machine address can spell a key;
/// a Python one only ever holds that, the bindings flattening every
/// reported site to its machine address. An exact key wins: it is the only
/// way to tell two `BranchIndirect`s in one instruction apart.
pub fn seated_in(
    map: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    addr: PcodeInsnAddr,
) -> Option<&ResolvedTargets> {
    map.get(&addr).or_else(|| {
        let start = PcodeInsnAddr::at_machine_start(addr.machine_addr.addr);
        (start != addr)
            .then_some(start)
            .and_then(|start| map.get(&start))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResolvedTarget;

    fn seated_over(entries: &[(PcodeInsnAddr, u64)]) -> FxHashMap<PcodeInsnAddr, ResolvedTargets> {
        entries
            .iter()
            .map(|&(k, target)| {
                (
                    k,
                    ResolvedTargets::Single(ResolvedTarget::new(target, None)),
                )
            })
            .collect()
    }

    fn opts_over(known: &FxHashMap<PcodeInsnAddr, ResolvedTargets>) -> CfgOptions<'_> {
        CfgOptions {
            known_targets: known,
            ..CfgOptions::default()
        }
    }

    fn target_of(seated: Option<&ResolvedTargets>) -> Option<u64> {
        match seated? {
            ResolvedTargets::Single(t) => Some(t.addr),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "must be > 0")]
    fn a_zero_size_bound_panics() {
        let _ = FunctionBound::size(0);
    }

    /// Options built over a caller's table hold a borrow of it, so neither
    /// building nor cloning them copies it.
    #[test]
    fn options_borrow_the_callers_table() {
        let known = seated_over(&[(PcodeInsnAddr::at_machine_start(0x1000), 0x2000)]);
        let opts = opts_over(&known);
        assert!(std::ptr::eq(opts.known_targets, &known));
        assert!(std::ptr::eq(opts.clone().known_targets, &known));
    }

    #[test]
    fn default_options_share_one_empty_table() {
        let (a, b) = (CfgOptions::default(), CfgOptions::default());
        assert!(a.known_targets.is_empty());
        assert!(std::ptr::eq(a.known_targets, b.known_targets));
    }

    #[test]
    fn seated_in_falls_back_to_the_machine_start_key() {
        let known = seated_over(&[(PcodeInsnAddr::at_machine_start(0x1000), 0x2000)]);
        let mid = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        assert_eq!(target_of(seated_in(&known, mid)), Some(0x2000));
        assert_eq!(
            target_of(seated_in(&known, PcodeInsnAddr::at_machine_start(0x1004))),
            None
        );
    }

    #[test]
    fn seated_in_prefers_an_exact_key_over_the_machine_start_key() {
        let start = PcodeInsnAddr::at_machine_start(0x1000);
        let mid = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        let known = seated_over(&[(start, 0x2000), (mid, 0x3000)]);
        assert_eq!(target_of(seated_in(&known, mid)), Some(0x3000));
        assert_eq!(target_of(seated_in(&known, start)), Some(0x2000));
    }

    /// `seated` is `seated_in` over `known_targets`: an exact key, the
    /// machine-start key, and a sibling `BranchIndirect` of that instruction.
    #[test]
    fn seated_reads_known_targets_through_seated_in() {
        let start = PcodeInsnAddr::at_machine_start(0x1000);
        let exact = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        let sibling = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 5,
        };
        let known = seated_over(&[(start, 0x2000), (exact, 0x3000)]);
        let opts = opts_over(&known);
        for addr in [exact, start, sibling] {
            assert_eq!(
                target_of(opts.seated(addr)),
                target_of(seated_in(&known, addr)),
                "{addr:?}"
            );
        }
        assert_eq!(target_of(opts.seated(sibling)), Some(0x2000));
    }
}
