use super::*;

fn addr(machine: u64, idx: u64) -> PcodeInsnAddr {
    PcodeInsnAddr::new(machine, idx)
}

#[test]
fn fingerprint_default_is_empty() {
    // The default state must mean "no provenance recorded yet" so it can
    // act as the identity for merge accumulation.
    let fp = Fingerprint::default();
    assert_eq!(fp.len(), 0);
    assert!(fp.is_empty());
    assert_eq!(fp.iter().count(), 0);
}

#[test]
fn fingerprint_from_single_contains_addr() {
    let a = addr(0x1000, 0);
    let fp = Fingerprint::from_single(a);
    assert_eq!(fp.len(), 1);
    assert!(fp.contains(a));
}

#[test]
fn fingerprint_merge_unions_two() {
    let a = addr(0x1000, 0);
    let b = addr(0x1004, 0);
    let merged = Fingerprint::merge(&Fingerprint::from_single(a), &Fingerprint::from_single(b));
    assert_eq!(merged.len(), 2);
    assert!(merged.contains(a));
    assert!(merged.contains(b));
}

#[test]
fn fingerprint_merge_dedupes_overlap() {
    // The same address present in both operands appears exactly once in
    // the union.
    let a = addr(0x1000, 0);
    let fa = Fingerprint::from_single(a);
    let merged = Fingerprint::merge(&fa, &fa);
    assert_eq!(merged.len(), 1);
    assert!(merged.contains(a));
}

#[test]
fn fingerprint_merge_many_handles_empty_iter() {
    // The identity of merge is the empty fingerprint.
    let merged = Fingerprint::merge_many(std::iter::empty());
    assert!(merged.is_empty());
}

#[test]
fn fingerprint_merge_many_handles_single() {
    let a = addr(0x2000, 0);
    let fa = Fingerprint::from_single(a);
    let merged = Fingerprint::merge_many([&fa]);
    assert_eq!(merged, fa);
}

#[test]
fn fingerprint_iter_yields_unique_addrs() {
    // Build a fingerprint with overlapping inputs, verify the iterator
    // yields each unique address exactly once.
    let a = addr(0x1000, 0);
    let b = addr(0x1004, 0);
    let c = addr(0x1008, 0);
    let fa = Fingerprint::from_single(a);
    let fb = Fingerprint::from_single(b);
    let fc = Fingerprint::from_single(c);
    let merged = Fingerprint::merge_many([&fa, &fb, &fa, &fc, &fb]);
    let collected: Vec<_> = merged.iter().collect();
    assert_eq!(collected.len(), 3);
    // Sorted ascending.
    assert_eq!(collected, vec![a, b, c]);
}

#[test]
fn fingerprint_len_matches_iter_count() {
    let fps: Vec<Fingerprint> = (0..10)
        .map(|i| Fingerprint::from_single(addr(i * 4, 0)))
        .collect();
    let merged = Fingerprint::merge_many(fps.iter());
    assert_eq!(merged.len(), merged.iter().count());
}
