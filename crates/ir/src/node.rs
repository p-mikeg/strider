use std::fmt::Debug;

use cranelift_entity::{
    EntityList, entity_impl, packed_option::PackedOption,
};

use crate::builder_ext::{bool::{BoolBinaryOpKind, BoolUnaryOpKind}, int::{ExtendOpKind, IntBinaryOpKind, IntCmpKind, IntUnaryOpKind}};

// This basic structure represents a unique Node struct
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId, "node");


// This basic structure represents a unique NodeOutput struct
#[derive(Default)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeOutputId(u32);
entity_impl!(NodeOutputId, "%");

// This basic structure represents a unique NodeInput struct
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInputId(u32);
entity_impl!(NodeInputId, "input");

// This represents a list of node inputs
pub(crate) type NodeInputIdList = EntityList<NodeInputId>;

// This represents a list of node inputs
pub(crate) type NodeOutputIdList = EntityList<NodeOutputId>;


// This stores the type of output that the node returns
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputType {
    Bool,
    U8,
    U16,
    U32,
    U64
}


impl NodeOutputType {
    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(self, NodeOutputType::U8 | NodeOutputType::U16 | NodeOutputType::U32 | NodeOutputType::U64)
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        matches!(self, NodeOutputType::Bool)
    }

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

    #[inline]
    pub fn bit_width(self) -> usize {
        self.byte_size() * 8
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputKind {
    // In case this is a generic block - just store what is the output type
    OutputType(NodeOutputType),
    /// Indicates a control flow dependency between nodes. Every region takes in a number of control
    /// values indicating the predecessors of the region, while every branch produces a number of
    /// control values that are then consumed by the regions to which they branch.
    Control,
    /// Special value produced only by control instructions to attach their phi nodes.
    ControlSelector,
    Memory
}

impl NodeOutputKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeOutputKind::OutputType(v) => v.as_str(),
            NodeOutputKind::ControlSelector => "control selector",
            NodeOutputKind::Control => "control",
            NodeOutputKind::Memory => "memory",
        }
    }
    #[inline]
    pub fn is_value(self) -> bool {
        matches!(self, Self::OutputType(..))
    }

    #[inline]
    pub fn as_value(self) -> Option<NodeOutputType> {
        match self {
            Self::OutputType(v) => Some(v),
            _ => None,
        }
    }

    #[inline]
    pub fn is_control(self) -> bool {
        self == Self::Control
    }

    #[inline]
    pub fn is_control_selector(self) -> bool {
        self == Self::ControlSelector
    }

    #[inline]
    pub fn is_memory(self) -> bool {
        self == Self::Memory
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_bool()
        } else  {
            false
        }
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        if let Some(output_type) = self.as_value() {
            output_type.is_integer()
        } else  {
            false
        }
    }
}

// This structure stores the output of a given node and tracks all its uses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeOutput {
    // What kind of value is this node 
    pub(crate) kind: NodeOutputKind,
    // What is the node that created this output
    pub(crate) source_id: NodeId,
    // What is the index in the outputs of the source node
    pub(crate) output_index: u32,
    // A linked list all uses of this specific output value (to change if we update this value for some reason)
    pub(crate) first_use: PackedOption<NodeInputId>,
}

impl NodeOutput {
    pub fn new(kind: NodeOutputKind, source_id: NodeId, output_index: u32) -> Self{
        NodeOutput { kind, source_id, output_index, first_use: None.into() }
    }
}

// This structure stores a usage of NodeOutput and what node uses this output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInput {
    // The node output to be used as input
    pub(crate) output_id: NodeOutputId,
    // This stores the previous use of the this node value
    pub(crate) prev: PackedOption<NodeInputId>,
    // This stores the next use of the this node value
    pub(crate) next: PackedOption<NodeInputId>,
    // The node that uses the input
    pub(crate) node_id: NodeId,
    // What is the index in the inputs of the node
    pub(crate) input_index: u32,
}

impl NodeInput {
    pub fn new(output_id: NodeOutputId, node_id: NodeId, input_index: u32) -> Self {
        NodeInput { output_id, prev: None.into(), next: None.into(), node_id, input_index}
    }
}

pub type Var = rsleigh::Vn;

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
    IntUnaryOp(IntUnaryOpKind),
    IntBinaryOp(IntBinaryOpKind),
    IntCmpOp(IntCmpKind),
    CastToInt,
    Truncate,
    Popcount,
    Extend(ExtendOpKind),

    // Bool operations
    BoolConst(bool),
    BoolUnaryOp(BoolUnaryOpKind),
    BoolBinaryOp(BoolBinaryOpKind),
    CastToBool,

    // Float operations
    // FloatConst(f64),
    // FloatUnaryOp(FloatUnaryOpKind),
    // FloatBinaryOp(FloatBinaryOpKind),
    // FloatCmpOp(FloatCmpOpKind),
    // CastToFloat,
}

fn pretty_print_vnspace(space: &rsleigh::VnSpace) -> &'static str {
    match *space {
        rsleigh::VnSpace::RAM => "ram",
        rsleigh::VnSpace::CONST => "const",
        rsleigh::VnSpace::REGISTER => "register",
        rsleigh::VnSpace::UNIQUE => "unique",
        _ => unreachable!()
    }
}

impl NodeKind {
    #[inline]
    pub fn is_const(self) -> bool {
        matches!(self, Self::BoolConst(..) | Self::IntConst(..))
    }

    pub fn as_str(&self) -> String {
        match self {
            NodeKind::CastToBool | NodeKind::CastToInt => "Cast".to_owned(),
            NodeKind::Truncate => "Truncate".to_owned(),
            NodeKind::Extend(op) => format!("{:?}", op),
            NodeKind::BoolConst(v) => format!("const {v}"),
            NodeKind::IntConst(v) => format!("const {v:#x}"),
            NodeKind::BoolBinaryOp(op) => format!("{:?}", op),
            NodeKind::IntBinaryOp(op) => format!("{:?}", op),
            NodeKind::BoolUnaryOp(op) => format!("{:?}", op),
            NodeKind::IntUnaryOp(op) => format!("{:?}", op),
            NodeKind::IntCmpOp(op) => format!("{:?}", op),
            NodeKind::Load(op) => format!("Load {}", pretty_print_vnspace(&op)), 
            NodeKind::Store(op) => format!("Store {}", pretty_print_vnspace(&op)), 
            _ => format!("{:?}", self)
        }
    }

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
                | Self::PostCallMemState

                | Self::MemSelector
                | Self::ControlSelector(..)
        )
    }

}

// This represents a general node in the graph
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) inputs: NodeInputIdList,
    pub(crate) outputs: NodeOutputIdList,
}

impl Node {
    pub fn new(kind: NodeKind) -> Self {
        Self { kind, inputs: NodeInputIdList::new(), outputs: NodeOutputIdList::new() }
    }
}