use std::fmt::Debug;

use cranelift_entity::{EntityList, entity_impl, packed_option::PackedOption};

/// A unique identifier for a node in the IR graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId, "node");

/// A unique identifier for one output slot of a node.
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeOutputId(u32);
entity_impl!(NodeOutputId, "%");

/// A unique identifier for one input slot of a node.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInputId(u32);
entity_impl!(NodeInputId, "input");

/// A list of input slot ids stored in an entity pool.
pub(crate) type NodeInputIdList = EntityList<NodeInputId>;

/// A list of output slot ids stored in an entity pool.
pub(crate) type NodeOutputIdList = EntityList<NodeOutputId>;

/// The value type carried by a node output.
///
/// Integer variants correspond directly to their C-style unsigned integer
/// widths.  `Bool` is a 1-bit logical value.  `F32`/`F64` are IEEE 754
/// floating-point types whose raw bit patterns are stored as `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputType {
    Bool,
    U8,
    U16,
    U32,
    U64,
    U128,
    U256,
    /// 32-bit IEEE 754 single-precision float.
    F32,
    /// 64-bit IEEE 754 double-precision float.
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeOutputTypeCategory {
    Bool,
    Int,
    Float,
}

struct TypeInfo {
    name: &'static str,
    byte_size: u8,
    category: NodeOutputTypeCategory,
}

// Order MUST match the `NodeOutputType` enum declaration order
// (asserted by `type_info_table_matches_variants`).
const TYPE_INFO: &[TypeInfo] = &[
    TypeInfo { name: "bool", byte_size: 1,  category: NodeOutputTypeCategory::Bool  },
    TypeInfo { name: "u8",   byte_size: 1,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u16",  byte_size: 2,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u32",  byte_size: 4,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u64",  byte_size: 8,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u128", byte_size: 16, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u256", byte_size: 32, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "f32",  byte_size: 4,  category: NodeOutputTypeCategory::Float },
    TypeInfo { name: "f64",  byte_size: 8,  category: NodeOutputTypeCategory::Float },
];

impl NodeOutputType {
    #[inline]
    fn info(self) -> &'static TypeInfo {
        &TYPE_INFO[self as usize]
    }

    /// Returns the canonical name of this type as a static string.
    #[inline]
    #[must_use] 
    pub fn as_str(self) -> &'static str {
        self.info().name
    }

    /// Returns the size of this type **in bytes**.
    ///
    /// Both `Bool` and `U8` return 1.
    #[inline]
    #[must_use] 
    pub fn byte_size(self) -> usize {
        self.info().byte_size as usize
    }

    /// Returns the width of this type **in bits** (`byte_size * 8`).
    #[inline]
    #[must_use] 
    pub fn bit_width(self) -> usize {
        self.byte_size() * 8
    }

    /// Whether a constant of this type fits in a `u64` (i.e. `byte_size <= 8`).
    ///
    /// Returns `true` for `Bool`, `U8`, `U16`, `U32`, `U64`, `F32`, and `F64`.
    /// Returns `false` for `U128` and `U256`.
    #[inline]
    #[must_use] 
    pub fn fits_u64(self) -> bool {
        self.byte_size() <= 8
    }

    /// Returns `true` if this type is `Bool`.
    #[inline]
    #[must_use] 
    pub fn is_bool(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Bool)
    }

    /// Returns `true` if this type is one of the unsigned integer variants
    /// (U8, U16, U32, U64, U128, U256).
    #[inline]
    #[must_use] 
    pub fn is_integer(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Int)
    }

    /// Returns `true` if this type is `F32` or `F64`.
    #[inline]
    #[must_use] 
    pub fn is_float(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Float)
    }

    /// Returns the unsigned integer type with the same byte size.
    /// (Bool→U8, F32→U32, F64→U64, Ux→Ux)
    #[inline]
    #[must_use] 
    pub fn to_natural_int_type(self) -> NodeOutputType {
        match self {
            NodeOutputType::Bool | NodeOutputType::U8 => NodeOutputType::U8,
            NodeOutputType::U16 => NodeOutputType::U16,
            NodeOutputType::U32 | NodeOutputType::F32 => NodeOutputType::U32,
            NodeOutputType::U64 | NodeOutputType::F64 => NodeOutputType::U64,
            NodeOutputType::U128 => NodeOutputType::U128,
            NodeOutputType::U256 => NodeOutputType::U256,
        }
    }

    /// Interprets `val` as an unsigned integer of this width and returns the
    /// truncated value, or `None` if this type is `Bool` or a float type.
    ///
    /// The truncation ensures that bits beyond the type's width are cleared,
    /// matching the hardware behaviour of narrower registers.
    #[inline]
    #[must_use] 
    pub fn get_unsigned_int(self, val: u64) -> Option<u64> {
        match self {
            NodeOutputType::Bool
            | NodeOutputType::U128
            | NodeOutputType::U256
            | NodeOutputType::F32
            | NodeOutputType::F64 => None,
            NodeOutputType::U8 => Some(val as u8 as u64),
            NodeOutputType::U16 => Some(val as u16 as u64),
            NodeOutputType::U32 => Some(val as u32 as u64),
            NodeOutputType::U64 => Some(val),
        }
    }

    /// Interprets `val` as a signed integer of this width with sign-extension
    /// and returns the result, or `None` if this type is `Bool` or a float type.
    ///
    /// Casting through the signed type of the same width sign-extends the
    /// value to 64 bits.
    #[inline]
    #[must_use] 
    pub fn get_signed_int(self, val: u64) -> Option<i64> {
        match self {
            NodeOutputType::Bool
            | NodeOutputType::U128
            | NodeOutputType::U256
            | NodeOutputType::F32
            | NodeOutputType::F64 => None,
            NodeOutputType::U8 => Some(val as i8 as i64),
            NodeOutputType::U16 => Some(val as i16 as i64),
            NodeOutputType::U32 => Some(val as i32 as i64),
            NodeOutputType::U64 => Some(val as i64),
        }
    }

    /// Sign-extends `val` from this type's width to 64 bits and returns the
    /// result as a `u64` bit pattern.
    ///
    /// Returns `None` if this type is `Bool`, `U128`, `U256`, or a float type,
    /// since those widths either are not integer or cannot be represented in 64
    /// bits.
    #[inline]
    #[must_use] 
    pub fn sign_extend(self, val: u64) -> Option<u64> {
        self.get_signed_int(val).map(|v| v as u64)
    }
}

impl TryFrom<u32> for NodeOutputType {
    type Error = crate::error::Error;

    fn try_from(value: u32) -> crate::error::Result<Self> {
        match value {
            1 => Ok(Self::U8),
            2 => Ok(Self::U16),
            4 => Ok(Self::U32),
            8 => Ok(Self::U64),
            16 => Ok(Self::U128),
            32 => Ok(Self::U256),
            n => Err(crate::error::ErrorKind::UnsupportedOutputSize(n).into()),
        }
    }
}

impl std::fmt::Display for NodeOutputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of data carried by a node output edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputKind {
    /// A concrete value output with an associated [`NodeOutputType`].
    OutputType(NodeOutputType),
    /// Control-flow token.  Every region consumes one control edge per
    /// predecessor and every branch node produces one per successor.
    Control,
    /// Phi-dispatch token produced by `ControlState` nodes and consumed by
    /// `ControlPhi` nodes.  Carries no data — it is a synchronisation edge
    /// that links each phi to exactly one `ControlState`.
    ControlPhi,
    /// Memory token tracking the current state of memory through the graph.
    Memory,
}

impl NodeOutputKind {
    /// Returns `true` if this is a value output (`OutputType` variant).
    #[inline]
    #[must_use] 
    pub fn is_value(self) -> bool {
        matches!(self, Self::OutputType(..))
    }

    /// Returns the inner [`NodeOutputType`] if this is a value output,
    /// otherwise `None`.
    #[inline]
    #[must_use] 
    pub fn as_value(self) -> Option<NodeOutputType> {
        match self {
            Self::OutputType(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the value type, or an error whose payload is `self` if this
    /// kind is not a value edge.
    #[track_caller]
    pub fn as_value_or_err(self) -> crate::Result<NodeOutputType> {
        self.as_value()
            .ok_or_else(|| crate::ErrorKind::ExpectedValueOutput(self).into())
    }

    /// Returns the value type, asserting it is integer. Errors as
    /// [`crate::ErrorKind::ExpectedValueOutput`] for non-value kinds and as
    /// [`crate::ErrorKind::ExpectedIntegerType`] for bool/float value kinds.
    #[track_caller]
    pub fn as_integer_or_err(self) -> crate::Result<NodeOutputType> {
        let ty = self.as_value_or_err()?;
        if ty.is_integer() {
            Ok(ty)
        } else {
            Err(crate::ErrorKind::ExpectedIntegerType(ty).into())
        }
    }

    /// Returns the value type, asserting it is float. Errors as
    /// [`crate::ErrorKind::ExpectedValueOutput`] for non-value kinds and as
    /// [`crate::ErrorKind::ExpectedFloatType`] for bool/int value kinds.
    #[track_caller]
    pub fn as_float_or_err(self) -> crate::Result<NodeOutputType> {
        let ty = self.as_value_or_err()?;
        if ty.is_float() {
            Ok(ty)
        } else {
            Err(crate::ErrorKind::ExpectedFloatType(ty).into())
        }
    }

    /// Returns `true` if this is a control-flow edge.
    #[inline]
    #[must_use] 
    pub fn is_control(self) -> bool {
        self == Self::Control
    }

    /// Returns `true` if this is a control-phi dispatch edge.
    #[inline]
    #[must_use] 
    pub fn is_control_phi(self) -> bool {
        self == Self::ControlPhi
    }

    /// Returns `true` if this is a memory edge.
    #[inline]
    #[must_use] 
    pub fn is_memory(self) -> bool {
        self == Self::Memory
    }

    /// Returns `true` if this is a value output carrying a `Bool` type.
    #[inline]
    #[must_use] 
    pub fn is_bool(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_bool()
        } else {
            false
        }
    }

    /// Returns `true` if this is a value output carrying an integer type.
    #[inline]
    #[must_use] 
    pub fn is_integer(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_integer()
        } else {
            false
        }
    }
}

/// Stores the output of a given node and tracks all of its uses via a
/// linked list of [`NodeInput`] ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeOutput {
    /// What kind of value this output carries.
    pub(crate) kind: NodeOutputKind,
    /// The node that produces this output.
    pub(crate) source_id: NodeId,
    /// The index of this output in the source node's output list.
    pub(crate) output_index: u32,
    /// Head of the linked list of all inputs that consume this output.
    pub(crate) first_use: PackedOption<NodeInputId>,
}

impl NodeOutput {
    /// Creates a new `NodeOutput` with no uses yet.
    #[must_use] 
    pub fn new(kind: NodeOutputKind, source_id: NodeId, output_index: u32) -> Self {
        NodeOutput {
            kind,
            source_id,
            output_index,
            first_use: None.into(),
        }
    }
}

/// Records a single use of a [`NodeOutput`] as the input of some node.
///
/// Forms part of a doubly-linked list of all uses of a particular output,
/// enabling efficient update of all consumers when an output changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInput {
    /// The output being consumed.
    pub(crate) output_id: NodeOutputId,
    /// Previous use in the linked list of uses for `output_id`.
    pub(crate) prev: PackedOption<NodeInputId>,
    /// Next use in the linked list of uses for `output_id`.
    pub(crate) next: PackedOption<NodeInputId>,
    /// The node that consumes this input.
    pub(crate) node_id: NodeId,
    /// The position of this input in the consuming node's input list.
    pub(crate) input_index: u32,
}

impl NodeInput {
    /// Creates a new `NodeInput` not yet linked into any use list.
    #[must_use] 
    pub fn new(output_id: NodeOutputId, node_id: NodeId, input_index: u32) -> Self {
        NodeInput {
            output_id,
            prev: None.into(),
            next: None.into(),
            node_id,
            input_index,
        }
    }
}

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
    /// [`opt::FunctionArgDetect`](../../../opt/src/function_args.rs) which
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
    /// and produces a fresh `Control` output plus a `ControlPhi` dispatch token.
    ControlState,
    /// Memory phi: selects the live memory token at a join point.
    MemPhi,
    /// Control phi: selects the value of varnode `Vn` at a join point,
    /// corresponding to the SSA φ-function from the literature.
    ControlPhi(rsleigh::Vn),
    /// Value phi not tied to any source varnode.  Synthesized by
    /// [`opt::StackLoadForward`](../../../opt/src/stack_load_forward.rs) when
    /// forwarding a `Load[sp+K]` across a `MemPhi`: each predecessor
    /// resolves to a stored value, and those values are merged here.  Shape
    /// matches `ControlPhi` — inputs `[phi_token, val_0, val_1, …]`, output
    /// is a single value — but without a `Vn` tag since the merged value
    /// has no source-level register/memory identity.  Non-cacheable for the
    /// same reason as `ControlPhi`/`MemPhi`: phi identity matters.
    ValuePhi,

    // ── Conditional branch ─────────────────────────────────────────────────────
    /// Conditional branch.  Consumes `(control, bool_cond)` and produces two
    /// `Control` outputs: index 0 for the true branch, index 1 for the false branch.
    If,

    // ── Calls and returns ──────────────────────────────────────────────────────
    /// Function call.  Clobbers caller-saved registers and the memory token.
    Call,
    /// Post-call memory state produced by a `Call` node.
    PostCallMemState,
    /// Post-call value of caller-saved varnode `Vn` produced by a `Call` node.
    PostCallVarState(rsleigh::Vn),
    /// Function return.  Consumes the outgoing control edge and any return-value outputs.
    Return,

    // ── Memory operations ──────────────────────────────────────────────────────
    /// Load from the given address space.
    Load(rsleigh::VnSpace),
    /// Store to the given address space.
    Store(rsleigh::VnSpace),

    // ── Stack-slot stores (produced by StackStoreDetect) ──────────────────────
    /// Store whose address has been resolved to `base + offset`, where `base`
    /// is an SP-rooted node (either `InitialVar(stack_ptr)` or a
    /// `ControlPhi(stack_ptr)` that could not be further reduced — typically
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
    /// A compile-time integer constant of value `u64`.
    IntConst(u64),
    /// Integer unary operation (e.g. bitwise NOT, two's-complement negate).
    IntUnaryOp(crate::ops::IntUnaryOp),
    /// Integer binary operation (e.g. add, shift, bitwise AND).
    IntBinaryOp(crate::ops::IntBinaryOp),
    /// Integer comparison operation; produces a `Bool` output.
    IntCmpOp(crate::ops::IntCmpOp),
    /// Reinterpret an integer value as `Bool` (`0` → `false`, non-zero → `true`).
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
            Self::BoolConst(..) | Self::IntConst(..) | Self::FloatConst(..)
        )
    }

    /// Returns `true` if nodes of this kind may be deduplicated in the graph
    /// cache.
    ///
    /// Nodes whose inputs are added incrementally after construction (e.g.
    /// `ControlState`, `ControlPhi`) or that must always produce a fresh node
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
                | Self::ControlState
                | Self::MemPhi
                | Self::ControlPhi(..)
                | Self::ValuePhi
                | Self::Call
                | Self::CallOther { .. }
                | Self::CPoolRef
                | Self::New
                | Self::StackStorePhi { .. }
        )
    }

    /// Returns `true` for any phi node kind: [`NodeKind::ControlPhi`],
    /// [`NodeKind::MemPhi`], [`NodeKind::StackStorePhi`], or
    /// [`NodeKind::ValuePhi`].
    #[inline]
    #[must_use] 
    pub fn is_phi(&self) -> bool {
        matches!(
            self,
            Self::ControlPhi(_)
                | Self::MemPhi
                | Self::StackStorePhi { .. }
                | Self::ValuePhi
        )
    }
}

/// A node in the IR graph.
///
/// Holds the node's kind along with its input and output slot lists (stored
/// externally in entity pools).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) inputs: NodeInputIdList,
    pub(crate) outputs: NodeOutputIdList,
}

impl Node {
    /// Creates a new node with the given kind and empty input/output lists.
    #[must_use] 
    pub fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            inputs: NodeInputIdList::new(),
            outputs: NodeOutputIdList::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── NodeOutputType ───────────────────────────────────────────────────────

    /// `get_unsigned_int` must mask the value to the declared width.
    /// Bits above the type's width must be cleared even if they are set in
    /// the raw u64.
    #[test]
    fn unsigned_int_masks_to_declared_width() {
        let wide: u64 = u64::MAX;
        assert_eq!(
            NodeOutputType::U8.get_unsigned_int(wide),
            Some(u8::MAX as u64)
        );
        assert_eq!(
            NodeOutputType::U16.get_unsigned_int(wide),
            Some(u16::MAX as u64)
        );
        assert_eq!(
            NodeOutputType::U32.get_unsigned_int(wide),
            Some(u32::MAX as u64)
        );
        assert_eq!(NodeOutputType::U64.get_unsigned_int(wide), Some(u64::MAX));
    }

    /// `get_unsigned_int` must return `None` for `Bool` because a boolean is
    /// not an integer representation.
    #[test]
    fn unsigned_int_is_none_for_bool() {
        assert_eq!(NodeOutputType::Bool.get_unsigned_int(1), None);
    }

    /// `get_signed_int` must sign-extend values.  The MSB of the declared
    /// width acts as the sign bit, so a value with the MSB set must come out
    /// negative.
    #[test]
    fn signed_int_sign_extends_from_declared_width() {
        // u8::MAX as i8 is -1
        assert_eq!(NodeOutputType::U8.get_signed_int(u8::MAX as u64), Some(-1));
        // i8::MIN (0x80) sign-extends to -128
        assert_eq!(
            NodeOutputType::U8.get_signed_int(i8::MIN as u8 as u64),
            Some(i8::MIN as i64)
        );
        // i8::MAX (0x7F) stays positive
        assert_eq!(
            NodeOutputType::U8.get_signed_int(i8::MAX as u64),
            Some(i8::MAX as i64)
        );
        // i16::MIN (0x8000) sign-extends to -32768
        assert_eq!(
            NodeOutputType::U16.get_signed_int(i16::MIN as u16 as u64),
            Some(i16::MIN as i64)
        );
        // u32::MAX as u64 sign-extends as i32 to -1
        assert_eq!(
            NodeOutputType::U32.get_signed_int(u32::MAX as u64),
            Some(-1)
        );
    }

    /// `get_signed_int` must return `None` for `Bool`.
    #[test]
    fn signed_int_is_none_for_bool() {
        assert_eq!(NodeOutputType::Bool.get_signed_int(1), None);
    }

    /// `bit_width` must equal `byte_size * 8` for every variant.
    #[test]
    fn bit_width_is_eight_times_byte_size() {
        for ty in [
            NodeOutputType::Bool,
            NodeOutputType::U8,
            NodeOutputType::U16,
            NodeOutputType::U32,
            NodeOutputType::U64,
            NodeOutputType::U128,
            NodeOutputType::U256,
        ] {
            assert_eq!(
                ty.bit_width(),
                ty.byte_size() * 8,
                "bit_width mismatch for {ty:?}"
            );
        }
    }

    // ── NodeOutputKind ───────────────────────────────────────────────────────

    /// `is_value` must be `true` only for `OutputType` variants.
    #[test]
    fn is_value_only_for_output_type() {
        assert!(NodeOutputKind::OutputType(NodeOutputType::U64).is_value());
        assert!(!NodeOutputKind::Control.is_value());
        assert!(!NodeOutputKind::ControlPhi.is_value());
        assert!(!NodeOutputKind::Memory.is_value());
    }

    /// `is_bool` must be `true` only when the wrapped type is `Bool`.
    #[test]
    fn is_bool_only_for_bool_output_type() {
        assert!(NodeOutputKind::OutputType(NodeOutputType::Bool).is_bool());
        assert!(!NodeOutputKind::OutputType(NodeOutputType::U8).is_bool());
        assert!(!NodeOutputKind::Control.is_bool());
    }

    /// `is_integer` must be `true` for all integer `OutputType` variants and
    /// `false` for `Bool`, `Control`, `ControlPhi`, and `Memory`.
    #[test]
    fn is_integer_for_all_integer_output_types() {
        for ty in [
            NodeOutputType::U8,
            NodeOutputType::U16,
            NodeOutputType::U32,
            NodeOutputType::U64,
            NodeOutputType::U128,
            NodeOutputType::U256,
        ] {
            assert!(
                NodeOutputKind::OutputType(ty).is_integer(),
                "{ty:?} should be integer"
            );
        }
        assert!(!NodeOutputKind::OutputType(NodeOutputType::Bool).is_integer());
        assert!(!NodeOutputKind::Control.is_integer());
        assert!(!NodeOutputKind::Memory.is_integer());
    }

    // ── NodeKind ─────────────────────────────────────────────────────────────

    /// Only `BoolConst` and `IntConst` should be considered constants; all
    /// other variants must not.
    #[test]
    fn is_const_only_for_constant_kinds() {
        assert!(NodeKind::BoolConst(true).is_const());
        assert!(NodeKind::IntConst(42).is_const());
        assert!(!NodeKind::Entry.is_const());
        assert!(!NodeKind::Return.is_const());
    }

    /// Non-cacheable node kinds must cover all nodes that receive inputs
    /// dynamically after creation.
    #[test]
    fn non_cacheable_kinds_are_not_cacheable() {
        let space = rsleigh::VnSpace::RAM;
        let non_cacheable = [
            NodeKind::Entry,
            NodeKind::InitialMemory,
            NodeKind::Return,
            NodeKind::ControlState,
            NodeKind::MemPhi,
            NodeKind::ValuePhi,
            NodeKind::Call,
            NodeKind::StackStorePhi { space },
        ];
        for kind in non_cacheable {
            assert!(!kind.is_cacheable(), "{kind:?} should not be cacheable");
        }
    }

    /// Arithmetic and logical operations are always cacheable — equal nodes
    /// with equal inputs produce the same result and can be deduplicated.
    #[test]
    fn arithmetic_kinds_are_cacheable() {
        assert!(NodeKind::IntConst(0).is_cacheable());
        assert!(NodeKind::BoolConst(false).is_cacheable());
        assert!(NodeKind::IntBinaryOp(crate::ops::IntBinaryOp::Add).is_cacheable());
        assert!(NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::Neg).is_cacheable());
        assert!(NodeKind::If.is_cacheable());
    }

    /// `StackStore` is a normal cacheable memory operation (its identity is
    /// fully determined by space+offset+inputs), while `StackStorePhi` must
    /// stay non-cacheable because its offsets live in a side-map.
    #[test]
    fn stack_store_cacheability() {
        let space = rsleigh::VnSpace::RAM;
        assert!(NodeKind::StackStore { space, offset: 0 }.is_cacheable());
        assert!(!NodeKind::StackStorePhi { space }.is_cacheable());
    }

    // ── Float NodeOutputType ─────────────────────────────────────────────────

    #[test]
    fn float_byte_sizes() {
        assert_eq!(NodeOutputType::F32.byte_size(), 4);
        assert_eq!(NodeOutputType::F64.byte_size(), 8);
    }

    #[test]
    fn float_bit_widths() {
        assert_eq!(NodeOutputType::F32.bit_width(), 32);
        assert_eq!(NodeOutputType::F64.bit_width(), 64);
    }

    #[test]
    fn float_as_str() {
        assert_eq!(NodeOutputType::F32.as_str(), "f32");
        assert_eq!(NodeOutputType::F64.as_str(), "f64");
    }

    #[test]
    fn is_float_only_for_float_types() {
        assert!(NodeOutputType::F32.is_float());
        assert!(NodeOutputType::F64.is_float());
        assert!(!NodeOutputType::U32.is_float());
        assert!(!NodeOutputType::U64.is_float());
        assert!(!NodeOutputType::Bool.is_float());
    }

    #[test]
    fn is_integer_false_for_float_types() {
        assert!(!NodeOutputType::F32.is_integer());
        assert!(!NodeOutputType::F64.is_integer());
    }

    #[test]
    fn get_unsigned_int_returns_none_for_floats() {
        assert_eq!(NodeOutputType::F32.get_unsigned_int(0x3F800000), None);
        assert_eq!(
            NodeOutputType::F64.get_unsigned_int(0x3FF0000000000000),
            None
        );
    }

    #[test]
    fn get_signed_int_returns_none_for_floats() {
        assert_eq!(NodeOutputType::F32.get_signed_int(0x3F800000), None);
        assert_eq!(NodeOutputType::F64.get_signed_int(0x3FF0000000000000), None);
    }

    // ── Float NodeKind ───────────────────────────────────────────────────────

    #[test]
    fn float_const_is_const_and_cacheable() {
        let fc = NodeKind::FloatConst(0x3F800000);
        assert!(fc.is_const());
        assert!(fc.is_cacheable());
    }

    #[test]
    fn float_ops_are_cacheable() {
        assert!(NodeKind::FloatBinaryOp(crate::ops::FloatBinaryOp::Add).is_cacheable());
        assert!(NodeKind::FloatUnaryOp(crate::ops::FloatUnaryOp::Neg).is_cacheable());
        assert!(NodeKind::FloatCmpOp(crate::ops::FloatCmpOp::Equal).is_cacheable());
        assert!(NodeKind::IntToFloat.is_cacheable());
        assert!(NodeKind::FloatToInt.is_cacheable());
        assert!(NodeKind::FloatToFloat.is_cacheable());
        assert!(NodeKind::IntBitsToFloat.is_cacheable());
        assert!(NodeKind::FloatBitsToInt.is_cacheable());
    }

    // ── as_value_or_err / as_integer_or_err / as_float_or_err ──────────────

    #[test]
    fn as_value_or_err_value_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U32);
        assert_eq!(kind.as_value_or_err().unwrap(), NodeOutputType::U32);
    }

    #[test]
    fn as_value_or_err_control_case() {
        let kind = NodeOutputKind::Control;
        let err = kind.as_value_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedValueOutput(_)));
    }

    #[test]
    fn as_integer_or_err_int_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U64);
        assert_eq!(kind.as_integer_or_err().unwrap(), NodeOutputType::U64);
    }

    #[test]
    fn as_integer_or_err_float_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::F32);
        let err = kind.as_integer_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedIntegerType(_)));
    }

    #[test]
    fn as_float_or_err_float_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::F64);
        assert_eq!(kind.as_float_or_err().unwrap(), NodeOutputType::F64);
    }

    #[test]
    fn as_float_or_err_int_case() {
        let kind = NodeOutputKind::OutputType(NodeOutputType::U32);
        let err = kind.as_float_or_err().unwrap_err();
        assert!(matches!(err.kind(), crate::ErrorKind::ExpectedFloatType(_)));
    }

    // ── is_phi ───────────────────────────────────────────────────────────

    #[test]
    fn is_phi_true_cases() {
        let phi_vn = rsleigh::Vn {
            size: 8,
            addr: rsleigh::VnAddr {
                off: 0,
                space: rsleigh::VnSpace::REGISTER,
            },
        };
        assert!(NodeKind::ControlPhi(phi_vn).is_phi());
        assert!(NodeKind::MemPhi.is_phi());
        assert!(NodeKind::ValuePhi.is_phi());
        let space = rsleigh::VnSpace::RAM;
        assert!(NodeKind::StackStorePhi { space }.is_phi());
    }

    #[test]
    fn is_phi_false_cases() {
        assert!(!NodeKind::Entry.is_phi());
        assert!(!NodeKind::InitialMemory.is_phi());
        assert!(!NodeKind::IntConst(0).is_phi());
    }

    #[test]
    fn type_info_table_matches_variants() {
        // Table indices must match discriminant order. Enumerate every variant
        // explicitly and check `info().name` / category.
        let cases: &[(NodeOutputType, &str, usize, bool, bool, bool)] = &[
            (NodeOutputType::Bool, "bool", 1, false, true, false),
            (NodeOutputType::U8,   "u8",   1, true,  false, false),
            (NodeOutputType::U16,  "u16",  2, true,  false, false),
            (NodeOutputType::U32,  "u32",  4, true,  false, false),
            (NodeOutputType::U64,  "u64",  8, true,  false, false),
            (NodeOutputType::U128, "u128", 16, true, false, false),
            (NodeOutputType::U256, "u256", 32, true, false, false),
            (NodeOutputType::F32,  "f32",  4, false, false, true),
            (NodeOutputType::F64,  "f64",  8, false, false, true),
        ];
        for (ty, name, size, is_int, is_bool, is_float) in cases {
            assert_eq!(ty.as_str(), *name);
            assert_eq!(ty.byte_size(), *size);
            assert_eq!(ty.bit_width(), *size * 8);
            assert_eq!(ty.is_integer(), *is_int);
            assert_eq!(ty.is_bool(), *is_bool);
            assert_eq!(ty.is_float(), *is_float);
        }
    }
}
