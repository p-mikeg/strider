use rustc_hash::FxHashMap;

use crate::indirect_resolver::ResolvedTargets;
use crate::types::PcodeInsnAddr;

#[derive(Clone, Default, Debug)]
pub struct CfgOptions {
    /// `Some(n)`: an unconditional branch to `>= start + n` is a tail call.
    /// `Some(0)` is coerced to unbounded by [`crate::Builder::for_arch`].
    pub fn_max_size: Option<u64>,
    /// When `false`, an unconditional branch *below* the function start is
    /// a tail call rather than an edge to follow.
    pub allow_code_before_start_addr: bool,
    /// A `BranchIndirect` listed here seats its cached terminator directly;
    /// every other site defers via `UnresolvedIndirectBranch`.  Read through
    /// [`CfgOptions::seated`], never indexed directly.
    pub known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// Caller classifications for user-op names, consulted before the built-in
    /// tables.  One override governs both the region terminator and the
    /// emitted node.
    pub call_other_overrides: strider_target::call_other_abi::CallOtherOverrides,
}

impl CfgOptions {
    /// The answer seated for `addr`, falling back to the machine address alone.
    ///
    /// An entry at p-code index 0 seats any `BranchIndirect` in that machine
    /// instruction, so a caller holding only a machine address (all
    /// `unresolved_indirect_branches` reports) can spell a key. An exact key
    /// wins: it is the only way to tell two `BranchIndirect`s in one
    /// instruction apart.
    pub fn seated(&self, addr: PcodeInsnAddr) -> Option<&ResolvedTargets> {
        self.known_targets.get(&addr).or_else(|| {
            let start = PcodeInsnAddr::at_machine_start(addr.machine_addr.addr);
            (start != addr)
                .then_some(start)
                .and_then(|start| self.known_targets.get(&start))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResolvedTarget;

    fn seated_over(entries: &[(PcodeInsnAddr, u64)]) -> CfgOptions {
        CfgOptions {
            known_targets: entries
                .iter()
                .map(|&(k, target)| {
                    (
                        k,
                        ResolvedTargets::Single(ResolvedTarget::new(target, None)),
                    )
                })
                .collect(),
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
    fn seated_falls_back_to_the_machine_start_key() {
        let opts = seated_over(&[(PcodeInsnAddr::at_machine_start(0x1000), 0x2000)]);
        let mid = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        assert_eq!(target_of(opts.seated(mid)), Some(0x2000));
        assert_eq!(
            target_of(opts.seated(PcodeInsnAddr::at_machine_start(0x1004))),
            None
        );
    }

    #[test]
    fn seated_prefers_an_exact_key_over_the_machine_start_key() {
        let start = PcodeInsnAddr::at_machine_start(0x1000);
        let mid = PcodeInsnAddr {
            machine_addr: 0x1000.into(),
            insn_index: 3,
        };
        let opts = seated_over(&[(start, 0x2000), (mid, 0x3000)]);
        assert_eq!(target_of(opts.seated(mid)), Some(0x3000));
        assert_eq!(target_of(opts.seated(start)), Some(0x2000));
    }
}
