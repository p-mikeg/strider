//! Alias-class classification for memory partitions.
//!
//! The closed [`AliasClass`] enum is defined in `strider-target`
//! (consumed by `CallOtherAbi::mem_clobbers`) and re-exported here so
//! `strider-ir` callers (and downstream pattern code) can keep referring
//! to `strider_ir::AliasClass`.
//!
//! `AliasClass` is stored on [`crate::node::NodeOutputKind::Memory`] and on
//! [`crate::node::NodeOutputKind::Memory`] so pass code can branch on the
//! class by querying the output slot's [`crate::node::NodeOutputKind`].

pub use strider_target::AliasClass;
