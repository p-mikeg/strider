//! Error types for the lift pipeline.
//!
//! All lift-stage failures propagate as [`anyhow::Error`] with an
//! informative message.  No typed marker structs — callers treat errors
//! as opaque.
