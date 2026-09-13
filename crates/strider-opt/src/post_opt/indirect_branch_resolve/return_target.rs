//! Whether a return instruction jumps back to the address the function was
//! entered with.

use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{NodeId, NodeKind, ValueId};
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
    /// Per `(offset, size)` slot off the entry SP, the memory values every path
    /// below which leaves that slot holding the return address. Shared by the
    /// function's return sites, which mostly probe the same slot.
    clean: FxHashMap<(i128, i128), FxHashSet<ValueId>>,
    /// Phis being checked further up the value chain; meeting one again is a
    /// cycle, which brings in no value of its own.
    open_phis: FxHashSet<NodeId>,
}

impl<'f> ReturnTargets<'f> {
    #[must_use]
    pub fn new(function: &'f Function, assumptions: &'f crate::AssumptionOptions) -> Self {
        Self {
            function,
            assumptions,
            clean: FxHashMap::default(),
            open_phis: FxHashSet::default(),
        }
    }

    /// Whether `target` is the address the function was entered with.
    pub fn returns_to_caller(&mut self, target: ValueId) -> bool {
        let function = self.function;
        let value = super::strip_isa_mode_mask(function, target);
        let producer = function.producer(value);
        match *function.node_kind(producer) {
            NodeKind::InitialVar(id) => {
                function.default_cc().link_register_vn == Some(function.initial_vn(id))
            }
            NodeKind::Phi => {
                if !self.open_phis.insert(producer) {
                    return true;
                }
                let inputs: Vec<ValueId> = function.phi_data_inputs(producer).collect();
                let all = inputs.into_iter().all(|v| self.returns_to_caller(v));
                self.open_phis.remove(&producer);
                all
            }
            NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                self.is_saved_return_address(producer, value)
            }
            _ => false,
        }
    }

    /// A `Load` of an entry-SP slot holding the return address.
    fn is_saved_return_address(&mut self, load: NodeId, value: ValueId) -> bool {
        let function = self.function;
        let Some(offset) = self.entry_sp_offset(function.load_addr(load)) else {
            return false;
        };
        let (Ok(ty), Some(mem)) = (function.value_type(value), function.memory_input_of(load))
        else {
            return false;
        };
        self.slot_holds_return_address(mem, offset, ty.byte_size() as i128)
    }

    /// Whether every path back from `start` finds `[offset, offset + size)`
    /// holding the return address: untouched since entry on a stack-push ISA,
    /// or last written, exactly, with a value that is one.
    fn slot_holds_return_address(&mut self, start: ValueId, offset: i128, size: i128) -> bool {
        let function = self.function;
        let key = (offset, size);
        let entry_slot = offset == 0 && function.default_cc().link_register_vn.is_none();
        let mut visited: FxHashSet<ValueId> = FxHashSet::default();
        let mut saves: Vec<NodeId> = Vec::new();
        let mut work = vec![start];
        while let Some(mem) = work.pop() {
            if self.clean.get(&key).is_some_and(|c| c.contains(&mem)) || !visited.insert(mem) {
                continue;
            }
            let node = function.producer(mem);
            match *function.node_kind(node) {
                NodeKind::InitialMemory if entry_slot => {}
                NodeKind::MemPhi => work.extend(function.phi_data_inputs(node)),
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
                        Some((at, len)) if at == offset && len == size => saves.push(node),
                        Some((at, len)) if at < offset + size && offset < at + len => return false,
                        _ => work.extend(function.memory_input_of(node)),
                    }
                }
                NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                    work.extend(function.memory_input_of(node));
                }
                _ => return false,
            }
        }
        if !saves
            .into_iter()
            .all(|save| self.returns_to_caller(function.store_data(save)))
        {
            return false;
        }
        // Every value reached leads only to what was just accepted.
        self.clean.entry(key).or_default().extend(visited);
        true
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
