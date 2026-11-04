use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::{Graph, NeverCacheable, NodeId};

use super::CastMask;
use super::vertex::{PatNode, PatValue, PostMatchFn};

pub(crate) type PatGraph = Graph<PatNode, PatValue, NeverCacheable>;

/// Pattern nodes one query may recurse through, counted tree-unfolded (a vertex once per consumer
/// slot) plus one per output vertex past a node's first. Under it a query fits a spawned thread's
/// 2 MiB stack.
pub const MAX_PATTERN_NODES: usize = 256;

/// Per pat node: the most nodes one match configuration recurses through
/// (each vertex once per consumer slot reaching it, each output vertex past the
/// first as one more), saturating; and the reads it unfolds to (see
/// [`super::walk::Read`]), `None` at 2^64 or more. An alternation charges its
/// heaviest arm to the first and every arm to the second.
fn unfolded(graph: &PatGraph) -> Vec<(usize, Option<u64>)> {
    // Id order is topological: producers are built first.
    let mut size: Vec<(usize, Option<u64>)> = Vec::with_capacity(graph.all_node_ids().count());
    for node in graph.all_node_ids() {
        let inputs = graph
            .node_inputs(node)
            .into_iter()
            .map(|value| size[graph.producer(value).as_u32() as usize]);
        let reads = inputs.clone().try_fold(1, |n: u64, s| n.checked_add(s.1?));
        let below = if graph.node_kind(node).is_alternation() {
            inputs.map(|s| s.0).max().unwrap_or(0)
        } else {
            inputs.fold(0, |n, s| s.0.saturating_add(n))
        };
        let extra_outputs = graph.node_outputs(node).len().saturating_sub(1);
        size.push((below.saturating_add(1).saturating_add(extra_outputs), reads));
    }
    size
}

/// The most nodes a query recurses through, at least the raw node count. Taken over every node,
/// since the root may not exist.
fn unfolded_nodes(size: &[(usize, Option<u64>)]) -> usize {
    size.iter().map(|s| s.0).max().unwrap_or(0).max(size.len())
}

/// The pat nodes consuming `node`'s outputs, once per consuming edge.
fn consumers(graph: &PatGraph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    (graph.node_outputs(node).iter()).flat_map(|&v| graph.value_uses(v).map(|(c, _)| c))
}

pub struct Pattern {
    pub(crate) graph: PatGraph,
    pub(crate) cast_mask: CastMask,
    /// Resolved at seal.
    root: Result<NodeId, String>,
    /// Per pat node, indexed by `NodeId::as_u32`.
    inputs: Vec<super::walk::NodeInputs>,
    pub(crate) holds_first_of: bool,
    /// The `first_of` that skips arms past one that matched: the first built, if one edge path
    /// reaches it from the root.
    pub(crate) pruned_first_of: Option<NodeId>,
}

impl Pattern {
    /// Whether the root's kind spec, payload included, can admit `kind`: a cheap pre-filter
    /// before `match_at`. `true` for a refused pattern and an alternation root.
    #[must_use]
    pub fn root_admits_kind(&self, kind: &strider_ir::node::NodeKind) -> bool {
        let Ok(root) = self.root() else {
            return true;
        };
        let vertex = self.graph.node_kind(root);
        vertex.is_alternation() || vertex.kind.matches(kind)
    }

    /// Seal point of [`MatcherBuilder`](crate::matcher::MatcherBuilder): resolves the match root.
    /// A build-time `refusal` takes precedence over the structural verdict.
    pub(crate) fn from_graph(graph: PatGraph, refusal: Option<String>) -> Self {
        let size = unfolded(&graph);
        let count = unfolded_nodes(&size);
        let root = if let Some(why) = refusal {
            Err(why)
        } else if count > MAX_PATTERN_NODES {
            // A chain of shared operands can unfold past `usize`.
            let count = match count {
                usize::MAX => format!("over 2^{}", usize::BITS),
                n => n.to_string(),
            };
            Err(format!(
                "pattern has {count} nodes, over the {MAX_PATTERN_NODES} a query can \
                 recurse through; split it or match the parts separately"
            ))
        } else {
            crate::graph_ext::derive_root(&graph).map_err(|e| e.to_string())
        };
        let inputs = super::walk::collect_node_inputs(&graph, &size, root.is_ok());
        let root = root.and_then(|r| {
            if size.iter().any(|s| s.1.is_none()) {
                return Err(
                    "pattern unfolds to 2^64 or more reads through its alternations; \
                     split it or match the parts separately"
                        .to_string(),
                );
            }
            Ok(r)
        });
        let first_of = graph
            .all_node_ids()
            .find(|&n| graph.node_kind(n).role == super::Role::FirstOf);
        let one_route = |p| {
            std::iter::successors(Some(p), |&n| consumers(&graph, n).next())
                .all(|n| consumers(&graph, n).nth(1).is_none())
        };
        let pruned_first_of = first_of.filter(|&p| one_route(p));
        Self {
            holds_first_of: first_of.is_some(),
            pruned_first_of,
            graph,
            cast_mask: CastMask::empty(),
            root,
            inputs,
        }
    }

    /// Nodes a query recurses through.
    #[cfg(test)]
    pub(crate) fn total_nodes(&self) -> usize {
        unfolded_nodes(&unfolded(&self.graph))
    }

    /// `node`'s inputs, resolved at seal.
    pub(crate) fn inputs_of(&self, node: NodeId) -> &super::walk::NodeInputs {
        &self.inputs[node.as_u32() as usize]
    }

    /// The match root, resolved at seal.
    ///
    /// # Errors
    /// If the pattern is rootless, multi-sink, or was refused at build time.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        self.root.clone().map_err(anyhow::Error::msg)
    }

    /// Whether a match may bind a value it reached through a join, which
    /// need not sit below the match root: a pat node admitting a `Phi`,
    /// `MemPhi` or `Region` pins an input.
    pub fn may_bind_through_a_join(&self) -> bool {
        use strider_ir::node::NodeKind;
        self.graph.all_node_ids().any(|n| {
            let node = self.graph.node_kind(n);
            !node.is_alternation()
                && !node.input_slots.is_empty()
                && [NodeKind::Phi, NodeKind::MemPhi, NodeKind::Region]
                    .iter()
                    .any(|k| node.kind.matches(k))
        })
    }

    /// Every capture appearing anywhere in the graph.
    pub fn bound_captures(&self) -> impl Iterator<Item = crate::capture::Capture> + '_ {
        self.graph
            .all_node_ids()
            .flat_map(|n| self.graph.node_kind(n).captures.iter().copied())
            .chain(
                self.graph
                    .all_value_ids()
                    .flat_map(|v| self.graph.value_kind_ref(v).captures.iter().copied()),
            )
    }

    /// The captures bound on every successful match. Differs from [`Self::bound_captures`] only
    /// by captures some but not all alternation arms bind.
    ///
    /// # Errors
    /// If the pattern is rootless or multi-sink.
    pub fn guaranteed_captures(&self) -> anyhow::Result<FxHashSet<crate::capture::Capture>> {
        let mut memo: FxHashMap<NodeId, FxHashSet<crate::capture::Capture>> = FxHashMap::default();
        Ok(self.guaranteed_from(self.root()?, &mut memo))
    }

    /// Memoized over the acyclic pattern graph, so linear.
    fn guaranteed_from(
        &self,
        node: NodeId,
        memo: &mut FxHashMap<NodeId, FxHashSet<crate::capture::Capture>>,
    ) -> FxHashSet<crate::capture::Capture> {
        if let Some(hit) = memo.get(&node) {
            return hit.clone();
        }
        let mut out: FxHashSet<crate::capture::Capture> = FxHashSet::default();
        out.extend(self.graph.node_kind(node).captures.iter().copied());
        for &vertex in self.graph.node_outputs(node) {
            out.extend(self.graph.value_kind_ref(vertex).captures.iter().copied());
        }
        // An alternation input's vertex capture belongs to its arm, so it stays inside the
        // intersection.
        let per_input: Vec<FxHashSet<crate::capture::Capture>> =
            crate::graph_ext::consumed_inputs(&self.graph, node)
                .into_iter()
                .map(|(_, vertex)| {
                    let mut caps = self.guaranteed_from(self.graph.producer(vertex), memo);
                    caps.extend(self.graph.value_kind_ref(vertex).captures.iter().copied());
                    caps
                })
                .collect();
        if self.graph.node_kind(node).is_alternation() {
            let mut arms = per_input.into_iter();
            if let Some(first) = arms.next() {
                let common = arms.fold(first, |acc, arm| acc.intersection(&arm).copied().collect());
                out.extend(common);
            }
        } else {
            for caps in per_input {
                out.extend(caps);
            }
        }
        memo.insert(node, out.clone());
        out
    }

    /// Runs after the whole match, seeing every capture; `false` rejects it.
    /// The type is the matched output's, `None` for a control, memory or
    /// `PhiToken` edge or a zero-output node.
    /// Composes with a guard already on the root. A no-op on a pattern with no unique root.
    pub fn with_root_post_match(mut self, f: PostMatchFn) -> Self {
        if let Ok(&root) = self.root.as_ref() {
            self.graph.node_kind_mut(root).post_matches.push(f);
        }
        self
    }

    /// Adds `m` to the cast mask: an operand produced by a selected cast also matches at the
    /// cast's input.
    ///
    /// Per operand, only the matches at the outermost cast level are kept. Operands settle after
    /// every `first_of`, in written order, inner before outer, so a guard coupling two of them
    /// depends on which is written first. Costs as [`OneOf::first`](crate::OneOf::first) does.
    pub fn ignore_casts_mask(mut self, m: CastMask) -> Self {
        self.cast_mask |= m;
        self
    }

    /// Walk through every value-passthrough cast kind.
    pub fn ignore_casts(self) -> Self {
        self.ignore_casts_mask(CastMask::all())
    }

    #[cfg(test)]
    pub(crate) fn control_output_count(&self) -> usize {
        self.graph
            .all_value_ids()
            .filter(|&v| {
                matches!(
                    self.graph.value_kind_ref(v).kind,
                    super::vertex::OutputKindSpec::Control
                )
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::{PatGraph, Pattern};
    use crate::MatchPat;
    use crate::matcher::MatcherBuilder;
    use crate::matcher::{KindSpec, PatValueRef};
    use rustc_hash::FxHashMap;
    use strider_graph::NodeId;
    use strider_ir::IntConstId;
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::Rng;

    /// A join bound as a leaf binds nothing through it; one whose inputs the
    /// pattern pins may.
    #[test]
    fn may_bind_through_a_join_reads_the_pattern_shape() {
        use crate::{Capture, anything, call, int_add, load, mem_phi, phi, region, var};
        let c = Capture::new();
        let shape = |p: Pattern| p.may_bind_through_a_join();
        assert!(!shape(int_add(phi(), var(c)).into_pattern()));
        assert!(!shape(int_add(var(c), anything()).into_pattern()));
        assert!(shape(
            int_add(phi().phi_input(0, var(c)), anything()).into_pattern()
        ));
        assert!(shape(mem_phi().phi_input(0, crate::store()).into_pattern()));
        assert!(!shape(load().mem(call().ctrl(region())).into_pattern()));
        assert!(shape(
            load()
                .mem(call().ctrl(region().any_input(anything())))
                .into_pattern()
        ));
        // An alternation's inputs are arms, not operands.
        let (a, b) = (Capture::new(), Capture::new());
        assert!(!shape(
            crate::one_of![int_add(var(a), var(b)), crate::int_mul(var(a), var(b))].into_pattern()
        ));
        assert!(shape(
            crate::one_of![phi().phi_input(0, var(a)), var(b)].into_pattern()
        ));
    }

    /// Each `one_of` reads its one vertex through both arms, so 63 of them
    /// unfold to 2^64 - 1 reads, the most a `u64` counts; 64 overflow it.
    #[test]
    fn a_pattern_overflowing_the_read_count_is_refused() {
        let chain = |n| {
            let mut b = MatcherBuilder::new();
            let mut v = b.leaf(KindSpec::Any);
            for _ in 0..n {
                v = b.one_of(&[v, v]);
            }
            b.finish().root().map_err(|e| e.to_string())
        };
        assert!(chain(63).is_ok());
        assert!(chain(64).unwrap_err().contains("2^64 or more reads"));
    }

    #[test]
    fn builds_bipartite_add_shape() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::matcher::KindSpec::Any);
        let k = b.leaf(crate::matcher::KindSpec::Exact(NodeKind::IntConst(
            IntConstId::from_u32(1),
        )));
        let _sum = b.binary(strider_ir::IntBinaryOp::Add, x, k);
        let p = b.finish();
        assert_eq!(p.graph.all_node_ids().count(), 3);
        assert_eq!(p.graph.all_value_ids().count(), 3);
        // Root is the unique sink (`int_add`).
        assert!(p.root().is_ok());
    }

    /// The tree unfolding of `n`, by memoised recursion.
    fn unfold(g: &PatGraph, n: NodeId, memo: &mut FxHashMap<NodeId, usize>) -> usize {
        if let Some(&s) = memo.get(&n) {
            return s;
        }
        let kids: Vec<usize> = g
            .node_inputs(n)
            .into_iter()
            .map(|v| unfold(g, g.producer(v), memo))
            .collect();
        let own = 1 + g.node_outputs(n).len().saturating_sub(1);
        let s = if g.node_kind(n).is_alternation() {
            own.saturating_add(kids.iter().copied().max().unwrap_or(0))
        } else {
            kids.iter().fold(own, |acc, &k| acc.saturating_add(k))
        };
        memo.insert(n, s);
        s
    }

    /// A wildcard node consuming `inner` at slot 0.
    fn wildcard_over(b: &mut MatcherBuilder, inner: PatValueRef) -> PatValueRef {
        let n = b.node(KindSpec::Any);
        b.input(n, 0, inner);
        b.value_output(n, 0)
    }

    /// A random pattern over at most twelve steps. Checks that `total_nodes`
    /// is the larger of the raw count and the deepest unfolding.
    ///
    /// `shapes` counts: the unfolding wins, the raw count wins, an alternation
    /// whose arms unfold to different sizes.
    fn random_pattern(rng: &mut Rng, shapes: &mut [usize; 3]) {
        let mut b = MatcherBuilder::new();
        let mut vals: Vec<PatValueRef> = vec![b.leaf(KindSpec::Any)];
        for _ in 0..rng.below(12) {
            let (l, r) = (vals[rng.below(vals.len())], vals[rng.below(vals.len())]);
            let v = match rng.below(4) {
                1 => wildcard_over(&mut b, l),
                2 => b.binary(strider_ir::IntBinaryOp::Add, l, r),
                3 => b.one_of(&[l, r]),
                _ => b.leaf(KindSpec::Any),
            };
            vals.push(v);
        }
        let p = b.finish();
        let mut memo = FxHashMap::default();
        let deepest = p
            .graph
            .all_node_ids()
            .map(|n| unfold(&p.graph, n, &mut memo))
            .max()
            .unwrap_or(0);
        let raw = p.graph.all_node_ids().count();
        assert_eq!(p.total_nodes(), raw.max(deepest));
        shapes[0] += usize::from(deepest > raw);
        shapes[1] += usize::from(deepest < raw);
        shapes[2] += usize::from(p.graph.all_node_ids().any(|n| {
            let arms: Vec<usize> = p
                .graph
                .node_inputs(n)
                .into_iter()
                .map(|v| memo[&p.graph.producer(v)])
                .collect();
            p.graph.node_kind(n).is_alternation() && arms.iter().any(|&a| a != arms[0])
        }));
    }

    /// `total_nodes` is the larger of the raw count and the deepest unfolding.
    #[test]
    fn unfolded_node_count_matches_a_recursive_unfolding() {
        let cases = if cfg!(debug_assertions) {
            1_000
        } else {
            20_000
        };
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut shapes = [0usize; 3];
        for _ in 0..cases {
            random_pattern(&mut rng, &mut shapes);
        }
        assert!(
            shapes.iter().all(|&c| c >= cases / 20),
            "too few of a shape, floor {}: {shapes:?}",
            cases / 20
        );
    }

    /// A 100,000-node chain is refused over the cap, not overflowed.
    #[test]
    #[cfg_attr(debug_assertions, ignore = "release-only scale test")]
    fn a_hundred_thousand_node_chain_is_refused_not_overflowed() {
        let mut b = MatcherBuilder::new();
        let mut v = b.leaf(KindSpec::Any);
        for _ in 0..100_000 {
            v = wildcard_over(&mut b, v);
        }
        let p = b.finish();
        assert_eq!(p.total_nodes(), 100_001);
        assert!(p.root().is_err());
    }
}
