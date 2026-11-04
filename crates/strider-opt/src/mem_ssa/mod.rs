mod layout;

#[cfg(test)]
pub(crate) use layout::CLIMB_STEPS;
pub(crate) use layout::{Answers, ClobberProbe, MemLayout, Shape};

#[cfg(test)]
mod tests;
