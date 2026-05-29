//! Shared one-step forward consumer walks for Load / Store / CallOther
//! pattern builders.  All three builders' `.next_mem(p)` /
//! `.next_ctrl(p)` methods route through these.

use strider_ir::node::NodeId;

use crate::pattern::matcher::Bindings;
use crate::pattern::matcher::consumer::next_unique_consumer;
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::match_consumer_node;
use crate::pattern::pat::traits::MatchCtx;

/// Match `pat` against the unique consumer of `node`'s output at
/// `output_index`.  Returns `false` if the output has zero or
/// multiple consumers (deterministic no-match), or if `pat` doesn't
/// match the consumer node.
///
/// [`next_unique_consumer`] is generic — it returns the unique consumer
/// of any output kind.  We reuse it for control AND memory walks.
pub(crate) fn match_unique_output_consumer(
    ctx: &MatchCtx,
    node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.function.node_outputs(node);
    let Some(&out) = outputs.get(output_index) else {
        return false;
    };
    let Some(consumer) = next_unique_consumer(ctx.matcher, out) else {
        return false;
    };
    match_consumer_node(ctx, consumer, pat, b)
}
