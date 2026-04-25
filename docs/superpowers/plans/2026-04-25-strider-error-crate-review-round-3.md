# strider-error Crate Review — Round 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Third-pass review of `strider-error` (rounds 1 & 2 are committed as of 2026-04-25). Round 3 targets a latent correctness bug in `format_traceback` that rounds 1/2 left behind, and an ergonomic fix to the `define_error!` macro: the enum moves *outside* the macro so the macro only emits wrapper infrastructure. Scope is deliberately narrow.

**Architecture:** After round 2 the crate is four files, ~330 lines total:
- [lib.rs](crates/strider-error/src/lib.rs) — module declarations + re-exports.
- [fields.rs](crates/strider-error/src/fields.rs) — `ErrorFields`, `LocationChain`, `fmt_chain_and_backtrace`.
- [define.rs](crates/strider-error/src/define.rs) — `define_error!` + `bridge_error!` macros.
- [format.rs](crates/strider-error/src/format.rs) — `format_traceback`.

Consumers: 7 `define_error!`-backed wrappers ([reader](crates/reader/src/error.rs), [cfg](crates/cfg/src/error.rs), [analyzer](crates/analyzer/src/error.rs), [target](crates/target/src/error.rs), [ir](crates/ir/src/error.rs), [opt](crates/opt/src/error.rs), [pattern](crates/pattern/src/error.rs)) + 1 hand-rolled generic wrapper [dot::Error<E>](crates/dot/src/error.rs). `format_traceback` has zero non-test callers workspace-wide — the only consumer is the planned `strider-py` PyO3 layer.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. Relies on trait upcasting (stable since Rust 1.86).

---

## Baseline (verified 2026-04-25)

- `cargo test -p strider-error` → 14 tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 2 in `tests/format.rs` + 3 doctests).
- `cargo test -p dot --test error` → 6 tests pass.
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- `grep -rn format_traceback crates/ | grep -v strider-error` → empty (no workspace callers outside the crate itself).
- `grep -rn '#\[error(".*\\\\n'` workspace-wide → empty (no multi-line Display strings exist today — the C1 bug below is latent).

---

## Review Findings — Executive Summary

**One latent correctness bug + one DSL ergonomic fix; small test/readability additions.** Rounds 1 and 2 addressed every other concern in the crate.

### Correctness (C)

- **C1 — `format_traceback`'s strip heuristic breaks on multi-line `Display`.** [format.rs:46-51](crates/strider-error/src/format.rs#L46-L51) strips the first line of `{err:?}` on the assumption that it equals the wrapper's Display line. That assumption holds **only** when Display is a single line. If any `#[error("...")]` attribute contains `\n` (today none do — verified by grep — but nothing prevents it), the current code would print the full multi-line Display after `"error: "`, then print `{err:?}` with only the first line stripped, leaving lines 2..N of Display in the tail. Result: lines 2..N appear twice. Same category of bug as round-1's F1, hidden behind a condition that's false today.

  The fix (deferred in round 1 as Q1 option B) is to introduce a `Traceback` trait:

  ```rust
  pub trait Traceback: std::error::Error {
      fn location_chain(&self) -> &LocationChain;
      fn origin_backtrace(&self) -> &std::backtrace::Backtrace;
  }
  ```

  …implemented inside `define_error!` and manually in `dot::Error<E>`. `format_traceback` then takes `&dyn Traceback` and builds output from known pieces — no `{err:?}` intermediate string, no strip-and-pray, no accidental Display-column duplication.

### Simplification (S)

- **S1 — `format_traceback` allocates `format!("{err:?}")` only to discard its first line.** Resolved as a side effect of C1 — once we know the pieces, we format them directly.

### Readability (R)

- **R1 — `format.rs` doctest uses a bare `Oops` struct, not a wrapper.** [format.rs:14-29](crates/strider-error/src/format.rs#L14-L29) demonstrates the function compiles, not the intended use case. A realistic example using `define_error!` communicates intent better and becomes load-bearing after C1 (`Oops: Error` but `Oops: !Traceback`).

- **R2 — `fields.rs` lacks a module-level doc block.** Every other file in the crate has one. Two lines is enough.

- **R3 — `define_error!` slurps the enum body via `$($body:tt)*`.** [define.rs:52-60](crates/strider-error/src/define.rs#L52-L60) The macro's grammar takes both a struct declaration AND the entire enum declaration, then re-emits the enum unchanged plus the wrapper impls. This causes the kind name to be repeated in two metavars (`$kind` in `wraps $kind;` and `$kind_enum` in the enum line), which can be typoed independently. Fix: **take the enum out of the macro entirely.** Users write vanilla `pub enum ErrorKind { ... }` (so rustfmt, rust-analyzer, and derive macros all see plain Rust) and invoke `define_error!` solely for the wrapper infrastructure. The macro's grammar shrinks to just the struct + optional sources. No enum body slurping.

  Before:
  ```rust
  strider_error::define_error! {
      pub struct Error wraps ErrorKind;
      sources: [std::io::Error];

      #[derive(Debug, thiserror::Error)]
      pub enum ErrorKind {
          #[error("boom")] Boom,
          #[error(transparent)] Io(#[from] std::io::Error),
      }
  }
  ```

  After:
  ```rust
  #[derive(Debug, thiserror::Error)]
  pub enum ErrorKind {
      #[error("boom")] Boom,
      #[error(transparent)] Io(#[from] std::io::Error),
  }

  strider_error::define_error! {
      pub struct Error wraps ErrorKind;
      sources: [std::io::Error];
  }
  ```

  Breaking change to 7 `define_error!` call sites + 3 macro invocations inside tests + 2 doctest invocations. Mechanical.

### Test coverage (T)

- **T1 — No regression test for multi-line Display in `format_traceback`.** If we land C1 we must pin that behavior going forward. Single new test in `tests/format.rs`.

---

## Task Order Rationale

Task 1 is the macro dedup (R3). Task 2 is the Traceback trait (C1/R1/R2/S1/T1). Doing the dedup first means Task 2's new tests/doctests are written in the final syntax without mid-refactor rewrites. Both tasks are low-risk and land under separate commits.

---

## File Structure (after execution)

```
crates/strider-error/
├── src/
│   ├── lib.rs            # + re-export Traceback (Task 2)
│   ├── format.rs         # rewritten against &dyn Traceback (Task 2)
│   ├── fields.rs         # + module doc; + Traceback trait (Task 2)
│   └── define.rs         # macro no longer slurps enum (Task 1); + impl Traceback (Task 2)
└── tests/
    ├── fields.rs         # (unchanged)
    ├── macro_contract.rs # 2 invocations migrated to new syntax (Task 1)
    └── format.rs         # rewritten; + multiline_display regression test (Task 2)

crates/{reader,cfg,analyzer,target,ir,opt,pattern}/src/error.rs
                         # enum pulled outside the define_error! block (Task 1)

crates/dot/src/error.rs   # + manual impl Traceback for Error<E> (Task 2)
```

---

## Task 1: Move the enum out of `define_error!` (R3)

**Atomic change:** the macro grammar and every call site move in lockstep. This task produces a single commit.

**Files (all in one commit):**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs) — macro grammar + 2 doctests
- Modify: [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs) — 2 invocations
- Modify: [crates/strider-error/tests/format.rs](crates/strider-error/tests/format.rs) — 1 invocation (keep as-is shape; Task 2 rewrites it fully, but we keep it compiling at the end of Task 1)
- Modify: [crates/reader/src/error.rs](crates/reader/src/error.rs)
- Modify: [crates/cfg/src/error.rs](crates/cfg/src/error.rs)
- Modify: [crates/analyzer/src/error.rs](crates/analyzer/src/error.rs)
- Modify: [crates/target/src/error.rs](crates/target/src/error.rs)
- Modify: [crates/ir/src/error.rs](crates/ir/src/error.rs)
- Modify: [crates/opt/src/error.rs](crates/opt/src/error.rs)
- Modify: [crates/pattern/src/error.rs](crates/pattern/src/error.rs)

- [ ] **Step 1: Rewrite the `define_error!` grammar**

Replace the entire `#[macro_export] macro_rules! define_error { ... }` block in `crates/strider-error/src/define.rs` (currently [define.rs:50-140](crates/strider-error/src/define.rs#L50-L140)). The new block, including updated doc comment:

```rust
/// Generates a wrapper struct over an existing `thiserror`-derived enum.
///
/// The enum is a separate, vanilla Rust declaration — the macro does
/// **not** take the enum body as input. This keeps the enum's attributes
/// (`#[derive(Debug, thiserror::Error)]`, variant-level `#[error(...)]`
/// and `#[from]`) in plain Rust, so rustfmt, rust-analyzer, and other
/// tooling see an ordinary enum.
///
/// The macro emits:
///   * a wrapper struct `$wrapper { kind: Box<$kind>, fields: ErrorFields }`;
///   * `impl Display` (delegates to the inner enum);
///   * `impl Debug` (prints kind + location chain + backtrace);
///   * `impl std::error::Error` (delegates `source()` to the enum so the
///     error-chain traversal works transparently);
///   * `impl From<$kind> for $wrapper` — captures a fresh backtrace and
///     seeds the location chain. This is the "origin" boundary.
///   * `impl From<$src> for $wrapper` for every `$src` listed in the
///     optional `sources: [...]` block. Each bridges through the
///     enum's `#[from]`-generated `From<$src> for $kind`.
///
/// All `From` impls are `#[track_caller]` so `Location::caller()` inside
/// [`ErrorFields::new`](crate::ErrorFields::new) resolves to the `?` site
/// in the caller.
///
/// # Example
///
/// ```
/// #[derive(Debug, thiserror::Error)]
/// pub enum ErrorKind {
///     #[error("address {0:#x} is not mapped")]
///     NotMapped(u64),
///     #[error("io: {0}")]
///     Io(#[from] std::io::Error),
/// }
///
/// strider_error::define_error! {
///     pub struct Error wraps ErrorKind;
///     sources: [std::io::Error];
/// }
///
/// let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
/// assert_eq!(err.to_string(), "address 0xdeadbeef is not mapped");
/// assert_eq!(err.locations().len(), 1);
/// ```
///
/// # Cross-crate bridges
///
/// Bridges that unwrap another crate's wrapper (e.g. `From<ir::Error> for
/// opt::Error`) use [`bridge_error!`](crate::bridge_error) so they can call
/// [`ErrorFields::push_caller`](crate::ErrorFields::push_caller) on the inner
/// fields instead of regenerating them. Do not list another crate's wrapper
/// in `sources: [...]`.
#[macro_export]
macro_rules! define_error {
    (
        $(#[$wrapper_attr:meta])*
        pub struct $wrapper:ident wraps $kind:ident;
        $( sources: [ $($src:ty),* $(,)? ]; )?
    ) => {
        $(#[$wrapper_attr])*
        pub struct $wrapper {
            kind: ::std::boxed::Box<$kind>,
            fields: $crate::ErrorFields,
        }

        impl $wrapper {
            /// Returns a reference to the underlying `ErrorKind`.
            pub fn kind(&self) -> &$kind { &self.kind }

            /// Consumes the wrapper and returns the owned `ErrorKind`.
            pub fn into_kind(self) -> $kind { *self.kind }

            /// Splits the wrapper into its boxed kind and shared fields.
            /// Used by downstream wrappers to extend the location chain
            /// across crate boundaries without losing the origin backtrace.
            pub fn decompose(self) -> (::std::boxed::Box<$kind>, $crate::ErrorFields) {
                (self.kind, self.fields)
            }

            /// Returns the per-`?` propagation chain (origin first).
            pub fn locations(&self) -> &$crate::LocationChain {
                &self.fields.locations
            }

            /// Returns the backtrace captured at the origin of this error.
            pub fn backtrace(&self) -> &::std::backtrace::Backtrace {
                &self.fields.backtrace
            }
        }

        impl ::std::fmt::Display for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&*self.kind, f)
            }
        }

        impl ::std::fmt::Debug for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                writeln!(f, "{}", self.kind)?;
                self.fields.fmt_chain_and_backtrace(f)
            }
        }

        impl ::std::error::Error for $wrapper {
            fn source(&self) -> ::std::option::Option<&(dyn ::std::error::Error + 'static)> {
                ::std::error::Error::source(&*self.kind)
            }
        }

        impl ::std::convert::From<$kind> for $wrapper {
            #[track_caller]
            fn from(kind: $kind) -> Self {
                Self {
                    kind: ::std::boxed::Box::new(kind),
                    fields: $crate::ErrorFields::new(),
                }
            }
        }

        $(
            $(
                impl ::std::convert::From<$src> for $wrapper {
                    #[track_caller]
                    fn from(e: $src) -> Self {
                        <$wrapper as ::std::convert::From<$kind>>::from(
                            <$kind as ::std::convert::From<$src>>::from(e),
                        )
                    }
                }
            )*
        )?
    };
}
```

Key differences from the current macro:
- Grammar no longer takes `$(#[$enum_attr:meta])* pub enum $kind_enum:ident { $($body:tt)* }`.
- Single `$kind` metavar (from `wraps $kind;`) used throughout the expansion.
- Expansion no longer emits `pub enum $kind_enum { ... }` — the user writes it themselves.

- [ ] **Step 2: Update the `bridge_error!` doctest in `define.rs`**

The `bridge_error!` doctest (currently [define.rs:152-175](crates/strider-error/src/define.rs#L152-L175)) uses the old `define_error!` syntax. Replace the `# Example` section of the `bridge_error!` macro's doc comment with:

```rust
/// # Example
///
/// ```
/// #[derive(Debug, thiserror::Error)]
/// pub enum InnerKind { #[error("boom")] Boom }
///
/// strider_error::define_error! {
///     pub struct InnerError wraps InnerKind;
/// }
///
/// #[derive(Debug, thiserror::Error)]
/// pub enum OuterKind {
///     #[error(transparent)]
///     Inner(InnerKind),
/// }
///
/// strider_error::define_error! {
///     pub struct OuterError wraps OuterKind;
/// }
///
/// strider_error::bridge_error!(InnerError => OuterError, OuterKind::Inner);
///
/// fn inner() -> Result<(), InnerError> { Err(InnerKind::Boom.into()) }
/// fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }
///
/// let err = outer().unwrap_err();
/// assert_eq!(err.locations().len(), 2, "origin + bridge push_caller");
/// ```
```

Leave the `bridge_error!` macro body itself and the "Expands to" section unchanged.

- [ ] **Step 3: Compile the crate on its own**

Run: `cargo check -p strider-error`
Expected: PASS. The doctests are compiled later during `cargo test --doc`; right now we only need the crate itself to build.

- [ ] **Step 4: Migrate `crates/reader/src/error.rs`**

Replace the entire file with:

```rust
/// Errors that can be produced by the reader crate.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// The requested address is not mapped in any loaded region.
    #[error("address {0:#x} is not mapped")]
    NotMapped(u64),

    /// A `MemRegion` was constructed with a (start_addr, len) pair
    /// whose end would exceed `u64::MAX`.
    #[error("region at {start_addr:#x} with length {len} would overflow u64")]
    RegionOverflow { start_addr: u64, len: u64 },

    /// An I/O error occurred while reading a file.
    #[error("failed to read file: {0}")]
    Io(#[from] std::io::Error),

    /// An `object` crate error occurred while parsing or loading an ELF.
    #[error("failed to parse ELF: {0}")]
    Object(#[from] object::Error),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [std::io::Error, object::Error];
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
```

Run: `cargo check -p reader`
Expected: PASS.

- [ ] **Step 5: Migrate `crates/cfg/src/error.rs`**

Replace the entire file with:

```rust
use crate::cfg::{PcodeInsnAddr, Region};
use petgraph::graph::NodeIndex;

#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error("empty region {0:?}")]
    EmptyRegion(Region),

    #[error("unknown register name by sleign {0:?}")]
    UnknownRegName(String),

    #[error("invalid branch target variable {0:?} at opcode {1:?}")]
    InvalidBranchTargetVaErr(rsleigh::Vn, PcodeInsnAddr),

    #[error("invalid tail call at opcode {0:?}")]
    InvalidTailCall(PcodeInsnAddr),

    #[error("cfg failed accessing starting region")]
    FailedCreatingStartRegion,

    #[error("failed spliting region {0:?} into 2 parts at {1:?}")]
    FailedSplitingRegion(NodeIndex, PcodeInsnAddr),

    #[error("builder about to build an empty instruction region")]
    NoInstructionsRegionBuilder,

    #[error("invalid register vn")]
    InvalidRegVn(rsleigh::Vn),

    #[error(transparent)]
    FormatError(#[from] core::fmt::Error),

    #[error("invalid region index {0:?}")]
    InvalidRegion(NodeIndex),

    #[error("region {0:?} has more than one outgoing edge of kind {1:?}")]
    DuplicateEdgeKind(NodeIndex, crate::cfg::RegionEdgeKind),

    #[error("non-entry work-queue item has no parent edge")]
    MissingParentEdge,

    #[error("unsupported varnode space for display: {0:?}")]
    UnsupportedVnSpaceDisplay(rsleigh::VnSpace),

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError, core::fmt::Error];
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
```

Run: `cargo check -p cfg`
Expected: PASS.

- [ ] **Step 6: Migrate `crates/target/src/error.rs`**

Replace the entire file with:

```rust
/// Errors produced while resolving a target description (architecture or
/// calling convention) against a Sleigh register table.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// A register name listed in the target description does not resolve
    /// to a known Sleigh register for the active architecture.
    #[error("unknown register name by sleigh {0:?}")]
    UnknownRegName(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
```

Run: `cargo check -p target`
Expected: PASS.

- [ ] **Step 7: Migrate `crates/ir/src/error.rs`**

Replace the entire file with:

```rust
use crate::node::{NodeId, NodeInputId, NodeOutputId, NodeOutputKind, NodeOutputType};

/// Errors that can be produced by the IR builder and graph operations.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// A node was constructed with the wrong number of parameter outputs.
    #[error("expected {1:?} params and got {0:?}")]
    InvalidNumberOfParams(Vec<NodeOutputId>, u64),

    /// An output was expected to carry a concrete value type but doesn't.
    #[error("output id {0:?} should be a value kind but got kind {1:?}")]
    InvalidOutputType(NodeOutputId, NodeOutputKind),

    /// A builder operation was attempted with no active region.
    #[error("no current region is set")]
    NoCurrentRegion,

    /// A builder operation was attempted on a region that has already been terminated.
    #[error("attempted to insert into terminated region {0}")]
    RegionTerminated(u32),

    /// An output was expected to be a `Control` edge.
    #[error("output {0:?} is not a control edge (got {1:?})")]
    ExpectedControl(NodeOutputId, NodeOutputKind),

    /// An output was expected to be a `Memory` edge.
    #[error("output {0:?} is not a memory edge (got {1:?})")]
    ExpectedMemory(NodeOutputId, NodeOutputKind),

    /// An output was expected to carry a concrete value.
    #[error("output {0:?} is not a value edge (got {1:?})")]
    ExpectedValue(NodeOutputId, NodeOutputKind),

    /// An output was expected to carry a concrete value type but is a
    /// control/memory/control-phi edge instead. Unlike [`Self::ExpectedValue`],
    /// this variant carries only the mismatched kind (no output id), used
    /// by [`crate::node::NodeOutputKind::as_value_or_err`].
    #[error("expected value output, got {0:?}")]
    ExpectedValueOutput(NodeOutputKind),

    /// An output was expected to carry a `Bool` value.
    #[error("output {0:?} is not a bool value")]
    ExpectedBool(NodeOutputId),

    /// An output was expected to carry an integer value.
    #[error("output {0:?} is not an integer value")]
    ExpectedInteger(NodeOutputId),

    /// An output was expected to carry a float value (F32 or F64).
    #[error("output {0:?} is not a float value")]
    ExpectedFloat(NodeOutputId),

    /// A type was expected to be a float type (F32 or F64).
    #[error("type {0:?} is not a float type")]
    ExpectedFloatType(NodeOutputType),

    /// A type was expected to be an integer type (U8/U16/U32/U64).
    #[error("type {0:?} is not an integer type")]
    ExpectedIntegerType(NodeOutputType),

    /// An output was expected to be a `ControlPhi` dispatch edge.
    #[error("output {0:?} is not a control-phi edge")]
    ExpectedControlPhi(NodeOutputId),

    /// An input index was out of range for the node's input list.
    #[error("input index {0} out of bounds (len {1})")]
    InputIndexOutOfBounds(usize, usize),

    /// A cursor operation was attempted on a null (empty) use.
    #[error("attempted to replace a null cursor use")]
    NullCursorUse,

    /// `add_node_input` was called on a cacheable (deduplicated) node.
    #[error("attempted to add input to cacheable node {0:?}")]
    AddInputToCacheableNode(NodeId),

    /// A varnode was referenced that is not tracked by the builder.
    #[error("variable {0:?} not found in builder")]
    VariableNotFound(rsleigh::Vn),

    /// A varnode had a byte size with no corresponding [`NodeOutputType`].
    #[error("unsupported node output size: {0} bytes")]
    UnsupportedOutputSize(u32),

    /// An input slot was already part of a use-list when it should be fresh.
    #[error("input {0:?} is already linked")]
    InputAlreadyLinked(NodeInputId),

    /// A node was queried for exactly `N` outputs but had a different count.
    #[error("node {0:?} does not have exactly {1} outputs (has {2})")]
    WrongOutputCount(NodeId, usize, usize),

    /// A node was queried for exactly `N` inputs but had a different count.
    #[error("node {0:?} does not have exactly {1} inputs (has {2})")]
    WrongInputCount(NodeId, usize, usize),

    /// Whole-graph validation detected one or more structural violations.
    #[error("ir validation failed:\n{0}")]
    ValidationFailed(crate::validate::ValidationErrors),

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}

/// Hand-rolled bridge so call sites can write
/// `validate::validate(...)?` and have the `ValidationErrors` bundle turn
/// into a fully-constructed [`Error`] (backtrace + seeded location chain).
/// `ValidationErrors` itself carries no origin info — all entries originate
/// in one validation pass, so capturing a single backtrace here is correct.
impl From<crate::validate::ValidationErrors> for Error {
    #[track_caller]
    fn from(e: crate::validate::ValidationErrors) -> Self {
        ErrorKind::ValidationFailed(e).into()
    }
}

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
```

Run: `cargo check -p ir`
Expected: PASS.

- [ ] **Step 8: Migrate `crates/opt/src/error.rs`**

Replace the entire file with:

```rust
/// Errors produced by optimization passes.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// Propagated from the underlying IR layer.
    #[error(transparent)]
    IrError(ir::ErrorKind),
    /// Propagated from the `pattern` crate — raised by rewrite-rule
    /// closures (`pattern::rewrite_rule`, `pattern::apply_rules_in_order`).
    #[error(transparent)]
    PatternError(pattern::ErrorKind),
    /// An output was expected to carry a concrete value but doesn't.
    #[error("expected value output, got {0:?}")]
    ExpectedValueOutput(ir::node::NodeOutputKind),
    /// An output was expected to carry an integer type but carries another.
    #[error("expected integer type, got {0:?}")]
    ExpectedIntegerType(ir::node::NodeOutputType),
    /// The function has no `Return` node (malformed IR).
    #[error("no Return node found in function")]
    NoReturnNode,
    /// Dead-branch elimination could not find the unique live control input to
    /// a `ControlState` node.
    #[error("unique control edge not found in control-state inputs")]
    UniqueCtrlNotFound,
    /// An expected node of a specific kind was not found at a given site.
    /// Carries a human-readable site label and the actual node kind present.
    #[error("expected {0} node, got {1:?}")]
    ExpectedNodeNotFound(&'static str, ir::node::NodeKind),
    /// A post-match capture extraction returned `None`. Indicates a bug in the
    /// pattern-rewrite pipeline: the match succeeded but a named capture
    /// couldn't be resolved (should be impossible if the pattern and
    /// extraction stay in sync).
    #[error("internal: pattern capture `{0}` not bound in successful match")]
    InternalCaptureMissing(&'static str),
    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}

// Preserves origin backtrace + location chain captured by `ir`.
strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);

/// `ir::ValidationErrors` is produced fresh at the validator call site, so
/// route it through `ir::Error` (which captures a fresh backtrace) and then
/// through the bridge above.
impl From<ir::ValidationErrors> for Error {
    #[track_caller]
    fn from(errs: ir::ValidationErrors) -> Self {
        Error::from(ir::Error::from(errs))
    }
}

// Preserves origin backtrace + location chain captured by `pattern`.
strider_error::bridge_error!(pattern::Error => Error, ErrorKind::PatternError);

/// Convenience `Result` alias that uses [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;
```

Run: `cargo check -p opt`
Expected: PASS.

- [ ] **Step 9: Migrate `crates/pattern/src/error.rs`**

In this file, move the enum declaration out of the `define_error!` block. The rest of the file (the `impl Error { skip(), ... }` block, the bridge_error invocation, the `Result` alias) stays unchanged.

Replace lines 1-51 with:

```rust
/// Errors that can be produced by the pattern crate.
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),

    /// Propagated from the underlying IR layer — raised by `make_value_node`,
    /// `replace_all_uses`, and similar graph-mutation helpers used by the
    /// rewrite engine.
    #[error(transparent)]
    IrError(ir::ErrorKind),

    /// A user-supplied closure inside a [`crate::build::Build`] tree (e.g.
    /// the body passed to `int_const_fn`, `bool_const_fn`, or
    /// `float_const_fn`) returned an error.  Carries the original error as
    /// a boxed trait object so rule authors can surface arbitrary error
    /// types without having to shoehorn them into a dedicated variant.
    #[error("rewrite-rule closure failed: {0}")]
    RewriteClosure(Box<dyn std::error::Error + Send + Sync>),

    /// A capture variable referenced by a [`crate::build::FromCtx`] impl
    /// was not bound during the LHS match.  Indicates a pattern-authoring
    /// bug — every capture variable used in the RHS macro must appear in
    /// the LHS pattern and have a corresponding binding emitted by the
    /// matcher.  The payload names the capture **kind** (e.g. `"IntVar"`,
    /// `"IntBinaryOpVar"`) so the site of the bug is obvious from the
    /// error message.
    #[error("missing binding for capture of kind {0}")]
    MissingBinding(&'static str),

    /// Signal from a rewrite-rule RHS closure that the rule doesn't apply
    /// after all.  Used by partial-oracle helpers (e.g. `eval_int_binary`
    /// on divide-by-zero) that need to opt out without surfacing a hard
    /// error.  The [`crate::rewrite_rule`] interpreter converts this
    /// error back to "no change"; every other error variant propagates
    /// as a real failure.
    #[error("rewrite rule opted to skip")]
    RewriteSkip,

    /// A pattern was used on the RHS of a `rewrite_rule` but does not
    /// support construction (wildcards, guards, and control patterns
    /// like `call` / `ret` / `if_node` have no build semantics today).
    #[error("pattern {0} is not buildable (match-only)")]
    NotBuildable(&'static str),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
}
```

Leave lines 53-124 untouched (the `impl Error { ... }` block, the `bridge_error!` line, and the `Result` alias).

Run: `cargo check -p pattern`
Expected: PASS.

- [ ] **Step 10: Migrate `crates/analyzer/src/error.rs`**

Replace lines 1-66 with:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error(transparent)]
    TargetError(target::ErrorKind),

    #[error("no region {0:?} in cfg")]
    CfgNoRegion(cfg::RegionId),

    #[error(transparent)]
    CfgError(cfg::ErrorKind),

    #[error(transparent)]
    IrError(ir::ErrorKind),

    #[error(transparent)]
    OptError(opt::ErrorKind),

    #[error("register {0:?} has no enclosing container in variable set")]
    NoRegisterContainer(rsleigh::Vn),

    #[error("instruction has no output varnode for opcode {0:?}")]
    MissingOutputVn(rsleigh::Opcode),

    #[error("IR region not found for CFG region {0:?}")]
    IrRegionNotFound(cfg::RegionId),

    #[error("attempted to write to CONST space: {0:?}")]
    WriteToConstSpace(rsleigh::VnSpace),

    #[error("unsupported varnode space {0:?}")]
    UnsupportedVnSpace(rsleigh::VnSpace),

    #[error("unsupported register size {0} bytes")]
    UnsupportedRegSize(u32),

    #[error("unimplemented p-code opcode {0:?}")]
    UnimplementedOpcode(rsleigh::Opcode),

    #[error("unsupported float varnode size {0} bytes (expected 4 or 8)")]
    UnsupportedFloatSize(u32),

    #[error("opcode {0:?} expects a CONST input at position {1}")]
    ExpectedConstInput(rsleigh::Opcode, usize),

    #[error("opcode {0:?} is decompiler-internal and should not appear in raw p-code")]
    UnexpectedDecompilerOpcode(rsleigh::Opcode),

    #[error("opcode {0:?} has too few inputs: expected at least {1}, got {2}")]
    TooFewInputs(rsleigh::Opcode, usize, usize),

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError];
}
```

Leave lines 68-75 untouched (the bridge_error invocations and `Result` alias).

Run: `cargo check -p analyzer`
Expected: PASS.

- [ ] **Step 11: Migrate the test invocations in `crates/strider-error/tests/macro_contract.rs`**

Replace lines 11-25 with:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyKind {
    #[error("boom")]
    Boom,
    // Non-transparent so `#[from]` implies `#[source]` and `source()`
    // returns the wrapped `io::Error` (transparent would forward to
    // io::Error::source() instead, which is typically None).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

strider_error::define_error! {
    pub struct MyError wraps MyKind;
    sources: [std::io::Error];
}
```

Replace lines 113-121 with:

```rust
#[derive(Debug, thiserror::Error)]
pub enum OuterKind {
    #[error(transparent)]
    Inner(MyKind),
}

strider_error::define_error! {
    pub struct OuterError wraps OuterKind;
}
```

Leave the bridge_error!, all `#[test]` functions, and their bodies unchanged.

- [ ] **Step 12: Migrate the test invocation in `crates/strider-error/tests/format.rs`**

Replace lines 6-14 with:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyKind {
    #[error("unique-display-marker-7a3f")]
    Boom,
}

strider_error::define_error! {
    pub struct MyError wraps MyKind;
}
```

(Task 2 will rewrite this file more extensively; this step just keeps it compiling.)

- [ ] **Step 13: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS. Every crate now has its enum outside the `define_error!` block; the macro emits only the wrapper struct and its impls.

- [ ] **Step 14: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. In particular:
- `cargo test -p strider-error` — still 14 tests + 3 doctests (the doctests in `define.rs` now exercise the new syntax).
- `cargo test -p dot --test error` — still 6 tests.
- `cargo test -p analyzer --test error_chain` — still passes.
- `cargo test -p reader --test error` — still passes.

- [ ] **Step 15: Strict lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 16: Commit**

```bash
git add crates/strider-error/src/define.rs crates/strider-error/tests/macro_contract.rs crates/strider-error/tests/format.rs crates/reader/src/error.rs crates/cfg/src/error.rs crates/analyzer/src/error.rs crates/target/src/error.rs crates/ir/src/error.rs crates/opt/src/error.rs crates/pattern/src/error.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): move ErrorKind enum out of define_error! macro

The macro no longer slurps the enum body via $($body:tt)* — user-crates
declare the enum as vanilla Rust and invoke define_error! only to emit
the wrapper struct, Display/Debug/Error impls, and From conversions.
Removes the $kind / $kind_enum metavariable split that allowed the two
names to drift. Mechanical migration across 7 consumer crates + 2 test
files + 2 doctests.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Introduce `Traceback` trait + rewrite `format_traceback` (C1 + S1 + R1 + R2 + T1)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs) — module doc (R2) + `Traceback` trait
- Modify: [crates/strider-error/src/lib.rs](crates/strider-error/src/lib.rs) — re-export `Traceback`
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs) — macro emits `impl Traceback`
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs) — rewrite `format_traceback` against `&dyn Traceback`; update doctest (R1)
- Modify: [crates/strider-error/tests/format.rs](crates/strider-error/tests/format.rs) — T1 test + rewrite existing tests
- Modify: [crates/dot/src/error.rs](crates/dot/src/error.rs) — manual `impl Traceback`

- [ ] **Step 1: Rewrite `tests/format.rs` with the failing tests first**

Replace the entire contents of `crates/strider-error/tests/format.rs` with:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins `format_traceback` output invariants against a `Traceback` wrapper:
//!   * Display line appears exactly once (no duplication across the Debug tail).
//!   * Location markers `  at [N] ` appear for each chain entry.
//!   * Multi-line Display does not duplicate any line (regression for C1).
//!   * Source-chain walk prints outer → caused-by → inner in order.

#[derive(Debug, thiserror::Error)]
pub enum MyKind {
    #[error("unique-display-marker-7a3f")]
    Boom,
    #[error("line1\nline2-marker-8b4e")]
    MultiLine,
}

strider_error::define_error! {
    pub struct MyError wraps MyKind;
}

#[derive(Debug, thiserror::Error)]
pub enum WithSourceKind {
    #[error("outer-marker")]
    Io(#[from] std::io::Error),
}

strider_error::define_error! {
    pub struct WithSource wraps WithSourceKind;
    sources: [std::io::Error];
}

#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
}

#[test]
fn format_traceback_does_not_duplicate_multiline_display() {
    // Regression for round-3 C1: the previous strip-first-line heuristic
    // duplicated every line past the first in a multi-line Display.
    let err: MyError = MyKind::MultiLine.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("line2-marker-8b4e").count();
    assert_eq!(
        count, 1,
        "multi-line Display second line must appear once; got {count} in:\n{s}",
    );
    let first = s.matches("line1").count();
    assert_eq!(
        first, 1,
        "multi-line Display first line must appear once; got {first} in:\n{s}",
    );
}

#[test]
fn format_traceback_walks_source_chain_top_to_bottom() {
    let io_err = std::fs::File::open("/definitely/not/a/real/path").unwrap_err();
    let err: WithSource = io_err.into();
    let s = strider_error::format_traceback(&err);

    let outer_at = s.find("outer-marker").expect("outer printed");
    let caused_at = s.find("caused by:").expect("caused-by line present");
    assert!(
        outer_at < caused_at,
        "outer must precede the caused-by line; got:\n{s}",
    );
}

#[test]
fn format_traceback_includes_location_marker() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    assert!(s.contains("  at [0] "), "missing location[0] marker in:\n{s}");
    // Output must contain more than just the locations — either the
    // backtrace Display or its "disabled backtrace" placeholder follows.
    assert!(!s.trim_end().ends_with("  at [0] "), "backtrace section missing in:\n{s}");
}
```

- [ ] **Step 2: Run the tests — they must fail (no `Traceback` trait yet, `format_traceback` signature unchanged)**

Run: `cargo test -p strider-error --test format 2>&1 | head -20`
Expected: compile error about the multi-line-Display assertion (fails at runtime) OR about `format_traceback` expecting `&dyn Traceback` (after Step 5). Either way, not all four tests pass yet.

- [ ] **Step 3: Add module doc + `Traceback` trait in `fields.rs`**

Prepend a module-level doc block to `crates/strider-error/src/fields.rs`, replacing the current leading `use` lines:

```rust
//! Core data types shared across `strider-error` wrappers.
//!
//! - [`ErrorFields`] — backtrace + per-`?` location chain.
//! - [`LocationChain`] — type alias for the chain vector.
//! - [`Traceback`] — trait implemented by every wrapper so
//!   [`crate::format_traceback`] can render locations/backtrace without
//!   inspecting `Debug` output.

use std::backtrace::Backtrace;
use std::panic::Location;
```

Append this trait to the bottom of the file (after the final `}` of `impl ErrorFields`):

```rust
/// Implemented by every error wrapper that carries an [`ErrorFields`] payload.
///
/// Supertrait on [`std::error::Error`] so that `&dyn Traceback` upcasts to
/// `&dyn Error` for source-chain walks (trait upcasting, stable since Rust
/// 1.86). Object-safe by design — [`crate::format_traceback`] takes
/// `&dyn Traceback` without monomorphizing.
///
/// Implementations are provided automatically by
/// [`crate::define_error!`] for non-generic wrappers, and by hand for the
/// generic `dot::error::Error<E>`.
pub trait Traceback: std::error::Error {
    /// Returns the propagation chain (origin first, top-of-stack last).
    fn location_chain(&self) -> &LocationChain;

    /// Returns the backtrace captured at the origin of this error.
    fn origin_backtrace(&self) -> &Backtrace;
}
```

- [ ] **Step 4: Re-export the trait from `lib.rs`**

Change [crates/strider-error/src/lib.rs:31](crates/strider-error/src/lib.rs#L31):

```rust
pub use fields::{ErrorFields, LocationChain, Traceback};
```

Add one bullet to the "Where to start" list in the module doc (after the `format_traceback` bullet):

```rust
//! - [`Traceback`] — trait every wrapper implements; [`format_traceback`]
//!   uses it to render locations + backtrace.
```

- [ ] **Step 5: Emit `impl Traceback` from `define_error!`**

In `crates/strider-error/src/define.rs`, inside the macro expansion body, insert a new impl block immediately after the existing `impl ::std::error::Error for $wrapper` block (which now sits in the grammar-simplified macro from Task 1):

```rust
        impl $crate::Traceback for $wrapper {
            fn location_chain(&self) -> &$crate::LocationChain {
                &self.fields.locations
            }
            fn origin_backtrace(&self) -> &::std::backtrace::Backtrace {
                &self.fields.backtrace
            }
        }
```

- [ ] **Step 6: Rewrite `format_traceback` against `&dyn Traceback`**

Replace the entire contents of `crates/strider-error/src/format.rs` with:

```rust
//! Human-readable and FFI-friendly formatting of error chains.

use std::error::Error;
use std::fmt::Write;

use crate::Traceback;

/// Renders a [`Traceback`]-bearing error into a single string:
/// Display line, source-chain walk (one `"  caused by: "` per hop),
/// per-`?` location chain (`"  at [N] file:line:column"`), then the
/// origin backtrace.
///
/// The `strider-py` PyO3 layer calls this to produce the body of the
/// Python exception's string representation.
///
/// ```
/// #[derive(Debug, thiserror::Error)]
/// pub enum MyKind {
///     #[error("something went wrong")]
///     Oops,
/// }
///
/// strider_error::define_error! {
///     pub struct MyError wraps MyKind;
/// }
///
/// let err: MyError = MyKind::Oops.into();
/// let s = strider_error::format_traceback(&err);
/// assert!(s.starts_with("error: something went wrong"));
/// assert!(s.contains("  at [0] "));
/// ```
pub fn format_traceback(err: &dyn Traceback) -> String {
    let mut out = String::new();
    // Writing into a String is infallible; the `let _` silences the
    // Result produced by the fmt::Write trait.
    let _ = writeln!(out, "error: {err}");

    // Source-chain walk via the Error supertrait. `&dyn Traceback` upcasts
    // to `&dyn Error` implicitly (trait upcasting, stable since 1.86).
    let err_ref: &(dyn Error + 'static) = err;
    let mut cur = err_ref.source();
    while let Some(e) = cur {
        let _ = writeln!(out, "  caused by: {e}");
        cur = e.source();
    }

    for (i, loc) in err.location_chain().iter().enumerate() {
        let _ = writeln!(
            out,
            "  at [{}] {}:{}:{}",
            i,
            loc.file(),
            loc.line(),
            loc.column(),
        );
    }

    let _ = write!(out, "{}", err.origin_backtrace());
    out
}
```

- [ ] **Step 7: Add manual `impl Traceback` for `dot::Error<E>`**

In `crates/dot/src/error.rs`, append after the existing `impl<E: Debug + std::error::Error + 'static> std::error::Error for Error<E>` block ([dot/src/error.rs:73-77](crates/dot/src/error.rs#L73-L77)):

```rust
impl<E: Debug + std::error::Error + std::fmt::Display + 'static> strider_error::Traceback for Error<E> {
    fn location_chain(&self) -> &strider_error::LocationChain {
        &self.fields.locations
    }
    fn origin_backtrace(&self) -> &std::backtrace::Backtrace {
        &self.fields.backtrace
    }
}
```

The bounds are the union of what `Debug`, `Display`, `Error + 'static`, and `Traceback` (which requires `Error`) all need on `E`. Any concrete dumper-error used today satisfies all four.

- [ ] **Step 8: Run the tests — all must pass**

Run: `cargo test -p strider-error --test format`
Expected: 4 passed.

Run: `cargo test -p strider-error`
Expected: all pre-existing tests still pass + the 4 rewritten format tests = 16 tests + 3 doctests.

Run: `cargo test -p dot --test error`
Expected: 6 passed.

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 9: Strict lint**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add crates/strider-error/src/fields.rs crates/strider-error/src/lib.rs crates/strider-error/src/define.rs crates/strider-error/src/format.rs crates/strider-error/tests/format.rs crates/dot/src/error.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): render traceback via Traceback trait, not Debug strip heuristic

Introduces `Traceback: Error` implemented by `define_error!` wrappers and
manually by `dot::Error<E>`. `format_traceback` now formats locations and
backtrace from the trait rather than stripping the first line of `{err:?}`.
Fixes a latent duplication bug when a wrapper's Display is multi-line.
No workspace callers today, so the signature change is internal.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. Specifically:
- `cargo test -p strider-error` — 16 tests + 3 doctests.
- `cargo test -p dot --test error` — 6 tests.
- `cargo test -p analyzer --test error_chain` — unaffected.
- `cargo test -p reader --test error` — unaffected.

- [ ] **Step 3: Workspace strict lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — confirms no runtime regression via the error path.

---

## Out of Scope (considered, rejected or deferred)

- **`Box<Backtrace>` → bare `Backtrace`.** Saves one allocation per origin but adds ~32 bytes to every wrapper struct. Round-1's "stable 1-pointer footprint" rationale still holds. Skip.
- **`LocationChain` → `SmallVec<[_; 1]>`.** Would eliminate the origin-only heap allocation. Adds a dep for a micro-optimization on an error path. Skip unless profiling demands it.
- **Privatize `ErrorFields.backtrace` / `ErrorFields.locations` fields, expose accessors.** Tests and `dot::Error<E>` legitimately need direct access; privatizing adds API surface without observable benefit. Skip.
- **`decompose` returns unboxed `Kind` instead of `Box<Kind>`.** Would shave a `*` from each `bridge_error!` call site (one place). Not worth the breakage of an API method. Skip.
- **Per-test `#![allow(clippy::panic, ...)]` dedup.** Three lines, perfectly clear. Skip.
- **Consolidating `ir::ValidationErrors` bridge in `opt::error` into `bridge_error!`.** Already deferred in round 2; still not worth a second macro form for one call site.
- **Auto-impl `Traceback` on `dot::Error<E>` via a `#[derive]`-style macro.** One hand-rolled wrapper, one hand-rolled `impl Traceback`. Not worth a proc-macro crate.
