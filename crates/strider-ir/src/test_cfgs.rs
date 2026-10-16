//! Generated control-flow functions, and their graphs as index lists, for
//! this crate's graph-algorithm tests.

use cranelift_entity::EntityRef;
use proptest::prelude::*;

use crate::builder::IRBuilderExt;
use crate::node::ValueType;
use crate::{CtrlKey, Function, FunctionBuilder};

/// One region's terminator; targets are region indices modulo the count.
#[derive(Clone, Debug)]
pub(crate) enum Term {
    Return,
    Unreachable,
    Branch(u8),
    If(u8, u8),
    Switch(Vec<u8>),
}

/// Up to `max_regions` regions and `max_arms` switch arms: self-loops,
/// duplicate targets, orphan regions, irreducible loops and branches back to
/// the entry all arise.
pub(crate) fn arb_terms(max_regions: usize, max_arms: usize) -> impl Strategy<Value = Vec<Term>> {
    let term = prop_oneof![
        1 => Just(Term::Return),
        1 => Just(Term::Unreachable),
        2 => any::<u8>().prop_map(Term::Branch),
        4 => (any::<u8>(), any::<u8>()).prop_map(|(t, f)| Term::If(t, f)),
        1 => prop::collection::vec(any::<u8>(), 1..=max_arms).prop_map(Term::Switch),
    ];
    prop::collection::vec(term, 1..=max_regions)
}

pub(crate) fn empty_builder() -> crate::error::Result<FunctionBuilder> {
    FunctionBuilder::new(
        vec![],
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
    )
}

/// The regions `terms` describe, before validation: an exit-free cycle
/// survives here.
pub(crate) fn lower(terms: &[Term]) -> Option<FunctionBuilder> {
    let mut b = empty_builder().ok()?;
    let regions: Vec<_> = (0..terms.len())
        .map(|_| b.create_region_all())
        .collect::<crate::error::Result<_>>()
        .ok()?;
    b.set_entry_region_all(regions[0]).ok()?;
    b.set_lift_addr(Some(0x5000));
    let at = |i: u8| regions[usize::from(i) % regions.len()];
    for (&region, term) in regions.iter().zip(terms) {
        b.set_region(region);
        match term {
            Term::Return => b.build_function_return().ok()?,
            Term::Unreachable => b.build_unreachable().ok()?,
            Term::Branch(t) => b.build_branch(at(*t)).ok()?,
            Term::If(t, f) => {
                let cond = b.build_boolean_const(true);
                b.build_if(cond, at(*t), at(*f)).ok()?;
            }
            Term::Switch(targets) => {
                let address = b.build_int_const(0u64, ValueType::I64).ok()?;
                let arms: Vec<_> = (0u64..).zip(targets).map(|(k, &t)| (at(t), k)).collect();
                b.build_switch(address, &arms).ok()?;
            }
        }
    }
    Some(b)
}

/// `n` regions, each branching to the next; the last returns. Unvalidated,
/// since `validate` itself walks the graph.
pub(crate) fn chain(n: usize) -> crate::error::Result<FunctionBuilder> {
    let mut b = empty_builder()?;
    let regions: Vec<_> = (0..n)
        .map(|_| b.create_region_all())
        .collect::<crate::error::Result<_>>()?;
    b.set_entry_region_all(regions[0])?;
    b.set_lift_addr(Some(0x7000));
    for pair in regions.windows(2) {
        b.set_region(pair[0]);
        b.build_branch(pair[1])?;
    }
    b.set_region(regions[n - 1]);
    b.build_function_return()?;
    Ok(b)
}

/// A `Switch` of `k` arms, each branching to one join that returns.
pub(crate) fn wide_join(k: usize) -> crate::error::Result<Function> {
    let mut b = empty_builder()?;
    let entry = b.create_region_all()?;
    let arms: Vec<_> = (0..k)
        .map(|_| b.create_region_all())
        .collect::<crate::error::Result<_>>()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(0x8000));
    b.set_region(entry);
    let address = b.build_int_const(0u64, ValueType::I64)?;
    let table: Vec<_> = (0u64..).zip(&arms).map(|(i, &r)| (r, i)).collect();
    b.build_switch(address, &table)?;
    for &arm in &arms {
        b.set_region(arm);
        b.build_branch(join)?;
    }
    b.set_region(join);
    b.build_function_return()?;
    b.build()
}

fn index(id: impl EntityRef) -> u32 {
    u32::try_from(id.index()).expect("an id fits u32")
}

fn node_slots(f: &Function) -> usize {
    f.graph()
        .all_node_ids()
        .map(|v| v.index() + 1)
        .max()
        .unwrap_or(0)
}

/// Each `Control` output to its consumers, by node index.
pub(crate) fn control_succs(f: &Function) -> Vec<Vec<u32>> {
    let g = f.graph();
    let mut succs = vec![Vec::new(); node_slots(f)];
    for v in g.all_node_ids() {
        for &out in g.node_outputs(v) {
            if g.value_kind(out).is_control() {
                succs[v.index()].extend(g.value_uses(out).map(|(user, _)| index(user)));
            }
        }
    }
    succs
}

/// The edge-split control graph by `CtrlKey` index: a node to its `Control`
/// outputs, an output to its consumers.
pub(crate) fn split_succs(f: &Function) -> Vec<Vec<u32>> {
    let g = f.graph();
    let slots = g
        .all_node_ids()
        .map(|v| CtrlKey::Node(v).index() + 1)
        .chain(g.all_value_ids().map(|x| CtrlKey::Edge(x).index() + 1))
        .max()
        .unwrap_or(0);
    let mut succs = vec![Vec::new(); slots];
    for v in g.all_node_ids() {
        for &out in g.node_outputs(v) {
            if g.value_kind(out).is_control() {
                let edge = CtrlKey::Edge(out);
                succs[CtrlKey::Node(v).index()].push(index(edge));
                succs[edge.index()] = g
                    .value_uses(out)
                    .map(|(user, _)| index(CtrlKey::Node(user)))
                    .collect();
            }
        }
    }
    succs
}

/// Every input's producer, plus control successors: what `walk_graph` follows.
pub(crate) fn walk_succs(f: &Function) -> Vec<Vec<u32>> {
    let g = f.graph();
    let mut succs = control_succs(f);
    for v in g.all_node_ids() {
        succs[v.index()].extend(
            g.node_inputs(v)
                .into_iter()
                .map(|x| index(g.value_definition(x).0)),
        );
    }
    succs
}

/// Each output to its consumers, control included.
pub(crate) fn def_use_succs(f: &Function) -> Vec<Vec<u32>> {
    let g = f.graph();
    let mut succs = vec![Vec::new(); node_slots(f)];
    for v in g.all_node_ids() {
        for &out in g.node_outputs(v) {
            succs[v.index()].extend(g.value_uses(out).map(|(user, _)| index(user)));
        }
    }
    succs
}
