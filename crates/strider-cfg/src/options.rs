use rustc_hash::{FxHashMap, FxHashSet};

use crate::indirect_resolver::ResolvedTargets;
use crate::types::PcodeInsnAddr;

// Statics, so `Default` can lend a `&'a`.

/// An empty target table, for a `CfgOptions` literal naming every field.
pub static NO_TARGETS: FxHashMap<PcodeInsnAddr, ResolvedTargets> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);
static NO_NO_RETURN_TARGETS: FxHashSet<u64> = FxHashSet::with_hasher(rustc_hash::FxBuildHasher);
/// No local entries, for a `CfgOptions` literal naming every field.
pub static NO_LOCAL_ENTRIES: LocalEntries = LocalEntries(Vec::new());

/// The extent of the function being built, which decides when a branch leaves
/// it, together with [`CfgOptions::fragment_ranges`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionBound {
    /// `[start, start + n)`: a branch outside it and every fragment is a tail
    /// call, and so is a sequential fall-through past its end, reported as
    /// [`crate::Reason::PastFunctionEnd`].
    Size(std::num::NonZeroU64),
    /// No extent: a branch below the start and outside every fragment is a
    /// tail call unless `code_before_start`.
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
    /// How far the function extends; a branch or fall-through leaving it and
    /// every [`Self::fragment_ranges`] range is a tail call.
    pub bound: FunctionBound,
    /// More of this function's code outside `bound`, such as GCC's
    /// `<name>.cold` partitions: a branch into one is an edge, and a
    /// fall-through past one's end is [`crate::Reason::PastFunctionEnd`]
    /// under either bound.
    pub fragment_ranges: DataRanges,
    /// Caller-seeded answers per `BranchIndirect`; an unlisted site defers.
    /// Read through [`CfgOptions::seated`].
    pub known_targets: &'a FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// A resolver's own answers, keyed by the `BranchIndirect`'s exact p-code
    /// address: one beats `known_targets` at that site, and
    /// [`ResolvedTargets::Withdrawn`] defers it.
    pub derived_targets: &'a FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// Direct-call targets that never return (`exit`, `abort`); a call to one
    /// ends its region `NoReturn`. Each is matched against the address the
    /// call names, an ISA bit masked off, through [`Self::local_entries`].
    pub no_return_targets: &'a FxHashSet<u64>,
    /// Where a call sharing a ppc64 ELFv2 callee's TOC enters it, so a
    /// `no_return_targets` or per-address convention entry keyed by the
    /// callee's global entry matches that call too.
    pub local_entries: &'a LocalEntries,
    /// Caller classifications for user-op names, ahead of the built-in tables.
    pub call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
    /// Bytes the image marks as data (ELF `$d`), never decoded except at the
    /// start address.
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

    /// [`Self::new`] over `(start, end)` pairs, refusing an empty range.
    ///
    /// # Errors
    ///
    /// The first pair ending at or below its start, described as option
    /// `what`'s.
    pub fn try_new(what: &str, ranges: Vec<(u64, u64)>) -> Result<Self, String> {
        if let Some((start, end)) = ranges.iter().find(|(start, end)| end <= start) {
            return Err(format!(
                "{what}: ({start:#x}, {end:#x}) is empty; a range runs start..end with end > start"
            ));
        }
        Ok(Self::new(ranges.into_iter().map(|(start, end)| start..end)))
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

/// `(local entry, global entry)` pairs, sorted: the ppc64 ELFv2 functions a
/// call sharing their TOC enters past their global entry, at the symbol plus
/// `strider_reader::elf::local_entry_offset`.
#[derive(Clone, Default, Debug)]
pub struct LocalEntries(Vec<(u64, u64)>);

impl LocalEntries {
    /// `(global, local)` pairs in any order; one whose two entries meet is
    /// dropped.
    pub fn new(entries: impl IntoIterator<Item = (u64, u64)>) -> Self {
        let mut by_local: Vec<(u64, u64)> = entries
            .into_iter()
            .filter(|(global, local)| global != local)
            .map(|(global, local)| (local, global))
            .collect();
        by_local.sort_unstable();
        by_local.dedup();
        Self(by_local)
    }

    /// `map`'s entry at `addr`, else at the lowest global entry whose local
    /// entry `addr` is.
    pub fn get<'m, V>(&self, map: &'m FxHashMap<u64, V>, addr: u64) -> Option<&'m V> {
        map.get(&addr)
            .or_else(|| self.globals_of(addr).find_map(|global| map.get(&global)))
    }

    /// Whether `set` holds `addr` or a global entry whose local entry `addr`
    /// is.
    pub fn contains(&self, set: &FxHashSet<u64>, addr: u64) -> bool {
        set.contains(&addr) || self.globals_of(addr).any(|global| set.contains(&global))
    }

    /// Ascending.
    fn globals_of(&self, local: u64) -> impl Iterator<Item = u64> + '_ {
        let from = self.0.partition_point(|&(l, _)| l < local);
        self.0[from..]
            .iter()
            .take_while(move |&&(l, _)| l == local)
            .map(|&(_, global)| global)
    }
}

impl Default for CfgOptions<'_> {
    fn default() -> Self {
        Self {
            bound: FunctionBound::default(),
            fragment_ranges: DataRanges::default(),
            known_targets: &NO_TARGETS,
            derived_targets: &NO_TARGETS,
            no_return_targets: &NO_NO_RETURN_TARGETS,
            local_entries: &NO_LOCAL_ENTRIES,
            call_other_overrides: &strider_target::call_other_abi::NO_OVERRIDES,
            data_ranges: DataRanges::default(),
        }
    }
}

impl CfgOptions<'_> {
    /// The answer the builder seats at `addr`: [`seated_from`] over
    /// `derived_targets` and `known_targets`.
    pub fn seated(&self, addr: PcodeInsnAddr) -> Option<&ResolvedTargets> {
        seated_from(self.derived_targets, self.known_targets, addr)
    }
}

/// The answer seated at `addr`: `derived` at its exact key, else
/// [`seated_in`] over `known`.
pub fn seated_from<'m>(
    derived: &'m FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    known: &'m FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    addr: PcodeInsnAddr,
) -> Option<&'m ResolvedTargets> {
    derived.get(&addr).or_else(|| seated_in(known, addr))
}

/// The entry for `addr` in `map`: its exact key, else the key of its
/// instruction's p-code index 0.
///
/// An index-0 key thus seats every `BranchIndirect` of its instruction.
pub fn seated_in(
    map: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    addr: PcodeInsnAddr,
) -> Option<&ResolvedTargets> {
    // At index 0 the fallback repeats the exact probe's miss.
    map.get(&addr)
        .or_else(|| map.get(&PcodeInsnAddr::at_machine_start(addr.machine())))
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

    /// An exact key answers for itself; a local entry answers through the
    /// lowest global entry the table holds, one level deep.
    #[test]
    fn local_entries_answer_through_the_lowest_global() {
        let entries = LocalEntries::new([
            (0x110, 0x118),
            (0x10c, 0x110),
            (0x114, 0x118),
            (0x200, 0x200),
        ]);
        let map: FxHashMap<u64, char> = [(0x110, 'a'), (0x114, 'b')].into_iter().collect();
        assert_eq!(entries.get(&map, 0x118), Some(&'a'));
        assert_eq!(entries.get(&map, 0x110), Some(&'a'));
        assert_eq!(entries.get(&map, 0x114), Some(&'b'));
        let own: FxHashMap<u64, char> = [(0x114, 'b'), (0x118, 'c')].into_iter().collect();
        assert_eq!(entries.get(&own, 0x118), Some(&'c'));
        let deep: FxHashMap<u64, char> = [(0x10c, 'd')].into_iter().collect();
        assert_eq!(entries.get(&deep, 0x110), Some(&'d'));
        assert_eq!(entries.get(&deep, 0x118), None);
        let set: FxHashSet<u64> = [0x114].into_iter().collect();
        assert!(entries.contains(&set, 0x118));
        assert!(!entries.contains(&set, 0x110));
    }

    #[test]
    fn try_new_refuses_an_empty_range() {
        let err = DataRanges::try_new("data_ranges", vec![(0x10, 0x18), (0x20, 0x20)]).unwrap_err();
        assert!(err.contains("(0x20, 0x20) is empty"), "{err}");
        let ok = DataRanges::try_new("data_ranges", vec![(0x10, 0x18)]).unwrap();
        assert_eq!(ok.ranges().len(), 1);
        assert_eq!((ok.ranges()[0].start, ok.ranges()[0].end), (0x10, 0x18));
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
        assert!(std::ptr::eq(a.known_targets, &NO_TARGETS));
        assert!(std::ptr::eq(a.derived_targets, &NO_TARGETS));
        assert!(std::ptr::eq(
            a.call_other_overrides,
            &strider_target::call_other_abi::NO_OVERRIDES
        ));
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

    /// A resolver's answer beats the caller's at its exact key, whichever key
    /// the caller spelled, and leaves a sibling `BranchIndirect` of that
    /// instruction the caller's machine-start seed.
    #[test]
    fn seated_prefers_a_derived_target_at_its_exact_key() {
        let start = PcodeInsnAddr::at_machine_start(0x1000);
        let exact = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        let sibling = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 5,
        };
        let derived = seated_over(&[(exact, 0x3000)]);
        for seed_key in [start, exact] {
            let known = seated_over(&[(seed_key, 0x2000)]);
            let opts = CfgOptions {
                known_targets: &known,
                derived_targets: &derived,
                ..CfgOptions::default()
            };
            assert_eq!(target_of(opts.seated(exact)), Some(0x3000), "{seed_key:?}");
            assert_eq!(
                target_of(opts.seated(sibling)),
                (seed_key == start).then_some(0x2000),
                "{seed_key:?}"
            );
        }
    }
}
