//! `Store` → `StackStore` rewrite (`detect`) and post-pass stack-arg
//! collection (`call_args`). The shared SP-decomposition machinery lives
//! in [`crate::opt::sp_expr`].

mod call_args;
mod detect;
#[cfg(test)]
mod tests;

pub use call_args::CallStackArgCollect;
pub use detect::StackStoreDetect;
