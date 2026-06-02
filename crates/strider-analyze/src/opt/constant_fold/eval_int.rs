use strider_ir::node::ValueType;
use strider_ir::{IntBinaryOp, IntCmpOp};

use anyhow::anyhow;

use crate::opt::error::Result;

// ── integer constant evaluation ───────────────────────────────────────────────

/// Evaluates `op(l, r)` as an integer arithmetic operation, returning the
/// result masked to `ty`, or `None` if the operation is undefined (e.g.
/// division by zero).
///
/// Both `l` and `r` are masked to `ty.bit_mask_u128()` at entry.
/// IntConst values are normally already masked by `build_int_const`, but
/// re-masking is cheap insurance against any caller passing raw bits.
/// Operations that aren't safe under masking-commutativity (Div, Rem,
/// ShiftRight, signed cmps) need this mask to give correct results.
pub(crate) fn eval_int_binary(
    op: IntBinaryOp,
    l: u128,
    r: u128,
    ty: ValueType,
) -> Option<u128> {
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;
    let bits = ty.bit_width() as u32;
    // Sleigh's `OpBehaviorIntLeft::evaluateBinary` (sleigh/src/opbehavior.cc:411)
    // returns 0 when the shift amount is `>= 8 * sizeout`.  `IntRight` matches.
    // `IntSright` returns `signbit ? calc_mask : 0`.  Mirroring this here keeps
    // the constant-fold's evaluation consistent with Sleigh's runtime semantics
    // — pre-fix the evaluator computed `r % bits` and diverged from Sleigh
    // by the full shift output for any literal `r >= bits`.
    let r_ge_bits = r >= u128::from(bits);
    // Shift arms below only call `shift` inside the `!r_ge_bits` branch,
    // so `s < bits <= u128::from(u32::MAX)` and the truncation is lossless.
    #[allow(clippy::cast_possible_truncation)]
    let shift = |s: u128| -> u32 { s as u32 };
    let raw: u128 = match op {
        IntBinaryOp::Add => l.wrapping_add(r),
        IntBinaryOp::Mul => l.wrapping_mul(r),
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => {
            if r_ge_bits {
                0
            } else {
                l.wrapping_shl(shift(r)) & mask
            }
        }
        IntBinaryOp::ShiftRight => {
            if r_ge_bits {
                0
            } else {
                l.wrapping_shr(shift(r))
            }
        }
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int(l)?;
            if r_ge_bits {
                // Sign-bit-set → fill with all-ones; sign-bit-clear → zero.
                if sl < 0 { mask } else { 0 }
            } else {
                sl.wrapping_shr(shift(r)) as u128 & mask
            }
        }
        IntBinaryOp::Div => {
            if r == 0 {
                return None;
            }
            l / r
        }
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            // Signed overflow: INT_MIN / -1 is undefined for every signed
            // integer width.  At narrow widths the i128 division "looks
            // well-defined" (e.g. -i32::MIN as i128 = 2^31 fits), but the
            // mask-back to ty would silently wrap to INT_MIN — not the
            // mathematical result.  Skip rather than emit a wraparound.
            let int_min: i128 = if bits >= 128 {
                i128::MIN
            } else {
                -(1i128 << (bits - 1))
            };
            if sl == int_min && sr == -1 {
                return None;
            }
            sl.wrapping_div(sr) as u128 & mask
        }
        IntBinaryOp::Rem => {
            if r == 0 {
                return None;
            }
            l % r
        }
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            // Signed-overflow guard: INT_MIN % -1 is mathematically 0 but
            // hardware idiv raises #DE; treat it as undefined and skip,
            // matching the Sdiv case.
            let int_min: i128 = if bits >= 128 {
                i128::MIN
            } else {
                -(1i128 << (bits - 1))
            };
            if sl == int_min && sr == -1 {
                return None;
            }
            sl.wrapping_rem(sr) as u128 & mask
        }
    };
    Some(raw & mask)
}

/// Evaluates a comparison on two constant integer values.
pub(crate) fn eval_int_cmp(op: IntCmpOp, l: u128, r: u128, ty: ValueType) -> Result<bool> {
    // Mask both inputs to ty at entry.  Unsigned comparisons (Equal, Less,
    // LessEqual, Carry) operate on raw u128s and would otherwise return
    // wrong answers for narrow IntConsts that carry high bits beyond the
    // type width.  The signed arms re-mask via get_signed_int so the
    // double-mask is idempotent for them.
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;

    let signed = |v: u128| -> Result<i128> {
        ty.get_signed_int(v)
            .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))
    };
    let unsigned_max = || -> Result<u128> {
        ty.get_unsigned_int(u128::MAX)
            .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))
    };
    let bits = ty.bit_width() as u32;
    let signed_min_max = || -> (i128, i128) {
        if bits >= 128 {
            (i128::MIN, i128::MAX)
        } else {
            let min = -(1i128 << (bits - 1));
            let max = (1i128 << (bits - 1)) - 1;
            (min, max)
        }
    };

    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => signed(l)? < signed(r)?,
        IntCmpOp::Carry => {
            // Unsigned add overflow: l + r > type's max unsigned value.
            // For ty < I128 we can promote to u128 safely.  For I128, detect
            // overflow via wrapping-add semantics: sum < l (a wrapped result
            // is always smaller than its addends).
            if bits >= 128 {
                l.wrapping_add(r) < l
            } else {
                let max = unsigned_max()?;
                l.wrapping_add(r) > max
            }
        }
        IntCmpOp::Scarry => {
            let (min, max) = signed_min_max();
            let sl = signed(l)?;
            let sr = signed(r)?;
            if bits >= 128 {
                // At i128: detect signed overflow via sign-bit logic
                // (same-sign inputs but different-sign output).
                let result = sl.wrapping_add(sr);
                let sign_l = sl < 0;
                let sign_r = sr < 0;
                let sign_res = result < 0;
                sign_l == sign_r && sign_l != sign_res
            } else {
                // SAFETY: at `bits < 128`, both `sl` and `sr` are sign-extended
                // from a narrower type into `i128`, so each lies in
                // `[-2^(bits-1), 2^(bits-1) - 1]`.  Their sum therefore lies
                // in `[-2^bits, 2^bits - 2]`, which fits well inside i128 for
                // any `bits < 128` — no `+` overflow on the host i128.  The
                // overflow we detect here is the source-type's overflow,
                // captured by the `< min || > max` range check.  When the
                // narrow-width invariant fails (e.g. a caller passes raw u128
                // values that `signed()` could not represent), `signed(l)?` /
                // `signed(r)?` short-circuit before reaching the `sl + sr`.
                debug_assert!(
                    bits < 128 && sl >= min && sl <= max && sr >= min && sr <= max,
                    "Scarry narrow-width invariant violated: bits={bits} \
                     sl={sl} sr={sr} min={min} max={max}"
                );
                let result = sl + sr;
                result < min || result > max
            }
        }
        IntCmpOp::Sborrow => {
            let (min, max) = signed_min_max();
            let sl = signed(l)?;
            let sr = signed(r)?;
            if bits >= 128 {
                let result = sl.wrapping_sub(sr);
                let sign_l = sl < 0;
                let sign_r = sr < 0;
                let sign_res = result < 0;
                sign_l != sign_r && sign_l != sign_res
            } else {
                // SAFETY: identical reasoning to `Scarry` above — at
                // `bits < 128` both operands sign-extend into
                // `[-2^(bits-1), 2^(bits-1) - 1]`, so `sl - sr` lies in
                // `[-2^bits + 1, 2^bits - 1]`, comfortably inside i128 with
                // no host-side `-` overflow.  The narrow-width signed
                // overflow we want to detect is captured by the subsequent
                // `< min || > max` range check.
                debug_assert!(
                    bits < 128 && sl >= min && sl <= max && sr >= min && sr <= max,
                    "Sborrow narrow-width invariant violated: bits={bits} \
                     sl={sl} sr={sr} min={min} max={max}"
                );
                let result = sl - sr;
                result < min || result > max
            }
        }
    })
}
