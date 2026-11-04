use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IntBinaryOp};

use super::{Claim, Held, Path, ReturnTargets, StoreEffect};
use crate::mem_analysis::wrap_to_addr_width;
use crate::post_opt::indirect_branch_resolve::strip_isa_mode_mask;

impl ReturnTargets<'_> {
    /// Pushes what `claim` rests on onto `out`; `false` when it fails outright.
    pub(super) fn premises(
        &mut self,
        claim: Claim,
        claimed: &mut FxHashMap<(Path, ValueId), i128>,
        out: &mut Vec<Claim>,
    ) -> bool {
        let function = self.function;
        match claim {
            Claim::Holds { path, value, held } => {
                let value = match held {
                    Held::ReturnAddress => strip_isa_mode_mask(function, value),
                    Held::EntrySp(offset) => {
                        if *claimed.entry((path, value)).or_insert(offset) != offset {
                            return false;
                        }
                        if let Some(at) = self.spine(value) {
                            return at.base.is_none() && at.offset == offset;
                        }
                        value
                    }
                };
                let producer = function.producer(value);
                match (*function.node_kind(producer), held) {
                    (NodeKind::InitialVar(id), Held::ReturnAddress) => {
                        function.default_cc().link_register == Some(function.initial_vn(id))
                    }
                    (NodeKind::IntBinaryOp(IntBinaryOp::Add), Held::EntrySp(offset)) => {
                        let Some((operand, addend)) =
                            crate::const_eval::const_addend(function, producer)
                        else {
                            return false;
                        };
                        let Some(offset) = offset.checked_sub(addend) else {
                            return false;
                        };
                        let offset = wrap_to_addr_width(function, value, offset);
                        out.push(Claim::Holds {
                            path,
                            value: operand,
                            held: Held::EntrySp(offset),
                        });
                        true
                    }
                    (NodeKind::Phi, _) => {
                        let arms = self.arms(path, producer);
                        out.extend(arms.map(|(path, value)| Claim::Holds { path, value, held }));
                        true
                    }
                    (NodeKind::Load(space), _) if space == rsleigh::VnSpace::RAM => {
                        self.push_slot(path, producer, value, held, out)
                    }
                    _ => false,
                }
            }
            Claim::Slot {
                mem,
                at,
                size,
                holds,
            } => {
                let node = function.producer(mem);
                let below = |mem| Claim::Slot {
                    mem,
                    at,
                    size,
                    holds,
                };
                match *function.node_kind(node) {
                    // Untouched since entry: a stack-push ISA's return slot.
                    NodeKind::InitialMemory => {
                        holds == Held::ReturnAddress
                            && at.base.is_none()
                            && at.offset == 0
                            && function.default_cc().link_register.is_none()
                    }
                    NodeKind::MemPhi => {
                        out.extend(function.phi_data_inputs(node).map(below));
                        true
                    }
                    NodeKind::Store(_) => {
                        match self.store_effect(node, at, size, Some(&mut *out)) {
                            StoreEffect::Writes(data) => {
                                out.push(Claim::Holds {
                                    path: None,
                                    value: data,
                                    held: holds,
                                });
                                true
                            }
                            StoreEffect::Clobbers => false,
                            StoreEffect::Passes => {
                                out.extend(function.memory_input_of(node).map(below));
                                true
                            }
                        }
                    }
                    // A call passes when it writes no memory, or under the
                    // claim that a callee leaves the return slots alone.
                    kind @ (NodeKind::Call { .. } | NodeKind::CallOther { .. }) => {
                        let passes = (matches!(kind, NodeKind::Call { .. })
                            && function.get_cc(node).preserves_memory)
                            || function.assumptions().return_slots_survive_untracked_writes;
                        if passes {
                            out.extend(function.memory_input_of(node).map(below));
                        }
                        passes
                    }
                    _ => false,
                }
            }
        }
    }

    /// Pushes the slot a `Load` reads, holding `holds`, for the loaded
    /// `value`; `false` when its address is no known stack address.
    fn push_slot(
        &mut self,
        path: Path,
        load: NodeId,
        value: ValueId,
        holds: Held,
        out: &mut Vec<Claim>,
    ) -> bool {
        let function = self.function;
        let Some(at) = self.locate(path, function.load_addr(load), out) else {
            return false;
        };
        let ty = function
            .value_type_opt(value)
            .expect("a Load's output is typed");
        out.push(Claim::Slot {
            mem: function.load_mem(load),
            at,
            size: ty.byte_size() as i128,
            holds,
        });
        true
    }
}
