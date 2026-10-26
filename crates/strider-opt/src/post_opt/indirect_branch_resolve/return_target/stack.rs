//! Where a value sits on the entry stack, and what each store does to it.

use rustc_hash::FxHashSet;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, IntBinaryOp};

use super::{Claim, Path, ReturnTargets, StackAddr, StoreEffect};
use crate::mem_analysis::{
    MemExpr, MemKind, decompose, entry_sp_span, ring_overlap, store_value_byte_size,
    wrap_to_addr_width,
};

impl ReturnTargets<'_> {
    /// `addr` as a stack address. One the address spine does not name is
    /// guessed as an entry-SP offset, and the claim that it is one pushed.
    pub(super) fn locate(
        &mut self,
        path: Path,
        addr: ValueId,
        out: &mut Vec<(Path, Claim)>,
    ) -> Option<StackAddr> {
        if let Some(at) = self.spine(addr) {
            return Some(at);
        }
        let offset = self.guess(path, addr)?;
        out.push((
            path,
            Claim::EntrySp {
                value: addr,
                offset,
            },
        ));
        Some(StackAddr { base: None, offset })
    }

    /// The stack address the spine of `addr` names.
    pub(super) fn spine(&self, addr: ValueId) -> Option<StackAddr> {
        stack_addr(self.function, decompose(self.function, addr)?)
    }

    /// An entry-SP offset `value` may hold along `path`, read off one chain of
    /// additions, phi arms and the stack slots address spines name. A guess
    /// only: a claim proves it.
    pub(super) fn guess(&mut self, path: Path, value: ValueId) -> Option<i128> {
        let function = self.function;
        let mut trail: Vec<((Path, ValueId), i128)> = Vec::new();
        let mut on_trail = FxHashSet::default();
        let (mut path, mut cur, mut accrued) = (path, value, 0i128);
        let found = loop {
            if let Some(&known) = self.guesses.get(&(path, cur)) {
                break known.and_then(|offset| offset.checked_add(accrued));
            }
            if !on_trail.insert((path, cur)) {
                break None;
            }
            trail.push(((path, cur), accrued));
            if let Some(at) = self.spine(cur) {
                break at
                    .base
                    .is_none()
                    .then_some(at.offset)
                    .and_then(|offset| offset.checked_add(accrued));
            }
            let producer = function.producer(cur);
            match *function.node_kind(producer) {
                NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                    let Some((operand, addend)) =
                        crate::const_eval::const_addend(function, producer)
                    else {
                        break None;
                    };
                    let Some(sum) = accrued.checked_add(addend) else {
                        break None;
                    };
                    accrued = sum;
                    cur = operand;
                }
                NodeKind::Phi => {
                    let Some(arm) = self
                        .arms(path, producer)
                        .find(|arm| !on_trail.contains(arm))
                    else {
                        break None;
                    };
                    (path, cur) = arm;
                }
                NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                    let (Some(at), Ok(ty), Some(mem)) = (
                        self.spine(function.load_addr(producer)),
                        function.value_type(cur),
                        function.memory_input_of(producer),
                    ) else {
                        break None;
                    };
                    let Some(data) = self.stored_value(mem, at, ty.byte_size() as i128) else {
                        break None;
                    };
                    path = None;
                    cur = data;
                }
                _ => break None,
            }
        };
        let found = found.map(|offset| wrap_to_addr_width(function, value, offset));
        for (key, accrued) in trail {
            let offset = found
                .and_then(|offset| offset.checked_sub(accrued))
                .map(|offset| wrap_to_addr_width(function, key.1, offset));
            self.guesses.insert(key, offset);
        }
        found
    }

    /// The value a store left in the `size` bytes at `at` in memory state
    /// `mem`: the first store to write them found searching back from `mem`.
    /// A guess only, cached for every state the search visits.
    pub(super) fn stored_value(
        &mut self,
        mem: ValueId,
        at: StackAddr,
        size: i128,
    ) -> Option<ValueId> {
        let function = self.function;
        let mut seen = Vec::new();
        let mut on_stack = FxHashSet::default();
        let mut stack = vec![mem];
        let mut found = None;
        while let Some(cur) = stack.pop() {
            if let Some(&known) = self.stored.get(&(cur, at, size)) {
                found = known;
                if found.is_some() {
                    break;
                }
                continue;
            }
            if !on_stack.insert(cur) {
                continue;
            }
            seen.push(cur);
            let node = function.producer(cur);
            match *function.node_kind(node) {
                NodeKind::Store(_) => match self.store_effect(None, node, at, size, None) {
                    StoreEffect::Writes(data) => {
                        found = Some(data);
                        break;
                    }
                    StoreEffect::Clobbers => {}
                    StoreEffect::Passes => stack.extend(function.memory_input_of(node)),
                },
                NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                    stack.extend(function.memory_input_of(node));
                }
                NodeKind::MemPhi => {
                    let arms: Vec<ValueId> = function.phi_data_inputs(node).collect();
                    stack.extend(arms.into_iter().rev());
                }
                _ => {}
            }
        }
        for cur in seen {
            self.stored.insert((cur, at, size), found);
        }
        found
    }

    /// What `store` does to the `size` bytes at `at`.
    ///
    /// An address the spine names is compared exactly. One it does not is an
    /// untracked write, and `return_slots_survive_untracked_writes` is what
    /// lets it pass. With that claim cleared and `prove` given, the address is
    /// located through [`Self::guess`] instead and the claim that the guess
    /// holds pushed onto `out`, so an unlocatable write answers `Clobbers`,
    /// except one a held claim puts off the stack ([`off_stack`]).
    ///
    /// The guess stays out of the optimistic answer because it is not a
    /// placement: a merged SP value guesses from one phi arm, which no claim
    /// then proves, and a prologue's own `push lr` would read as a write that
    /// cannot be shown to land anywhere. Without `prove` this is itself part of
    /// a guess (`stored_value`), where a spine-less address passes and whatever
    /// offset the walk reaches is proven by a claim of its own.
    pub(super) fn store_effect(
        &mut self,
        path: Path,
        store: NodeId,
        at: StackAddr,
        size: i128,
        prove: Option<&mut Vec<(Path, Claim)>>,
    ) -> StoreEffect {
        let function = self.function;
        let NodeKind::Store(space) = *function.node_kind(store) else {
            unreachable!("store_effect on a non-Store");
        };
        if space != rsleigh::VnSpace::RAM {
            return StoreEffect::Passes;
        }
        let addr = function.store_addr(store);
        let data = function.store_data(store);
        let optimistic = self
            .function
            .assumptions()
            .return_slots_survive_untracked_writes;
        // One decomposition answers both where the store lands and, when it
        // lands nowhere on the stack, which region a claim puts it in.
        let spine = decompose(function, addr);
        let located = spine.and_then(|e| stack_addr(function, e));
        let (written, guessed) = match (located, prove.is_some() && !optimistic) {
            (Some(written), _) => (written, false),
            (None, false) => return StoreEffect::Passes,
            (None, true) if off_stack(function, addr, spine) => return StoreEffect::Passes,
            (None, true) => match self.guess(path, addr) {
                Some(offset) => (StackAddr { base: None, offset }, true),
                None => return StoreEffect::Clobbers,
            },
        };
        // The guess is what a claim turns into a proof, and the answer rests on
        // it either way round, so one is pushed for every guessed address.
        if let (true, Some(out)) = (guessed, prove) {
            out.push((
                path,
                Claim::EntrySp {
                    value: addr,
                    offset: written.offset,
                },
            ));
        }
        self.overlap(function.value_type_opt(addr), written, at, size, data)
    }

    /// What a store of `data` at `written` does to the `size` bytes at `at`,
    /// carrying `data` out of an exact hit. `ty` is the address type, whose
    /// width the offsets wrap at.
    pub(super) fn overlap(
        &self,
        ty: Option<ValueType>,
        written: StackAddr,
        at: StackAddr,
        size: i128,
        data: ValueId,
    ) -> StoreEffect {
        let len = store_value_byte_size(self.function, data);
        if written.base == at.base {
            return if written.offset == at.offset && len == size {
                StoreEffect::Writes(data)
            } else if ring_overlap(ty, written.offset, len, at.offset, size) {
                StoreEffect::Clobbers
            } else {
                StoreEffect::Passes
            };
        }
        // Offsets off two different bases compare through the range of entry-SP
        // offsets each base can stand at.
        let span = |lo: i128, hi: i128, len: i128| hi.checked_sub(lo)?.checked_add(len);
        match (self.entry_span(written), self.entry_span(at)) {
            (Some((w_lo, w_hi)), Some((a_lo, a_hi)))
                if span(w_lo, w_hi, len)
                    .zip(span(a_lo, a_hi, size))
                    .is_some_and(|(w_len, a_len)| !ring_overlap(ty, w_lo, w_len, a_lo, a_len)) =>
            {
                StoreEffect::Passes
            }
            _ => StoreEffect::Clobbers,
        }
    }

    /// The lowest and highest entry-SP offset `at` can stand at.
    pub(super) fn entry_span(&self, at: StackAddr) -> Option<(i128, i128)> {
        let (lo, hi) = match at.base {
            None => (0, 0),
            Some(base) => entry_sp_span(self.function, base)?,
        };
        Some((lo.checked_add(at.offset)?, hi.checked_add(at.offset)?))
    }
}
/// `expr` as a stack address, `None` when it roots anywhere else.
fn stack_addr(function: &Function, expr: MemExpr) -> Option<StackAddr> {
    (expr.kind == MemKind::Stack).then(|| StackAddr {
        base: (!function.is_entry_sp(expr.base)).then_some(expr.base),
        offset: expr.offset,
    })
}

/// Whether a claim the function holds puts `addr`, decomposed to `spine`, off
/// the stack: a constant under `stack_global_disjoint`, or a listed
/// allocator's memory.  A constant decomposes to no terminal at all.
fn off_stack(function: &Function, addr: ValueId, spine: Option<MemExpr>) -> bool {
    match spine {
        Some(e) => matches!(e.kind, MemKind::Heap | MemKind::HeapOpaque),
        None => {
            function.int_const_u128(addr).is_some() && function.assumptions().stack_global_disjoint
        }
    }
}
