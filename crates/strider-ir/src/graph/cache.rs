//! [`IrCacheable`] — the IR's dedup-or-create *policy* for the generic
//! [`strider_graph::Graph`].
//!
//! The dedup-cache *mechanism* (the table, per-node hashes, O(1) eviction,
//! rebuild, and the `u64::MAX → 0` sentinel avoidance) lives generically in
//! `strider-graph`. This is purely the strider-specific policy: a stateless ZST
//! supplying the four [`strider_graph::NodeCacheable`] hooks the mechanism
//! consults:
//!
//! - [`canonicalize`](IrCacheable::canonicalize) — normalises an `IntConst`
//!   payload to its declared width, so semantically-equal constants minted by
//!   different paths share one canonical form. This is the single source of
//!   truth for `IntConst` payload masking (a big-endian read can mint
//!   `IntConst(0xff..fc):I64` while another path mints the 64-bit-masked form;
//!   both are `-4:I64` and must dedup). Applied to EVERY node before alloc.
//! - [`should_cache`](IrCacheable::should_cache) — gates dedup on
//!   [`NodeKind::is_cacheable`]; non-cacheable kinds (`Region`, `Phi`,
//!   `MemPhi`, `Call`, …) always allocate fresh.
//! - [`hash`](IrCacheable::hash) — a pure `FxHasher` over the
//!   `(kind, inputs, output_kinds)` key. It returns a RAW hash with no sentinel
//!   knowledge; the generic cache remaps the lone `u64::MAX` internally.
//! - [`eq`](IrCacheable::eq) — re-reads a candidate's `(kind, inputs,
//!   output_kinds)` out of the store and compares against the query, so no
//!   owned key payloads are kept and hash collisions are resolved exactly.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use strider_graph::{NodeCacheable, NodeId, RawStore, ValueId};

use crate::node::{NodeKind, ValueKind, ValueType};

/// The IR's deduplication policy: a stateless ZST supplying the four
/// [`NodeCacheable`] hooks. It owns no state — the generic
/// `strider_graph::Graph` owns the dedup table and per-node hashes.
///
/// Cacheable node kinds (see [`NodeKind::is_cacheable`]) are deduplicated by
/// their `(NodeKind, inputs, output_kinds)` structure; non-cacheable kinds
/// (`Region`, `Phi`, `MemPhi`, `Call`, …) always allocate a fresh node.
pub struct IrCacheable;

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    /// Normalises an `IntConst` payload by masking it to its declared integer
    /// output type's bit width, so every creation path (lifter sub-register
    /// read, rewrite closure, `build_int_const`, …) keys the cache on the same
    /// canonical narrow payload.
    ///
    /// Only the narrow integer `Typed` case is touched: wide constants
    /// (`I256`/`I512`) flow through `IntConstWide`, and non-integer /
    /// non-value outputs are left alone.
    fn canonicalize(kind: NodeKind, _inputs: &[ValueId], outputs: &[ValueKind]) -> NodeKind {
        match (kind, outputs) {
            (NodeKind::IntConst(v), [ValueKind::Typed(ty)])
                if ty.is_integer() && !matches!(ty, ValueType::I256 | ValueType::I512) =>
            {
                NodeKind::IntConst(v & ty.bit_mask_u128())
            }
            (kind, _) => kind,
        }
    }

    /// Gates dedup on [`NodeKind::is_cacheable`].
    fn should_cache(kind: &NodeKind) -> bool {
        kind.is_cacheable()
    }

    /// Hashes a `(kind, inputs, output_kinds)` structural key into a `u64`.
    ///
    /// The fields are hashed in declaration order (`kind`, then the input-value
    /// slice, then the output-kind slice). `[T]: Hash` hashes the length
    /// followed by each element, so hashing a borrowed query slice and hashing
    /// a node's re-read `SmallVec` of the same contents agree element-for-
    /// element — which is what lets a query probe land in the same bucket the
    /// node was inserted under.
    ///
    /// Returns a RAW `FxHash` with no sentinel handling: the generic cache
    /// remaps the lone `u64::MAX` value itself.
    fn hash(kind: &NodeKind, inputs: &[ValueId], outputs: &[ValueKind]) -> u64 {
        let mut h = FxHasher::default();
        kind.hash(&mut h);
        inputs.hash(&mut h);
        outputs.hash(&mut h);
        h.finish()
    }

    /// Re-reads candidate node `cand` from the store and reports whether its
    /// stored `(kind, inputs, output_kinds)` structure equals the query. This
    /// is the equality half of the hash-on-demand probe: no owned key payloads
    /// are kept, so structural identity is recomputed from the live store.
    fn eq(
        store: &RawStore<NodeKind, ValueKind>,
        cand: NodeId,
        kind: &NodeKind,
        inputs: &[ValueId],
        outputs: &[ValueKind],
    ) -> bool {
        store.kind_of(cand) == kind
            && store.input_values(cand).as_slice() == inputs
            && store.output_kinds(cand).as_slice() == outputs
    }
}
