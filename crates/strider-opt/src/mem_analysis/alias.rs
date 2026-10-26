//! Address classes and the pairwise alias verdict between a load and a store.

use strider_ir::node::ValueId;
use strider_ir::{Function, IRViewer};

use super::AddrClass::*;
use super::analyzer::MemOptions;
use super::decompose::decompose;
use super::ring::{comparable_distance, ring_overlap_at};
use super::{AddrClass, AliasVerdict, MemExpr, MemKind};

pub(super) fn classify_addr(function: &Function, addr: ValueId) -> AddrClass {
    match decompose(function, addr) {
        Some(MemExpr { base, offset, kind }) => match kind {
            MemKind::Heap => AddrClass::HeapRooted { base, offset },
            MemKind::HeapOpaque => AddrClass::HeapOpaque,
            MemKind::Stack => AddrClass::StackRooted { base, offset },
        },
        None => {
            if let Some(c) = function.int_const_u128(addr) {
                AddrClass::Constant { addr: c as i128 }
            } else {
                AddrClass::Anchor { value: addr }
            }
        }
    }
}

/// Endpoints saturate, so an oversized `size` cannot panic or wrap; a saturated
/// endpoint reports "not disjoint", treating the range as unbounded.
#[inline]
pub(super) fn ranges_disjoint(a_off: i128, a_size: i128, b_off: i128, b_size: i128) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    if a_end == i128::MAX || b_end == i128::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Whether offsets of `class` at `bits` are congruences mod 2^`bits` that
/// compare on the ring rather than in integer order.
///
/// `wrap_to_addr_width` reduces a `StackRooted` / `HeapRooted` offset into the
/// address type's signed range, which is exact mod 2^w but not
/// order-preserving across the top of that range: two addresses two bytes
/// apart mod 2^32 reduce to `+2147483646` and `-2147483648`, which an interval
/// test reads as nearly 2^32 apart.  On the ring every distance places.
///
/// A `Constant` offset is the raw unsigned address instead, so its wrap is the
/// one at 0 / 2^w and [`offsets_comparable`]'s bound is written for that half
/// of the circle.  Whether a global access running past 2^w back to 0 is a
/// real shape is unmeasured, so it keeps the bound.
///
/// Past the `i128` carrier nothing reduces, so the offsets are exact sums that
/// cannot wrap and integer order is the MORE precise test: `None` there too.
pub(super) fn modular_offsets(class: AddrClass, bits: u32) -> Option<u32> {
    matches!(class, StackRooted { .. } | HeapRooted { .. })
        .then_some(bits)
        .filter(|&bits| bits <= 128)
}

/// Only the same-base `StackRooted`, same-base `HeapRooted`, and
/// `Constant`/`Constant` arms of [`alias_verdict`] reach here; each carries an
/// offset, so the unwraps hold.
pub(super) fn offset_range_verdict(load: SizedAddr, store: SizedAddr) -> AliasVerdict {
    let load_off = load.class.offset().expect("offset-bearing load class");
    let store_off = store.class.offset().expect("offset-bearing store class");
    // Two offsets at different widths are values mod different powers of two,
    // so their difference names no distance at all, on the ring or in integer
    // order. Taking one side's width would read a wrapped range as far away.
    if load.addr_bits != store.addr_bits {
        return AliasVerdict::MayAlias;
    }
    if load_off == store_off {
        return AliasVerdict::Match;
    }
    let disjoint = match modular_offsets(load.class, load.addr_bits) {
        Some(bits) => !ring_overlap_at(bits, load_off, load.size, store_off, store.size),
        None => {
            offsets_comparable(load, store, load_off, store_off)
                && ranges_disjoint(load_off, load.size, store_off, store.size)
        }
    };
    if disjoint {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// Whether two offsets [`modular_offsets`] leaves in integer order lie close
/// enough to compare there: within half the modulus, less the wider access,
/// of each other.  The widths agree, which the caller has checked.
pub(super) fn offsets_comparable(
    load: SizedAddr,
    store: SizedAddr,
    load_off: i128,
    store_off: i128,
) -> bool {
    debug_assert_eq!(load.addr_bits, store.addr_bits, "one modulus per pair");
    let bits = load.addr_bits;
    // Past the i128 carrier `wrap_to_addr_width` cannot reduce, so the offsets
    // are the exact sums and compare in integer order. At exactly 128 it DOES
    // reduce, and `1i128 << 127` would overflow, so there is no bound to test.
    if bits > 128 {
        return true;
    }
    if bits == 128 {
        return false;
    }
    load_off.abs_diff(store_off) <= comparable_distance(bits, load.size.max(store.size))
}

/// One operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
    /// Bit width of the address expression, for the modular-comparability
    /// check in [`offsets_comparable`].
    pub(crate) addr_bits: u32,
}

pub(super) fn alias_verdict(
    load: SizedAddr,
    store: SizedAddr,
    options: &MemOptions,
) -> AliasVerdict {
    let distinct_sp_bases_disjoint = options.distinct_sp_bases_disjoint;
    match (load.class, store.class) {
        // Different SP bases differ by an unknown amount, so their offsets are
        // unrelated and normally may-alias.  `distinct_sp_bases_disjoint` opts
        // into assuming they address disjoint regions, which stack-arg
        // detection relies on: incoming-arg slots above the entry SP do not
        // overlap frame locals rooted at an alignment-masked SP.
        //
        // The test is `lb != sb`, which is WIDER than that justification: it
        // also calls two distinct alignment anchors disjoint from each other,
        // which nothing proves. Unreachable today because `detect_stack_args`
        // is the only reader and filters `base != initial_sp` before probing,
        // so one side is always the entry SP. A second reader needs the
        // `entry_sp_span` proof `in_own_frame` does for the same geometric
        // fact, not this comparison.
        (StackRooted { base: lb, .. }, StackRooted { base: sb, .. }) => {
            if lb == sb {
                offset_range_verdict(load, store)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Distinct bases from a listed noalias allocator are taken as disjoint,
        // and no such allocation as coinciding with the stack or a global.  The
        // claim rests on `AssumptionOptions::noalias_allocators` being right,
        // not on the IR.  Same base falls through to offsets;
        // a heap base against an opaque `Anchor` stays may-alias (the `_` arm),
        // since the opaque pointer could be this allocation laundered.
        (HeapRooted { base: lb, .. }, HeapRooted { base: sb, .. }) => {
            if lb == sb {
                offset_range_verdict(load, store)
            } else {
                AliasVerdict::Disjoint
            }
        }
        // An allocation of unknown identity is still an allocation: no
        // allocation overlaps the stack or a global.
        (HeapRooted { .. } | HeapOpaque, StackRooted { .. } | Constant { .. })
        | (StackRooted { .. } | Constant { .. }, HeapRooted { .. } | HeapOpaque) => {
            AliasVerdict::Disjoint
        }
        // SOUNDNESS: an unknown identity may be ANY identity, including the
        // very base it is paired with.  Only the region is proven, so nothing
        // here is disjoint and no offset is comparable.
        (HeapOpaque, HeapRooted { .. } | HeapOpaque) | (HeapRooted { .. }, HeapOpaque) => {
            AliasVerdict::MayAlias
        }
        (Constant { .. }, Constant { .. }) => offset_range_verdict(load, store),
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Distinct ids can still compute the same runtime address.
                AliasVerdict::MayAlias
            }
        }
        (StackRooted { .. }, Constant { .. }) | (Constant { .. }, StackRooted { .. }) => {
            if options.stack_global_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        // Anchor against anything bails either way.
        _ => AliasVerdict::MayAlias,
    }
}
