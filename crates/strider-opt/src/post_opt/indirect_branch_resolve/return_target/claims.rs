//! What each claim about a return target rests on.

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
        path: Path,
        claim: Claim,
        claimed: &mut FxHashMap<(Path, ValueId), i128>,
        out: &mut Vec<(Path, Claim)>,
    ) -> bool {
        let function = self.function;
        match claim {
            Claim::Target(target) => {
                let value = strip_isa_mode_mask(function, target);
                let producer = function.producer(value);
                match *function.node_kind(producer) {
                    NodeKind::InitialVar(id) => {
                        function.default_cc().link_register == Some(function.initial_vn(id))
                    }
                    NodeKind::Phi => {
                        let arms = self.arms(path, producer);
                        out.extend(arms.map(|(path, arm)| (path, Claim::Target(arm))));
                        true
                    }
                    NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                        self.push_slot(path, producer, value, Held::ReturnAddress, out)
                    }
                    _ => false,
                }
            }
            Claim::EntrySp { value, offset } => {
                if *claimed.entry((path, value)).or_insert(offset) != offset {
                    return false;
                }
                if let Some(at) = self.spine(value) {
                    return at.base.is_none() && at.offset == offset;
                }
                let producer = function.producer(value);
                match *function.node_kind(producer) {
                    NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                        let Some((operand, addend)) =
                            crate::const_eval::const_addend(function, producer)
                        else {
                            return false;
                        };
                        let Some(offset) = offset.checked_sub(addend) else {
                            return false;
                        };
                        let offset = wrap_to_addr_width(function, value, offset);
                        out.push((
                            path,
                            Claim::EntrySp {
                                value: operand,
                                offset,
                            },
                        ));
                        true
                    }
                    NodeKind::Phi => {
                        let arms = self.arms(path, producer);
                        out.extend(
                            arms.map(|(path, value)| (path, Claim::EntrySp { value, offset })),
                        );
                        true
                    }
                    NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                        self.push_slot(path, producer, value, Held::EntrySp(offset), out)
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
                let below = |mem| {
                    (
                        None,
                        Claim::Slot {
                            mem,
                            at,
                            size,
                            holds,
                        },
                    )
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
                        match self.store_effect(path, node, at, size, Some(&mut *out)) {
                            StoreEffect::Writes(data) => {
                                out.push((
                                    None,
                                    match holds {
                                        Held::ReturnAddress => Claim::Target(data),
                                        Held::EntrySp(offset) => Claim::EntrySp {
                                            value: data,
                                            offset,
                                        },
                                    },
                                ));
                                true
                            }
                            StoreEffect::Clobbers => false,
                            StoreEffect::Passes => {
                                out.extend(function.memory_input_of(node).map(below));
                                true
                            }
                        }
                    }
                    NodeKind::Call { .. } if function.get_cc(node).preserves_memory => {
                        out.extend(function.memory_input_of(node).map(below));
                        true
                    }
                    NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                        if !self
                            .function
                            .assumptions()
                            .return_slots_survive_untracked_writes
                        {
                            return false;
                        }
                        out.extend(function.memory_input_of(node).map(below));
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Pushes the slot a `Load` reads, holding `holds`, for the loaded
    /// `value`; `false` when its address is no known stack address.
    pub(super) fn push_slot(
        &mut self,
        path: Path,
        load: NodeId,
        value: ValueId,
        holds: Held,
        out: &mut Vec<(Path, Claim)>,
    ) -> bool {
        let function = self.function;
        let (Some(at), Ok(ty), Some(mem)) = (
            self.locate(path, function.load_addr(load), out),
            function.value_type(value),
            function.memory_input_of(load),
        ) else {
            return false;
        };
        out.push((
            None,
            Claim::Slot {
                mem,
                at,
                size: ty.byte_size() as i128,
                holds,
            },
        ));
        true
    }
}
