//! Address classes and the pairwise alias verdict between a load and a store.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

use super::AddrClass::*;
use super::decompose::decompose;
use super::query::{Policy, store_value_byte_size};
use super::ring::{addr_bit_width, ring_overlap_at, wrap_to_addr_width};
use super::{AddrClass, AliasVerdict, MemExpr, MemKind};

/// The class of `addr`.  Every offset it carries is reduced at the address
/// width, so offsets compare on the ring; an address past the `i128` carrier
/// has no reduction and is an `Anchor`.
pub(super) fn classify_addr(function: &Function, addr: ValueId) -> AddrClass {
    if addr_bit_width(function, addr) > 128 {
        return AddrClass::Anchor { value: addr };
    }
    match decompose(function, addr) {
        Some(MemExpr { base, offset, kind }) => match kind {
            MemKind::Heap => AddrClass::HeapRooted { base, offset },
            MemKind::HeapOpaque => AddrClass::HeapOpaque,
            MemKind::Stack => AddrClass::StackRooted { base, offset },
        },
        None => match function.int_const_u128(addr) {
            Some(c) => AddrClass::Constant {
                addr: wrap_to_addr_width(function, addr, c as i128),
            },
            None => AddrClass::Anchor { value: addr },
        },
    }
}

/// The verdict between `load` at `load_off` and `store` at `store_off`, two
/// offsets from one base.
fn offset_range_verdict(load: Loc, load_off: i128, store: Loc, store_off: i128) -> AliasVerdict {
    // Offsets reduced at different widths name no distance.
    if load.bits != store.bits {
        return AliasVerdict::MayAlias;
    }
    if load_off == store_off {
        return AliasVerdict::Match;
    }
    if ring_overlap_at(load.bits, load_off, load.size, store_off, store.size) {
        AliasVerdict::MayAlias
    } else {
        AliasVerdict::Disjoint
    }
}

/// One access: an operand of [`alias_verdict`] and a probed location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Loc {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
    /// Bit width of the address, the modulus its offset is reduced at.
    pub(crate) bits: u32,
    pub(crate) space: rsleigh::VnSpace,
}

impl Loc {
    fn at(function: &Function, addr: ValueId, size: i128, space: rsleigh::VnSpace) -> Self {
        Self {
            class: classify_addr(function, addr),
            size,
            bits: addr_bit_width(function, addr),
            space,
        }
    }

    /// What `load` reads.
    pub(super) fn read_by(function: &Function, load: NodeId) -> Self {
        let NodeKind::Load(space) = *function.node_kind(load) else {
            unreachable!("read_by on a non-Load");
        };
        let (_, ty) = function
            .single_value_output(load)
            .expect("Load has 1 typed output per node signature");
        Self::at(
            function,
            function.load_addr(load),
            ty.byte_size() as i128,
            space,
        )
    }

    /// What `store` writes.
    pub(super) fn written_by(function: &Function, store: NodeId) -> Self {
        let NodeKind::Store(space) = *function.node_kind(store) else {
            unreachable!("written_by on a non-Store");
        };
        let size = store_value_byte_size(function, function.store_data(store));
        Self::at(function, function.store_addr(store), size, space)
    }
}

pub(super) fn alias_verdict(load: Loc, store: Loc, policy: Policy) -> AliasVerdict {
    // Distinct spaces never alias, even at the same numeric address.
    if load.space != store.space {
        return AliasVerdict::Disjoint;
    }
    let distinct_sp_bases_disjoint = policy.distinct_sp_bases_disjoint;
    match (load.class, store.class) {
        // Different SP bases differ by an unknown amount.  Under
        // `distinct_sp_bases_disjoint` the probe is at the entry SP
        // (asserted in `MemQuery::nearest`), and incoming-arg slots above it
        // do not overlap frame locals rooted at an alignment-masked SP.
        (
            StackRooted {
                base: lb,
                offset: lo,
            },
            StackRooted {
                base: sb,
                offset: so,
            },
        ) => {
            if lb == sb {
                offset_range_verdict(load, lo, store, so)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Distinct noalias-allocator bases are disjoint, by the
        // `noalias_allocators` claim.  Against an `Anchor` a heap base stays
        // may-alias (the `_` arm).
        (
            HeapRooted {
                base: lb,
                offset: lo,
            },
            HeapRooted {
                base: sb,
                offset: so,
            },
        ) => {
            if lb == sb {
                offset_range_verdict(load, lo, store, so)
            } else {
                AliasVerdict::Disjoint
            }
        }
        // No allocation overlaps the stack or a global.
        (HeapRooted { .. } | HeapOpaque, StackRooted { .. } | Constant { .. })
        | (StackRooted { .. } | Constant { .. }, HeapRooted { .. } | HeapOpaque) => {
            AliasVerdict::Disjoint
        }
        // An unknown allocation may be the very base it is paired with.
        (HeapOpaque, HeapRooted { .. } | HeapOpaque) | (HeapRooted { .. }, HeapOpaque) => {
            AliasVerdict::MayAlias
        }
        (Constant { addr: la }, Constant { addr: sa }) => offset_range_verdict(load, la, store, sa),
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Distinct ids can still compute the same runtime address.
                AliasVerdict::MayAlias
            }
        }
        (StackRooted { .. }, Constant { .. }) | (Constant { .. }, StackRooted { .. }) => {
            if policy.stack_global_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Anything against an `Anchor`.
        _ => AliasVerdict::MayAlias,
    }
}
