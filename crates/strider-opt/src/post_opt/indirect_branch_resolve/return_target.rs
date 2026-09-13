//! Whether a return instruction jumps back to the address the function was
//! entered with.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use crate::mem_analysis::{MemExpr, MemKind, decompose, store_value_byte_size};

/// Answers, for the address each Sleigh `RETURN` op of one function jumps to,
/// whether it is the one the function was entered with: the entry link
/// register, or on a stack-push ISA the entry-SP slot a `call` pushed it into.
///
/// Read off the optimised IR, so a return address saved to the stack and
/// restored is followed back to the store that saved it. `false` when that
/// cannot be shown, and the site is then an indirect branch.
///
/// The one claim this rests on beyond the IR: only a store this function makes
/// at a known offset from its entry SP can overwrite a saved return address.
/// A callee, a user-op, or a store through any other pointer (a different SP
/// base included) is taken to leave it alone; code that does otherwise smashes
/// its own stack.
pub struct ReturnTargets<'f> {
    function: &'f Function,
    assumptions: &'f crate::AssumptionOptions,
    /// Every claim a query has decided. Return sites mostly share their
    /// claims, and each is explored once.
    settled: FxHashMap<Claim, bool>,
}

/// One fact a return target's verification rests on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Claim {
    /// The value is the address the function was entered with.
    Target(ValueId),
    /// In memory state `mem`, `[offset, offset + size)` off the entry SP holds
    /// that address.
    Slot {
        mem: ValueId,
        offset: i128,
        size: i128,
    },
}

impl<'f> ReturnTargets<'f> {
    #[must_use]
    pub fn new(function: &'f Function, assumptions: &'f crate::AssumptionOptions) -> Self {
        Self {
            function,
            assumptions,
            settled: FxHashMap::default(),
        }
    }

    /// Whether `target` is the address the function was entered with.
    ///
    /// A claim holds unless one it rests on fails outright, so a cycle of
    /// claims (a loop rewriting a slot with its own value, a phi of itself)
    /// brings in no value of its own. Linear in the claims reached.
    pub fn returns_to_caller(&mut self, target: ValueId) -> bool {
        let root = Claim::Target(target);
        if let Some(&holds) = self.settled.get(&root) {
            return holds;
        }
        let mut index: FxHashMap<Claim, usize> = FxHashMap::default();
        index.insert(root, 0);
        let mut claims = vec![root];
        // Per claim, the claims resting on it.
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new()];
        let mut failed: Vec<usize> = Vec::new();
        let mut premises = Vec::new();
        let mut next = 0;
        while let Some(&claim) = claims.get(next) {
            premises.clear();
            if !self.premises(claim, &mut premises) {
                failed.push(next);
            }
            for &premise in &premises {
                match self.settled.get(&premise) {
                    Some(true) => {}
                    Some(false) => failed.push(next),
                    None => {
                        let at = *index.entry(premise).or_insert_with(|| {
                            claims.push(premise);
                            dependents.push(Vec::new());
                            claims.len() - 1
                        });
                        dependents[at].push(next);
                    }
                }
            }
            next += 1;
        }
        let mut holds = vec![true; claims.len()];
        while let Some(at) = failed.pop() {
            if std::mem::replace(&mut holds[at], false) {
                failed.extend_from_slice(&dependents[at]);
            }
        }
        self.settled.extend(claims.into_iter().zip(holds));
        self.settled[&root]
    }

    /// Pushes what `claim` rests on onto `out`; `false` when it fails outright.
    fn premises(&self, claim: Claim, out: &mut Vec<Claim>) -> bool {
        let function = self.function;
        match claim {
            Claim::Target(target) => {
                let value = super::strip_isa_mode_mask(function, target);
                let producer = function.producer(value);
                match *function.node_kind(producer) {
                    NodeKind::InitialVar(id) => {
                        function.default_cc().link_register_vn == Some(function.initial_vn(id))
                    }
                    NodeKind::Phi => {
                        out.extend(function.phi_data_inputs(producer).map(Claim::Target));
                        true
                    }
                    NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                        let Some(offset) = self.entry_sp_offset(function.load_addr(producer))
                        else {
                            return false;
                        };
                        let (Ok(ty), Some(mem)) = (
                            function.value_type(value),
                            function.memory_input_of(producer),
                        ) else {
                            return false;
                        };
                        out.push(Claim::Slot {
                            mem,
                            offset,
                            size: ty.byte_size() as i128,
                        });
                        true
                    }
                    _ => false,
                }
            }
            Claim::Slot { mem, offset, size } => {
                let node = function.producer(mem);
                let below = |mem| Claim::Slot { mem, offset, size };
                match *function.node_kind(node) {
                    // Untouched since entry: a stack-push ISA's return slot.
                    NodeKind::InitialMemory => {
                        offset == 0 && function.default_cc().link_register_vn.is_none()
                    }
                    NodeKind::MemPhi => {
                        out.extend(function.phi_data_inputs(node).map(below));
                        true
                    }
                    NodeKind::Store(space) => {
                        let written = (space == rsleigh::VnSpace::RAM)
                            .then(|| self.entry_sp_offset(function.store_addr(node)))
                            .flatten()
                            .map(|at| {
                                (
                                    at,
                                    store_value_byte_size(function, function.store_data(node)),
                                )
                            });
                        match written {
                            Some((at, len)) if at == offset && len == size => {
                                out.push(Claim::Target(function.store_data(node)));
                                true
                            }
                            Some((at, len)) if at < offset + size && offset < at + len => false,
                            _ => {
                                out.extend(function.memory_input_of(node).map(below));
                                true
                            }
                        }
                    }
                    NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                        out.extend(function.memory_input_of(node).map(below));
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// The offset of `addr` from the entry SP, `None` for any other address.
    fn entry_sp_offset(&self, addr: ValueId) -> Option<i128> {
        let function = self.function;
        let MemExpr {
            base,
            offset,
            kind: MemKind::Stack,
        } = decompose(function, addr, &self.assumptions.noalias_allocators)?
        else {
            return None;
        };
        matches!(
            *function.node_kind(function.producer(base)),
            NodeKind::InitialVar(id) if function.initial_vn(id) == function.default_cc().stack_vn
        )
        .then_some(offset)
    }
}
