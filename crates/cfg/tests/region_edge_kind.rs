//! Tests for `RegionEdgeKind` — verify its four variants are pairwise distinct.

use cfg::RegionEdgeKind;

#[test]
fn variants_are_pairwise_distinct() {
    let kinds = [
        RegionEdgeKind::Fallthrough,
        RegionEdgeKind::Branch,
        RegionEdgeKind::IfCaseTrue,
        RegionEdgeKind::IfCaseFalse,
    ];
    for i in 0..kinds.len() {
        for j in (i + 1)..kinds.len() {
            assert_ne!(kinds[i], kinds[j]);
        }
    }
}
