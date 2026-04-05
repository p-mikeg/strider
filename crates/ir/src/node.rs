use std::fmt::Debug;

use cranelift_entity::{
    EntityList, entity_impl, packed_option::PackedOption,
};

/// A unique identifier for a node in the IR graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId, "node");


/// A unique identifier for one output slot of a node.
#[derive(Default)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
/// widths.  `Bool` is a 1-bit logical value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputType {
    Bool,
    U8,
    U16,
    U32,
    U64
}


impl NodeOutputType {
    /// Returns `true` if this type is one of the unsigned integer variants
    /// (U8, U16, U32, U64).
    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(self, NodeOutputType::U8 | NodeOutputType::U16 | NodeOutputType::U32 | NodeOutputType::U64)
    }

    /// Returns `true` if this type is `Bool`.
    #[inline]
    pub fn is_bool(self) -> bool {
        matches!(self, NodeOutputType::Bool)
    }

    /// Returns the canonical name of this type as a static string.
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            NodeOutputType::Bool => "bool",
            NodeOutputType::U8 => "u8",
            NodeOutputType::U16 => "u16",
            NodeOutputType::U32 => "u32",
            NodeOutputType::U64 => "u64",
        }
    }

    /// Returns the size of this type **in bytes**.
    ///
    /// Both `Bool` and `U8` return 1.
    #[inline]
    pub fn byte_size(self) -> usize {
        match self {
            NodeOutputType::Bool => 1,
            NodeOutputType::U8 => 1,
            NodeOutputType::U16 => 2,
            NodeOutputType::U32 => 4,
            NodeOutputType::U64 => 8,
        }
    }

    /// Returns the width of this type **in bits** (`byte_size * 8`).
    #[inline]
    pub fn bit_width(self) -> usize {
        self.byte_size() * 8
    }

    /// Interprets `val` as an unsigned integer of this width and returns the
    /// truncated value, or `None` if this type is `Bool`.
    ///
    /// The truncation ensures that bits beyond the type's width are cleared,
    /// matching the hardware behaviour of narrower registers.
    #[inline]
    pub fn get_unsigned_int(self, val: u64) -> Option<u64> {
        match self {
            NodeOutputType::Bool => None,
            NodeOutputType::U8 => Some(val as u8 as u64),
            NodeOutputType::U16 => Some(val as u16 as u64),
            NodeOutputType::U32 => Some(val as u32 as u64),
            NodeOutputType::U64 => Some(val as u64),
        }
    }

    /// Interprets `val` as a signed integer of this width with sign-extension
    /// and returns the result, or `None` if this type is `Bool`.
    ///
    /// Casting through the signed type of the same width sign-extends the
    /// value to 64 bits.
    #[inline]
    pub fn get_signed_int(self, val: u64) -> Option<i64> {
        match self {
            NodeOutputType::Bool => None,
            NodeOutputType::U8 => Some(val as i8 as i64),
            NodeOutputType::U16 => Some(val as i16 as i64),
            NodeOutputType::U32 => Some(val as i32 as i64),
            NodeOutputType::U64 => Some(val as i64),
        }
    }
}

impl From<u32> for NodeOutputType {

    fn from(value: u32) -> Self {
        match value {
            1 => Self::U8,
            2 => Self::U16,
            4 => Self::U16,
            8 => Self::U64,
            _ => unreachable!()
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
    /// Selector token produced by `ControlState` nodes and consumed by
    /// `ControlSelector` phi nodes.
    ControlSelector,
    /// Memory token tracking the current state of memory through the graph.
    Memory
}

impl NodeOutputKind {
    /// Returns `true` if this is a value output (`OutputType` variant).
    #[inline]
    pub fn is_value(self) -> bool {
        matches!(self, Self::OutputType(..))
    }

    /// Returns the inner [`NodeOutputType`] if this is a value output,
    /// otherwise `None`.
    #[inline]
    pub fn as_value(self) -> Option<NodeOutputType> {
        match self {
            Self::OutputType(v) => Some(v),
            _ => None,
        }
    }

    /// Returns `true` if this is a control-flow edge.
    #[inline]
    pub fn is_control(self) -> bool {
        self == Self::Control
    }

    /// Returns `true` if this is a control-selector edge.
    #[inline]
    pub fn is_control_selector(self) -> bool {
        self == Self::ControlSelector
    }

    /// Returns `true` if this is a memory edge.
    #[inline]
    pub fn is_memory(self) -> bool {
        self == Self::Memory
    }

    /// Returns `true` if this is a value output carrying a `Bool` type.
    #[inline]
    pub fn is_bool(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_bool()
        } else  {
            false
        }
    }

    /// Returns `true` if this is a value output carrying an integer type.
    #[inline]
    pub fn is_integer(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_integer()
        } else  {
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
    pub fn new(kind: NodeOutputKind, source_id: NodeId, output_index: u32) -> Self{
        NodeOutput { kind, source_id, output_index, first_use: None.into() }
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
    pub fn new(output_id: NodeOutputId, node_id: NodeId, input_index: u32) -> Self {
        NodeInput { output_id, prev: None.into(), next: None.into(), node_id, input_index}
    }
}


/// The operation or role of a node in the IR graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    // Initial state
    Entry,
    InitialMemory,
    InitialVar(rsleigh::Vn),

    // General state
    ControlState,
    MemSelector,
    ControlSelector(rsleigh::Vn),

    // If
    If,
    IfCase(bool),

    // Call
    Call,
    PostCallMemState,
    PostCallVarState(rsleigh::Vn),
    Return,

    // Memory operations
    Load(rsleigh::VnSpace),
    Store(rsleigh::VnSpace),

    // int operations
    IntConst(u64),
    IntUnaryOp(crate::ops::IntUnaryOp),
    IntBinaryOp(crate::ops::IntBinaryOp),
    IntCmpOp(crate::ops::IntCmpOp),
    CastToInt,
    Truncate,
    Popcount,
    Extend(crate::ops::ExtendOp),

    // Bool operations
    BoolConst(bool),
    BoolUnaryOp(crate::ops::BoolUnaryOp),
    BoolBinaryOp(crate::ops::BoolBinaryOp),
    CastToBool,

    // Float operations
    // FloatConst(f64),
    // FloatUnaryOp(FloatUnaryOpKind),
    // FloatBinaryOp(FloatBinaryOpKind),
    // FloatCmpOp(FloatCmpOpKind),
    // CastToFloat,
}

impl NodeKind {
    /// Returns `true` if this node represents a compile-time constant
    /// (`BoolConst` or `IntConst`).
    #[inline]
    pub fn is_const(self) -> bool {
        matches!(self, Self::BoolConst(..) | Self::IntConst(..))
    }

    /// Returns `true` if nodes of this kind may be deduplicated in the graph
    /// cache.
    ///
    /// Nodes whose inputs are added incrementally after construction (e.g.
    /// `ControlState`, `ControlSelector`) or that must always produce a fresh
    /// node (e.g. `Return`) are not cacheable.
    #[inline]
    pub fn is_cacheable(&self) -> bool {
        // TODO: is it all that should be cached?
        // We can't cache anything that we want to add inputs to it later / its constructed without inputs and are added later
        !matches!(
            self,
                  Self::Entry
                | Self::InitialMemory
                | Self::InitialVar(..)

                | Self::Return

                | Self::ControlState

                | Self::MemSelector
                | Self::ControlSelector(..)
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
    pub fn new(kind: NodeKind) -> Self {
        Self { kind, inputs: NodeInputIdList::new(), outputs: NodeOutputIdList::new() }
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
        assert_eq!(NodeOutputType::U8.get_unsigned_int(wide),  Some(u8::MAX  as u64));
        assert_eq!(NodeOutputType::U16.get_unsigned_int(wide), Some(u16::MAX as u64));
        assert_eq!(NodeOutputType::U32.get_unsigned_int(wide), Some(u32::MAX as u64));
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
        assert_eq!(NodeOutputType::U8.get_signed_int(u8::MAX as u64),     Some(-1));
        // i8::MIN (0x80) sign-extends to -128
        assert_eq!(NodeOutputType::U8.get_signed_int(i8::MIN as u8 as u64), Some(i8::MIN as i64));
        // i8::MAX (0x7F) stays positive
        assert_eq!(NodeOutputType::U8.get_signed_int(i8::MAX as u64),     Some(i8::MAX as i64));
        // i16::MIN (0x8000) sign-extends to -32768
        assert_eq!(NodeOutputType::U16.get_signed_int(i16::MIN as u16 as u64), Some(i16::MIN as i64));
        // u32::MAX as u64 sign-extends as i32 to -1
        assert_eq!(NodeOutputType::U32.get_signed_int(u32::MAX as u64), Some(-1));
    }

    /// `get_signed_int` must return `None` for `Bool`.
    #[test]
    fn signed_int_is_none_for_bool() {
        assert_eq!(NodeOutputType::Bool.get_signed_int(1), None);
    }

    /// `bit_width` must equal `byte_size * 8` for every variant.
    #[test]
    fn bit_width_is_eight_times_byte_size() {
        for ty in [NodeOutputType::Bool, NodeOutputType::U8, NodeOutputType::U16,
                   NodeOutputType::U32, NodeOutputType::U64] {
            assert_eq!(ty.bit_width(), ty.byte_size() * 8,
                "bit_width mismatch for {ty:?}");
        }
    }

    // ── NodeOutputKind ───────────────────────────────────────────────────────

    /// `is_value` must be `true` only for `OutputType` variants.
    #[test]
    fn is_value_only_for_output_type() {
        assert!(NodeOutputKind::OutputType(NodeOutputType::U64).is_value());
        assert!(!NodeOutputKind::Control.is_value());
        assert!(!NodeOutputKind::ControlSelector.is_value());
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
    /// `false` for `Bool`, `Control`, `ControlSelector`, and `Memory`.
    #[test]
    fn is_integer_for_all_integer_output_types() {
        for ty in [NodeOutputType::U8, NodeOutputType::U16,
                   NodeOutputType::U32, NodeOutputType::U64] {
            assert!(NodeOutputKind::OutputType(ty).is_integer(),
                "{ty:?} should be integer");
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
        let non_cacheable = [
            NodeKind::Entry,
            NodeKind::InitialMemory,
            NodeKind::Return,
            NodeKind::ControlState,
            NodeKind::MemSelector,
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
}
