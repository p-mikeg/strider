use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A capture variable that binds a [`ir::node::NodeOutputId`] (data value edge).
///
/// Each `Var::new()` call produces a globally unique id.  The same `Var` can
/// appear in multiple positions of a pattern; the matcher requires all
/// occurrences to bind to the **same** output.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Var(u32);

impl Var {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for Var {
    fn default() -> Self {
        Self::new()
    }
}

/// A capture variable that binds a [`ir::node::NodeId`] (control-level node).
///
/// Shared counter with [`Var`] so IDs are globally unique across both types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeVar(u32);

impl NodeVar {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for NodeVar {
    fn default() -> Self {
        Self::new()
    }
}

/// A capture variable that binds the **integer constant value** (`u64`) carried
/// by an `IntConst` node.
///
/// Shares the global ID counter with [`Var`] and [`NodeVar`] so every capture
/// id across the process is globally unique.  Future pattern variants (e.g.
/// `AnyIntConst(IntVar)`) will populate this binding automatically; for now the
/// storage and accessors are the foundation for Phase A2.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IntVar(u32);

impl IntVar {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for IntVar {
    fn default() -> Self {
        Self::new()
    }
}

/// A capture variable that binds the **boolean constant value** (`bool`) carried
/// by a `BoolConst` node.
///
/// Shares the global ID counter with [`Var`] and [`NodeVar`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BoolVar(u32);

impl BoolVar {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for BoolVar {
    fn default() -> Self {
        Self::new()
    }
}

/// A capture variable that binds the **IEEE 754 bit pattern** (`u64`) carried
/// by a `FloatConst` node.
///
/// Shares the global ID counter with [`Var`] and [`NodeVar`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FloatVar(u32);

impl FloatVar {
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for FloatVar {
    fn default() -> Self {
        Self::new()
    }
}
