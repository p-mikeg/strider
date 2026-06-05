//! The function data structures: the [`Function`] graph-plus-overlay
//! ([`data`]), the self-cleaning editing context [`EditFunction`] ([`edit`])
//! and its [`FunctionState`] bookkeeping ([`state`]), and the IR-specific dot
//! rendering ([`dot`]).

mod data;
pub(crate) mod dot;
mod edit;
mod state;

pub use data::Function;
pub use edit::EditFunction;
pub use state::FunctionState;
