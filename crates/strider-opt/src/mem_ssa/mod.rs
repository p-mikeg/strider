//! Nearest-clobber queries over the memory graph.
//!
//! [`layout`] answers one by climbing the memory dominator tree over an index
//! built once per graph state.

mod layout;

#[cfg(test)]
pub(crate) use layout::CLIMB_STEPS;
pub(crate) use layout::{Answers, ClobberProbe, MemLayout, Shape};

#[cfg(test)]
mod tests;
