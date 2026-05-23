//! `NodeKind` — the closed enum of every operation/role a node can take —
//! and `FunctionArgSource`, the calling-convention source for a `FunctionArg`
//! node.

/// Where a function argument originates in the calling convention.
///
/// Used inside [`NodeKind::FunctionArg`] to capture whether the argument was
/// passed in a register (e.g. RDI on x86_64 System V) or on the stack at a
/// positional offset from the entry stack pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionArgSource {
    /// The argument was passed in the given register varnode.  This is always
    /// the full-width container register (e.g. `RDI`, not `EDI`) so that
    /// sub-register reads can be expressed as `Truncate(FunctionArg)`.
    Register(rsleigh::Vn),
    /// The argument was passed on the stack at byte offset `offset` from the
    /// entry-time stack pointer (`InitialVar(sp)`).  `space` is the address
    /// space of the stack (typically the architecture's RAM space).
    Stack {
        space: rsleigh::VnSpace,
        offset: i64,
    },
}

/// Structural classification of a [`NodeKind`].  Single source of truth
/// for "structural / region / initial-state" predicates that several
/// passes need (cacheability, asm-fingerprint exemption, …).
///
/// Adding a new [`NodeKind`] variant requires extending the exhaustive
/// match in [`NodeKind::category`], which forces an explicit decision
/// about which structural bucket the new variant lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeCategory {
    /// Region header (`ControlState`).  Inputs grow dynamically as
    /// CFG predecessors are wired in, so identity must be preserved.
    Region,
    /// Initial-state nodes synthesised at function entry: `Entry`,
    /// `InitialMemory`, `InitialVar(_)`, `FunctionArg { .. }`.
    /// `FunctionArg` carries a per-index uniqueness invariant.
    InitialState,
    /// SSA / memory / stack phis whose inputs are wired in alongside
    /// region predecessors: `Phi`, `MemPhi`, `StackStorePhi`.
    Phi,
    /// Control-flow terminators and call-shaped nodes whose identity
    /// must stay distinct: `Return`, `IndirectBranch`, `Call`,
    /// `CallOther`.
    Terminator,
    /// Sleigh user-ops with opaque side effects: `CPoolRef`, `New`.
    /// (Note: `CallOther` and `SegmentOp` are Sleigh user-ops too, but
    /// `CallOther` is a terminator-shaped call and `SegmentOp` is pure;
    /// only `CPoolRef` and `New` need a fresh identity per occurrence.)
    OpaqueCall,
    /// Pure value-producing computation: constants, arithmetic,
    /// comparisons, conversions, plain `Load` / `Store` / `StackStore`,
    /// `SegmentOp`, `If`, etc.  Cacheable; deduplicated by
    /// `(kind, inputs, output_kinds)`.
    PureValue,
}

/// The operation or role of a node in the IR graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    // ── Initial state ──────────────────────────────────────────────────────────
    /// Function entry point.  Produces a single `Control` output.
    Entry,
    /// Initial memory state.  Produces a single `Memory` output.
    InitialMemory,
    /// Initial value of varnode `Vn` at the function entry.  Produces a
    /// value output of the appropriate integer type.
    InitialVar(rsleigh::Vn),
    /// Canonical marker for a function argument at position `index` in the
    /// calling convention.  Introduced by
    /// [`opt::FunctionArgDetect`](../../../opt/src/function_args/mod.rs) which
    /// replaces register-passed arg reads (`InitialVar(arg_reg)`) and
    /// stack-passed arg reads (`Load[InitialVar(sp) + K]`) with this node.
    ///
    /// The per-graph invariant is that at most one `FunctionArg` node exists
    /// per `index` (enforced by `validate::graph_invariants`).  Source nodes
    /// may become unreferenced after rewiring; `FunctionArg` itself is not
    /// cacheable, since identity matters for the uniqueness invariant.
    ///
    /// Inputs: `[]`.  Outputs: `[value]` of a width determined by `source`
    /// (register width for `Register`; the widest observed load width for
    /// `Stack`).
    FunctionArg {
        source: FunctionArgSource,
        index: u32,
    },

    // ── Region / join nodes ────────────────────────────────────────────────────
    /// Region header.  Consumes incoming control edges (one per predecessor)
    /// and produces a fresh `Control` output plus a `PhiToken`.
    ControlState,
    /// Memory phi: selects the live memory token at a join point.
    MemPhi,
    /// SSA φ at a join point.  Inputs: `[phi_token, val_0, val_1, …]`
    /// where `phi_token` is the `PhiToken` output of the joining
    /// `ControlState` and the rest are one value per CFG predecessor
    /// in the same order as the `ControlState`'s `Control` inputs.
    /// Output: `[value]`.
    ///
    /// Some phis carry a source-level varnode tag (lifter-emitted SSA
    /// φ for register-aliased reads); others are anonymous value phis
    /// synthesised by `StackLoadForward` when forwarding a
    /// `Load[sp+K]` across a `MemPhi`.  The tag (when present) is
    /// stored in [`crate::graph::Graph::phi_var_tag`] (an
    /// `Option<Vn>` side-table); query it via
    /// [`crate::graph::Graph::phi_var_tag`].  Anonymous phis have no
    /// entry (the side-table returns `None`).  Non-cacheable: phi
    /// identity matters.
    Phi,

    // ── Conditional branch ─────────────────────────────────────────────────────
    /// Conditional branch.  Consumes `(control, bool_cond)` and produces two
    /// `Control` outputs: index 0 for the true branch, index 1 for the false branch.
    If,

    // ── Calls and returns ──────────────────────────────────────────────────────
    /// Function call.  Clobbers caller-saved registers and the memory token.
    Call,
    /// Function return.  Consumes the outgoing control edge and any return-value outputs.
    Return,
    /// Unresolved indirect-branch placeholder.  Emitted by the lifter when
    /// the CFG terminator is `UnresolvedIndirectBranch`.  Inputs:
    /// `[control, memory, target_value]`.  Outputs: `[]`.
    ///
    /// The indirect-branch resolver inspects the producer of `target_value`
    /// after the stable optimiser pipeline has run, then either rewrites
    /// this node into a `Return` (link-register tail-return shapes) or
    /// splices in a `Call`+`Return` pair (tail-call shape).  An
    /// `IndirectBranch` surviving the destructive pipeline means the
    /// resolver couldn't classify the producer; the IR is still valid.
    ///
    /// The `memory` slot is anchored alongside `control` so the resolver
    /// can wire a real `Return` (or `Call`+`Return`) at the same program
    /// point without re-walking the CFG to find the live memory token.
    IndirectBranch,

    // ── Memory operations ──────────────────────────────────────────────────────
    /// Load from the given address space.
    Load(rsleigh::VnSpace),
    /// Store to the given address space.
    Store(rsleigh::VnSpace),

    // ── Stack-slot stores (produced by StackStoreDetect) ──────────────────────
    /// Store whose address has been resolved to `base + offset`, where `base`
    /// is an SP-rooted node (either `InitialVar(stack_ptr)` or a
    /// `VarPhi(stack_ptr)` that could not be further reduced — typically
    /// a loop-header SP phi with a back-edge to itself).
    ///
    /// Inputs: `[memory, base, data]`.  Outputs: `[Memory]`.
    ///
    /// The base is tracked explicitly so that stores with identical offsets
    /// taken from different SP versions are not conflated.
    StackStore {
        space: rsleigh::VnSpace,
        offset: i64,
    },
    /// Store whose address is an SP-phi of known per-branch offsets.
    /// Inputs: `[phi_token, memory, data]`.  Outputs: `[Memory]`.
    /// The per-branch offsets are stored in
    /// [`Graph::stack_phi_offsets`](crate::Graph::stack_phi_offsets) rather
    /// than inline so that `NodeKind` remains `Copy`.
    StackStorePhi { space: rsleigh::VnSpace },

    // ── Integer constants and operations ──────────────────────────────────────
    /// A compile-time integer constant of value `u128`.  Covers
    /// `Bool`/`U8`/`U16`/`U32`/`U64`/`U80`/`U128`.  Wider integer types
    /// (`U256`/`U512`) use [`Self::IntConstWide`] which references
    /// `Graph::wide_consts` off-side.
    IntConst(u128),
    /// A compile-time integer constant whose value doesn't fit in
    /// `u128` — `U256` or `U512`.  The actual byte payload lives in
    /// `Graph::wide_consts` and this node carries a
    /// [`crate::wide_const::WideConstId`] index.
    ///
    /// Interning makes structural equality work: two `IntConstWide(id)`
    /// nodes with the same `id` reference the same value (the
    /// [`crate::Graph::intern_wide_const`] contract).
    IntConstWide(crate::wide_const::WideConstId),
    /// Integer unary operation (e.g. bitwise NOT, two's-complement negate).
    IntUnaryOp(crate::ops::IntUnaryOp),
    /// Integer binary operation (e.g. add, shift, bitwise AND).
    IntBinaryOp(crate::ops::IntBinaryOp),
    /// Integer comparison operation; produces a `Bool` output.
    IntCmpOp(crate::ops::IntCmpOp),
    /// Cast any value (int / bool / float) to an integer of the declared
    /// output type.
    CastToInt,
    /// Narrow an integer value by dropping high bits.
    Truncate,
    /// Count the number of set bits in an integer value.
    Popcount,
    /// Count the number of leading zero bits in an integer value.
    Lzcount,
    /// Widen an integer value by zero- or sign-extending it.
    Extend(crate::ops::ExtendOp),

    // ── Boolean constants and operations ──────────────────────────────────────
    /// A compile-time boolean constant.
    BoolConst(bool),
    /// Boolean unary operation (logical NOT).
    BoolUnaryOp(crate::ops::BoolUnaryOp),
    /// Boolean binary operation (AND, OR, XOR).
    BoolBinaryOp(crate::ops::BoolBinaryOp),
    /// Convert an integer value to `Bool`.
    CastToBool,

    // ── Float constants and operations ────────────────────────────────────────
    /// A compile-time IEEE 754 floating-point constant.  The value is stored
    /// as its raw bit pattern in a `u64` (upper 32 bits are zero for `F32`).
    FloatConst(u64),
    /// Floating-point binary operation (add, sub, mul, div).
    FloatBinaryOp(crate::ops::FloatBinaryOp),
    /// Floating-point unary operation (neg, abs, sqrt, ceil, floor, round).
    FloatUnaryOp(crate::ops::FloatUnaryOp),
    /// Floating-point comparison; produces a `Bool` output.
    FloatCmpOp(crate::ops::FloatCmpOp),

    // ── Float / integer conversions ───────────────────────────────────────────
    /// Convert an integer value to the nearest representable float
    /// (like C's `(float)n`).  Input is integer, output is `F32` or `F64`.
    IntToFloat,
    /// Convert a float to an integer by truncating toward zero
    /// (like C's `(int)f`).  Input is `F32`/`F64`, output is an integer type.
    FloatToInt,
    /// Change floating-point precision (`F32` ↔ `F64`).
    FloatToFloat,

    // ── Bitcasts (reinterpretation of bit patterns) ───────────────────────────
    /// Reinterpret an integer's raw bits as a float of the same size.
    /// `U32` → `F32`, `U64` → `F64`.  No value conversion — bits are unchanged.
    IntBitsToFloat,
    /// Reinterpret a float's raw bits as an integer of the same size.
    /// `F32` → `U32`, `F64` → `U64`.  No value conversion — bits are unchanged.
    FloatBitsToInt,

    // ── Generic float cast ────────────────────────────────────────────────────
    /// Generic cast of any value to a floating-point type (F32 or F64).
    ///
    /// The optimizer lowers this to the appropriate specific form based on the
    /// actual input type:
    /// - Integer same size (U32→F32, U64→F64) → `IntBitsToFloat`
    /// - Float same type → eliminated (identity)
    /// - Float different size → `FloatToFloat`
    CastToFloat,

    // ── User-defined / opaque opcodes ─────────────────────────────────────────
    /// User-defined operation (`CallOther` in Sleigh p-code): CPU intrinsics
    /// such as `cpuid`, `rdtsc`, `syscall`, x87 transcendentals, etc.
    ///
    /// Inputs: `[control, memory, arg0, arg1, …]`.
    /// Outputs: `[Control, Memory]` if the instruction has no output varnode,
    /// or `[Control, Memory, OutputType]` if it does.  Memory is always
    /// clobbered — downstream loads must depend on the new memory token.
    /// Non-cacheable.
    CallOther { user_op_id: u64 },

    /// Segmented-address lookup (`SegmentOp` in Sleigh p-code).  Resolves a
    /// (segment, offset) pair to a flat pointer.  Pure computation.
    ///
    /// Inputs: `[segment, offset]`.  Outputs: `[OutputType]` (pointer-sized).
    /// Cacheable.
    SegmentOp { op_id: u64 },

    /// Java constant-pool reference (`CPoolRef` in Sleigh p-code).  Looks up a
    /// value in the class's constant pool.  Opaque.
    ///
    /// Inputs: `[ref0, ref1, …]`.  Outputs: `[OutputType]`.  Non-cacheable
    /// because resolution may have observable side effects (class loading).
    CPoolRef,

    /// Java object allocation (`New` in Sleigh p-code).  Allocates a fresh
    /// object of the given type.  Opaque.
    ///
    /// Inputs: `[size, …]`.  Outputs: `[OutputType]` (pointer-sized).
    /// Non-cacheable — each allocation yields a distinct object.
    New,
}

impl NodeKind {
    /// Returns `true` if this node represents a compile-time constant
    /// (`BoolConst`, `IntConst`, `IntConstWide`, or `FloatConst`).
    ///
    /// Exhaustive (no `_` arm) so adding a new const-shape `NodeKind`
    /// variant is a compile error here — see [`crate::walk::cast_mask_of`]
    /// for the same pattern.  This forces an explicit decision at every
    /// constant-handling site (constant folding, validator typing,
    /// pattern matching, …) when the IR grows a new constant kind.
    /// All non-const variants are listed explicitly under one `false`
    /// arm to keep the compile-time exhaustiveness check while
    /// satisfying clippy's `match_same_arms` lint.
    #[inline]
    #[must_use]
    pub fn is_const(self) -> bool {
        match self {
            Self::BoolConst(..)
            | Self::IntConst(..)
            | Self::IntConstWide(..)
            | Self::FloatConst(..) => true,

            // Every other variant — explicitly named so adding a new
            // `NodeKind` is a compile error here.
            Self::Entry
            | Self::InitialMemory
            | Self::InitialVar(..)
            | Self::FunctionArg { .. }
            | Self::ControlState
            | Self::MemPhi
            | Self::Phi
            | Self::If
            | Self::Call
            | Self::Return
            | Self::IndirectBranch
            | Self::CallOther { .. }
            | Self::Load(..)
            | Self::Store(..)
            | Self::StackStore { .. }
            | Self::StackStorePhi { .. }
            | Self::IntUnaryOp(..)
            | Self::IntBinaryOp(..)
            | Self::IntCmpOp(..)
            | Self::CastToInt
            | Self::Truncate
            | Self::Popcount
            | Self::Lzcount
            | Self::Extend(..)
            | Self::BoolUnaryOp(..)
            | Self::BoolBinaryOp(..)
            | Self::CastToBool
            | Self::FloatBinaryOp(..)
            | Self::FloatUnaryOp(..)
            | Self::FloatCmpOp(..)
            | Self::IntToFloat
            | Self::FloatToInt
            | Self::FloatToFloat
            | Self::IntBitsToFloat
            | Self::FloatBitsToInt
            | Self::CastToFloat
            | Self::SegmentOp { .. }
            | Self::CPoolRef
            | Self::New => false,
        }
    }

    /// Returns the structural [`NodeCategory`] of this kind.
    ///
    /// This is the single source of truth for "structural / region /
    /// initial-state" predicates (cacheability, asm-fingerprint
    /// exemption, …).  The match is exhaustive on purpose: adding a
    /// new [`NodeKind`] variant forces an explicit decision about its
    /// category here, and the derived predicates ([`Self::is_cacheable`],
    /// the validator's `asm_fingerprint_exempt`) update automatically.
    #[inline]
    #[must_use]
    pub fn category(&self) -> NodeCategory {
        match self {
            // Region header.
            Self::ControlState => NodeCategory::Region,

            // Initial state at function entry.
            Self::Entry
            | Self::InitialMemory
            | Self::InitialVar(..)
            | Self::FunctionArg { .. } => NodeCategory::InitialState,

            // Phis (SSA / memory / stack).
            Self::Phi | Self::MemPhi | Self::StackStorePhi { .. } => NodeCategory::Phi,

            // Control-flow terminators / call-shaped nodes.
            Self::Return | Self::IndirectBranch | Self::Call | Self::CallOther { .. } => {
                NodeCategory::Terminator
            }

            // Sleigh user-ops with opaque side effects (each occurrence
            // is distinct).  `CallOther` is also a Sleigh user-op but
            // lives in the `Terminator` bucket above because of its
            // control/memory shape; `SegmentOp` is pure and falls into
            // `PureValue`.
            Self::CPoolRef | Self::New => NodeCategory::OpaqueCall,

            // Everything else: pure value-producing computation.
            Self::If
            | Self::Load(..)
            | Self::Store(..)
            | Self::StackStore { .. }
            | Self::IntConst(..)
            | Self::IntConstWide(..)
            | Self::IntUnaryOp(..)
            | Self::IntBinaryOp(..)
            | Self::IntCmpOp(..)
            | Self::CastToInt
            | Self::Truncate
            | Self::Popcount
            | Self::Lzcount
            | Self::Extend(..)
            | Self::BoolConst(..)
            | Self::BoolUnaryOp(..)
            | Self::BoolBinaryOp(..)
            | Self::CastToBool
            | Self::FloatConst(..)
            | Self::FloatBinaryOp(..)
            | Self::FloatUnaryOp(..)
            | Self::FloatCmpOp(..)
            | Self::IntToFloat
            | Self::FloatToInt
            | Self::FloatToFloat
            | Self::IntBitsToFloat
            | Self::FloatBitsToInt
            | Self::CastToFloat
            | Self::SegmentOp { .. } => NodeCategory::PureValue,
        }
    }

    /// Returns `true` if nodes of this kind may be deduplicated in the graph
    /// cache.
    ///
    /// Nodes whose inputs are added incrementally after construction (e.g.
    /// `ControlState`, `Phi`) or that must always produce a fresh node
    /// (e.g. `Return`) are not cacheable.  Derived from [`Self::category`]:
    /// only [`NodeCategory::PureValue`] nodes are cacheable.
    #[inline]
    #[must_use]
    pub fn is_cacheable(&self) -> bool {
        matches!(self.category(), NodeCategory::PureValue)
    }

    /// Returns a stable, human-readable name for this variant.
    ///
    /// The name elides scalar payload that's irrelevant to structural
    /// shape (e.g. the numeric value of an [`Self::IntConst`], or the
    /// varnode of an [`Self::InitialVar`]) but keeps inner operator
    /// payload that *is* a structural property of the source program
    /// (e.g. [`Self::IntBinaryOp(Add)`](Self::IntBinaryOp) →
    /// `"IntBinaryOp(Add)"`).  Names are stable across releases and
    /// suitable for use as map keys, snapshot text, and debug logs.
    ///
    /// This is the canonical "variant name" function; previously
    /// duplicated in test code under `cross_arch_shape::kind_bucket`.
    #[inline]
    #[must_use]
    pub fn as_static_str(&self) -> &'static str {
        use crate::ops::{
            BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp,
            IntBinaryOp, IntCmpOp, IntUnaryOp,
        };
        match self {
            Self::Entry => "Entry",
            Self::InitialMemory => "InitialMemory",
            Self::InitialVar(_) => "InitialVar",
            Self::FunctionArg { .. } => "FunctionArg",
            Self::ControlState => "ControlState",
            Self::MemPhi => "MemPhi",
            Self::Phi => "Phi",
            Self::If => "If",
            Self::Call => "Call",
            Self::Return => "Return",
            Self::IndirectBranch => "IndirectBranch",
            Self::Load(_) => "Load",
            Self::Store(_) => "Store",
            Self::StackStore { .. } => "StackStore",
            Self::StackStorePhi { .. } => "StackStorePhi",
            Self::IntConst(_) => "IntConst",
            Self::IntConstWide(_) => "IntConstWide",
            Self::IntUnaryOp(op) => match op {
                IntUnaryOp::BitNot => "IntUnaryOp(BitNot)",
                IntUnaryOp::Neg => "IntUnaryOp(Neg)",
            },
            Self::IntBinaryOp(op) => match op {
                IntBinaryOp::Add => "IntBinaryOp(Add)",
                IntBinaryOp::Mul => "IntBinaryOp(Mul)",
                IntBinaryOp::Div => "IntBinaryOp(Div)",
                IntBinaryOp::Sdiv => "IntBinaryOp(Sdiv)",
                IntBinaryOp::Rem => "IntBinaryOp(Rem)",
                IntBinaryOp::Srem => "IntBinaryOp(Srem)",
                IntBinaryOp::And => "IntBinaryOp(And)",
                IntBinaryOp::Or => "IntBinaryOp(Or)",
                IntBinaryOp::Xor => "IntBinaryOp(Xor)",
                IntBinaryOp::ShiftLeft => "IntBinaryOp(ShiftLeft)",
                IntBinaryOp::ShiftRight => "IntBinaryOp(ShiftRight)",
                IntBinaryOp::SShiftRight => "IntBinaryOp(SShiftRight)",
            },
            Self::IntCmpOp(op) => match op {
                IntCmpOp::Equal => "IntCmpOp(Equal)",
                IntCmpOp::Less => "IntCmpOp(Less)",
                IntCmpOp::Sless => "IntCmpOp(Sless)",
                IntCmpOp::Carry => "IntCmpOp(Carry)",
                IntCmpOp::Scarry => "IntCmpOp(Scarry)",
                IntCmpOp::Sborrow => "IntCmpOp(Sborrow)",
            },
            Self::CastToInt => "CastToInt",
            Self::Truncate => "Truncate",
            Self::Popcount => "Popcount",
            Self::Lzcount => "Lzcount",
            Self::Extend(op) => match op {
                ExtendOp::ZeroExtend => "Extend(ZeroExtend)",
                ExtendOp::SignExtend => "Extend(SignExtend)",
            },
            Self::BoolConst(_) => "BoolConst",
            Self::BoolUnaryOp(op) => match op {
                BoolUnaryOp::Neg => "BoolUnaryOp(Neg)",
            },
            Self::BoolBinaryOp(op) => match op {
                BoolBinaryOp::And => "BoolBinaryOp(And)",
                BoolBinaryOp::Or => "BoolBinaryOp(Or)",
                BoolBinaryOp::Xor => "BoolBinaryOp(Xor)",
            },
            Self::CastToBool => "CastToBool",
            Self::FloatConst(_) => "FloatConst",
            Self::FloatBinaryOp(op) => match op {
                FloatBinaryOp::Add => "FloatBinaryOp(Add)",
                FloatBinaryOp::Mul => "FloatBinaryOp(Mul)",
                FloatBinaryOp::Div => "FloatBinaryOp(Div)",
            },
            Self::FloatUnaryOp(op) => match op {
                FloatUnaryOp::Neg => "FloatUnaryOp(Neg)",
                FloatUnaryOp::Abs => "FloatUnaryOp(Abs)",
                FloatUnaryOp::Sqrt => "FloatUnaryOp(Sqrt)",
                FloatUnaryOp::Ceil => "FloatUnaryOp(Ceil)",
                FloatUnaryOp::Floor => "FloatUnaryOp(Floor)",
                FloatUnaryOp::Round => "FloatUnaryOp(Round)",
            },
            Self::FloatCmpOp(op) => match op {
                FloatCmpOp::Equal => "FloatCmpOp(Equal)",
                FloatCmpOp::Less => "FloatCmpOp(Less)",
            },
            Self::IntToFloat => "IntToFloat",
            Self::FloatToInt => "FloatToInt",
            Self::FloatToFloat => "FloatToFloat",
            Self::IntBitsToFloat => "IntBitsToFloat",
            Self::FloatBitsToInt => "FloatBitsToInt",
            Self::CastToFloat => "CastToFloat",
            Self::CallOther { .. } => "CallOther",
            Self::SegmentOp { .. } => "SegmentOp",
            Self::CPoolRef => "CPoolRef",
            Self::New => "New",
        }
    }

    /// Returns `true` if this node kind is commutative under operand swap.
    ///
    /// A binary operator `op(a, b)` is commutative iff `op(a, b) == op(b, a)`
    /// always holds.  Patterns matching commutative nodes can match both
    /// operand orderings; non-commutative nodes match only the declared order.
    ///
    /// Float operators that ignore NaN ordering (`FloatAdd`, `FloatMul`) are
    /// commutative per the IEEE-754 commutativity-up-to-NaN convention used
    /// throughout the IR.  Comparison ops are commutative only when symmetric
    /// (`IntCmpOp::Equal` yes; `IntCmpOp::Less` no, etc.).  `Carry(l, r)` and
    /// `Scarry(l, r)` ask whether `l + r` overflows (unsigned / signed
    /// respectively); since addition commutes, so do these comparisons.
    /// `FloatCmpOp::Equal` is symmetric for IEEE 754 (yields the same result
    /// regardless of operand order, including for NaN inputs).
    ///
    /// All other kinds (non-binary ops, calls, loads, phis, …) return `false`.
    /// This method is the single source of truth — replaces the per-op-enum
    /// helpers that previously lived under `pattern::matcher::commutativity`.
    #[inline]
    #[must_use]
    pub fn is_commutative(&self) -> bool {
        use crate::ops::{BoolBinaryOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
        match self {
            Self::IntBinaryOp(op) => matches!(
                op,
                IntBinaryOp::Add
                    | IntBinaryOp::Mul
                    | IntBinaryOp::And
                    | IntBinaryOp::Or
                    | IntBinaryOp::Xor
            ),
            Self::BoolBinaryOp(op) => {
                matches!(op, BoolBinaryOp::And | BoolBinaryOp::Or | BoolBinaryOp::Xor)
            }
            Self::FloatBinaryOp(op) => matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul),
            Self::IntCmpOp(op) => {
                matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
            }
            Self::FloatCmpOp(op) => matches!(op, FloatCmpOp::Equal),
            _ => false,
        }
    }
}
