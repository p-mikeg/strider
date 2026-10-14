//! Differential fuzzer: random IR, evaluated by an independent p-code
//! reference interpreter before and after optimisation.
//!
//! Both generators emit integer arithmetic, compares, extend/truncate,
//! popcount/lzcount, phis and `If` / `Return` / `Unreachable` control, and
//! nothing else: no `Load`, `Store`, `Call`, `CallOther`, `Switch` or float
//! node, so `LoadForward`, `StoreDead`, the memory and SP-alias analyses and
//! every float fold are outside what this checks.
//!
//! Four environment variables scale a run past the committed default:
//! `FUZZ_N` seeds, `FUZZ_START` the first of them, `FUZZ_STEPS` nodes per DAG,
//! and `FUZZ_PASS` runs `cf` / `kb` / `flag` alone instead of the whole
//! pipeline.

use rustc_hash::FxHashMap;
use strider_ir::node::{ExtendOp, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::test_support::Rng;

pub(crate) const TYPES: &[(u32, ValueType)] = &[
    (1, ValueType::I8),
    (2, ValueType::I16),
    (3, ValueType::I24),
    (4, ValueType::I32),
    (5, ValueType::I40),
    (8, ValueType::I64),
    (9, ValueType::I72),
    (16, ValueType::I128),
];

fn mask(ty: ValueType) -> u128 {
    ty.bit_mask_u128()
}

fn sext(v: u128, ty: ValueType) -> i128 {
    ty.get_signed_int(v).unwrap()
}

/// Independent reference semantics.
pub(crate) fn ref_eval(
    f: &strider_ir::Function,
    v: ValueId,
    inputs: &dyn Fn(usize, ValueType) -> u128,
    memo: &mut FxHashMap<ValueId, Option<u128>>,
) -> Option<u128> {
    if let Some(r) = memo.get(&v) {
        return *r;
    }
    let node = f.producer(v);
    let kind = *f.node_kind(node);
    let ty = f.value_type_opt(v).unwrap();
    let ins: Vec<ValueId> = f.node_inputs(node).into_iter().collect();
    let m = mask(ty);
    let w = ty.bit_width() as u32;
    let r: Option<u128> = (|| {
        Some(match kind {
            NodeKind::IntConst(_) => f.int_const_u128(v).unwrap(),
            NodeKind::InitialVar(id) => inputs(id.index(), ty) & m,
            NodeKind::IntBinaryOp(op) => {
                let a = ref_eval(f, ins[0], inputs, memo)?;
                let b = ref_eval(f, ins[1], inputs, memo)?;
                let bty = f.value_type_opt(ins[1]).unwrap();
                assert_eq!(f.value_type_opt(ins[0]).unwrap(), ty, "{kind:?}");
                match op {
                    IntBinaryOp::Add => a.wrapping_add(b) & m,
                    IntBinaryOp::Mul => a.wrapping_mul(b) & m,
                    IntBinaryOp::And => a & b,
                    IntBinaryOp::Or => a | b,
                    IntBinaryOp::Xor => a ^ b,
                    IntBinaryOp::ShiftLeft => {
                        if b >= u128::from(w) {
                            0
                        } else {
                            (a << b) & m
                        }
                    }
                    IntBinaryOp::ShiftRight => {
                        if b >= u128::from(w) {
                            0
                        } else {
                            a >> b
                        }
                    }
                    IntBinaryOp::SShiftRight => {
                        let sa = sext(a, ty);
                        if b >= u128::from(w) {
                            if sa < 0 { m } else { 0 }
                        } else {
                            ((sa >> b) as u128) & m
                        }
                    }
                    IntBinaryOp::Div => {
                        assert_eq!(bty, ty);
                        if b == 0 {
                            return None;
                        }
                        a / b
                    }
                    IntBinaryOp::Rem => {
                        if b == 0 {
                            return None;
                        }
                        a % b
                    }
                    IntBinaryOp::Sdiv | IntBinaryOp::Srem => {
                        let (sa, sb) = (sext(a, ty), sext(b, ty));
                        if sb == 0 {
                            return None;
                        }
                        let min = if w == 128 {
                            i128::MIN
                        } else {
                            -(1i128 << (w - 1))
                        };
                        if sa == min && sb == -1 {
                            return None;
                        }
                        let q = if op == IntBinaryOp::Sdiv {
                            sa.checked_div(sb)?
                        } else {
                            sa.checked_rem(sb)?
                        };
                        (q as u128) & m
                    }
                }
            }
            NodeKind::IntUnaryOp(IntUnaryOp::Neg) => {
                let a = ref_eval(f, ins[0], inputs, memo)?;
                a.wrapping_neg() & m
            }
            NodeKind::IntCmpOp(op) => {
                let a = ref_eval(f, ins[0], inputs, memo)?;
                let b = ref_eval(f, ins[1], inputs, memo)?;
                let t = f.value_type_opt(ins[0]).unwrap();
                assert_eq!(f.value_type_opt(ins[1]).unwrap(), t);
                let tw = t.bit_width() as u32;
                let (sa, sb) = (sext(a, t), sext(b, t));
                let (min, max) = if tw == 128 {
                    (i128::MIN, i128::MAX)
                } else {
                    (-(1i128 << (tw - 1)), (1i128 << (tw - 1)) - 1)
                };
                u128::from(match op {
                    IntCmpOp::Equal => a == b,
                    IntCmpOp::Less => a < b,
                    IntCmpOp::Sless => sa < sb,
                    IntCmpOp::Carry => a.checked_add(b).is_none_or(|s| s > t.bit_mask_u128()),
                    IntCmpOp::Scarry => sa.checked_add(sb).is_none_or(|s| s < min || s > max),
                    IntCmpOp::Sborrow => sa.checked_sub(sb).is_none_or(|s| s < min || s > max),
                })
            }
            NodeKind::Truncate => ref_eval(f, ins[0], inputs, memo)? & m,
            NodeKind::Extend(ExtendOp::ZeroExtend) => ref_eval(f, ins[0], inputs, memo)?,
            NodeKind::Extend(ExtendOp::SignExtend) => {
                let t = f.value_type_opt(ins[0]).unwrap();
                (sext(ref_eval(f, ins[0], inputs, memo)?, t) as u128) & m
            }
            NodeKind::Popcount => u128::from(ref_eval(f, ins[0], inputs, memo)?.count_ones()) & m,
            NodeKind::Lzcount => {
                let t = f.value_type_opt(ins[0]).unwrap();
                let a = ref_eval(f, ins[0], inputs, memo)?;
                let bits = (t.byte_size() * 8) as u32;
                let lz = if a == 0 {
                    bits
                } else {
                    a.leading_zeros() - (128 - bits)
                };
                u128::from(lz) & m
            }
            NodeKind::Phi => {
                let data: Vec<ValueId> = f.phi_data_inputs(node).collect();
                assert_eq!(data.len(), 1, "multi-arm phi");
                ref_eval(f, data[0], inputs, memo)?
            }
            other => panic!("ref_eval: unsupported {other:?}"),
        })
    })();
    memo.insert(v, r);
    r
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
    fn special(&mut self, ty: ValueType) -> u128 {
        let m = mask(ty);
        let w = ty.bit_width() as u128;
        let c = match self.rng.below(14) {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => m,
            4 => m >> 1,
            5 => (m >> 1) + 1,
            6 => w - 1,
            7 => w,
            8 => w + 1,
            9 => self.rng.u128() % 16,
            10 => m - (self.rng.u128() % 4),
            11 => 1u128 << (self.rng.below(w as usize)),
            12 => !((1u128 << self.rng.below(8)) - 1),
            _ => self.rng.u128(),
        };
        c & m
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
            .filter(|&v| b.function().int_const_u128(v).is_none())
            .collect();
        if nonconst.is_empty() {
            self.val(b, ty)
        } else {
            nonconst[self.rng.below(nonconst.len())]
        }
    }

    pub fn step(&mut self, b: &mut FunctionBuilder) {
        const BIN: [IntBinaryOp; 12] = [
            IntBinaryOp::Add,
            IntBinaryOp::And,
            IntBinaryOp::Or,
            IntBinaryOp::Xor,
            IntBinaryOp::Div,
            IntBinaryOp::Sdiv,
            IntBinaryOp::Rem,
            IntBinaryOp::Srem,
            IntBinaryOp::ShiftRight,
            IntBinaryOp::SShiftRight,
            IntBinaryOp::ShiftLeft,
            IntBinaryOp::Mul,
        ];
        const CMP: [IntCmpOp; 6] = [
            IntCmpOp::Equal,
            IntCmpOp::Sless,
            IntCmpOp::Less,
            IntCmpOp::Carry,
            IntCmpOp::Scarry,
            IntCmpOp::Sborrow,
        ];
        let ty = self.pick_ty();
        match self.rng.below(40) {
            0..=11 => {
                let op = BIN[self.rng.below(BIN.len())];
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
                let ones = b.build_int_const(mask(ty), ty).unwrap();
                let v = b
                    .build_int_binary_operation(x, ones, IntBinaryOp::Xor, ty)
                    .unwrap();
                self.push(v, ty);
            }
            14..=16 => {
                let op = CMP[self.rng.below(CMP.len())];
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
        let zty = [ValueType::I8, ValueType::I32][self.rng.below(2)];
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
        let m = mask(ty);
        let a = self.var_or_val(b, ty);
        let n = self.special(ty);
        let out = match self.rng.below(6) {
            0 | 1 => {
                // Or(Less(a,N), Equal(Add(a,M),0)), HI dual
                let mv = if self.rng.chance(70) {
                    n.wrapping_neg() & m
                } else {
                    self.special(ty)
                };
                let nc = b.build_int_const(n, ty).unwrap();
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
                    c1.wrapping_sub(n) & m
                } else {
                    self.special(ty)
                };
                let c1c = b.build_int_const(c1, ty).unwrap();
                let c2c = b.build_int_const(c2, ty).unwrap();
                let nc = b.build_int_const(n, ty).unwrap();
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
        let ty = [ValueType::I8, ValueType::I32, ValueType::I64][self.rng.below(3)];
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

fn input_value(rng: &mut Rng, ty: ValueType) -> u128 {
    let m = mask(ty);
    let w = ty.bit_width() as u32;
    (match rng.below(10) {
        0 => 0,
        1 => m,
        2 => 1u128 << (w - 1),
        3 => (1u128 << (w - 1)) - 1,
        4 => rng.u128() % 8,
        5 => m - rng.u128() % 8,
        6 => 1u128 << rng.below(w as usize),
        _ => rng.u128(),
    }) & m
}

/// Returns the number of compared (trial, return value) pairs, or a
/// description of the first mismatch. A pair whose reference value is `None`
/// -- division by zero, `INT_MIN / -1` -- is not comparable and not counted.
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
    let mut vectors: Vec<Vec<u128>> = Vec::new();
    for _ in 0..ntrials {
        vectors.push((0..8).map(|_| rng.u128()).collect());
    }
    // Map every input index to a width-appropriate special value.
    let mut rng2 = Rng(seed ^ 0x1357_9BDF | 1);
    let specials: Vec<Vec<u128>> = (0..ntrials)
        .map(|_| {
            (0..8)
                .map(|_| input_value(&mut rng2, ValueType::I128))
                .collect()
        })
        .collect();
    let mut expected: Vec<Vec<Option<u128>>> = Vec::new();
    for t in 0..ntrials {
        let vec = &vectors[t];
        let sp = &specials[t];
        let inputs = |i: usize, ty: ValueType| {
            let mut r = Rng(vec[i] as u64 | 1);
            if t % 2 == 0 {
                input_value(&mut r, ty)
            } else {
                sp[i]
            }
        };
        let mut memo = FxHashMap::default();
        expected.push(
            rets.iter()
                .map(|&v| ref_eval(&f, v, &inputs, &mut memo))
                .collect(),
        );
    }
    if let Err(e) = run(&mut f) {
        return Err(format!("seed {seed}: pipeline error {e:#}\n{before_text}"));
    }
    let after = return_values(&f);
    if after.len() != rets.len() {
        return Err(format!("seed {seed}: return arity changed"));
    }
    let mut compared = 0usize;
    for t in 0..ntrials {
        let vec = &vectors[t];
        let sp = &specials[t];
        let inputs = |i: usize, ty: ValueType| {
            let mut r = Rng(vec[i] as u64 | 1);
            if t % 2 == 0 {
                input_value(&mut r, ty)
            } else {
                sp[i]
            }
        };
        let mut memo = FxHashMap::default();
        for (k, &v) in after.iter().enumerate() {
            let got = ref_eval(&f, v, &inputs, &mut memo);
            let Some(want) = expected[t][k] else { continue };
            compared += 1;
            if got != Some(want) {
                let mut ins = Vec::new();
                for i in 0..4 {
                    ins.push(format!("{:#x}", inputs(i, ValueType::I128)));
                }
                return Err(format!(
                    "seed {seed}: ret {k} trial {t}: want {want:#x} got {got:?}; raw inputs(before per-type mask) {ins:?}\nBEFORE:\n{before_text}\nAFTER:\n{}",
                    f.to_text(false)
                ));
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
        .unwrap_or(300);
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
            p.run(f, &mut crate::OptCtx::at_floor(None))
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
    // A floor on the yield: a generator change or an early `?` that stops
    // producing comparable return values fails here rather than reading green.
    assert!(
        compared >= n as usize * 450,
        "only {compared} value comparisons over {n} seeds"
    );
    if !failures.is_empty() {
        panic!(
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n=====\n")
        );
    }
}

// CFG-level differential fuzzer.

#[derive(Debug, PartialEq)]
enum Outcome {
    Ret(Vec<Option<u128>>),
    Unreachable,
    Diverge,
}

fn run_cfg(f: &strider_ir::Function, inputs: &dyn Fn(usize, ValueType) -> u128) -> Outcome {
    run_cfg_checked(f, inputs, &mut |_, _, _| {})
}

fn run_cfg_checked(
    f: &strider_ir::Function,
    inputs: &dyn Fn(usize, ValueType) -> u128,
    check: &mut dyn FnMut(strider_ir::node::NodeId, ValueId, u128),
) -> Outcome {
    use strider_ir::IRWalker;
    let live: rustc_hash::FxHashSet<strider_ir::node::NodeId> = f.walk().collect();
    let mut phi_env: FxHashMap<strider_ir::node::NodeId, Option<u128>> = FxHashMap::default();
    let entry = f.entry();
    let mut ctrl = f.node_outputs(entry)[0];
    for _ in 0..400 {
        let uses: Vec<(strider_ir::node::NodeId, u32)> = f
            .value_uses(ctrl)
            .filter(|(n, _)| live.contains(n))
            .collect();
        assert_eq!(
            uses.len(),
            1,
            "control value with {} live consumers",
            uses.len()
        );
        let (node, slot) = uses[0];
        match *f.node_kind(node) {
            NodeKind::Region => {
                let k = slot as usize;
                let token = f.node_outputs(node)[1];
                let phis: Vec<_> = f
                    .value_uses(token)
                    .map(|(n, _)| n)
                    .filter(|n| live.contains(n) && matches!(f.node_kind(*n), NodeKind::Phi))
                    .collect();
                let mut new_vals = Vec::new();
                for &phi in &phis {
                    let data: Vec<ValueId> = f.phi_data_inputs(phi).collect();
                    let mut memo = FxHashMap::default();
                    new_vals.push(eval_env(f, data[k], inputs, &phi_env, &mut memo));
                }
                for (phi, v) in phis.into_iter().zip(new_vals) {
                    phi_env.insert(phi, v);
                }
                ctrl = f.node_outputs(node)[0];
            }
            NodeKind::If => {
                let cond = f.node_inputs(node)[1];
                let mut memo = FxHashMap::default();
                if matches!(f.node_kind(f.producer(cond)), NodeKind::IntCmpOp(_)) {
                    for op in f.node_inputs(f.producer(cond)) {
                        if let Some(val) = eval_env(f, op, inputs, &phi_env, &mut memo) {
                            check(node, op, val);
                        }
                    }
                }
                let Some(c) = eval_env(f, cond, inputs, &phi_env, &mut memo) else {
                    return Outcome::Diverge; // UB in the condition
                };
                ctrl = f.node_outputs(node)[if c != 0 { 0 } else { 1 }];
            }
            NodeKind::Return => {
                let mut memo = FxHashMap::default();
                let vals: Vec<Option<u128>> = f
                    .node_inputs(node)
                    .into_iter()
                    .skip(2)
                    .map(|v| eval_env(f, v, inputs, &phi_env, &mut memo))
                    .collect();
                for (v, val) in f.node_inputs(node).into_iter().skip(2).zip(&vals) {
                    if let Some(val) = val {
                        check(node, v, *val);
                    }
                }
                return Outcome::Ret(vals);
            }
            NodeKind::Unreachable => return Outcome::Unreachable,
            other => panic!("run_cfg: control reaches {other:?}"),
        }
    }
    Outcome::Diverge
}

fn eval_env(
    f: &strider_ir::Function,
    v: ValueId,
    inputs: &dyn Fn(usize, ValueType) -> u128,
    phi_env: &FxHashMap<strider_ir::node::NodeId, Option<u128>>,
    memo: &mut FxHashMap<ValueId, Option<u128>>,
) -> Option<u128> {
    // Seed the memo with every phi's current value, then reuse `ref_eval`.
    let node = f.producer(v);
    if matches!(f.node_kind(node), NodeKind::Phi) {
        return *phi_env
            .get(&node)
            .expect("phi read before its region was entered");
    }
    for (&phi, &val) in phi_env {
        let out = f.node_outputs(phi)[0];
        memo.entry(out).or_insert(val);
    }
    ref_eval(f, v, inputs, memo)
}

fn build_random_cfg(seed: u64) -> Option<strider_ir::Function> {
    let mut rng = Rng(seed.wrapping_mul(0xA24B_AED4_963E_E407) | 1);
    let nvars = 1 + rng.below(3);
    let mut rs = RegisterSet::new();
    let mut vns = Vec::new();
    for i in 0..nvars {
        let (sz, _) = [(1, ValueType::I8), (4, ValueType::I32)][rng.below(2)];
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
                b.build_int_const(u128::from(rng.next() % 4), ty).ok()?
            };
            let op = [
                IntBinaryOp::Add,
                IntBinaryOp::Xor,
                IntBinaryOp::And,
                IntBinaryOp::Mul,
                IntBinaryOp::Or,
            ][rng.below(5)];
            let nv = if rng.chance(20) {
                b.build_int_const(u128::from(rng.next() % 3), ty).ok()?
            } else {
                b.build_int_binary_operation(vj, other, op, ty).ok()?
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
                        let c = b.build_int_const(u128::from(rng.next() % 3), ty).ok()?;
                        let d = b.build_int_const(u128::from(rng.next() % 3), ty).ok()?;
                        b.build_int_cmp_operation(c, d, IntCmpOp::Equal, ty).ok()?
                    }
                    _ => {
                        let c = b.build_int_const(u128::from(rng.next() % 5), ty).ok()?;
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
        .unwrap_or(300);
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
                let vecs: Vec<Vec<u128>> = (0..16)
                    .map(|_| (0..4).map(|_| u128::from(rng.next())).collect())
                    .collect();
                for vec in &vecs {
                    outcomes.push(run_cfg(&f, &|i, ty| {
                        input_value(&mut Rng(vec[i] as u64 | 1), ty)
                    }));
                }
                crate::default_pipeline()
                    .run(&mut f, &mut crate::OptCtx::at_floor(None))
                    .map_err(|e| format!("seed {seed}: pipeline error {e:#}\n{before_text}"))?;
                let mut cmp = 0;
                for (vec, want) in vecs.iter().zip(&outcomes) {
                    let got = run_cfg(&f, &|i, ty| input_value(&mut Rng(vec[i] as u64 | 1), ty));
                    match want {
                        Outcome::Ret(_) => {
                            cmp += 1;
                            if &got != want {
                                return Err(format!(
                                    "seed {seed}: inputs {vec:?} want {want:?} got {got:?}\nBEFORE:\n{before_text}\nAFTER:\n{}",
                                    f.to_text(false)
                                ));
                            }
                        }
                        Outcome::Unreachable => {
                            if !matches!(got, Outcome::Unreachable | Outcome::Diverge) {
                                return Err(format!(
                                    "seed {seed}: unreachable became {got:?}\n{before_text}"
                                ));
                            }
                        }
                        Outcome::Diverge => {}
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
    if !failures.is_empty() {
        panic!(
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n=====\n")
        );
    }
}

// Targeted proofs.

/// `op(C1, C2)` at I256 with both constants past the u128 carrier. The
/// pipeline leaves a `ConstValue::Wide` operand unfolded, so what the run must
/// preserve is the operation and both operands' limbs, unswapped by
/// const-on-right and untruncated by the u128 carrier.
#[test]
fn a_wide_constant_does_not_ping_pong_across_const_on_right() {
    fn le_limbs(f: &strider_ir::Function, v: ValueId) -> Vec<u64> {
        f.int_const_wide_le_bytes(f.producer(v))
            .unwrap_or_else(|| panic!("{v:?} is not a wide constant"))
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }
    let w1 = [1u64, 2, 3, 4];
    let w2 = [5u64, 6, 7, 8];
    for op in [IntBinaryOp::And, IntBinaryOp::Add, IntBinaryOp::Xor] {
        let var = reg_vn(0x1000, 32);
        let mut b = RegisterSet::new()
            .tracked(var)
            .build_fn_single_region()
            .unwrap();
        b.set_lift_addr(Some(0xA0));
        let c1 = b.build_int_const_limbs(&w1, ValueType::I256).unwrap();
        let c2 = b.build_int_const_limbs(&w2, ValueType::I256).unwrap();
        let v = b
            .build_int_binary_operation(c1, c2, op, ValueType::I256)
            .unwrap();
        b.build_return(None, &[v]).unwrap();
        b.set_lift_addr(None);
        let mut f = b.build().unwrap();
        crate::default_pipeline()
            .run(&mut f, &mut crate::OptCtx::at_floor(None))
            .unwrap_or_else(|e| panic!("{op:?}: {e:#}"));
        let ret = return_values(&f);
        assert_eq!(ret.len(), 1, "{op:?}: return arity");
        let node = f.producer(ret[0]);
        assert_eq!(
            *f.node_kind(node),
            NodeKind::IntBinaryOp(op),
            "{op:?}: the wide operation is gone\n{}",
            f.to_text(false)
        );
        let ins = f.node_inputs(node);
        assert_eq!(
            [le_limbs(&f, ins[0]), le_limbs(&f, ins[1])],
            [w1.to_vec(), w2.to_vec()],
            "{op:?}: operand limbs\n{}",
            f.to_text(false)
        );
    }
}

/// Every `If` compare operand and every returned value of a random CFG lies
/// inside the interval `RangeMap::range_of` claims for it, stride included.
#[test]
fn a_range_contains_every_runtime_value() {
    let n: u64 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
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
            .run(&mut f, &mut crate::OptCtx::at_floor(None))
            .is_err()
        {
            continue;
        }
        let known = crate::opt::known_bits::analyze(&f).unwrap();
        let doms = strider_ir::control_dominator_tree(&f);
        let mut ranges = crate::value_range::compute_value_ranges(&f, &doms, &known);
        let mut rng = Rng(seed | 1);
        for _ in 0..24 {
            let vec: Vec<u128> = (0..4).map(|_| u128::from(rng.next() % 8)).collect();
            let mut bad: Option<String> = None;
            run_cfg_checked(&f, &|i, _| vec[i], &mut |at, v, val| {
                if f.value_type_opt(v)
                    .is_none_or(|t| !t.is_integer() || t == ValueType::I1)
                {
                    return;
                }
                let iv = ranges.range_of(v, at);
                checks += 1;
                let m = f.value_type_opt(v).unwrap().bit_mask_u128();
                if !iv.is_top(m) {
                    nontop += 1;
                }
                let ok =
                    iv.lo <= val && val <= iv.hi && (val - iv.lo).is_multiple_of(iv.stride.max(1));
                if !ok && bad.is_none() {
                    bad = Some(format!(
                        "seed {seed}: inputs {vec:?} value {v:?}={val:#x} at {at:?} outside {iv:?}\n{}",
                        f.to_text(false)
                    ));
                }
            });
            if let Some(b) = bad {
                failures.push(b);
                break;
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    eprintln!("checks {checks} nontop {nontop}");
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
}
