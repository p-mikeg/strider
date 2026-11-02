//! Integer evaluation at a declared width, over the [`UBig`] the interner
//! stores.
//!
//! `UBig` is unbounded, so two's-complement-at-a-declared-width is strider's
//! rule, and it lives here: every function below is total at every declared
//! integer width. `None` means only that the operation is undefined (divide
//! by zero, `INT_MIN / -1`) or that `ty` is a float, which has no integer
//! bits.
//!
//! [`bit`], [`mask_below`] and [`sign_bit`] are the exception: they are the
//! `u128`-carrier width helpers [`ValueType`]'s own accessors are built from.

use dashu_int::ops::BitTest;
use dashu_int::{IBig, UBig};

use crate::node::{IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType};

/// `v` with every bit above `ty`'s width cleared. A float has no integer
/// bits: 0.
#[must_use]
pub fn mask(v: &UBig, ty: ValueType) -> UBig {
    match ty {
        // A width the carrier holds masks without touching the heap: `&` with
        // a carrier mask reads only the low dword of a wider `v`.
        ValueType::Int(w) if w <= 128 => match u128::try_from(v) {
            Ok(n) => UBig::from(n & mask_below(w.into())),
            Err(_) => v & UBig::from(mask_below(w.into())),
        },
        ValueType::Int(w) => {
            let mut m = v.clone();
            m.clear_high_bits(w.into());
            m
        }
        ValueType::Float(_) => UBig::ZERO,
    }
}

/// Every bit of `ty` set, 0 for a float.
#[must_use]
pub fn all_ones(ty: ValueType) -> UBig {
    match ty {
        ValueType::Int(w) if w <= 128 => UBig::from(mask_below(w.into())),
        ValueType::Int(w) => (UBig::ONE << usize::from(w)) - UBig::ONE,
        ValueType::Float(_) => UBig::ZERO,
    }
}

/// `!v` within `ty`.
#[must_use]
pub fn complement(v: &UBig, ty: ValueType) -> UBig {
    all_ones(ty) ^ mask(v, ty)
}

/// A run of `bits` ones at `[shift, shift + bits)`, 0 at `bits == 0`.
///
/// Unbounded: a caller wanting it inside a type masks with [`mask`] or
/// [`complement`].
#[must_use]
pub fn ones_at(bits: usize, shift: usize) -> UBig {
    ((UBig::ONE << bits) - UBig::ONE) << shift
}

/// `v`'s little-endian bytes, zero-extended to the bytes `ty` spans:
/// `UBig::to_le_bytes` drops its own trailing zero bytes.
#[cfg(any(test, feature = "test-util"))]
#[must_use]
pub fn le_bytes(v: &UBig, ty: ValueType) -> Vec<u8> {
    let mut out = v.to_le_bytes().into_vec();
    out.resize(ty.byte_size(), 0);
    out
}

/// Bit `n` of the `u128` carrier, 0 at and past its top: the saturating form
/// of `1u128 << n`, which panics there.
#[inline]
#[must_use]
pub const fn bit(n: u32) -> u128 {
    if n >= 128 { 0 } else { 1u128 << n }
}

/// The low `bits` bits set, saturating to `u128::MAX` at and past the
/// carrier. A type's own mask is [`ValueType::int_mask_u128`].
#[inline]
#[must_use]
pub const fn mask_below(bits: u32) -> u128 {
    bit(bits).wrapping_sub(1)
}

/// The top bit of a `bits`-wide integer: 0 at width 0, and past the carrier,
/// where the carrier does not hold it.
#[inline]
#[must_use]
pub const fn sign_bit(bits: u32) -> u128 {
    bit(bits.wrapping_sub(1))
}

/// `ty`'s width; `None` for a float, which has no integer bits.
fn int_bits(ty: ValueType) -> Option<usize> {
    match ty {
        ValueType::Int(w) => Some(w.into()),
        ValueType::Float(_) => None,
    }
}

/// `v` read as two's complement at `ty`'s width; `None` for a float.
#[must_use]
pub fn signed(v: &UBig, ty: ValueType) -> Option<IBig> {
    let bits = int_bits(ty)?;
    debug_assert!(bits > 0, "a supported integer width is at least 1");
    let m = mask(v, ty);
    Some(if m.bit(bits.saturating_sub(1)) {
        IBig::from(m) - (IBig::ONE << bits)
    } else {
        IBig::from(m)
    })
}

/// `v` wrapped into `ty`'s two's-complement ring. A float has no integer
/// bits: 0.
#[must_use]
pub fn unsigned(v: IBig, ty: ValueType) -> UBig {
    v & all_ones(ty)
}

/// The least value `ty` holds in two's complement.
fn signed_min(ty: ValueType) -> IBig {
    -(IBig::ONE << ty.bit_width().saturating_sub(1))
}

/// The greatest value `ty` holds in two's complement.
fn signed_max(ty: ValueType) -> IBig {
    (IBig::ONE << ty.bit_width().saturating_sub(1)) - IBig::ONE
}

/// Whether a shift by `amount` is at or past `ty`'s declared width, where
/// Sleigh (opbehavior.cc:411) returns 0 for IntLeft / IntRight and
/// `signbit ? calc_mask : 0` for IntSright.
///
/// The DECLARED bit width matches Sleigh's `8 * sizeout` at every type except
/// `I1`, whose width is 1 while its varnode is a byte; this module reads `I1`
/// as 1-bit signed for the same reason, so an `I1` shift or signed compare
/// answers off strider's boolean width, not Sleigh's. The lifter emits `I1` only from compares and
/// `Truncate(..):I1`, so no shift ever reaches it.
///
/// Do NOT reduce `amount` modulo the width, nor mask it to `ty` before this
/// test: either wraps an out-of-range literal back into range, diverging from
/// Sleigh by the full shift output.
#[inline]
#[must_use]
pub fn shift_out_of_range(amount: u128, ty: ValueType) -> bool {
    amount >= ty.bit_width() as u128
}

/// Result masked to `ty`, or `None` when the operation is undefined (division
/// by zero, `INT_MIN / -1`) or `ty` is a float.
///
/// Both operands are masked to `ty` at entry, the shift amount only after it
/// is clamped to the width. Div, Rem and ShiftRight are not safe under
/// masking-commutativity, so they need the inputs already narrowed to give the
/// right answer on a caller that passed raw bits.
#[inline]
pub fn int_binary(op: IntBinaryOp, l: &UBig, r: &UBig, ty: ValueType) -> Option<UBig> {
    let bits = int_bits(ty)?;
    // Sleigh answers a count at or past the width as one exactly at it.
    let shift = usize::try_from(r).map_or(bits, |a| a.min(bits));
    let l = mask(l, ty);
    let r = mask(r, ty);
    let raw: UBig = match op {
        IntBinaryOp::Add => l + r,
        IntBinaryOp::Mul => l * r,
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => l << shift,
        IntBinaryOp::ShiftRight => l >> shift,
        // `IBig`'s shift floors, so it sign-fills.
        IntBinaryOp::SShiftRight => unsigned(signed(&l, ty)? >> shift, ty),
        IntBinaryOp::Div | IntBinaryOp::Rem => {
            if r == UBig::ZERO {
                return None;
            }
            if op == IntBinaryOp::Div { l / r } else { l % r }
        }
        IntBinaryOp::Sdiv => {
            let (sl, sr) = (signed(&l, ty)?, signed(&r, ty)?);
            if sr == IBig::ZERO || (sl == signed_min(ty) && sr == IBig::NEG_ONE) {
                // INT_MIN / -1 is undefined at every signed width: the
                // mathematical result is one past the top of `ty`.
                return None;
            }
            // `IBig` division truncates toward zero, as Sleigh's Sdiv does.
            unsigned(sl / sr, ty)
        }
        IntBinaryOp::Srem => {
            let (sl, sr) = (signed(&l, ty)?, signed(&r, ty)?);
            // INT_MIN % -1 is mathematically 0, but hardware idiv raises #DE.
            // Treat it as undefined, matching the Sdiv arm.
            if sr == IBig::ZERO || (sl == signed_min(ty) && sr == IBig::NEG_ONE) {
                return None;
            }
            // The remainder takes the dividend's sign, as Sleigh's Srem does.
            unsigned(sl % sr, ty)
        }
    };
    Some(mask(&raw, ty))
}

/// `None` for a float.
#[inline]
pub fn int_cmp(op: IntCmpOp, l: &UBig, r: &UBig, ty: ValueType) -> Option<bool> {
    int_bits(ty)?;
    // A narrow IntConst carrying high bits beyond the type width would compare
    // wrong unmasked. The signed arms re-mask via `signed`, so the double-mask
    // is idempotent there.
    let l = mask(l, ty);
    let r = mask(r, ty);

    Some(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => signed(&l, ty)? < signed(&r, ty)?,
        // The exact sum / difference against `ty`'s range: an overflow is a
        // result the width cannot hold.
        IntCmpOp::Carry => l + r > all_ones(ty),
        IntCmpOp::Scarry => {
            let sum = signed(&l, ty)? + signed(&r, ty)?;
            sum > signed_max(ty) || sum < signed_min(ty)
        }
        IntCmpOp::Sborrow => {
            let diff = signed(&l, ty)? - signed(&r, ty)?;
            diff > signed_max(ty) || diff < signed_min(ty)
        }
    })
}

/// `op` applied to `v` at `ty`'s width; `None` for a float.
#[inline]
pub fn int_unary(op: IntUnaryOp, v: &UBig, ty: ValueType) -> Option<UBig> {
    int_bits(ty)?;
    Some(match op {
        IntUnaryOp::Neg => unsigned(-IBig::from(mask(v, ty)), ty),
    })
}

/// `v`, read signed at `in_ty`, at `out_ty`; `None` when either is a float.
#[inline]
pub fn sign_extend(v: &UBig, in_ty: ValueType, out_ty: ValueType) -> Option<UBig> {
    let s = signed(v, in_ty)?;
    int_bits(out_ty)?;
    Some(unsigned(s, out_ty))
}

/// The set bits of `v` within `in_ty`'s width; `None` for a float.
#[inline]
pub fn popcount(v: &UBig, in_ty: ValueType) -> Option<UBig> {
    int_bits(in_ty)?;
    Some(UBig::from(mask(v, in_ty).count_ones()))
}

/// Leading-zero count within `in_ty`'s width, not the host's; `None` for a
/// float.
///
/// Counts over `8 * byte_size`, which is what `opbehavior.cc:791` does
/// (`count_leading_zeros(in1) - 8*(sizeof(uintb) - sizein)`). Only `I1`
/// separates that from `bit_width`.
#[inline]
pub fn lzcount(v: &UBig, in_ty: ValueType) -> Option<UBig> {
    int_bits(in_ty)?;
    Some(UBig::from(
        (in_ty.byte_size() * 8).saturating_sub(mask(v, in_ty).bit_len()),
    ))
}

#[cfg(test)]
mod tests {
    /// The variants are public and only the CONSTRUCTORS check the width, so
    /// `Int(0)` reaches these `pub fn`s. Both answered 0 before the carrier
    /// fast paths; a `128 - bits` shift there overflows instead.
    #[test]
    fn the_width_primitives_are_total_at_both_ends_of_the_carrier() {
        use super::{all_ones, mask};
        use crate::UBig;
        use crate::node::ValueType;
        assert_eq!(all_ones(ValueType::Int(0)), UBig::ZERO);
        assert_eq!(mask(&UBig::from(0xffu32), ValueType::Int(0)), UBig::ZERO);
        assert_eq!(all_ones(ValueType::Int(128)), UBig::from(u128::MAX));
        assert_eq!(
            mask(&(UBig::ONE << 200usize), ValueType::Int(128)),
            UBig::ZERO,
            "a value past the carrier still masks at the carrier's own width"
        );
    }

    use super::*;

    fn set_bits(v: &UBig) -> usize {
        v.as_words().iter().map(|l| l.count_ones() as usize).sum()
    }

    /// A `u128` as an operand.
    fn u(v: u128) -> UBig {
        UBig::from(v)
    }

    /// `v` in the `u128` carrier; panics past it.
    fn carrier(v: &UBig) -> u128 {
        u128::try_from(v).unwrap_or_else(|_| panic!("past the carrier: {:?}", v.as_words()))
    }

    /// [`crate::IRBuilderExt::build_window_preserve_mask`]'s value: every bit
    /// of `ty` the window does not cover.
    fn preserve(set_bits: usize, shift: usize, ty: ValueType) -> UBig {
        complement(&ones_at(set_bits, shift), ty)
    }

    fn preserve_u128(set_bits: usize, shift: usize, ty: ValueType) -> u128 {
        carrier(&preserve(set_bits, shift, ty))
    }

    /// The positioned mask selects exactly the bits reg occupies.
    #[test]
    fn positioned_mask_isolates_reg_bits_inside_16byte_container() {
        let q0 = ones_at(128, 0);
        assert_eq!(carrier(&q0), u128::MAX, "container mask is all-ones");
        assert_eq!(
            carrier(&ones_at(32, 0)),
            0xFFFF_FFFF,
            "s0 occupies bits 0..32"
        );
        let d0 = ones_at(64, 0);
        assert_eq!(
            carrier(&d0),
            0xFFFF_FFFF_FFFF_FFFF,
            "d0 occupies bits 0..64"
        );
        // The AArch64 SIMD upper-half hot spot.
        let upper8 = ones_at(64, 64);
        assert_eq!(
            carrier(&upper8),
            0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "the 8-byte sub at offset 8 occupies bits 64..128"
        );
        assert_eq!(carrier(&d0) & carrier(&upper8), 0, "disjoint");
        assert_eq!(carrier(&d0) | carrier(&upper8), carrier(&q0), "covering");
    }

    /// The preserve mask keeps exactly the bits that don't belong to reg.
    #[test]
    fn container_mask_for_upper_half_write_keeps_lower_half() {
        assert_eq!(
            preserve_u128(64, 64, ValueType::int(128)),
            0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF,
            "an upper-8 write preserves the lower 8 bytes of q0"
        );
    }

    /// Same, for the shift-0 lower-half write.
    #[test]
    fn container_mask_for_lower_half_write_keeps_upper_half() {
        assert_eq!(
            preserve_u128(64, 0, ValueType::int(128)),
            0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "a d0 (lower 8) write preserves the upper 8 bytes of q0"
        );
    }

    /// Pins the invariant that makes preserve-then-let-Sleigh-zero exact: the
    /// scalar write's positioned mask and the zero-writes' positioned masks
    /// are disjoint and together tile the container, so after all of them the
    /// container is fully determined with no stale bits and no gaps.
    ///
    /// Over the container the lifter really builds masks for: the AArch64
    /// register table puts `s0` in the 32-byte `z0` (`SIMD_SIZE = 32`), not a
    /// 16-byte `V0`, so this exercises the multi-limb path.
    #[test]
    fn aarch64_scalar_fp_write_then_upper_zero_tiles_the_container() {
        // `fmov s0,w0` lifts to `s0 = Copy(w0)` plus zero-writes tiling the
        // rest of `z0`: observed as 4 bytes at offset 4, then 8 each at 8, 16
        // and 24.
        const Z0_BITS: usize = 256;
        let pieces = [
            ones_at(32, 0),
            ones_at(32, 32),
            ones_at(64, 64),
            ones_at(64, 128),
            ones_at(64, 192),
        ];
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                assert_eq!(
                    a & b,
                    UBig::ZERO,
                    "the scalar write and each zero-fill must be disjoint"
                );
            }
        }
        let union = pieces.iter().fold(UBig::ZERO, |acc, m| acc | m);
        assert_eq!(
            union,
            ones_at(Z0_BITS, 0),
            "s0 and the zero-fills tile the full z0 container"
        );
    }

    /// Exercises every shift count the LE formula produces.
    #[test]
    fn container_mask_byte_subregs_in_4byte_container() {
        let cases: [(usize, u32); 4] = [
            (0, 0xFFFF_FF00),
            (8, 0xFFFF_00FF),
            (16, 0xFF00_FFFF),
            (24, 0x00FF_FFFF),
        ];
        for (shift, expected) in cases {
            assert_eq!(
                preserve_u128(8, shift, ValueType::int(32)),
                u128::from(expected),
                "byte-sub at shift {shift}: expected container_mask 0x{expected:08x}"
            );
        }
    }

    /// Every container width the sub-register path builds masks for.
    #[test]
    fn mask_covers_only_the_declared_width() {
        for bytes in [1usize, 2, 4, 6, 8, 10, 12, 14, 16] {
            let bits = bytes * 8;
            let mask = carrier(&ones_at(bits, 0));
            let want = if bits == 128 {
                u128::MAX
            } else {
                (1u128 << bits) - 1
            };
            assert_eq!(mask, want, "{bytes}-byte mask is the low {bits} bits");
        }
    }

    /// ymm / zmm / SVE `z`: the widths a `u128` mask could not hold.
    #[test]
    fn wide_container_masks_are_representable() {
        let i256 = ValueType::int(256);
        let i512 = ValueType::int(512);
        assert_eq!(ones_at(64, 0), UBig::from_words(&[u64::MAX, 0, 0, 0]));
        assert_eq!(
            preserve(64, 0, i256),
            UBig::from_words(&[0, u64::MAX, u64::MAX, u64::MAX]),
        );
        assert_eq!(
            ones_at(256, 128),
            UBig::from_words(&[0, 0, u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0]),
        );
        assert_eq!(
            preserve(512, 0, i512),
            UBig::ZERO,
            "the full-width mask complements to zero"
        );
    }

    /// The window is clipped by the type, not by its own arithmetic.
    #[test]
    fn a_window_past_the_width_or_empty() {
        let i32_ = ValueType::int(32);
        assert_eq!(ones_at(0, 0), UBig::ZERO);
        assert_eq!(ones_at(0, 300), UBig::ZERO);
        assert_eq!(preserve_u128(0, 0, i32_), 0xFFFF_FFFF, "empty window");
        assert_eq!(preserve_u128(0, 24, i32_), 0xFFFF_FFFF, "empty, shifted");
        assert_eq!(
            preserve_u128(16, 24, i32_),
            0x00FF_FFFF,
            "the half of the window above i32 clears nothing"
        );
        assert_eq!(preserve_u128(64, 0, i32_), 0, "a window wider than i32");
        assert_eq!(
            preserve_u128(8, 32, i32_),
            0xFFFF_FFFF,
            "a window entirely above i32"
        );
        let i256 = ValueType::int(256);
        assert_eq!(preserve(0, 0, i256), UBig::from_words(&[u64::MAX; 4]));
        assert_eq!(
            preserve(128, 192, i256),
            UBig::from_words(&[u64::MAX, u64::MAX, u64::MAX, 0]),
            "the window runs off the top of i256"
        );
    }

    /// Total at every supported integer width, the two past the `u128`
    /// carrier included: all-ones has exactly `bit_width` bits set, and
    /// complementing it twice is the identity.
    #[test]
    fn all_ones_and_complement_are_total_across_every_width() {
        for ty in ValueType::SUPPORTED.into_iter().filter(|t| t.is_integer()) {
            let ones = all_ones(ty);
            assert_eq!(set_bits(&ones), ty.bit_width(), "{ty:?}");
            assert_eq!(
                complement(&ones, ty),
                UBig::ZERO,
                "{ty:?}: all-ones complements to zero"
            );
            assert_eq!(
                complement(&UBig::ZERO, ty),
                ones,
                "{ty:?}: zero complements to all-ones"
            );
            assert_eq!(mask(&ones, ty), ones, "{ty:?}: masking all-ones is a no-op");
        }
    }

    /// The widths a `u128` cannot hold: every bit is addressable, and nothing
    /// above the declared width survives.
    #[test]
    fn complement_and_mask_are_exact_past_the_carrier() {
        let i256 = ValueType::int(256);
        // Bit 200 alone, well above the carrier.
        let v = UBig::from_words(&[0, 0, 0, 1 << 8]);
        assert_eq!(set_bits(&v), 1);
        let c = complement(&v, i256);
        assert_eq!(set_bits(&c), 255);
        assert_eq!(
            complement(&c, i256),
            v,
            "complement is an involution at I256"
        );
        assert_eq!(all_ones(i256), UBig::from_words(&[u64::MAX; 4]));
        let i512 = ValueType::int(512);
        assert_eq!(all_ones(i512), UBig::from_words(&[u64::MAX; 8]));
        // A value spelled wider than its type keeps only the declared bits.
        assert_eq!(
            mask(&UBig::from_words(&[u64::MAX; 8]), i256),
            all_ones(i256)
        );
        // I256 and I512 fill whole limbs, so what pins the trailing mask is a
        // multi-limb width whose top limb is only partly live: I112, 48 spare.
        let i112 = ValueType::int(112);
        assert_eq!(
            u128::try_from(complement(&UBig::ZERO, i112)),
            Ok((1u128 << 112) - 1)
        );
        assert_eq!(
            complement(&UBig::from_words(&[0, 1 << 48]), i112),
            UBig::from_words(&[u64::MAX, (1u64 << 48) - 1]),
            "bit 112 is above the width, so complementing it leaves nothing there"
        );
        // I72: the top limb is live for 8 bits only.
        let i72 = ValueType::int(72);
        assert_eq!(u128::try_from(all_ones(i72)), Ok((1u128 << 72) - 1));
        assert_eq!(
            mask(&UBig::from_words(&[u64::MAX, u64::MAX]), i72),
            all_ones(i72)
        );
        assert_eq!(
            u128::try_from(complement(&UBig::from(0xFFu64), i72)),
            Ok(((1u128 << 72) - 1) ^ 0xFF)
        );
    }

    /// A float type has no integer bits, as [`ValueType::wrap_u128`] has it.
    #[test]
    fn a_float_type_has_no_integer_bits() {
        for ty in ValueType::SUPPORTED.into_iter().filter(|t| t.is_float()) {
            assert_eq!(mask(&UBig::from(u64::MAX), ty), UBig::ZERO, "{ty:?}");
            assert_eq!(all_ones(ty), UBig::ZERO, "{ty:?}");
            assert_eq!(complement(&UBig::ZERO, ty), UBig::ZERO, "{ty:?}");
        }
    }

    /// `le_bytes` pads to the type's bytes where `to_le_bytes` trims, and
    /// truncates nothing a masked value holds.
    #[test]
    fn le_bytes_pads_to_the_declared_byte_count() {
        assert_eq!(le_bytes(&UBig::ZERO, ValueType::int(256)), vec![0u8; 32]);
        let mut five_256 = vec![0u8; 32];
        five_256[0] = 5;
        assert_eq!(le_bytes(&UBig::from(5u64), ValueType::int(256)), five_256);
        assert_eq!(
            le_bytes(&UBig::from(5u64), ValueType::int(64)),
            vec![5, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(le_bytes(&UBig::ZERO, ValueType::I1), vec![0]);
        let mut wide = vec![0u8; 64];
        wide[0] = 1;
        wide[8] = 2;
        wide[16] = 3;
        assert_eq!(
            le_bytes(
                &UBig::from_words(&[1, 2, 3, 0, 0, 0, 0, 0]),
                ValueType::int(512)
            ),
            wide
        );
    }

    /// Saturating where `1u128 << n` panics, so a width past the carrier
    /// answers rather than aborting.
    #[test]
    fn width_helpers_saturate_at_the_carrier() {
        assert_eq!(bit(0), 1);
        assert_eq!(bit(127), 1u128 << 127);
        assert_eq!(bit(128), 0);
        assert_eq!(bit(512), 0);
        assert_eq!(mask_below(0), 0);
        assert_eq!(mask_below(1), 1);
        assert_eq!(mask_below(64), u128::from(u64::MAX));
        assert_eq!(mask_below(127), u128::MAX >> 1);
        assert_eq!(mask_below(128), u128::MAX);
        assert_eq!(mask_below(512), u128::MAX);
        assert_eq!(sign_bit(0), 0);
        assert_eq!(sign_bit(1), 1);
        assert_eq!(sign_bit(8), 0x80);
        assert_eq!(sign_bit(128), 1u128 << 127);
        assert_eq!(sign_bit(256), 0);
    }

    /// One mask per width, whichever spelling reaches it.
    #[test]
    fn the_three_helpers_agree_at_every_supported_width() {
        for ty in ValueType::SUPPORTED.into_iter().filter(|t| t.is_integer()) {
            let bits = ty.bit_width() as u32;
            assert_eq!(
                ty.int_mask_u128().unwrap_or(u128::MAX),
                mask_below(bits),
                "{ty:?}"
            );
            if bits < 128 {
                assert_eq!(mask_below(bits) + 1, bit(bits), "{ty:?}");
            }
            assert_eq!(
                mask_below(bits) & !(mask_below(bits) >> 1),
                sign_bit(bits.min(128)),
                "{ty:?}"
            );
        }
    }

    /// Total past the `u128` carrier: the width arm that used to refuse here
    /// is gone, so each answers at the declared width.
    #[test]
    fn width_past_the_u128_carrier_folds() {
        let i256 = ValueType::int(256);
        let i512 = ValueType::int(512);
        assert_eq!(lzcount(&u(1), i256), Some(UBig::from(255u32)));
        assert_eq!(popcount(&u(1), i256), Some(UBig::ONE));
        assert_eq!(
            int_unary(IntUnaryOp::Neg, &u(1), i512),
            Some(all_ones(i512))
        );
        assert_eq!(sign_extend(&u(1), i256, i512), Some(UBig::ONE));
        assert_eq!(
            sign_extend(&all_ones(i256), i256, i512),
            Some(all_ones(i512)),
            "-1 at I256 sign-extends to -1 at I512"
        );
        assert_eq!(
            int_binary(IntBinaryOp::ShiftLeft, &u(1), &u(130), i256),
            Some(UBig::ONE << 130usize),
            "a shift in range at I256 is not reduced modulo the carrier"
        );
        assert_eq!(
            int_cmp(IntCmpOp::Carry, &all_ones(ValueType::int(128)), &u(1), i256),
            Some(false),
            "2^128 fits I256, so the add does not carry out"
        );
    }

    /// A float answers `None` everywhere, which `mask` alone cannot say: it
    /// returns zero for one.
    #[test]
    fn a_float_type_refuses_every_integer_op() {
        let f64_ = ValueType::float(64);
        let i32_ = ValueType::int(32);
        assert_eq!(int_binary(IntBinaryOp::Add, &u(1), &u(1), f64_), None);
        assert_eq!(int_cmp(IntCmpOp::Equal, &u(1), &u(1), f64_), None);
        assert_eq!(int_unary(IntUnaryOp::Neg, &u(1), f64_), None);
        assert_eq!(sign_extend(&u(1), f64_, i32_), None);
        assert_eq!(sign_extend(&u(1), i32_, f64_), None);
        assert_eq!(popcount(&u(1), f64_), None);
        assert_eq!(lzcount(&u(1), f64_), None);
    }

    /// `IBig`'s division truncates toward zero and its remainder takes the
    /// dividend's sign, which is what Sleigh's Sdiv / Srem do; flooring
    /// instead would give -4 and +1 here.
    #[test]
    fn signed_division_truncates_toward_zero() {
        let i8_ = ValueType::int(8);
        // -7 / 2 == -3, -7 % 2 == -1.
        assert_eq!(
            int_binary(IntBinaryOp::Sdiv, &u(0xF9), &u(2), i8_),
            Some(u(0xFD))
        );
        assert_eq!(
            int_binary(IntBinaryOp::Srem, &u(0xF9), &u(2), i8_),
            Some(u(0xFF))
        );
        // 7 / -2 == -3, 7 % -2 == +1.
        assert_eq!(
            int_binary(IntBinaryOp::Sdiv, &u(7), &u(0xFE), i8_),
            Some(u(0xFD))
        );
        assert_eq!(
            int_binary(IntBinaryOp::Srem, &u(7), &u(0xFE), i8_),
            Some(u(1))
        );
    }

    /// Every result is reduced to `ty`, including at a width whose top limb
    /// is only partly live.
    #[test]
    fn int_binary_masks_its_result_to_the_width() {
        assert_eq!(
            int_binary(IntBinaryOp::Add, &u(0xFF), &u(1), ValueType::int(8)),
            Some(UBig::ZERO),
            "the carry out of I8 is dropped"
        );
        let i72 = ValueType::int(72);
        assert_eq!(
            int_binary(IntBinaryOp::Add, &all_ones(i72), &u(1), i72),
            Some(UBig::ZERO),
            "I72 wraps at bit 72, not at the limb boundary"
        );
        assert_eq!(
            int_binary(IntBinaryOp::ShiftLeft, &u(1), &u(71), i72),
            Some(UBig::ONE << 71usize),
        );
        let i112 = ValueType::int(112);
        assert_eq!(
            int_binary(IntBinaryOp::Mul, &(UBig::ONE << 111usize), &u(2), i112),
            Some(UBig::ZERO),
            "I112 wraps at bit 112"
        );
        assert_eq!(
            int_binary(IntBinaryOp::Add, &all_ones(i112), &u(2), i112),
            Some(UBig::ONE),
        );
    }

    /// Sleigh's `OpBehaviorIntLeft` returns 0 once the amount reaches the
    /// output width, so the amount must not be masked back into range first.
    #[test]
    fn an_out_of_range_shift_amount_saturates_rather_than_wrapping() {
        assert_eq!(
            int_binary(IntBinaryOp::ShiftLeft, &u(1), &u(0x100), ValueType::int(8)),
            Some(UBig::ZERO)
        );
    }

    #[test]
    fn unary_neg_masks_to_width() {
        assert_eq!(
            int_unary(IntUnaryOp::Neg, &u(1), ValueType::int(8)),
            Some(u(0xFF))
        );
    }

    #[test]
    fn sign_extend_i8_to_i32() {
        assert_eq!(
            sign_extend(&u(0x80), ValueType::int(8), ValueType::int(32)),
            Some(u(0xFFFF_FF80))
        );
    }

    #[test]
    fn popcount_masks_input_width() {
        assert_eq!(popcount(&u(0x1FF), ValueType::int(8)), Some(u(8)));
    }

    #[test]
    fn lzcount_zero_is_width_and_msb_is_zero() {
        assert_eq!(lzcount(&u(0), ValueType::int(8)), Some(u(8)));
        assert_eq!(lzcount(&u(0x80), ValueType::int(8)), Some(UBig::ZERO));
    }

    /// `expected == None` means the fold must skip.
    #[test]
    fn int_binary_masking_and_overflow_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
        }
        #[rustfmt::skip]
        let cases = [
            // INT_MIN / -1 overflows at every narrow signed width too, not just u64.
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Sdiv, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::int(32), expected: None },
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Sdiv, l: 0x8000, r: 0xFFFF, ty: ValueType::int(16), expected: None },
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Sdiv, l: 0x80, r: 0xFF, ty: ValueType::int(8), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Srem, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::int(32), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Srem, l: 0x8000, r: 0xFFFF, ty: ValueType::int(16), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Srem, l: 0x80, r: 0xFF, ty: ValueType::int(8), expected: None },
            // The rows below carry garbage bits above the type width in `l`, so an
            // unmasked eval gives a different (wrong) answer: 0x1FF / 2 = 0xFF, not
            // the correct 0xFF / 2 = 0x7F.
            Case { case: "int_binary_unsigned_div_unmasked_u8", op: IntBinaryOp::Div, l: 0x1FF, r: 2, ty: ValueType::int(8), expected: Some(0x7F) },
            // 7 is chosen to distinguish masked from raw: 0xFFFF % 7 = 1 but
            // 0x1FFFF % 7 = 3.
            Case { case: "int_binary_unsigned_rem_unmasked_u16", op: IntBinaryOp::Rem, l: 0x1FFFF, r: 7, ty: ValueType::int(16), expected: Some(1) },
            Case { case: "int_binary_unsigned_shr_unmasked_u8", op: IntBinaryOp::ShiftRight, l: 0x1FF, r: 1, ty: ValueType::int(8), expected: Some(0x7F) },
        ];
        for c in &cases {
            assert_eq!(
                int_binary(c.op, &u(c.l), &u(c.r), c.ty)
                    .as_ref()
                    .map(carrier),
                c.expected,
                "{}",
                c.case
            );
        }
    }

    /// `I1` is the one type whose `bit_width` is not its byte size times 8, so the
    /// shift guard reads 1 where Sleigh's `8 * sizeout` reads 8, and
    /// `get_signed_int` reads the low bit as the sign.  Neither shape is producible
    /// by the lifter; the rows pin what the guard's comment names as its exception.
    #[test]
    fn int_shift_guard_at_i1_diverges_from_sleigh() {
        // Sleigh: `1 < 8` and `signbit_negative(1, 1 byte) == false`, so `1 >> 1`
        // is 0; strider takes the `r >= bits` branch over a negative `l`.
        assert_eq!(
            int_binary(IntBinaryOp::SShiftRight, &u(1), &u(1), ValueType::I1),
            Some(UBig::ONE),
        );
        // Sleigh reads both bytes as 1 and 0; strider reads the 1-bit `l` as -1.
        assert_eq!(
            int_cmp(IntCmpOp::Sless, &u(1), &u(0), ValueType::I1),
            Some(true)
        );
    }

    /// Input-masking guards, all at I8.
    #[test]
    fn int_cmp_masking_cases() {
        struct Case {
            case: &'static str,
            op: IntCmpOp,
            l: u128,
            r: u128,
            expected: bool,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            // Every row's `l` carries bits above I8, so an unmasked eval flips the
            // expected verdict.
            Case { case: "int_cmp_equal_unmasked_u8", op: IntCmpOp::Equal, l: 0x1FF, r: 0xFF, expected: true, why: "Equal must mask both sides to I8 before comparing" },
            Case { case: "int_cmp_less_unmasked_u8", op: IntCmpOp::Less, l: 0x100, r: 0x01, expected: true, why: "Less must mask both sides to I8 before comparing" },
            Case { case: "int_cmp_carry_unmasked_u8", op: IntCmpOp::Carry, l: 0x100, r: 0, expected: false, why: "Carry must mask both sides before checking overflow" },
        ];
        for c in &cases {
            assert_eq!(
                int_cmp(c.op, &u(c.l), &u(c.r), ValueType::int(8)).unwrap(),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }

    /// The `bits >= 128` arms, which use wrapping add/sub plus sign-bit logic
    /// rather than the narrow range check.
    #[test]
    fn int_cmp_i128_overflow_arms_cases() {
        struct Case {
            case: &'static str,
            op: IntCmpOp,
            l: u128,
            r: u128,
            expected: bool,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            Case { case: "scarry_i128_max_plus_one", op: IntCmpOp::Scarry, l: i128::MAX as u128, r: 1, expected: true, why: "i128::MAX + 1 signed-overflows" },
            Case { case: "sborrow_i128_min_minus_one", op: IntCmpOp::Sborrow, l: i128::MIN as u128, r: 1, expected: true, why: "i128::MIN - 1 signed-overflows" },
            Case { case: "carry_u128_max_plus_one", op: IntCmpOp::Carry, l: u128::MAX, r: 1, expected: true, why: "u128::MAX + 1 unsigned-overflows (sum wraps below l)" },
            // Controls: no overflow on the same arms.
            Case { case: "scarry_i128_zero_no_overflow", op: IntCmpOp::Scarry, l: 0, r: 1, expected: false, why: "0 + 1 does not signed-overflow at I128" },
            Case { case: "sborrow_i128_zero_no_overflow", op: IntCmpOp::Sborrow, l: 0, r: 1, expected: false, why: "0 - 1 = -1 does not signed-overflow at I128 (both operands non-negative)" },
            Case { case: "carry_i128_zero_no_overflow", op: IntCmpOp::Carry, l: 0, r: 1, expected: false, why: "0 + 1 does not unsigned-overflow at I128" },
        ];
        for c in &cases {
            assert_eq!(
                int_cmp(c.op, &u(c.l), &u(c.r), ValueType::int(128)).unwrap(),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }

    /// The value-producing signed arms.
    #[test]
    fn int_binary_signed_value_folds_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
        }
        #[rustfmt::skip]
        let cases = [
            // 2, truncated toward zero.
            Case { case: "sdiv_neg_by_neg_i8", op: IntBinaryOp::Sdiv, l: 0xFB, r: 0xFE, ty: ValueType::int(8), expected: Some(2) },
            // The remainder takes the dividend's sign, then masks back to I8.
            Case { case: "srem_neg_by_pos_i8", op: IntBinaryOp::Srem, l: 0xF9, r: 2, ty: ValueType::int(8), expected: Some(0xFF) },
            // Sign fill, not the logical zero-fill that would give 0x40.
            Case { case: "sshr_neg_one_i8", op: IntBinaryOp::SShiftRight, l: 0x80, r: 1, ty: ValueType::int(8), expected: Some(0xC0) },
        ];
        for c in &cases {
            assert_eq!(
                int_binary(c.op, &u(c.l), &u(c.r), c.ty)
                    .as_ref()
                    .map(carrier),
                c.expected,
                "{}",
                c.case
            );
        }
    }

    // Shifting by the full bit width or beyond zeroes the value in Sleigh
    // (opbehavior.cc:411), and sign-fills for INT_SRIGHT. Reducing the amount
    // modulo the width instead diverges by a factor of 2^bits.

    /// Out-of-range (>= bit-width) shift semantics; each `why` names the Sleigh
    /// rule the row pins.
    #[test]
    fn int_binary_out_of_range_shift_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            Case { case: "int_binary_shl_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 1, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: 1u32 << 32 = 0 (`r >= 8*sizeout` returns 0 per opbehavior.cc:411). A modulo-reduced amount computes `1 << (32 % 32) = 1 << 0 = 1`, diverging from Sleigh." },
            Case { case: "int_binary_shl_at_bit_width_returns_zero_u64", op: IntBinaryOp::ShiftLeft, l: 1, r: 64, ty: ValueType::int(64), expected: Some(0),
                   why: "Sleigh: 1u64 << 64 = 0.  A modulo-reduced amount computes `1 << (64 % 64) = 1`." },
            Case { case: "int_binary_shl_above_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 0xFF, r: 40, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: shift > bit-width still returns 0.  A modulo-reduced amount computes `0xFF << (40 % 32) = 0xFF << 8 = 0xFF00`." },
            Case { case: "int_binary_shr_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: 0xFFFFFFFFu32 >> 32 = 0 per opbehavior.cc:432.  A modulo-reduced amount computes `0xFFFFFFFF >> (32 % 32) = 0xFFFFFFFF`." },
            Case { case: "int_binary_sshr_at_bit_width_negative_returns_all_ones_u32", op: IntBinaryOp::SShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0xFFFF_FFFF),
                   why: "Sleigh: signed-negative >> bit-width fills with the sign bit (= 0xFFFFFFFF) per opbehavior.cc:454-460." },
            Case { case: "int_binary_sshr_at_bit_width_positive_returns_zero_u32", op: IntBinaryOp::SShiftRight, l: 0x7FFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: signed-non-negative >> bit-width = 0 (no sign bit to fill)." },
        ];
        for c in &cases {
            assert_eq!(
                int_binary(c.op, &u(c.l), &u(c.r), c.ty)
                    .as_ref()
                    .map(carrier),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }

    /// Past the carrier, a count at the width, one no `usize` holds and one no
    /// `u128` holds all saturate the way a count at the width does.
    #[test]
    fn a_saturated_shift_past_the_carrier() {
        for w in [256u16, 512] {
            let ty = ValueType::int(w);
            let neg = all_ones(ty) ^ u(1);
            let pos = (UBig::ONE << (usize::from(w) - 2)) + u(3);
            let counts = [UBig::from(w), UBig::ONE << 70usize, UBig::ONE << 128usize];
            for count in &counts {
                for l in [&neg, &pos] {
                    for op in [IntBinaryOp::ShiftLeft, IntBinaryOp::ShiftRight] {
                        assert_eq!(int_binary(op, l, count, ty), Some(UBig::ZERO), "{op:?} {w}");
                    }
                }
                assert_eq!(
                    int_binary(IntBinaryOp::SShiftRight, &neg, count, ty),
                    Some(all_ones(ty))
                );
                assert_eq!(
                    int_binary(IntBinaryOp::SShiftRight, &pos, count, ty),
                    Some(UBig::ZERO)
                );
            }
            let below = UBig::from(w - 1);
            assert_eq!(
                int_binary(IntBinaryOp::SShiftRight, &neg, &below, ty),
                Some(all_ones(ty))
            );
            assert_eq!(
                int_binary(IntBinaryOp::ShiftLeft, &u(1), &below, ty),
                Some(UBig::ONE << (usize::from(w) - 1))
            );
        }
    }

    #[test]
    fn lzcount_past_the_carrier_counts_from_the_declared_top() {
        let v = UBig::ONE << 200usize;
        assert_eq!(lzcount(&v, ValueType::int(256)), Some(u(55)));
        assert_eq!(lzcount(&v, ValueType::int(512)), Some(u(311)));
        assert_eq!(lzcount(&UBig::ZERO, ValueType::int(512)), Some(u(512)));
    }

    #[test]
    fn mask_is_exact_for_either_dashu_form_at_every_width_class() {
        let large = (UBig::ONE << 200usize) + u(0xff);
        for (w, want) in [
            (8, u(0xff)),
            (64, u(0xff)),
            (128, u(0xff)),
            (256, large.clone()),
        ] {
            assert_eq!(mask(&large, ValueType::int(w)), want, "i{w}");
        }
        assert_eq!(mask(&u(u128::MAX), ValueType::int(512)), u(u128::MAX));
        assert_eq!(mask(&u(0x1ff), ValueType::int(8)), u(0xff));
        assert_eq!(mask(&large, ValueType::float(64)), UBig::ZERO);
    }
}
