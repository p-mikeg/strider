//! Two's-complement integer evaluation at a declared width, over unbounded
//! [`UBig`]. Total at every integer width; `None` means a divide by zero, a
//! signed `INT_MIN / -1` or `INT_MIN % -1`, or a float `ty`.
//!
//! [`bit`], [`mask_below`] and [`sign_bit`] are `u128` helpers instead.

use dashu_int::ops::BitTest;
use dashu_int::{IBig, UBig};

use crate::node::{IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType};

/// `v` with every bit above `ty`'s width cleared; 0 for a float.
#[must_use]
pub fn mask(v: &UBig, ty: ValueType) -> UBig {
    match ty {
        // Up to 128 bits, mask without allocating.
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

/// 0 for a float.
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

/// A run of `bits` ones at `[shift, shift + bits)`. Not masked to any type.
#[must_use]
pub fn ones_at(bits: usize, shift: usize) -> UBig {
    ((UBig::ONE << bits) - UBig::ONE) << shift
}

/// `v`'s little-endian bytes, zero-padded to `ty`'s byte size (which
/// `UBig::to_le_bytes` trims).
#[cfg(any(test, feature = "test-util"))]
#[must_use]
pub fn le_bytes(v: &UBig, ty: ValueType) -> Vec<u8> {
    let mut out = v.to_le_bytes().into_vec();
    out.resize(ty.byte_size(), 0);
    out
}

/// `1u128 << n`, but 0 for `n >= 128`.
#[inline]
#[must_use]
pub const fn bit(n: u32) -> u128 {
    if n >= 128 { 0 } else { 1u128 << n }
}

/// The low `bits` bits set, saturating to `u128::MAX` at 128 and past.
#[inline]
#[must_use]
pub const fn mask_below(bits: u32) -> u128 {
    bit(bits).wrapping_sub(1)
}

/// The top bit of a `bits`-wide integer; 0 at width 0 and past 128.
#[inline]
#[must_use]
pub const fn sign_bit(bits: u32) -> u128 {
    bit(bits.wrapping_sub(1))
}

/// `None` for a float.
fn int_bits(ty: ValueType) -> Option<usize> {
    match ty {
        ValueType::Int(w) => Some(w.into()),
        ValueType::Float(_) => None,
    }
}

/// `v` read as two's complement at `ty`; `None` for a float.
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

/// `v` wrapped to `ty`'s width; 0 for a float.
#[must_use]
pub fn unsigned(v: IBig, ty: ValueType) -> UBig {
    v & all_ones(ty)
}

fn signed_min(ty: ValueType) -> IBig {
    -(IBig::ONE << ty.bit_width().saturating_sub(1))
}

fn signed_max(ty: ValueType) -> IBig {
    (IBig::ONE << ty.bit_width().saturating_sub(1)) - IBig::ONE
}

/// Whether a shift by `amount` is at or past `ty`'s bit width, where Sleigh
/// answers 0 (the sign fill for `IntSright`); Sleigh's bound is 8 bits at
/// `I1`. Takes the raw, unmasked amount.
#[inline]
#[must_use]
pub fn shift_out_of_range(amount: u128, ty: ValueType) -> bool {
    amount >= ty.bit_width() as u128
}

/// Result masked to `ty`. Operands are masked first, except a shift amount,
/// which is clamped to the width before masking.
#[inline]
pub fn int_binary(op: IntBinaryOp, l: &UBig, r: &UBig, ty: ValueType) -> Option<UBig> {
    let bits = int_bits(ty)?;
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
            // `INT_MIN / -1` is left unfolded at every width: Ghidra's `intb`
            // division is UB at 64 bits and machines disagree (x86 `idiv` traps).
            if sr == IBig::ZERO || (sl == signed_min(ty) && sr == IBig::NEG_ONE) {
                return None;
            }
            // Truncates toward zero, as Sleigh does.
            unsigned(sl / sr, ty)
        }
        IntBinaryOp::Srem => {
            let (sl, sr) = (signed(&l, ty)?, signed(&r, ty)?);
            // INT_MIN % -1 is 0; left unfolded, as Sdiv.
            if sr == IBig::ZERO || (sl == signed_min(ty) && sr == IBig::NEG_ONE) {
                return None;
            }
            // Takes the dividend's sign, as Sleigh does.
            unsigned(sl % sr, ty)
        }
    };
    Some(mask(&raw, ty))
}

/// `None` for a float.
#[inline]
pub fn int_cmp(op: IntCmpOp, l: &UBig, r: &UBig, ty: ValueType) -> Option<bool> {
    int_bits(ty)?;
    let l = mask(l, ty);
    let r = mask(r, ty);

    Some(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => signed(&l, ty)? < signed(&r, ty)?,
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

/// `None` for a float.
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

/// `None` for a float.
#[inline]
pub fn popcount(v: &UBig, in_ty: ValueType) -> Option<UBig> {
    int_bits(in_ty)?;
    Some(UBig::from(mask(v, in_ty).count_ones()))
}

/// Leading zeros over `8 * byte_size` bits, as Ghidra's `OpBehaviorLzcount`
/// counts (differs from `bit_width` only at `I1`); `None` for a float.
#[inline]
pub fn lzcount(v: &UBig, in_ty: ValueType) -> Option<UBig> {
    int_bits(in_ty)?;
    Some(UBig::from(
        (in_ty.byte_size() * 8).saturating_sub(mask(v, in_ty).bit_len()),
    ))
}

#[cfg(test)]
mod tests {
    /// The variants are public and only the constructors check the width, so
    /// `Int(0)` reaches these `pub fn`s.
    #[test]
    fn the_width_primitives_are_total_at_width_zero() {
        use super::{all_ones, mask};
        use crate::UBig;
        use crate::node::ValueType;
        assert_eq!(all_ones(ValueType::Int(0)), UBig::ZERO);
        assert_eq!(mask(&UBig::from(0xffu32), ValueType::Int(0)), UBig::ZERO);
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

    /// `ones_at` and its complement at the sub-register windows the lifter
    /// builds: (set bits, shift, container, ones_at, preserve).
    #[test]
    fn sub_register_window_masks() {
        #[rustfmt::skip]
        let rows: [(usize, usize, u16, u128, u128); 10] = [
            (128, 0, 128, u128::MAX, 0),
            (32, 0, 128, 0xFFFF_FFFF, !0xFFFF_FFFF),
            // d0 / the upper 8 bytes of q0
            (64, 0, 128, 0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF << 64),
            (64, 64, 128, 0xFFFF_FFFF_FFFF_FFFF << 64, 0xFFFF_FFFF_FFFF_FFFF),
            (8, 0, 32, 0xFF, 0xFFFF_FF00),
            (8, 8, 32, 0xFF00, 0xFFFF_00FF),
            (8, 16, 32, 0xFF_0000, 0xFF00_FFFF),
            (8, 24, 32, 0xFF00_0000, 0x00FF_FFFF),
            (48, 0, 64, 0xFFFF_FFFF_FFFF, 0xFFFF << 48),
            (80, 0, 128, (1 << 80) - 1, !((1 << 80) - 1)),
        ];
        for (bits, shift, w, ones, kept) in rows {
            assert_eq!(
                carrier(&ones_at(bits, shift)),
                ones,
                "ones_at({bits}, {shift})"
            );
            assert_eq!(
                preserve_u128(bits, shift, ValueType::int(w)),
                kept,
                "preserve({bits}, {shift}, i{w})"
            );
        }
    }

    /// The scalar write's mask and the zero-writes' masks are disjoint and
    /// tile the container, so no stale bits survive. `s0` lives in the 32-byte
    /// `z0`, exercising the multi-limb path.
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

    /// A float type has no integer bits: every value masks to 0.
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

    /// Each answers at the declared width past the `u128` carrier.
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

    /// At `I1` the shift guard reads width 1 where Sleigh reads 8, and the low
    /// bit is the sign. The lifter never produces either shape.
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
    }
}
