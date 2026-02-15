pub mod builder;
pub mod bool;
pub mod int;
pub mod memory;
pub mod control;

pub use builder::{BuilderExt, GraphBuilder, FunctionBody};
pub use bool::BoolBuilderExt;
pub use int::IntBuilderExt;
pub use memory::MemoryBuilderExt;
pub use control::ControlBuilderExt;

impl BoolBuilderExt for GraphBuilder<'_> {}
impl IntBuilderExt for GraphBuilder<'_> {}
impl MemoryBuilderExt for GraphBuilder<'_> {}
impl ControlBuilderExt for GraphBuilder<'_> {}
