//! Acyclic value-slice egraph adapter (egg with phi-as-opaque-leaves).
//!
//! Phase 1 Task 1.5 spike — V1 verification execution. Proves Graph
//! round-trips through `egg::EGraph` without information loss in the
//! zero-rewrite case. Production integration is Phase 3's task.
//!
//! # Model
//!
//! The adapter slices each strider [`crate::Graph`] into:
//!
//! - **Opaque leaves** — `VarPhi`, `MemPhi`, `InitialVar`, `InitialMemory`,
//!   `FunctionArg`, `Load` value outputs, and `Call`/`CallOther` value
//!   outputs. Each carries a stable u64 identity derived from the
//!   originating `NodeId` so distinct strider nodes never collide in the
//!   egraph (the plan's "no accidental unification across phi sites"
//!   invariant).
//!
//! - **Internal e-nodes** — `IntConst`, `BoolConst`, `FloatConst`, and the
//!   value-producing arithmetic / comparison / boolean / cast operations.
//!   egg saturates over these.
//!
//! - **Out of egraph (preserved structurally)** — `Control`, `Memory`,
//!   `PhiToken` edges; multi-output node bookkeeping; everything the
//!   `extract_into_graph` path threads through using the original
//!   [`crate::Graph`] as a side reference.

pub mod extract;
pub mod from_graph;
pub mod language;

pub use from_graph::EGraphAdapter;
pub use language::StriderLang;
