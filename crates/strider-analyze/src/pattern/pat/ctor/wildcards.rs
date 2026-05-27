//! Wildcard, capture, and constant-literal pattern constructors.

use std::sync::Arc;

use strider_ir::node::{NodeKind, NodeOutputId, NodeOutputType};

use crate::pattern::pat::any::{AnyPat, InputWidthPat, ValueWidthPat, VarPat};
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};
use crate::pattern::pat::{IntoPat, Pat};
use crate::pattern::var::Capture;

/// Matches any single output unconditionally.
#[must_use]
pub fn any() -> Pat {
    Pat::from_dyn(Arc::new(AnyPat))
}

/// Matches any value output that is exactly `n` bits wide.
///
/// The width-limit mechanism for querying by output type: `value_of_width(1)`
/// (a.k.a. [`bool_value`]) selects booleans (the 1-bit `I1`); `value_of_width(32)`
/// matches any `I32`- or `F32`-typed value, etc.
#[must_use]
pub fn value_of_width(n: u32) -> Pat {
    Pat::from_dyn(Arc::new(ValueWidthPat { width: n }))
}

/// Matches any boolean value — i.e. any value output 1 bit wide (`I1`).
/// Sugar for [`value_of_width`]`(1)`.
#[must_use]
pub fn bool_value() -> Pat {
    value_of_width(1)
}

/// Matches `inner` **and** requires all of the matched node's value inputs
/// to be `n` bits wide.  The input-side width filter: `inputs_of_width(1, …)`
/// (a.k.a. [`bool_inputs`]) selects operations that *operate on* booleans
/// (`And`/`Or`/`Xor`/`BitNot` on `I1`) and excludes comparisons (whose
/// operands are wider even though they produce `I1`).
#[must_use]
pub fn inputs_of_width(n: u32, inner: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(InputWidthPat {
        width: n,
        inner: inner.into(),
    }))
}

/// Matches `inner` whose value inputs are all booleans (1-bit `I1`).
/// Sugar for [`inputs_of_width`]`(1, inner)`.
#[must_use]
pub fn bool_inputs(inner: impl Into<Pat>) -> Pat {
    inputs_of_width(1, inner)
}

/// Matches any output and binds it to `c`.
///
/// If `c` is already bound the output must equal the stored binding.
/// Equivalent in behavior to `any().capture(c)`, but constructs a dedicated
/// `VarPat` rather than wrapping `AnyPat` in a `CapturePat` — one
/// fewer vtable hop and no backtracking snapshot per match.
#[must_use]
pub fn var(c: Capture) -> Pat {
    Pat::from_dyn(Arc::new(VarPat { capture: c }))
}

/// Matches an `IntConst` node whose stored value, when masked to the node's
/// declared width, equals `v`'s representation at that same width.
///
/// `v` accepts any signed or unsigned integer literal (`i32`, `i64`, `i128`,
/// `u32`, `u64`).  Negative values are sign-extended to the IntConst's width
/// before comparison, so `int_const(-50)` matches both
/// `IntConst(0xffff_ffce, I32)` and `IntConst(0xffff_ffff_ffff_ffce, I64)` —
/// no per-arch default needed.
///
/// Values larger than `i128::MAX` (e.g. `u128::MAX` as raw bits) require
/// passing an `i128` constructed via `as i128` — the `Into<i128>` conversion
/// reinterprets the high bit as sign.
///
/// In build position (RHS of a rewrite rule), constructs an `IntConst(v
/// masked to the root's output type)` node.
#[must_use]
pub fn int_const(v: impl Into<i128>) -> Pat {
    let v_signed: i128 = v.into();
    let v_unsigned: u128 = v_signed as u128;
    // Discriminant-only prefilter; the width-aware equality is done in
    // post_match where we have the output type.
    NodePat::matcher(
        KindSpec::variant(&NodeKind::IntConst(0u128)),
        InputsSpec::None,
    )
    .with_post_match(Arc::new(move |ctx, node, _b| {
        let NodeKind::IntConst(stored) = *ctx.function.node_kind(node) else {
            return false;
        };
        // Determine the output type from the node's single value output.
        let ty = ctx
            .function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value());
        let Some(ty) = ty else { return false; };
        let mask = ty.bit_mask_u128();
        (stored & mask) == (v_unsigned & mask)
    }))
    .with_build_fn(
        Arc::new(move |ctx| {
            let mask = ctx.root_ty.bit_mask_u128();
            Ok(NodeKind::IntConst(v_unsigned & mask))
        }),
        BuildTy::InheritRoot,
    )
    .into_pat()
}

/// Matches an `IntConst` whose stored value (masked to the node's
/// declared width) equals any value in `values` (also masked to the
/// same width).  Set-membership variant of [`int_const`] — useful when
/// querying a call site whose target may be one of several known
/// addresses, e.g. multiple lock-acquire helpers in a kernel binary:
///
/// ```rust
/// # use strider_analyze::pattern::{call, int_const_any_of};
/// let pat = call().target(int_const_any_of([
///     0xffff_8000_0010_0000u64,  // __mtx_lock
///     0xffff_8000_0010_0040u64,  // __mtx_lock_sleep
///     0xffff_8000_0010_0080u64,  // __mtx_lock_spin
/// ]));
/// # let _ = pat;
/// ```
///
/// An empty `values` set vacuously fails — every membership test is
/// false.  Match-only — has no build-side semantics, so it returns
/// `NotBuildable` from a rewrite-rule RHS context.
#[must_use]
pub fn int_const_any_of<I, V>(values: I) -> Pat
where
    I: IntoIterator<Item = V>,
    V: Into<i128>,
{
    let values_unsigned: Vec<u128> = values.into_iter().map(|v| v.into() as u128).collect();
    NodePat::matcher(
        KindSpec::variant(&NodeKind::IntConst(0u128)),
        InputsSpec::None,
    )
    .with_post_match(Arc::new(move |ctx, node, _b| {
        let NodeKind::IntConst(stored) = *ctx.function.node_kind(node) else {
            return false;
        };
        let ty = ctx
            .function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value());
        let Some(ty) = ty else { return false; };
        let mask = ty.bit_mask_u128();
        let stored_masked = stored & mask;
        values_unsigned
            .iter()
            .any(|&v| stored_masked == (v & mask))
    }))
    .into_pat()
}

/// Matches an `IntConst` whose stored value, interpreted as a signed
/// integer at *some* natural width ≤ the node's output width, equals
/// `v`.  Strictly more permissive than [`int_const`]: also matches
/// the *zero-extended* form of a narrower signed value, which is
/// what compilers emit when a 32-bit signed result feeds a 64-bit
/// register on (e.g.) x86-64.
///
/// Concretely, for each width `w` ∈ {8, 16, 32, 64, 128} with
/// `w ≤ output_width`, the matcher tests whether the stored value:
///   * has its low `w` bits equal to `v` mod `2^w`, AND
///   * is consistent with either a sign-extension or a zero-extension
///     from width `w` to the output width.
///
/// **Why this matters.**  At -O2 on x86-64, `return -50;` lowers to
/// `mov eax, 0xffffffce; ret` — the high 32 bits of `RAX` are zero
/// (zero-extended from a 32-bit move), so the IR carries
/// `IntConst(0x00000000FFFFFFCE)` at I64.  The conventional
/// `int_const(-50)` does an exact-bit-pattern match at the output
/// width and reads that value as `+4294967246`; `signed_int_const(-50)`
/// recognises the I32-narrow signed form and matches.  Symmetrically
/// it also matches the I32-only IntConst and the I64
/// sign-extended `0xFFFFFFFFFFFFFFCE` form, so a single pattern
/// covers every natural width a compiler might emit.
///
/// `int_const(v)` remains the right call when bit-pattern equality
/// at the exact output width is what you want (e.g. low-level
/// rewrites that depend on the storage shape).  Use
/// `signed_int_const(v)` for source-semantic matches like
/// `add(x, signed_int_const(-1))` for `x--`.
#[must_use]
pub fn signed_int_const(v: impl Into<i128>) -> Pat {
    let v_signed: i128 = v.into();
    let v_unsigned: u128 = v_signed as u128;
    NodePat::matcher(
        KindSpec::variant(&NodeKind::IntConst(0u128)),
        InputsSpec::None,
    )
    .with_post_match(Arc::new(move |ctx, node, _b| {
        let NodeKind::IntConst(stored) = *ctx.function.node_kind(node) else {
            return false;
        };
        let Some(ty) = ctx
            .function
            .node_outputs(node)
            .iter()
            .find_map(|&out| ctx.function.output_kind(out).as_value())
        else {
            return false;
        };
        let output_width = ty.bit_width();
        if output_width == 0 {
            return false;
        }
        let output_mask = ty.bit_mask_u128();
        // Iterate widths up to and including the output width.  For each
        // width `w`, treat the stored value as a w-bit signed value (low
        // `w` bits) and check whether that signed value equals `v` AND
        // the high bits above `w` are consistent with either zero- or
        // sign-extension to the output width.
        for &w in &[8usize, 16, 32, 64, 128] {
            if w > output_width {
                break;
            }
            let w_mask: u128 = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
            let low = stored & w_mask;
            let v_low = v_unsigned & w_mask;
            if low != v_low {
                continue;
            }
            // Above `w` bits, within the output type, the stored value
            // must be either all-zero (zero-extended) or all-one (sign-
            // extended, for negative-at-w values).
            let above_w_mask = output_mask & !w_mask;
            let above = stored & above_w_mask;
            if above == 0 {
                return true; // zero-extended form of v at width w
            }
            // Sign-extended only makes sense when the w-th bit is set
            // (i.e. v is negative at width w).
            let sign_bit_w = if w >= 128 { 0 } else { 1u128 << (w - 1) };
            if sign_bit_w != 0 && (low & sign_bit_w) != 0 && above == above_w_mask {
                return true; // sign-extended form
            }
        }
        false
    }))
    // RHS use-site: same encoding as `int_const` — write `v_unsigned`
    // masked to the root type.
    .with_build_fn(
        Arc::new(move |ctx| {
            let mask = ctx.root_ty.bit_mask_u128();
            Ok(NodeKind::IntConst(v_unsigned & mask))
        }),
        BuildTy::InheritRoot,
    )
    .into_pat()
}

/// Matches a boolean constant node with value exactly `v`.  Booleans are
/// `I1` integers, so this matches/builds `IntConst(v as u128)` typed `I1`.
///
/// Unlike [`any_bool_const`], this constructor is also usable on the RHS of
/// a rewrite rule (it builds an `IntConst` typed `I1`), so it deliberately
/// carries no match-time width guard — the exact value pin plus the I1
/// build type are sufficient, and a guard would make it match-only.
#[must_use]
pub fn bool_const(v: bool) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::IntConst(v as u128)), InputsSpec::None)
        .with_build_exact(NodeKind::IntConst(v as u128), BuildTy::Fixed(NodeOutputType::I1))
        .into_pat()
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
#[must_use]
pub fn float_const(bits: u64) -> Pat {
    NodePat::matcher(KindSpec::Exact(NodeKind::FloatConst(bits)), InputsSpec::None)
        .with_build_exact(NodeKind::FloatConst(bits), BuildTy::InheritRoot)
        .into_pat()
}

/// Matches any output for which `f` returns `true`.  Equivalent to
/// `any().when(f)`.
pub fn predicate<F>(f: F) -> Pat
where
    F: Fn(&strider_ir::Graph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
{
    any().when(f)
}

