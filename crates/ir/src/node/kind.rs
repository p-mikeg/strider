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
    /// per `index` (enforced by [`crate::validate::layer_c`]).  Source nodes
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
    /// SSA φ for varnode `Vn` at a join point.  Inputs:
    /// `[phi_token, val_0, val_1, …]` where `phi_token` is the
    /// `PhiToken` output of the joining `ControlState` and the rest
    /// are one value per CFG predecessor in the same order as the
    /// `ControlState`'s `Control` inputs.  Output: `[value]`.
    VarPhi(rsleigh::Vn),
    /// Value phi not tied to any source varnode.  Synthesized by
    /// [`opt::StackLoadForward`](../../../opt/src/stack_load_forward/mod.rs) when
    /// forwarding a `Load[sp+K]` across a `MemPhi`: each predecessor
    /// resolves to a stored value, and those values are merged here.  Shape
    /// matches `VarPhi` — inputs `[phi_token, val_0, val_1, …]`, output
    /// is a single value — but without a `Vn` tag since the merged value
    /// has no source-level register/memory identity.  Non-cacheable for the
    /// same reason as `VarPhi`/`MemPhi`: phi identity matters.
    ValuePhi,

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
    /// [`crate::Graph::wide_consts`] off-side.
    IntConst(u128),
    /// A compile-time integer constant whose value doesn't fit in
    /// `u128` — `U256` or `U512`.  The actual byte payload lives in
    /// [`crate::Graph::wide_consts`] and this node carries a
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
    /// (`BoolConst`, `IntConst`, or `FloatConst`).
    #[inline]
    #[must_use]
    pub fn is_const(self) -> bool {
        matches!(
            self,
            Self::BoolConst(..)
                | Self::IntConst(..)
                | Self::IntConstWide(..)
                | Self::FloatConst(..)
        )
    }

    /// Returns `true` if nodes of this kind may be deduplicated in the graph
    /// cache.
    ///
    /// Nodes whose inputs are added incrementally after construction (e.g.
    /// `ControlState`, `VarPhi`) or that must always produce a fresh node
    /// (e.g. `Return`) are not cacheable.
    #[inline]
    #[must_use]
    pub fn is_cacheable(&self) -> bool {
        !matches!(
            self,
            Self::Entry
                | Self::InitialMemory
                | Self::InitialVar(..)
                | Self::FunctionArg { .. }
                | Self::Return
                | Self::IndirectBranch
                | Self::ControlState
                | Self::MemPhi
                | Self::VarPhi(..)
                | Self::ValuePhi
                | Self::Call
                | Self::CallOther { .. }
                | Self::CPoolRef
                | Self::New
                | Self::StackStorePhi { .. }
        )
    }

}
