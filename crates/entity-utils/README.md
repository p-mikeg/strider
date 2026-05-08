# `entity-utils` — entity-keyed sets and worklists

Two `no_std`-friendly data structures keyed by Cranelift `EntityRef` types.
Used pervasively by [`ir`](../ir), [`opt`](../opt), [`pattern`](../pattern), and
[`graphwalk`](../graphwalk) to track visited / queued nodes during graph walks.

## Public surface

- `set::DenseEntitySet<E>` — bitset over a `cranelift_entity::EntityRef` key.
  O(1) `insert`, `remove`, `contains`, plus an `Iter<'_, E>` for iteration in
  index order.
- `worklist::Worklist<E>` — FIFO queue with a built-in dedup bitset, so an
  entity can be enqueued at most once until it is dequeued. Used by fixed-point
  passes that need "process every entity that may have changed".

## Architecture

Two single-file modules. `set.rs` wraps `cranelift_bitset::CompoundBitSet` and
provides the standard set surface plus an iterator. `worklist.rs` pairs a
`VecDeque<E>` with a `DenseEntitySet<E>` to ensure each entity sits in the
queue at most once between dequeues.

The crate is `no_std` outside of tests (`extern crate alloc`); it depends only
on `cranelift-bitset` and `cranelift-entity`.

## Key invariants

- `DenseEntitySet::contains` after `insert(e)` is `true` until `remove(e)` is
  called.
- `Worklist::push(e)` is idempotent within a single "pop cycle" — the second
  push is dropped if `e` is already queued. Once popped, `e` becomes
  pushable again.
- `Iter` yields entities in increasing `EntityRef::index()` order.

## Tests

Inline unit tests in each module file. No integration tests directory.

```
cargo test --package entity-utils
```

## Gotchas

- Both types are dense — memory usage scales with the largest `EntityRef::index()`
  ever inserted, not the number of live entities. Don't use for sparse keys.
- `EntityRef` keys come from `cranelift_entity::PrimaryMap`; this crate doesn't
  produce them itself.
- `no_std` outside of tests; if you need `std::collections::HashSet` semantics
  reach for those instead.
