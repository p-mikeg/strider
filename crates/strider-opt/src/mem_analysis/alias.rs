//! Address classes and the pairwise alias verdict between a load and a store.

use rustc_hash::FxHashSet;
use strider_ir::node::ValueId;
use strider_ir::{Function, IRViewer};

use super::AddrClass::*;
use super::analyzer::MemOptions;
use super::decompose::decompose;
use super::{AddrClass, AliasVerdict, MemExpr, MemKind};

pub(super) fn classify_addr(
    function: &Function,
    addr: ValueId,
    noalias_allocators: &FxHashSet<u64>,
) -> AddrClass {
    // A heap class needs a listed allocator, so with none configured it is
    // another analyzer's, read back out of the shared `decompose` memo.
    let decomposed = decompose(function, addr, noalias_allocators)
        .filter(|e| e.kind == MemKind::Stack || !noalias_allocators.is_empty());
    match decomposed {
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

/// Only the same-base `StackRooted`, same-base `HeapRooted`, and
/// `Constant`/`Constant` arms of [`alias_verdict`] reach here; each carries an
/// offset, so the unwraps hold.
fn offset_range_verdict(load: SizedAddr, store: SizedAddr) -> AliasVerdict {
    let load_off = load.class.offset().expect("offset-bearing load class");
    let store_off = store.class.offset().expect("offset-bearing store class");
    if !offsets_comparable(load, store, load_off, store_off) {
        AliasVerdict::MayAlias
    } else if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load.size, store_off, store.size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// Whether the two offsets can be compared in integer order.
///
/// `wrap_to_addr_width` reduces a `StackRooted` / `HeapRooted` offset into the
/// address type's signed range, which is correct mod 2^w but not
/// order-preserving across the wrap: two addresses two bytes apart mod 2^32
/// reduce to `+2147483646` and `-2147483648`, which any interval test reads as
/// nearly 2^32 apart. Whenever the distance exceeds half the modulus the true
/// distance is the short way round, and only `MayAlias` is sound.
///
/// A `Constant` offset is the raw unsigned address, not a reduced one, so the
/// same bound cuts a different pair: two globals adjacent across the 0 / 2^w
/// wrap (`0xFFFF_FFF0` against `0x10`) read as far apart and stay `MayAlias`.
/// A modular distance, `min((a - b) mod 2^w, (b - a) mod 2^w)`, would place
/// every pair of either class; this bound places the half of the circle each
/// class is written for.
pub(super) fn offsets_comparable(
    load: SizedAddr,
    store: SizedAddr,
    load_off: i128,
    store_off: i128,
) -> bool {
    let bits = match (load.addr_bits, store.addr_bits) {
        // A missing width is "not known", not "not reduced": the offsets
        // cannot be shown to share a modulus, so only `MayAlias` is sound.
        (None, _) | (_, None) => return false,
        (Some(load_bits), Some(store_bits)) => {
            // Two reductions at different widths land in different moduli, so
            // their difference names no distance at all and only `MayAlias` is
            // sound. Taking one side's width would compute the bound from the
            // wrong modulus and read a wrapped range as far away.
            if load_bits != store_bits {
                return false;
            }
            load_bits
        }
    };
    // Past the i128 carrier `wrap_to_addr_width` cannot reduce, so the offsets
    // are the exact sums and compare in integer order. At exactly 128 it DOES
    // reduce, and `1i128 << 127` would overflow, so there is no bound to test.
    if bits > 128 {
        return true;
    }
    if bits == 128 {
        return false;
    }
    let half = 1i128 << (bits - 1);
    let span = load.size.max(store.size);
    // A span past the half-modulus leaves no comparable distance at all, so
    // the bound floors at zero rather than turning negative.
    let bound = half.saturating_sub(span).max(0);
    load_off.abs_diff(store_off) <= bound.unsigned_abs()
}

/// One operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
    /// Bit width of the address expression, for the modular-comparability
    /// check in [`offsets_comparable`]. `None` disables it.
    pub(crate) addr_bits: Option<u32>,
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
        // `at_or_below_entry_sp` proof `in_own_frame` does for the same
        // geometric fact, not this comparison.
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
