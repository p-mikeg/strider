//! Differential fuzzer: random IR, run on the reference interpreter
//! (`strider_ir_test_utils::interp`) before and after optimisation.
//!
//! A CONSISTENCY oracle, not an independent one: the interpreter runs on both
//! sides, so a misconception it shares with a pass is invisible here. What it
//! catches is a pass changing the meaning of a graph.
//!
//! Both generators emit integer arithmetic, compares, shifts,
//! extend/truncate, popcount/lzcount, phis and `If` / `Return` /
//! `Unreachable` control, and nothing else: no `Load`, `Store`, `Call`,
//! `CallOther`, `Switch` or float node, so `LoadForward`, the memory and
//! SP-alias analyses and every float fold are outside what this checks.
//!
//! Four environment variables scale a run past the committed default:
//! `FUZZ_N` seeds, `FUZZ_START` the first of them, `FUZZ_STEPS` nodes per DAG,
//! and `FUZZ_PASS` runs `cf` / `kb` / `flag` alone instead of the whole
//! pipeline.

use crate::post_opt::indirect_branch_resolve::index_range;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir::{UBig, eval};
use strider_ir_test_utils::interp::{self, EntryVar, Outcome, Slot};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::test_support::Rng;

pub(crate) const TYPES: &[(u32, ValueType)] = &[
    (1, ValueType::int(8)),
    (2, ValueType::int(16)),
    (3, ValueType::int(24)),
    (4, ValueType::int(32)),
    (5, ValueType::int(40)),
    (8, ValueType::int(64)),
    (9, ValueType::int(72)),
    (16, ValueType::int(128)),
];

/// The `u128` carrier mask, for the index-range oracles below, whose values
/// are `I32` and `I64` throughout.
fn mask(ty: ValueType) -> u128 {
    ty.int_mask_u128()
        .expect("an index-range oracle draws integers of at most 128 bits")
}

/// `a - b` in `ty`'s two's-complement ring.
fn wrapping_sub(a: &UBig, b: &UBig, ty: ValueType) -> UBig {
    eval::mask(&(a + eval::complement(b, ty) + UBig::ONE), ty)
}

pub(crate) struct Gen<'r> {
    pub rng: &'r mut Rng,
    pub pools: FxHashMap<ValueType, Vec<ValueId>>,
}

impl Gen<'_> {
    fn push(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.pools.entry(ty).or_default().push(v);
        v
    }
    fn pick_ty(&mut self) -> ValueType {
        let tys: Vec<ValueType> = self
            .pools
            .keys()
            .copied()
            .filter(|t| *t != ValueType::I1)
            .collect();
        let mut tys = tys;
        tys.sort_by_key(|t| t.bit_width());
        tys[self.rng.below(tys.len())]
    }
    fn special(&mut self, ty: ValueType) -> UBig {
        let m = eval::all_ones(ty);
        let w = ty.bit_width();
        let c = match self.rng.below(14) {
            0 => UBig::ZERO,
            1 => UBig::ONE,
            2 => UBig::from(2u8),
            3 => m,
            4 => m >> 1usize,
            5 => (m >> 1usize) + UBig::ONE,
            6 => UBig::from(w - 1),
            7 => UBig::from(w),
            8 => UBig::from(w + 1),
            9 => UBig::from(self.rng.u128() % 16),
            10 => m - UBig::from(self.rng.u128() % 4),
            11 => UBig::ONE << self.rng.below(w),
            12 => eval::complement(&((UBig::ONE << self.rng.below(8)) - UBig::ONE), ty),
            _ => UBig::from(self.rng.u128()),
        };
        eval::mask(&c, ty)
    }
    fn konst(&mut self, b: &mut FunctionBuilder, ty: ValueType) -> ValueId {
        let c = self.special(ty);
        b.build_int_const(c, ty).unwrap()
    }
    fn val(&mut self, b: &mut FunctionBuilder, ty: ValueType) -> ValueId {
        if ty == ValueType::I1 && self.rng.chance(10) {
            return b.build_boolean_const(self.rng.chance(50));
        }
        let pool = self.pools.get(&ty).cloned().unwrap_or_default();
        if pool.is_empty() || (ty != ValueType::I1 && self.rng.chance(25)) {
            if ty == ValueType::I1 {
                return b.build_boolean_const(self.rng.chance(50));
            }
            return self.konst(b, ty);
        }
        pool[self.rng.below(pool.len())]
    }
    fn var_or_val(&mut self, b: &mut FunctionBuilder, ty: ValueType) -> ValueId {
        let pool = self.pools.get(&ty).cloned().unwrap_or_default();
        let nonconst: Vec<ValueId> = pool
            .iter()
            .copied()
            .filter(|&v| b.function().int_const(v).is_none())
            .collect();
        if nonconst.is_empty() {
            self.val(b, ty)
        } else {
            nonconst[self.rng.below(nonconst.len())]
        }
    }

    pub fn step(&mut self, b: &mut FunctionBuilder) {
        let ty = self.pick_ty();
        match self.rng.below(40) {
            0..=11 => {
                let op = IntBinaryOp::ALL[self.rng.below(IntBinaryOp::ALL.len())];
                let x = self.var_or_val(b, ty);
                let y = self.val(b, ty);
                let v = b.build_int_binary_operation(x, y, op, ty).unwrap();
                self.push(v, ty);
            }
            12 => {
                let x = self.var_or_val(b, ty);
                let v = b.build_int_unary_operation(x, IntUnaryOp::Neg, ty).unwrap();
                self.push(v, ty);
            }
            13 => {
                // Sleigh IntNeg lowered: complement.
                let x = self.var_or_val(b, ty);
                let ones = b.build_int_const(eval::all_ones(ty), ty).unwrap();
                let v = b
                    .build_int_binary_operation(x, ones, IntBinaryOp::Xor, ty)
                    .unwrap();
                self.push(v, ty);
            }
            14..=16 => {
                let op = IntCmpOp::ALL[self.rng.below(IntCmpOp::ALL.len())];
                let x = self.var_or_val(b, ty);
                let y = self.val(b, ty);
                let v = b.build_int_cmp_operation(x, y, op, ty).unwrap();
                self.push(v, ValueType::I1);
            }
            17..=19 => {
                // bool ops
                let x = self.val(b, ValueType::I1);
                let v = match self.rng.below(3) {
                    0 => {
                        let one = b.build_boolean_const(true);
                        b.build_int_binary_operation(x, one, IntBinaryOp::Xor, ValueType::I1)
                    }
                    1 => {
                        let y = self.val(b, ValueType::I1);
                        b.build_int_binary_operation(x, y, IntBinaryOp::And, ValueType::I1)
                    }
                    _ => {
                        let y = self.val(b, ValueType::I1);
                        b.build_int_binary_operation(x, y, IntBinaryOp::Or, ValueType::I1)
                    }
                }
                .unwrap();
                self.push(v, ValueType::I1);
            }
            20 | 21 => {
                // truncate / extend
                let src_ty = if self.rng.chance(20) {
                    ValueType::I1
                } else {
                    ty
                };
                let x = self.var_or_val(b, src_ty);
                let (_, dst) = TYPES[self.rng.below(TYPES.len())];
                let v = if dst.bit_width() < src_ty.bit_width() {
                    b.build_single_output_pure(NodeKind::Truncate, [x], dst)
                } else if dst.bit_width() > src_ty.bit_width() {
                    let op = if self.rng.chance(50) {
                        ExtendOp::ZeroExtend
                    } else {
                        ExtendOp::SignExtend
                    };
                    b.build_single_output_pure(NodeKind::Extend(op), [x], dst)
                } else {
                    return;
                };
                self.push(v, dst);
                if self.rng.chance(30) && dst != ValueType::I1 {
                    let t = b.build_single_output_pure(NodeKind::Truncate, [v], ValueType::I1);
                    self.push(t, ValueType::I1);
                }
            }
            22 => {
                let x = self.var_or_val(b, ty);
                let (_, out) = TYPES[self.rng.below(TYPES.len())];
                let v = if self.rng.chance(50) {
                    b.build_popcount(x, out)
                } else {
                    b.build_lzcount(x, out)
                }
                .unwrap();
                self.push(v, out);
            }
            23..=26 => self.flag_tree(b, ty),
            27..=30 => self.cf_shape(b, ty),
            31 | 32 => self.cr_pack(b),
            33 | 34 => self.const_cmp_shape(b, ty),
            _ => {
                // Subtraction canonical form.
                let x = self.var_or_val(b, ty);
                let y = self.val(b, ty);
                let n = b.build_int_unary_operation(y, IntUnaryOp::Neg, ty).unwrap();
                let v = b
                    .build_int_binary_operation(x, n, IntBinaryOp::Add, ty)
                    .unwrap();
                self.push(v, ty);
            }
        }
    }

    fn not(&mut self, b: &mut FunctionBuilder, x: ValueId) -> ValueId {
        let one = b.build_boolean_const(true);
        b.build_int_binary_operation(x, one, IntBinaryOp::Xor, ValueType::I1)
            .unwrap()
    }
    fn band(&mut self, b: &mut FunctionBuilder, x: ValueId, y: ValueId) -> ValueId {
        let (x, y) = if self.rng.chance(50) { (x, y) } else { (y, x) };
        b.build_int_binary_operation(x, y, IntBinaryOp::And, ValueType::I1)
            .unwrap()
    }
    fn bor(&mut self, b: &mut FunctionBuilder, x: ValueId, y: ValueId) -> ValueId {
        let (x, y) = if self.rng.chance(50) { (x, y) } else { (y, x) };
        b.build_int_binary_operation(x, y, IntBinaryOp::Or, ValueType::I1)
            .unwrap()
    }
    fn cmp(
        &mut self,
        b: &mut FunctionBuilder,
        x: ValueId,
        y: ValueId,
        op: IntCmpOp,
        ty: ValueType,
    ) -> ValueId {
        b.build_int_cmp_operation(x, y, op, ty).unwrap()
    }

    fn flag_tree(&mut self, b: &mut FunctionBuilder, ty: ValueType) {
        let a = self.var_or_val(b, ty);
        let bb = self.val(b, ty);
        let (a, bb) = if self.rng.chance(15) { (a, a) } else { (a, bb) };
        let nb = b
            .build_int_unary_operation(bb, IntUnaryOp::Neg, ty)
            .unwrap();
        let diff = b
            .build_int_binary_operation(a, nb, IntBinaryOp::Add, ty)
            .unwrap();
        let zero = b.build_int_const(0u128, ty).unwrap();
        let z = self.cmp(b, diff, zero, IntCmpOp::Equal, ty);
        let n = self.cmp(b, diff, zero, IntCmpOp::Sless, ty);
        let lt = self.cmp(b, a, bb, IntCmpOp::Less, ty);
        let c = self.not(b, lt);
        let v = self.cmp(b, a, bb, IntCmpOp::Sborrow, ty);
        let eqab = self.cmp(b, a, bb, IntCmpOp::Equal, ty);
        let slt = self.cmp(b, a, bb, IntCmpOp::Sless, ty);
        let zty = [ValueType::int(8), ValueType::int(32)][self.rng.below(2)];
        let zn = b.build_single_output_pure(NodeKind::Extend(ExtendOp::ZeroExtend), [n], zty);
        let zv = b.build_single_output_pure(NodeKind::Extend(ExtendOp::ZeroExtend), [v], zty);
        let (zn, zv) = if self.rng.chance(50) {
            (zn, zv)
        } else {
            (zv, zn)
        };
        let ge = self.cmp(b, zn, zv, IntCmpOp::Equal, zty);
        let out = match self.rng.below(16) {
            0 => z,
            1 => {
                let nz = self.not(b, z);
                self.band(b, c, nz)
            }
            2 => {
                let ne = self.not(b, eqab);
                let nl = self.not(b, lt);
                self.band(b, ne, nl)
            }
            3 => self.bor(b, lt, z),
            4 => self.bor(b, eqab, lt),
            5 => self.not(b, ge),
            6 => ge,
            7 => {
                let nz = self.not(b, z);
                self.band(b, nz, ge)
            }
            8 => {
                let ne = self.not(b, eqab);
                let ns = self.not(b, slt);
                self.band(b, ne, ns)
            }
            9 => {
                let nge = self.not(b, ge);
                self.bor(b, z, nge)
            }
            10 => self.bor(b, eqab, slt),
            11 => {
                // Thumb's carry test through a widened flag: `zext(c) == 0`.
                let zb =
                    b.build_single_output_pure(NodeKind::Extend(ExtendOp::ZeroExtend), [c], zty);
                let z0 = b.build_int_const(0u128, zty).unwrap();
                let e = self.cmp(b, zb, z0, IntCmpOp::Equal, zty);
                if self.rng.chance(50) {
                    self.not(b, e)
                } else {
                    e
                }
            }
            12 => {
                // Mixed: raw diff with different operands (should NOT fold).
                let other = self.val(b, ty);
                let lt2 = self.cmp(b, a, other, IntCmpOp::Less, ty);
                self.bor(b, lt2, z)
            }
            13 => {
                let nl = self.not(b, lt);
                self.bor(b, nl, z)
            }
            14 => n,
            _ => {
                let ns = self.not(b, slt);
                self.band(b, ns, eqab)
            }
        };
        for x in [z, n, c, v, out] {
            self.push(x, ValueType::I1);
        }
    }

    fn const_cmp_shape(&mut self, b: &mut FunctionBuilder, ty: ValueType) {
        let a = self.var_or_val(b, ty);
        let n = self.special(ty);
        let out = match self.rng.below(6) {
            0 | 1 => {
                // Or(Less(a,N), Equal(Add(a,M),0)), HI dual
                let mv = if self.rng.chance(70) {
                    wrapping_sub(&UBig::ZERO, &n, ty)
                } else {
                    self.special(ty)
                };
                let nc = b.build_int_const(n.clone(), ty).unwrap();
                let mc = b.build_int_const(mv, ty).unwrap();
                let z = b.build_int_const(0u128, ty).unwrap();
                let lt = self.cmp(b, a, nc, IntCmpOp::Less, ty);
                let add = b
                    .build_int_binary_operation(a, mc, IntBinaryOp::Add, ty)
                    .unwrap();
                let eq = self.cmp(b, add, z, IntCmpOp::Equal, ty);
                if self.rng.chance(50) {
                    self.bor(b, lt, eq)
                } else {
                    let nl = self.not(b, lt);
                    let ne = self.not(b, eq);
                    self.band(b, nl, ne)
                }
            }
            2 | 3 => {
                let c1 = self.special(ty);
                let c2 = if self.rng.chance(70) {
                    wrapping_sub(&c1, &n, ty)
                } else {
                    self.special(ty)
                };
                let c1c = b.build_int_const(c1, ty).unwrap();
                let c2c = b.build_int_const(c2, ty).unwrap();
                let nc = b.build_int_const(n.clone(), ty).unwrap();
                let z = b.build_int_const(0u128, ty).unwrap();
                let x = b
                    .build_int_binary_operation(a, c1c, IntBinaryOp::Add, ty)
                    .unwrap();
                let lt = self.cmp(b, x, nc, IntCmpOp::Less, ty);
                let add = b
                    .build_int_binary_operation(a, c2c, IntBinaryOp::Add, ty)
                    .unwrap();
                let eq = self.cmp(b, add, z, IntCmpOp::Equal, ty);
                if self.rng.chance(50) {
                    self.bor(b, lt, eq)
                } else {
                    let nl = self.not(b, lt);
                    let ne = self.not(b, eq);
                    self.band(b, nl, ne)
                }
            }
            4 => {
                // Sless(Shl(x, C), 0)
                let cc = self.special(ty);
                let cc = b.build_int_const(cc, ty).unwrap();
                let sh = b
                    .build_int_binary_operation(a, cc, IntBinaryOp::ShiftLeft, ty)
                    .unwrap();
                let z = b.build_int_const(0u128, ty).unwrap();
                self.cmp(b, sh, z, IntCmpOp::Sless, ty)
            }
            _ => {
                // Equal(op(x, C1), C2)
                let c1 = self.konst(b, ty);
                let c2 = self.konst(b, ty);
                let inner = match self.rng.below(3) {
                    0 => b
                        .build_int_binary_operation(a, c1, IntBinaryOp::Add, ty)
                        .unwrap(),
                    1 => b
                        .build_int_binary_operation(a, c1, IntBinaryOp::Xor, ty)
                        .unwrap(),
                    _ => b.build_int_unary_operation(a, IntUnaryOp::Neg, ty).unwrap(),
                };
                self.cmp(b, inner, c2, IntCmpOp::Equal, ty)
            }
        };
        self.push(out, ValueType::I1);
    }

    fn cr_pack(&mut self, b: &mut FunctionBuilder) {
        let ty = [ValueType::int(8), ValueType::int(32), ValueType::int(64)][self.rng.below(3)];
        let w = ty.bit_width();
        let nterms = 1 + self.rng.below(4);
        let mut pack: Option<ValueId> = None;
        for _ in 0..nterms {
            let pos = if self.rng.chance(85) {
                self.rng.below(4)
            } else {
                self.rng.below(w + 2)
            } as u128;
            let term = match self.rng.below(4) {
                0 | 1 => {
                    let c = self.val(b, ValueType::I1);
                    let z =
                        b.build_single_output_pure(NodeKind::Extend(ExtendOp::ZeroExtend), [c], ty);
                    let p = b.build_int_const(pos, ty).unwrap();
                    b.build_int_binary_operation(z, p, IntBinaryOp::ShiftLeft, ty)
                        .unwrap()
                }
                2 => {
                    let x = self.var_or_val(b, ty);
                    let c = b.build_int_const(1u128 << (pos % w as u128), ty).unwrap();
                    b.build_int_binary_operation(x, c, IntBinaryOp::And, ty)
                        .unwrap()
                }
                _ => self.val(b, ty),
            };
            pack = Some(match pack {
                None => term,
                Some(p) => b
                    .build_int_binary_operation(p, term, IntBinaryOp::Or, ty)
                    .unwrap(),
            });
        }
        let pack = pack.unwrap();
        let k = if self.rng.chance(85) {
            self.rng.below(4)
        } else {
            self.rng.below(w + 2)
        } as u128;
        let src = if k == 0 && self.rng.chance(50) {
            pack
        } else {
            let kc = b.build_int_const(k, ty).unwrap();
            b.build_int_binary_operation(pack, kc, IntBinaryOp::ShiftRight, ty)
                .unwrap()
        };
        let src = if self.rng.chance(30) {
            let one = b.build_int_const(1u128, ty).unwrap();
            b.build_int_binary_operation(src, one, IntBinaryOp::And, ty)
                .unwrap()
        } else {
            src
        };
        let out = if self.rng.chance(50) {
            b.build_single_output_pure(NodeKind::Truncate, [src], ValueType::I1)
        } else {
            let z = b.build_int_const(0u128, ty).unwrap();
            let e = self.cmp(b, src, z, IntCmpOp::Equal, ty);
            self.not(b, e)
        };
        self.push(pack, ty);
        self.push(out, ValueType::I1);
    }

    fn cf_shape(&mut self, b: &mut FunctionBuilder, ty: ValueType) {
        let x = self.var_or_val(b, ty);
        let y = self.var_or_val(b, ty);
        let c1 = self.konst(b, ty);
        let c2 = self.konst(b, ty);
        let c3 = self.konst(b, ty);
        let bin =
            |b: &mut FunctionBuilder, l, r, op| b.build_int_binary_operation(l, r, op, ty).unwrap();
        let neg = |b: &mut FunctionBuilder, l| {
            b.build_int_unary_operation(l, IntUnaryOp::Neg, ty).unwrap()
        };
        use IntBinaryOp::*;
        let v = match self.rng.below(16) {
            0 => {
                let m = bin(b, x, c1, Mul);
                bin(b, x, m, Add)
            }
            1 => {
                let m1 = bin(b, x, c1, Mul);
                let m2 = bin(b, x, c2, Mul);
                bin(b, m1, m2, Add)
            }
            2 => {
                let s = bin(b, x, c1, ShiftLeft);
                bin(b, x, s, Add)
            }
            3 => {
                let m = bin(b, x, c1, Mul);
                let nm = neg(b, m);
                bin(b, x, nm, Add)
            }
            4 => {
                let s1 = bin(b, x, c1, ShiftLeft);
                let s2 = bin(b, x, c2, ShiftLeft);
                let ns2 = neg(b, s2);
                if self.rng.chance(50) {
                    bin(b, s1, ns2, Add)
                } else {
                    bin(b, s1, s2, Add)
                }
            }
            5 => {
                let m = bin(b, x, c1, Mul);
                let s = bin(b, x, c2, ShiftLeft);
                let ns = neg(b, s);
                if self.rng.chance(50) {
                    bin(b, m, ns, Add)
                } else {
                    bin(b, m, s, Add)
                }
            }
            6 => {
                let s = bin(b, x, c1, ShiftLeft);
                let m = bin(b, x, c2, Mul);
                let nm = neg(b, m);
                bin(b, s, nm, Add)
            }
            7 => {
                let m = bin(b, x, c1, Mul);
                let nx = neg(b, x);
                bin(b, m, nx, Add)
            }
            8 => {
                let s = bin(b, x, c1, ShiftLeft);
                let nx = neg(b, x);
                bin(b, s, nx, Add)
            }
            9 => {
                let a = bin(b, x, c1, Add);
                bin(b, a, c2, Add)
            }
            10 => {
                let a = bin(b, x, c1, And);
                bin(b, a, c2, And)
            }
            11 => {
                let a = bin(b, x, c1, And);
                let bb = bin(b, y, c2, And);
                let o = bin(b, a, bb, Or);
                bin(b, o, c3, And)
            }
            12 => {
                let o = bin(b, x, c1, Or);
                bin(b, o, c2, And)
            }
            13 => {
                // Truncate(Or(a, And(C, b))) and Truncate(And(C, x))
                let a = bin(b, c1, y, And);
                let o = if self.rng.chance(50) {
                    bin(b, x, a, Or)
                } else {
                    a
                };
                let (_, dst) = TYPES[self.rng.below(TYPES.len())];
                if dst.bit_width() >= ty.bit_width() {
                    return;
                }
                let t = b.build_single_output_pure(NodeKind::Truncate, [o], dst);
                self.push(t, dst);
                return;
            }
            14 => {
                // Truncate(Mul(Sext a, Sext b)), ext round trips
                let (_, dst) = TYPES[self.rng.below(TYPES.len())];
                if dst.bit_width() <= ty.bit_width() {
                    return;
                }
                let op = if self.rng.chance(50) {
                    ExtendOp::SignExtend
                } else {
                    ExtendOp::ZeroExtend
                };
                let ea = b.build_single_output_pure(NodeKind::Extend(op), [x], dst);
                let eb =
                    b.build_single_output_pure(NodeKind::Extend(ExtendOp::SignExtend), [y], dst);
                let mm = b.build_int_binary_operation(ea, eb, Mul, dst).unwrap();
                let pick = if self.rng.chance(50) { mm } else { ea };
                let t = b.build_single_output_pure(NodeKind::Truncate, [pick], ty);
                self.push(t, ty);
                return;
            }
            _ => {
                // nested casts
                let (_, mid) = TYPES[self.rng.below(TYPES.len())];
                let (_, dst) = TYPES[self.rng.below(TYPES.len())];
                if !(ty.bit_width() < mid.bit_width() && mid.bit_width() < dst.bit_width()) {
                    return;
                }
                let op1 = if self.rng.chance(50) {
                    ExtendOp::SignExtend
                } else {
                    ExtendOp::ZeroExtend
                };
                let op2 = if self.rng.chance(50) {
                    ExtendOp::SignExtend
                } else {
                    ExtendOp::ZeroExtend
                };
                let e1 = b.build_single_output_pure(NodeKind::Extend(op1), [x], mid);
                let e2 = b.build_single_output_pure(NodeKind::Extend(op2), [e1], dst);
                self.push(e2, dst);
                let t1 = b.build_single_output_pure(NodeKind::Truncate, [e2], mid);
                let t2 = b.build_single_output_pure(NodeKind::Truncate, [t1], ty);
                self.push(t1, mid);
                self.push(t2, ty);
                return;
            }
        };
        self.push(v, ty);
    }
}

pub(crate) fn build_random_fn(seed: u64, steps: usize) -> (strider_ir::Function, usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut rs = RegisterSet::new();
    let nvars = 1 + rng.below(3);
    let mut vns = Vec::new();
    for i in 0..nvars {
        let (sz, _) = TYPES[rng.below(TYPES.len())];
        let vn = reg_vn(0x1000 * (i as u64 + 1), sz);
        rs = rs.tracked(vn).arg(vn);
        vns.push(vn);
    }
    let mut b = rs.build_fn_single_region().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let mut g = Gen {
        rng: &mut rng,
        pools: FxHashMap::default(),
    };
    for vn in &vns {
        let v = b.read_variable(vn).unwrap();
        let ty = b.function().value_type_opt(v).unwrap();
        g.push(v, ty);
    }
    for _ in 0..steps {
        g.step(&mut b);
    }
    // Return the last few values of every pool.
    let mut rets: Vec<ValueId> = Vec::new();
    let mut keys: Vec<ValueType> = g.pools.keys().copied().collect();
    keys.sort_by_key(|t| t.bit_width());
    for k in keys {
        let p = &g.pools[&k];
        rets.extend(p.iter().rev().take(6).copied());
    }
    let n = rets.len();
    b.build_return(None, &rets).unwrap();
    b.set_lift_addr(None);
    (b.build().unwrap(), n)
}

pub(crate) fn return_values(f: &strider_ir::Function) -> Vec<ValueId> {
    let g = f.graph();
    use strider_ir::IRWalker;
    let ret = f
        .walk()
        .find(|&n| matches!(g.node_kind(n), NodeKind::Return))
        .unwrap();
    g.node_inputs(ret).into_iter().skip(2).collect()
}

fn input_value(rng: &mut Rng, ty: ValueType) -> UBig {
    let m = eval::all_ones(ty);
    let w = ty.bit_width();
    let v = match rng.below(10) {
        0 => UBig::ZERO,
        1 => m,
        2 => UBig::ONE << (w - 1),
        3 => (UBig::ONE << (w - 1)) - UBig::ONE,
        4 => UBig::from(rng.u128() % 8),
        5 => m - UBig::from(rng.u128() % 8),
        6 => UBig::ONE << rng.below(w),
        _ => UBig::from(rng.u128()),
    };
    eval::mask(&v, ty)
}

/// Returns the number of compared (trial, return value) pairs, or a
/// description of the first mismatch. A pair whose reference value is
/// undefined -- division by zero, `INT_MIN / -1` -- is not counted.
pub(crate) fn check_seed(
    seed: u64,
    steps: usize,
    run: &dyn Fn(&mut strider_ir::Function) -> crate::Result<()>,
) -> Result<usize, String> {
    let (mut f, _) = build_random_fn(seed, steps);
    let before_text = f.to_text(false);
    let rets = return_values(&f);
    let mut rng = Rng(seed ^ 0xDEAD_BEEF_1234_5678 | 1);
    let ntrials = 48;
    let mut vectors: Vec<Vec<u64>> = Vec::new();
    for _ in 0..ntrials {
        vectors.push((0..8).map(|_| rng.next_u64()).collect());
    }
    // Each entry register is drawn at ITS OWN width: a seed drawn at the
    // carrier and masked down leaves every wide register's high limbs zero,
    // which is the arithmetic this checks.
    let entry = |t: usize, e: EntryVar| input_value(&mut Rng(vectors[t][e.index] | 1), e.ty);
    let returned = |f: &strider_ir::Function, t: usize| match run_cfg(f, &|e| entry(t, e)) {
        Ok(Outcome::Return { values, .. }) => Ok(values),
        other => Err(format!("trial {t} ended {other:?}")),
    };
    let expected = (0..ntrials)
        .map(|t| returned(&f, t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("seed {seed}: before the pipeline, {e}\n{before_text}"))?;
    if let Err(e) = run(&mut f) {
        return Err(format!("seed {seed}: pipeline error {e:#}\n{before_text}"));
    }
    let after = return_values(&f);
    if after.len() != rets.len() {
        return Err(format!("seed {seed}: return arity changed"));
    }
    let mut compared = 0usize;
    for (t, wants) in expected.iter().enumerate() {
        let gots = returned(&f, t).map_err(|e| {
            format!(
                "seed {seed}: after the pipeline, {e}\nBEFORE:\n{before_text}\nAFTER:\n{}",
                f.to_text(false)
            )
        })?;
        for (k, (want, got)) in wants.iter().zip(&gots).enumerate() {
            match want.admits(got) {
                Some(true) => compared += usize::from(matches!(want, Slot::Known(_))),
                Some(false) => {
                    return Err(format!(
                        "seed {seed}: ret {k} trial {t}: want {want:?} got {got:?}; entry seeds {:x?}\nBEFORE:\n{before_text}\nAFTER:\n{}",
                        &vectors[t][..4],
                        f.to_text(false)
                    ));
                }
                // The generator emits only kinds the interpreter runs.
                None => {
                    return Err(format!(
                        "seed {seed}: ret {k} trial {t}: {want:?}\n{before_text}"
                    ));
                }
            }
        }
    }
    Ok(compared)
}

/// Random integer DAGs: every return value agrees with the reference
/// interpreter before and after the pipeline.
#[test]
fn the_pipeline_preserves_dag_semantics() {
    let n: u64 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FUZZ_SEEDS);
    let start: u64 = std::env::var("FUZZ_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let steps: usize = std::env::var("FUZZ_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let which = std::env::var("FUZZ_PASS").unwrap_or_default();
    let mut failures = Vec::new();
    let mut compared = 0usize;
    for seed in start..start + n {
        let run = |f: &mut strider_ir::Function| -> crate::Result<()> {
            let mut p = crate::OptimizerPipeline::new();
            match which.as_str() {
                "cf" => p.add(crate::ConstantFold::new()),
                "kb" => p.add(crate::KnownBits),
                "flag" => p.add(crate::FlagCmpCanonicalize::new()),
                _ => p = crate::default_pipeline(),
            }
            p.run(f, &crate::OptCtx::new(None))
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_seed(seed, steps, &run)
        }));
        match r {
            Ok(Ok(c)) => compared += c,
            Ok(Err(e)) => failures.push(e),
            Err(_) => failures.push(format!("seed {seed}: PANIC")),
        }
        if failures.len() >= 3 {
            break;
        }
    }
    // Reported BEFORE the yield floors: three real failures drive the
    // counters far below them, so asserting a floor first would report a
    // miscompile as a yield message and discard its BEFORE/AFTER dumps.
    if !failures.is_empty() {
        panic!(
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n=====\n")
        );
    }
    // A floor on the yield: a generator change or an early `?` that stops
    // producing comparable return values fails here rather than reading green.
    assert!(
        compared >= n as usize * 450,
        "only {compared} value comparisons over {n} seeds"
    );
}

/// `Gen::step` draws an op by index into `ALL`, so a reordered or added
/// variant changes every seed's graph: re-measure the yield floors then.
#[test]
fn the_op_rosters_keep_their_seeded_order() {
    use IntBinaryOp::*;
    use IntCmpOp::*;
    assert_eq!(
        IntBinaryOp::ALL,
        [
            Add,
            And,
            Or,
            Xor,
            Div,
            Sdiv,
            Rem,
            Srem,
            ShiftRight,
            SShiftRight,
            ShiftLeft,
            Mul
        ]
    );
    assert_eq!(IntCmpOp::ALL, [Equal, Sless, Less, Carry, Scarry, Sborrow]);
}

// CFG-level differential fuzzer.

/// The reference runs' environment. The budget is high enough that `Diverge`
/// means a loop, not a long finite run, so the CFG comparison can assert on
/// it.
fn reference_env(entry: &dyn Fn(EntryVar) -> UBig) -> interp::Env<'_> {
    interp::Env {
        entry,
        rom: None,
        seed: 0,
        step_budget: 20_000,
        private: None,
    }
}

fn run_cfg(
    f: &strider_ir::Function,
    entry: &dyn Fn(EntryVar) -> UBig,
) -> Result<Outcome, interp::Unsupported> {
    interp::run(f, &reference_env(entry))
}

/// Whether the optimised `got` may stand for the raw `want`: equal, except
/// that anything refines an undefined raw slot.
fn refines(want: &Outcome, got: &Outcome) -> Result<bool, String> {
    let slots = |want: &[Slot], got: &[Slot]| -> Result<bool, String> {
        if want.len() != got.len() {
            return Ok(false);
        }
        for (w, g) in want.iter().zip(got) {
            match w.admits(g) {
                Some(true) => {}
                Some(false) => return Ok(false),
                None => return Err(format!("the reference cannot evaluate {w:?}")),
            }
        }
        Ok(true)
    };
    let (
        Outcome::Return {
            values: wv,
            writes: ww,
            calls: wc,
        },
        Outcome::Return {
            values: gv,
            writes: gw,
            calls: gc,
        },
    ) = (want, got)
    else {
        return Ok(false);
    };
    if ww != gw || wc.len() != gc.len() || !slots(wv, gv)? {
        return Ok(false);
    }
    for (w, g) in wc.iter().zip(gc) {
        if w.target != g.target || w.writes != g.writes || !slots(&w.args, &g.args)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn build_random_cfg(seed: u64) -> Option<strider_ir::Function> {
    let mut rng = Rng(seed.wrapping_mul(0xA24B_AED4_963E_E407) | 1);
    let nvars = 1 + rng.below(3);
    let mut rs = RegisterSet::new();
    let mut vns = Vec::new();
    for i in 0..nvars {
        let (sz, _) = [(1, ValueType::int(8)), (4, ValueType::int(32))][rng.below(2)];
        let vn = reg_vn(0x1000 * (i as u64 + 1), sz);
        rs = rs.tracked(vn).arg(vn);
        vns.push(vn);
    }
    let mut b = rs.build_fn().ok()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let nregions = 2 + rng.below(8);
    let regions: Vec<_> = (0..nregions)
        .map(|_| b.create_region_all().unwrap())
        .collect();
    b.set_entry_region_all(regions[0]).ok()?;
    for i in 0..nregions {
        b.set_region(regions[i]);
        // assignments
        for _ in 0..rng.below(3) {
            let j = rng.below(nvars);
            let vj = b.read_variable(&vns[j]).ok()?;
            let ty = b.function().value_type_opt(vj)?;
            let k = rng.below(nvars);
            let vk = b.read_variable(&vns[k]).ok()?;
            let tyk = b.function().value_type_opt(vk)?;
            let other = if tyk == ty && rng.chance(50) {
                vk
            } else {
                b.build_int_const(u128::from(rng.next_u64() % 4), ty).ok()?
            };
            let op = [
                IntBinaryOp::Add,
                IntBinaryOp::Xor,
                IntBinaryOp::And,
                IntBinaryOp::Mul,
                IntBinaryOp::Or,
                // A shift by a constant is what `index_range` reads as a
                // scaled hop; without one `ScaleOp` and `const_scale` were
                // unreachable from this generator, and the range oracle below
                // never exercised them.  The count deliberately reaches the
                // output width and past it.
                IntBinaryOp::ShiftLeft,
                IntBinaryOp::ShiftRight,
                IntBinaryOp::SShiftRight,
            ][rng.below(8)];
            let shifting = op.is_shift();
            let rhs = if shifting {
                // 0..=w+1: at and past the width, where the saturating count
                // and the lifter's `|W` guard matter.
                let w = ty.bit_width() as u128 + 2;
                b.build_int_const(u128::from(rng.next_u64()) % w, ty).ok()?
            } else {
                other
            };
            let nv = if rng.chance(20) {
                b.build_int_const(u128::from(rng.next_u64() % 3), ty).ok()?
            } else if rng.chance(15) && ty == ValueType::int(32) {
                // A truncate / extend round trip: the cast ladder `Hop` walks,
                // and the only shape here that changes width.
                let narrow =
                    b.build_single_output_pure(NodeKind::Truncate, [vj], ValueType::int(8));
                let ext = if rng.chance(50) {
                    ExtendOp::ZeroExtend
                } else {
                    ExtendOp::SignExtend
                };
                b.build_single_output_pure(NodeKind::Extend(ext), [narrow], ty)
            } else {
                b.build_int_binary_operation(vj, rhs, op, ty).ok()?
            };
            b.write_variable(&vns[j], nv).ok()?;
        }
        let is_last = i + 1 == nregions;
        let choice = if is_last { 0 } else { rng.below(10) };
        match choice {
            0 | 1 => {
                let vals: Vec<ValueId> =
                    vns.iter().map(|vn| b.read_variable(vn).unwrap()).collect();
                b.build_return(None, &vals).ok()?;
            }
            2 => b.build_unreachable().ok()?,
            3 | 4 => {
                let t = regions[rng.below(nregions)];
                b.build_branch(t).ok()?;
            }
            _ => {
                let j = rng.below(nvars);
                let vj = b.read_variable(&vns[j]).ok()?;
                let ty = b.function().value_type_opt(vj)?;
                let mut cond = match rng.below(5) {
                    0 => b.build_boolean_const(rng.chance(50)),
                    1 => {
                        let c = b.build_int_const(u128::from(rng.next_u64() % 3), ty).ok()?;
                        let d = b.build_int_const(u128::from(rng.next_u64() % 3), ty).ok()?;
                        b.build_int_cmp_operation(c, d, IntCmpOp::Equal, ty).ok()?
                    }
                    _ => {
                        let c = b.build_int_const(u128::from(rng.next_u64() % 5), ty).ok()?;
                        let op = [IntCmpOp::Equal, IntCmpOp::Less, IntCmpOp::Sless][rng.below(3)];
                        if rng.chance(50) {
                            b.build_int_cmp_operation(vj, c, op, ty).ok()?
                        } else {
                            b.build_int_cmp_operation(c, vj, op, ty).ok()?
                        }
                    }
                };
                if rng.chance(30) {
                    let one = b.build_boolean_const(true);
                    cond = b
                        .build_int_binary_operation(cond, one, IntBinaryOp::Xor, ValueType::I1)
                        .ok()?;
                }
                let t = regions[rng.below(nregions)];
                let e = regions[rng.below(nregions)];
                if t == e {
                    b.build_branch(t).ok()?;
                } else {
                    b.build_if(cond, t, e).ok()?;
                }
            }
        }
    }
    b.set_lift_addr(None);
    let f = b.build().ok()?;
    strider_ir::validate::validate(&f).ok()?;
    Some(f)
}

/// Random CFGs: every return value agrees with a control-flow interpreter
/// over the sea of nodes before and after the pipeline.
#[test]
fn the_pipeline_preserves_cfg_semantics() {
    let n: u64 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FUZZ_SEEDS);
    let start: u64 = std::env::var("FUZZ_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut failures = Vec::new();
    let (mut built, mut compared) = (0, 0);
    for seed in start..start + n {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(bool, usize), String> {
                let Some(mut f) = build_random_cfg(seed) else {
                    return Ok((false, 0));
                };
                let before_text = f.to_text(false);
                let mut outcomes = Vec::new();
                let mut rng = Rng(seed | 1);
                let vecs: Vec<Vec<u64>> = (0..16)
                    .map(|_| (0..4).map(|_| rng.next_u64()).collect())
                    .collect();
                for vec in &vecs {
                    let want = run_cfg(&f, &|e| input_value(&mut Rng(vec[e.index] | 1), e.ty))
                    .map_err(|why| {
                        format!("seed {seed}: inputs {vec:?} before the pipeline: {why:?}\n{before_text}")
                    })?;
                    outcomes.push(want);
                }
                crate::default_pipeline()
                    .run(&mut f, &crate::OptCtx::new(None))
                    .map_err(|e| format!("seed {seed}: pipeline error {e:#}\n{before_text}"))?;
                let mut cmp = 0;
                for (vec, want) in vecs.iter().zip(&outcomes) {
                    let dump = |what: String| {
                        format!(
                            "seed {seed}: inputs {vec:?} {what}\nBEFORE:\n{before_text}\nAFTER:\n{}",
                            f.to_text(false)
                        )
                    };
                    let got = run_cfg(&f, &|e| input_value(&mut Rng(vec[e.index] | 1), e.ty))
                        .map_err(|why| dump(format!("after the pipeline: {why:?}")))?;
                    match want {
                        Outcome::Return { .. } => {
                            cmp += 1;
                            if !refines(want, &got).map_err(dump)? {
                                return Err(dump(format!("want {want:?} got {got:?}")));
                            }
                        }
                        Outcome::Unreachable { .. } => {
                            if !matches!(got, Outcome::Unreachable { .. }) {
                                return Err(format!(
                                    "seed {seed}: unreachable became {got:?}\n{before_text}"
                                ));
                            }
                        }
                        // Folding a non-terminating loop into a `Return` is a
                        // miscompile, and the one `DeadBranchElimination`'s
                        // exit-free-cycle guard and `LoadForward`'s back-edge
                        // handling exist to prevent.  `Unreachable` is not an
                        // honest second answer: no pass here CONSTRUCTS one.
                        // `Diverge` also covers budget exhaustion, so a long
                        // but terminating run can reach a real sink once
                        // folded. The same truncation already produced `Ret`,
                        // which this arm already rejected, so accepting
                        // `Unreachable` closed nothing and hid a rerouted
                        // sink.
                        Outcome::Diverge(_) => {
                            if !matches!(got, Outcome::Diverge(_)) {
                                return Err(format!(
                                    "seed {seed}: a non-terminating run became \
                                     {got:?}\n{before_text}\nAFTER:\n{}",
                                    f.to_text(false)
                                ));
                            }
                        }
                    }
                }
                Ok((true, cmp))
            },
        ));
        match r {
            Ok(Ok((b, c))) => {
                built += usize::from(b);
                compared += c;
            }
            Ok(Err(e)) => failures.push(e),
            Err(_) => failures.push(format!("seed {seed}: PANIC")),
        }
        if failures.len() >= 3 {
            break;
        }
    }
    // Reported BEFORE the yield floors: three real failures drive the
    // counters far below them, so asserting a floor first would report a
    // miscompile as a yield message and discard its BEFORE/AFTER dumps.
    if !failures.is_empty() {
        panic!(
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n=====\n")
        );
    }
    // Floors on the yield. `build_random_cfg` bails for most seeds, so a
    // change that drops it to none must not read as a pass.
    assert!(
        built >= n as usize / 5,
        "only {built} of {n} seeds built a CFG"
    );
    assert!(
        compared >= built * 4,
        "{built} CFGs yielded only {compared} returning runs"
    );
}

// Targeted proofs.

/// `op(C1, C2)` at I256 with both constants past the u128 carrier. The fold
/// runs at the declared width, so the result carries all four limbs, none of
/// them lost to a 128-bit carrier.
#[test]
fn a_wide_constant_folds_across_every_limb() {
    fn le_limbs(f: &strider_ir::Function, v: ValueId) -> Vec<u64> {
        let ty = f.value_kind(v).as_value().expect("a typed value");
        assert!(ty.bit_width() > 64, "{v:?} is not a wide constant");
        strider_ir::eval::le_bytes(f.int_const(v).expect("an int const"), ty)
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }
    let w1 = [1u64, 2, 3, 4];
    let w2 = [5u64, 6, 7, 8];
    // No limb carries, so each result limb is the op of the two input limbs.
    let cases = [
        (IntBinaryOp::And, [1u64, 2, 3, 0]),
        (IntBinaryOp::Add, [6, 8, 10, 12]),
        (IntBinaryOp::Xor, [4, 4, 4, 12]),
    ];
    for (op, expected) in cases {
        let var = reg_vn(0x1000, 32);
        let mut b = RegisterSet::new()
            .tracked(var)
            .build_fn_single_region()
            .unwrap();
        b.set_lift_addr(Some(0xA0));
        let c1 = b
            .build_int_const(UBig::from_words(&w1), ValueType::int(256))
            .unwrap();
        let c2 = b
            .build_int_const(UBig::from_words(&w2), ValueType::int(256))
            .unwrap();
        let v = b
            .build_int_binary_operation(c1, c2, op, ValueType::int(256))
            .unwrap();
        b.build_return(None, &[v]).unwrap();
        b.set_lift_addr(None);
        let mut f = b.build().unwrap();
        crate::default_pipeline()
            .run(&mut f, &crate::OptCtx::new(None))
            .unwrap_or_else(|e| panic!("{op:?}: {e:#}"));
        let ret = return_values(&f);
        assert_eq!(ret.len(), 1, "{op:?}: return arity");
        assert!(
            matches!(f.node_kind(f.producer(ret[0])), NodeKind::IntConst(_)),
            "{op:?}: the wide operation must fold\n{}",
            f.to_text(false)
        );
        assert_eq!(
            le_limbs(&f, ret[0]),
            expected.to_vec(),
            "{op:?}: result limbs\n{}",
            f.to_text(false)
        );
    }
}

/// One random bit extract of the `I32` word `w`, in the shapes the lifters
/// emit: `srl`, a low mask, MIPS64 `ext` on the sign-extended word, casts, a
/// scaled field, and an arithmetic shift's sign copies.
fn random_extract(b: &mut FunctionBuilder, rng: &mut Rng, w: ValueId) -> ValueId {
    let (i32t, i64t) = (ValueType::int(32), ValueType::int(64));
    let bin = |b: &mut FunctionBuilder, x: ValueId, c: u64, op: IntBinaryOp, ty: ValueType| {
        let c = b.build_int_const(c, ty).unwrap();
        b.build_int_binary_operation(x, c, op, ty).unwrap()
    };
    let k = 1 + rng.below(31) as u64;
    let m = 1 + rng.below(8) as u64;
    match rng.below(9) {
        0 => w,
        1 => bin(b, w, k, IntBinaryOp::ShiftRight, i32t),
        2 => {
            let up = bin(b, w, k, IntBinaryOp::ShiftRight, i32t);
            bin(b, up, (1 << m) - 1, IntBinaryOp::And, i32t)
        }
        3 => {
            let up = bin(b, w, k, IntBinaryOp::ShiftRight, i32t);
            b.extend_if_needed(up, i64t, ExtendOp::SignExtend).unwrap()
        }
        4 => {
            let wide = b.extend_if_needed(w, i64t, ExtendOp::SignExtend).unwrap();
            let up = bin(b, wide, 32, IntBinaryOp::ShiftLeft, i64t);
            let field = bin(b, up, 32 + k, IntBinaryOp::ShiftRight, i64t);
            bin(b, field, 0xff, IntBinaryOp::And, i64t)
        }
        5 => {
            let wide = b.extend_if_needed(w, i64t, ExtendOp::SignExtend).unwrap();
            bin(b, wide, k, IntBinaryOp::ShiftRight, i64t)
        }
        6 => {
            let up = bin(b, w, k, IntBinaryOp::ShiftRight, i32t);
            b.truncate_if_needed(up, ValueType::int(8)).unwrap()
        }
        7 => {
            let up = bin(b, w, k, IntBinaryOp::ShiftRight, i32t);
            let field = bin(b, up, (1 << m) - 1, IntBinaryOp::And, i32t);
            let wide = b
                .extend_if_needed(field, i64t, ExtendOp::ZeroExtend)
                .unwrap();
            bin(b, wide, 1 << (m % 4), IntBinaryOp::Mul, i64t)
        }
        _ => {
            let up = bin(b, w, k, IntBinaryOp::ShiftLeft, i32t);
            bin(
                b,
                up,
                k + rng.below(32 - k as usize) as u64,
                IntBinaryOp::ShiftRight,
                i32t,
            )
        }
    }
}

/// `if (extract(w) < n) return [extracts of w]`, with every returned value
/// inside the interval `range_of` claims at the `Return`: a guard on one bit
/// extract carries to the others only where it bounds them.
#[test]
fn a_guard_on_one_bit_extract_bounds_its_siblings_soundly() {
    let mut failures: Vec<String> = Vec::new();
    let (mut checks, mut runs, mut nontop) = (0usize, 0usize, 0usize);
    for seed in 0..400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let vn = reg_vn(0x1000, 4);
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let entry = b.create_region_all().unwrap();
        let then_arm = b.create_region_all().unwrap();
        let else_arm = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        let w = b.read_variable(&vn).unwrap();
        let tested = random_extract(&mut b, &mut rng, w);
        let ty = b.value_type_opt(tested).unwrap();
        let draw =
            |rng: &mut Rng| u128::from(rng.next_u64()) & mask(ty) >> rng.below(ty.bit_width());
        let n = (1 + draw(&mut rng)).min(mask(ty));
        let n = b.build_int_const(n, ty).unwrap();
        // Half the guards have a low end too: `tested - lo < n`.
        let tested = if rng.chance(50) {
            let lo = draw(&mut rng);
            let minus = b.build_int_const(lo.wrapping_neg() & mask(ty), ty).unwrap();
            b.build_int_binary_operation(tested, minus, IntBinaryOp::Add, ty)
                .unwrap()
        } else {
            tested
        };
        let cond = b
            .build_int_cmp_operation(tested, n, IntCmpOp::Less, ty)
            .unwrap();
        b.build_if(cond, then_arm, else_arm).unwrap();
        b.set_region(then_arm);
        let values: Vec<ValueId> = (0..6)
            .map(|_| random_extract(&mut b, &mut rng, w))
            .collect();
        b.build_return(None, &values).unwrap();
        b.set_region(else_arm);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let mut f = b.build().unwrap();
        {
            let mut p = crate::OptimizerPipeline::new();
            p.add(crate::PhiCollapse);
            p.add(crate::RegionCollapse);
            p.run(&mut f, &crate::OptCtx::new(None)).unwrap();
        }
        let ranges = std::cell::RefCell::new(index_range::RangeMap::new(&f).unwrap());
        for _ in 0..64 {
            let word = u128::from(rng.next_u64() as u32) >> rng.below(32);
            let mut bad: Option<String> = None;
            interp::run_observed(
                &f,
                &reference_env(&|_| UBig::from(word)),
                &mut |at, v, val| {
                    if !matches!(f.node_kind(at), NodeKind::Return)
                        || f.value_type_opt(v).is_none_or(|t| !t.is_integer())
                    {
                        return;
                    }
                    // `index_range` works in the carrier; every value here is
                    // `I32` or `I64`.
                    let val = u128::try_from(val).expect("a value inside the carrier");
                    let iv = ranges.borrow_mut().range_of(v, at);
                    checks += 1;
                    if !iv.is_top(mask(f.value_type_opt(v).unwrap())) {
                        nontop += 1;
                    }
                    let ok = iv.lo() <= val
                        && val <= iv.hi()
                        && (val - iv.lo()).is_multiple_of(iv.stride());
                    if !ok && bad.is_none() {
                        bad = Some(format!(
                            "seed {seed}: w={word:#x} {v:?}={val:#x} outside {iv:?}\n{}",
                            f.to_text(false)
                        ));
                    }
                },
            )
            .unwrap_or_else(|why| panic!("seed {seed}: the reference interpreter: {why:?}"));
            runs += 1;
            if let Some(why) = bad {
                failures.push(why);
                break;
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
    eprintln!("checks {checks} runs {runs} nontop {nontop}");
    assert!(
        nontop * 4 >= checks,
        "only {nontop} of {checks} ranges were narrower than top"
    );
}

/// Seeds this test sweeps unless `FUZZ_N` overrides it, and the threshold the
/// population floors below are asserted at.
const DEFAULT_FUZZ_SEEDS: u64 = 300;

/// Every `If` compare operand and every returned value of a random CFG lies
/// inside the interval `RangeMap::range_of` claims for it, stride included.
#[test]
fn a_range_contains_every_runtime_value() {
    let n: u64 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FUZZ_SEEDS);
    let start: u64 = std::env::var("FUZZ_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut failures: Vec<String> = Vec::new();
    let mut checks = 0usize;
    let mut nontop = 0usize;
    for seed in start..start + n {
        let Some(mut f) = build_random_cfg(seed) else {
            continue;
        };
        if crate::default_pipeline()
            .run(&mut f, &crate::OptCtx::new(None))
            .is_err()
        {
            continue;
        }
        let mut ranges = index_range::RangeMap::new(&f).unwrap();
        let mut rng = Rng(seed | 1);
        for _ in 0..24 {
            let vec: Vec<UBig> = (0..4).map(|_| UBig::from(rng.next_u64() % 8)).collect();
            let mut bad: Option<String> = None;
            interp::run_observed(&f, &reference_env(&|e| vec[e.index].clone()), &mut |at, v, val| {
                if f.value_type_opt(v)
                    .is_none_or(|t| !t.is_integer() || t == ValueType::I1)
                {
                    return;
                }
                let val = u128::try_from(val).expect("a value inside the carrier");
                let iv = ranges.range_of(v, at);
                checks += 1;
                let m = f.value_type_opt(v).unwrap().int_mask_u128().expect("at most 128 bits");
                if !iv.is_top(m) {
                    nontop += 1;
                }
                let ok =
                    iv.lo() <= val && val <= iv.hi() && (val - iv.lo()).is_multiple_of(iv.stride());
                if !ok && bad.is_none() {
                    bad = Some(format!(
                        "seed {seed}: inputs {vec:?} value {v:?}={val:#x} at {at:?} outside {iv:?}\n{}",
                        f.to_text(false)
                    ));
                }
            })
            .unwrap_or_else(|why| panic!("seed {seed}: the reference interpreter: {why:?}"));
            if let Some(b) = bad {
                failures.push(b);
                break;
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
    // Printed so the floor below can be re-baselined without editing this.
    eprintln!("checks {checks} nontop {nontop}");
    // `Interval::top` satisfies the soundness check for every value, so a
    // `range_of` that answered `top` everywhere passed while proving nothing.
    // Measured at the committed seed count: checks ~3.68M, nontop ~1.85M, so
    // the ratio floor sits at half the measured margin.
    //
    // Population statistics, asserted only over the full seed range: a single
    // CFG can legitimately check nothing, or have every range come out `top`
    // (seed 5 does), and a narrowed `FUZZ_N` is how a real failure is
    // bisected. Failing that run on an unrelated floor would read as a
    // product defect.
    if n >= DEFAULT_FUZZ_SEEDS {
        assert!(checks > 0, "the range oracle compared nothing");
        assert!(
            nontop * 4 >= checks,
            "only {nontop} of {checks} ranges were narrower than top"
        );
    }
}

/// A random state machine over `s`: each region may set `s` to a constant,
/// then branches on the argument or on `s` itself to the next region and to
/// another, forward or back, so its merges and loop headers are phis of
/// constants and of each other.  The last region returns `s`.
fn build_random_state_machine(seed: u64) -> Option<strider_ir::Function> {
    let mut rng = Rng(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1);
    let (size, ty) = [(1, ValueType::int(8)), (4, ValueType::int(32))][rng.below(2)];
    let s_vn = reg_vn(0x1000, size);
    let x_vn = reg_vn(0x2000, 4);
    let mut b = RegisterSet::new()
        .tracked(s_vn)
        .tracked(x_vn)
        .arg(x_vn)
        .build_fn()
        .ok()?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let nregions = 3 + rng.below(8);
    let regions: Vec<_> = (0..nregions)
        .map(|_| b.create_region_all().unwrap())
        .collect();
    b.set_entry_region_all(regions[0]).ok()?;
    for (i, &region) in regions.iter().enumerate() {
        b.set_region(region);
        if i + 1 == nregions {
            let s = b.read_variable(&s_vn).ok()?;
            b.build_return(None, &[s]).ok()?;
            continue;
        }
        if i == 0 || rng.chance(60) {
            let k = b.build_int_const(u128::from(rng.next_u64() % 9), ty).ok()?;
            b.write_variable(&s_vn, k).ok()?;
        }
        let (v, vty) = if rng.chance(50) {
            (b.read_variable(&s_vn).ok()?, ty)
        } else {
            (b.read_variable(&x_vn).ok()?, ValueType::int(32))
        };
        let c = b
            .build_int_const(u128::from(rng.next_u64() % 9), vty)
            .ok()?;
        let op = [IntCmpOp::Equal, IntCmpOp::Less][rng.below(2)];
        let cond = b.build_int_cmp_operation(v, c, op, vty).ok()?;
        let other = regions[rng.below(nregions)];
        if other == regions[i + 1] {
            b.build_branch(other).ok()?;
        } else if rng.chance(50) {
            b.build_if(cond, regions[i + 1], other).ok()?;
        } else {
            b.build_if(cond, other, regions[i + 1]).ok()?;
        }
    }
    b.set_lift_addr(None);
    let f = b.build().ok()?;
    strider_ir::validate::validate(&f).ok()?;
    Some(f)
}

/// Every runtime value of a phi of a random state machine lies among the
/// constants `index_range::phi_constants` names for it and in its range: the
/// table walk enumerates the constants the range holds.  The range can be the
/// tighter, since an edge guard bounds the arm it carries.
#[test]
fn a_phi_of_constants_holds_only_its_constants() {
    let mut failures: Vec<String> = Vec::new();
    let n: u64 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FUZZ_SEEDS);
    let start: u64 = std::env::var("FUZZ_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (mut checks, mut phis, mut functions, mut ruled) = (0usize, 0usize, 0usize, 0usize);
    for seed in start..start + n {
        let Some(mut f) = build_random_state_machine(seed) else {
            continue;
        };
        if crate::default_pipeline()
            .run(&mut f, &crate::OptCtx::new(None))
            .is_err()
        {
            continue;
        }
        let mut ranges = index_range::RangeMap::new(&f).unwrap();
        let mut sets: FxHashMap<ValueId, Vec<u128>> = FxHashMap::default();
        for n in strider_ir::IRWalker::walk(&f) {
            if !matches!(f.node_kind(n), NodeKind::Phi) {
                continue;
            }
            let v = f.node_outputs(n)[0];
            if let Some(set) = index_range::phi_constants(&f, v, 64) {
                sets.insert(v, set);
            }
        }
        if sets.is_empty() {
            continue;
        }
        functions += 1;
        phis += sets.len();
        let mut ruled_out: FxHashMap<(ValueId, NodeId), FxHashSet<u128>> = FxHashMap::default();
        let mut rng = Rng(seed | 1);
        for _ in 0..8 {
            let x = u128::from(rng.next_u64() % 10);
            let mut bad: Option<String> = None;
            interp::run_observed(&f, &reference_env(&|_| UBig::from(x)), &mut |at, v, val| {
                let Some(set) = sets.get(&v) else {
                    return;
                };
                let val = u128::try_from(val).expect("a value inside the carrier");
                checks += 1;
                let iv = ranges.range_of(v, at);
                let out = ruled_out
                    .entry((v, at))
                    .or_insert_with(|| ranges.ruled_out(v, at));
                ruled += usize::from(!out.is_empty());
                if (set.binary_search(&val).is_err() || !iv.contains(val) || out.contains(&val))
                    && bad.is_none()
                {
                    bad = Some(format!(
                        "seed {seed}: x={x} phi {v:?}={val:#x} at {at:?} outside {set:?} or \
                         {iv:?}, or ruled out by {out:?}\n{}",
                        f.to_text(false)
                    ));
                }
            })
            .unwrap_or_else(|why| panic!("seed {seed}: the reference interpreter: {why:?}"));
            if let Some(b) = bad {
                failures.push(b);
                break;
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
    // Measured: 106 functions, 154 phis, 32022 observations at a point with
    // a constant ruled out.  A floor over the full seed range only, as in
    // `a_range_contains_every_runtime_value`.
    eprintln!("functions {functions} phis {phis} checks {checks} ruled {ruled}");
    if n >= DEFAULT_FUZZ_SEEDS {
        assert!(
            functions * 6 >= n as usize && phis >= functions && checks > 0 && ruled > 0,
            "functions {functions} phis {phis} checks {checks} ruled {ruled}"
        );
    }
}
