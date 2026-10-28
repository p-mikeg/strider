extern crate alloc;

pub(crate) mod interner;
pub mod set;
pub(crate) mod union_dag;
pub(crate) mod worklist;

pub use interner::EntityInterner;
pub use set::DenseEntitySet;
pub use union_dag::UnionDag;
pub use worklist::Worklist;
