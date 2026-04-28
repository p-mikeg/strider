#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Pure value-producing pcode → IR lifter, factored out of `strider`.
//!
//! The [`ValueLifter`] owns the per-opcode handlers for every pcode opcode
//! that produces a value (arithmetic, integer, float, boolean, casts,
//! memory loads, miscellaneous value ops).  Control-flow opcodes (Branch,
//! CondBranch, Return, BranchIndirect, Call, CallIndirect, Store) are NOT
//! handled here — [`ValueLifter::lift`] returns `Ok(false)` so the caller
//! can route them through its own (region-aware) machinery.
//!
//! This separation lets two callers reuse the value-lifting logic:
//!
//! * `strider`, which uses it as the inner-loop dispatch for translating
//!   a CFG region into the per-region IR;
//! * `cfg`, which uses it (planned) to build a stand-alone single-block
//!   mini-IR for resolving the targets of indirect branches.

pub mod error;
pub mod value;
pub mod vn_io;

pub use error::{Error, ErrorKind, Result};

/// Lifts a single value-producing pcode instruction into IR nodes.
///
/// Holds borrows to the IR [`FunctionBuilder`] being filled in, the
/// [`rsleigh::Sleigh`] context (for address-space / register metadata),
/// and the target architecture's endianness (used by the register
/// aliasing logic in [`vn_io`]).
///
/// Construct one per pcode insn (or per region — the lifter is
/// stateless beyond the borrows it carries).
///
/// [`FunctionBuilder`]: ir::FunctionBuilder
pub struct ValueLifter<'a, R: rsleigh::MemReader> {
    /// IR builder receiving the lifted nodes.
    pub builder: &'a mut ir::FunctionBuilder,
    /// Sleigh context for the source binary.  Needed to decode address
    /// spaces (e.g. CONST, REGISTER, default-code space) when reading
    /// and writing varnodes.
    pub sleigh: &'a rsleigh::Sleigh<R>,
    /// Target endianness — drives the bit-shift formula used when
    /// reading or writing a sub-register inside a wider container.
    pub endianness: target::Endianness,
}

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Creates a new [`ValueLifter`] borrowing the given builder, sleigh
    /// context, and endianness.
    pub fn new(
        builder: &'a mut ir::FunctionBuilder,
        sleigh: &'a rsleigh::Sleigh<R>,
        endianness: target::Endianness,
    ) -> Self {
        Self {
            builder,
            sleigh,
            endianness,
        }
    }

    /// Lifts a single pcode instruction.
    ///
    /// Returns `Ok(true)` when `insn`'s opcode is value-producing and
    /// was lifted into IR nodes.  Returns `Ok(false)` when the opcode
    /// is a control-flow / call / store op that the caller is
    /// responsible for handling — the caller observes the `false` and
    /// dispatches via its own machinery.
    ///
    /// # Errors
    ///
    /// Returns an error when the instruction is malformed (missing
    /// output varnode, wrong number of inputs, unsupported sizes,
    /// etc.) or when an underlying IR builder call fails.
    pub fn lift(&mut self, insn: &rsleigh::Insn) -> Result<bool> {
        value::lift(self, insn)
    }

    /// Lifts a single pcode instruction AND seeds every newly-created node's
    /// [`ir::Fingerprint`] with `addr`.
    ///
    /// Implementation: snapshot the node-arena length before the lift, run
    /// the value lifter, then for every node created by the lift call merge
    /// `Fingerprint::from_single(addr)` into its existing (auto-merged from
    /// inputs) fingerprint.  Cache hits in `create_node` return existing
    /// nodes that ALREADY carry the addrs of every prior pcode insn that
    /// caused them — so the merge correctly accumulates provenance over the
    /// lifetime of the graph.
    ///
    /// CORRECTNESS — semantic for control-flow opcodes: when `lift` returns
    /// `Ok(false)` (the caller's responsibility to handle), the snapshot
    /// range may still include nodes the value lifter created before
    /// returning false (rare, but possible for malformed dispatch).  Seeding
    /// runs unconditionally — every node in the post-lift range belongs to
    /// this pcode insn either way.
    ///
    /// # Errors
    ///
    /// Same as [`ValueLifter::lift`].
    pub fn lift_with_addr(
        &mut self,
        insn: &rsleigh::Insn,
        addr: ir::PcodeInsnAddr,
    ) -> Result<bool> {
        let pre_count = self.builder.body().graph.node_count();
        let lifted = value::lift(self, insn)?;
        // Seed every node created during this lift call with the addr.
        // Nodes returned via dedup-cache hit are not in the new range, but
        // their fingerprints ALREADY carry the relevant addrs from the
        // original construction call(s) — so the contract holds.
        let post_count = self.builder.body().graph.node_count();
        let seed = ir::Fingerprint::from_single(addr);
        for raw in pre_count..post_count {
            let node_id = ir::node::NodeId::from_u32(raw as u32);
            let merged = ir::Fingerprint::merge(
                self.builder.body().graph.fingerprint_of(node_id),
                &seed,
            );
            self.builder
                .body_mut()
                .graph
                .set_fingerprint(node_id, merged);
        }
        Ok(lifted)
    }
}

/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.
pub(crate) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output
        .as_ref()
        .ok_or_else(|| ErrorKind::MissingOutputVn(insn.opcode).into())
}
