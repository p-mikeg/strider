//! The graph both builders write into. Ids run producers first: an input wired from a later
//! node is refused, a template capture aside.

use strider_graph::{Graph, NeverCacheable, NodeId, ValueId};

/// A graph under construction, its first refusal and its nesting budget.
pub(crate) struct BuildGraph<N, V> {
    pub(crate) graph: Graph<N, V, NeverCacheable>,
    /// The first build-time refusal, surfaced as an `Err`, never a panic.
    pub(crate) refusal: Option<String>,
    /// Nesting levels being lowered; see [`Self::enter_nesting`].
    levels: usize,
    /// Recursion steps being lowered, levels or not.
    depth: usize,
    /// Frame address of the outermost live step, the base stack use is measured from.
    stack_base: usize,
}

/// Stack one lowering may consume before [`BuildGraph::enter_nesting`] bails, since a level
/// count alone does not bound stack. Below a spawned thread's default 2 MiB; a normal pattern
/// hits [`MAX_PATTERN_NODES`](crate::matcher::graph::MAX_PATTERN_NODES) levels first.
const MAX_PATTERN_STACK: usize = 1024 * 1024;

/// Address of a local in the calling frame. Assumes a downward-growing stack.
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u8;
    std::hint::black_box(std::ptr::from_ref(&probe)) as usize
}

impl<N, V> Default for BuildGraph<N, V> {
    fn default() -> Self {
        Self {
            graph: Graph::new(),
            refusal: None,
            levels: 0,
            depth: 0,
            stack_base: 0,
        }
    }
}

impl<N, V> BuildGraph<N, V> {
    /// Opens one recursion step of lowering, refusing once the stack budget is spent or, for a
    /// `level`, past `MAX_PATTERN_NODES` levels. Lowering recurses before nodes exist, so the
    /// node cap at finish would come too late.
    pub(crate) fn enter_nesting(&mut self, level: bool) -> bool {
        let here = frame_mark();
        let max = crate::matcher::graph::MAX_PATTERN_NODES;
        if level && self.levels >= max {
            self.reject(format!(
                "pattern nests more than {max} levels deep, over what lowering can \
                 recurse through"
            ));
            return false;
        }
        if self.depth == 0 {
            self.stack_base = here;
        } else {
            let used = self.stack_base.saturating_sub(here);
            if used > MAX_PATTERN_STACK {
                self.reject(format!(
                    "a pattern or template nests too deep for this thread's stack ({} KiB \
                     used of a {} KiB lowering budget, {} steps down); nest less, or lower \
                     it on a thread with a larger stack",
                    used / 1024,
                    MAX_PATTERN_STACK / 1024,
                    self.depth,
                ));
                return false;
            }
        }
        self.depth += 1;
        self.levels += usize::from(level);
        true
    }

    pub(crate) fn leave_nesting(&mut self, level: bool) {
        self.depth -= 1;
        self.levels -= usize::from(level);
    }

    pub(crate) fn add_node(&mut self, kind: N) -> NodeId {
        self.graph.create_node(kind, [], [])
    }

    pub(crate) fn add_output(&mut self, node: NodeId, out: V) -> ValueId {
        self.graph.add_node_output(node, out)
    }

    /// `false`, refused, when `prod` does not come from a node built before `consumer` (the only
    /// way to form a cycle), unless `leaf` says its producer takes no input.
    pub(crate) fn add_input(&mut self, consumer: NodeId, prod: ValueId, leaf: bool) -> bool {
        if !leaf && self.graph.producer(prod).as_u32() >= consumer.as_u32() {
            self.reject(
                "an input is wired from a node built after its consumer; build each \
                 operand before the node consuming it"
                    .to_string(),
            );
            return false;
        }
        self.graph.add_node_input(consumer, prod);
        true
    }

    /// Keeps the first refusal; later ones are dropped.
    pub(crate) fn reject(&mut self, why: String) {
        self.refusal.get_or_insert(why);
    }
}
