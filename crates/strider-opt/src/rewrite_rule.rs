use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{EditFunction, IRBuilder, IRViewer, IRWalker};

use strider_pattern::{
    Capture, MatchPat, Matcher, OperandWidthMismatch, Pattern, Result, Template, TemplatePat,
    instantiate_guarded, is_skip,
};

/// Builds a rule that matches `lhs` at a root and redirects the root's single
/// value output to the built `rhs`.
///
/// The closure declines with `Ok(None)` on no match, a skipped RHS, an unused
/// root, a width change, or a through-join replacement that would read the
/// root or is unavailable at its uses. It errs when built nodes would carry no
/// asm-fingerprint, when any other replacement would read the root, or when
/// the root has other than one output. Panics if either side is refused, or
/// the RHS reads a [`Capture`] the LHS does not bind.
pub fn rewrite_rule<L: MatchPat + 'static, T: TemplatePat + 'static>(lhs: L, rhs: T) -> BoxedRule {
    Rewrite::typed(lhs, rhs).boxed()
}

/// [`rewrite_rule`] over a built [`Pattern`] and [`Template`], erring where
/// that panics. Template nodes are checked against their kind's signature only.
pub fn rewrite_rule_runtime(lhs: Pattern, rhs: Template) -> Result<BoxedRule> {
    Ok(Rewrite::checked(lhs, rhs)?.boxed())
}

/// A compiled LHS -> RHS rewrite.
pub(crate) struct Rewrite {
    lhs: Pattern,
    rhs: Template,
    through_join: bool,
    /// A built-in table rule, whose replacement of another width is a bug.
    built_in: bool,
}

impl Rewrite {
    /// Refuses a refused side, or an RHS reading a capture the LHS does not
    /// bind on every match.
    fn checked(lhs: Pattern, rhs: Template) -> Result<Self> {
        check_capture_coverage(&lhs, &rhs)?;
        Ok(Self {
            through_join: lhs.may_bind_through_a_join(),
            lhs,
            rhs,
            built_in: false,
        })
    }

    /// [`Self::checked`] over typed sides.
    fn typed<L: MatchPat, T: TemplatePat>(lhs: L, rhs: T) -> Self {
        Self::checked(lhs.into_pattern(), rhs.into_template()).expect("rewrite_rule")
    }

    /// The rule closure: [`Self::apply`] behind the root-kind check.
    fn boxed(self) -> BoxedRule {
        Box::new(move |edit: &mut EditFunction<'_>, node: NodeId| {
            // Checked before `Matcher::new` / `match_at` allocate.
            if !self.lhs.root_admits_kind(edit.function().node_kind(node)) {
                return Ok(None);
            }
            self.apply(edit, node)
        })
    }

    /// Matches at `node` and, on success, redirects its value to the
    /// instantiated RHS.
    fn apply(&self, edit: &mut EditFunction<'_>, node: NodeId) -> Result<Option<ValueId>> {
        let (lhs, rhs) = (&self.lhs, &self.rhs);
        let (bindings, matched_nodes) = {
            let matcher = Matcher::new(edit.function());
            match matcher.match_at(node, lhs)? {
                Some(m) => (m.bindings().clone(), m.matched_nodes().to_vec()),
                None => return Ok(None),
            }
        };

        let (root_value, root_ty) = edit.function().single_value_output(node)?;
        if edit.value_uses(root_value).next().is_none() {
            return Ok(None);
        }

        // A footprint of exempt nodes only (an `InitialVar` or `Phi` root)
        // would leave every built node unfingerprinted, which `validate` rejects.
        let attributable = |f: &strider_ir::Function| {
            let side = f.side_tables();
            if matched_nodes
                .iter()
                .all(|&n| side.asm_fingerprint_is_empty(n))
            {
                anyhow::bail!(
                    "rewrite matched no node with an instruction address, so the \
                     nodes its replacement builds would carry none; match a node \
                     an instruction produced, or replace with a capture"
                );
            }
            Ok(())
        };

        // The build can dedup onto a live node, so the footprint is unioned in
        // only once the rewrite is accepted.
        let mut build = Unattributed {
            edit,
            built: Vec::new(),
        };
        let new_value = match instantiate_guarded(
            rhs,
            &mut build,
            &bindings,
            node,
            &matched_nodes,
            root_ty,
            attributable,
        ) {
            Ok(value) => value,
            Err(e) if is_skip(&e) => return Ok(None),
            // A cast-walking match binds captures of any width.
            Err(e) if lhs.ignores_casts() && e.is::<OperandWidthMismatch>() => return Ok(None),
            Err(e) => return Err(e),
        };
        let Unattributed { edit, built } = build;

        // Reachable through `ignore_casts` or a template pinning its own root
        // width.
        if edit.function().value_type(new_value)? != root_ty {
            debug_assert!(
                !self.built_in,
                "a built-in rule replaced a {root_ty} root with a value of another width"
            );
            return Ok(None);
        }

        // An RHS reading the root would close a cycle under `replace_value`.
        // Through a join that declines; otherwise it errs, and the walk prunes
        // at the footprint.
        if new_value != root_value {
            let f = edit.function();
            let crosses_join = self.through_join
                && matched_nodes.iter().any(|&n| {
                    matches!(
                        f.node_kind(n),
                        NodeKind::Phi | NodeKind::MemPhi | NodeKind::Region
                    )
                });
            let cycle = if crosses_join {
                // A phi breaks the cycle, and a control input is no read.
                reads(
                    f,
                    new_value,
                    root_value,
                    |n| matches!(f.node_kind(n), NodeKind::Phi | NodeKind::MemPhi),
                    |input| !f.value_kind(input).is_control(),
                )
            } else {
                reads(
                    f,
                    new_value,
                    root_value,
                    |n| matched_nodes.contains(&n),
                    |_| true,
                )
            };
            if crosses_join && (cycle || !available_at(edit, new_value, root_value)) {
                return Ok(None);
            }
            if cycle {
                anyhow::bail!(
                    "rewrite would make the replacement read the value it replaces, \
                     closing a data cycle; an RHS may not consume its own match root, \
                     and rebuilding the LHS shape reaches the same node through the \
                     dedup cache"
                );
            }
        }

        // Every built node takes the footprint's fingerprints, and so does the
        // survivor of a bare-capture RHS such as `add(x, 0) -> x`.
        let new_producer = edit.producer(new_value);
        for into in built.into_iter().chain([new_producer]) {
            for &matched in &matched_nodes {
                edit.absorb_node_fingerprint(into, matched);
            }
        }

        let changed = edit.replace_value(root_value, new_value);
        Ok(changed.then_some(new_value))
    }
}

/// The RHS build: records each built node instead of attributing it.
struct Unattributed<'e, 'g> {
    edit: &'e mut EditFunction<'g>,
    built: Vec<NodeId>,
}

impl IRViewer for Unattributed<'_, '_> {
    fn function(&self) -> &strider_ir::Function {
        self.edit.function()
    }
}

impl IRBuilder for Unattributed<'_, '_> {
    fn function_mut(&mut self) -> &mut strider_ir::Function {
        IRBuilder::function_mut(self.edit)
    }

    fn intern_int_const(
        &mut self,
        val: impl Into<strider_ir::UBig>,
        ty: strider_ir::node::ValueType,
    ) -> strider_ir::node::IntConstId {
        self.edit.intern_int_const(val, ty)
    }

    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        _contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = strider_ir::node::ValueKind>,
    {
        let node = self.edit.create_node_attributed(kind, inputs, outputs, &[]);
        self.built.push(node);
        node
    }
}

/// One entry of a [`RuleTable`].
pub(crate) enum Rule {
    Rewrite(Box<Rewrite>),
    /// A rule that decides its root itself, tried at the kinds `roots`
    /// admits.
    Fold {
        roots: fn(&NodeKind) -> bool,
        fold: fn(&mut EditFunction<'_>, NodeId) -> Result<Option<ValueId>>,
    },
}

impl Rule {
    /// [`rewrite_rule`] as a table entry.
    pub(crate) fn rewrite<L: MatchPat + 'static, T: TemplatePat + 'static>(lhs: L, rhs: T) -> Self {
        Rule::Rewrite(Box::new(Rewrite {
            built_in: true,
            ..Rewrite::typed(lhs, rhs)
        }))
    }

    /// The rule as a lone closure would run it: nothing unless the root
    /// admits `node`'s kind.
    #[cfg(test)]
    pub(crate) fn try_at(
        &self,
        edit: &mut EditFunction<'_>,
        node: NodeId,
    ) -> Result<Option<ValueId>> {
        if !self.admits(edit.node_kind(node)) {
            return Ok(None);
        }
        self.run(edit, node)
    }

    fn run(&self, edit: &mut EditFunction<'_>, node: NodeId) -> Result<Option<ValueId>> {
        match self {
            Rule::Rewrite(rewrite) => rewrite.apply(edit, node),
            Rule::Fold { fold, .. } => fold(edit, node),
        }
    }

    fn admits(&self, kind: &NodeKind) -> bool {
        match self {
            Rule::Rewrite(rewrite) => rewrite.lhs.root_admits_kind(kind),
            Rule::Fold { roots, .. } => roots(kind),
        }
    }
}

/// An ordered rule list tried first-fire-wins at a root, each kind seeing
/// only the rules whose root admits it. No rule roots at a kind naming a
/// per-function entity.
pub(crate) struct RuleTable {
    rules: Vec<Rule>,
    /// Per root kind, the indices of the rules it admits, in table order.
    by_kind: std::cell::RefCell<rustc_hash::FxHashMap<NodeKind, Box<[usize]>>>,
}

impl RuleTable {
    pub(crate) fn new(rules: Vec<Rule>) -> Self {
        debug_assert!(
            entity_samples().all(|kind| rules.iter().all(|rule| !rule.admits(&kind))),
            "a rule roots at an entity kind"
        );
        Self {
            rules,
            by_kind: std::cell::RefCell::default(),
        }
    }

    /// Whether any rule's root admits `kind`.
    pub(crate) fn admits(&self, kind: &NodeKind) -> bool {
        !names_an_entity(kind) && !self.admitted(*kind).is_empty()
    }

    /// The indices of the rules `kind` admits, in table order.
    fn admitted(&self, kind: NodeKind) -> std::cell::RefMut<'_, [usize]> {
        std::cell::RefMut::map(self.by_kind.borrow_mut(), |by_kind| {
            &mut **by_kind.entry(kind).or_insert_with(|| {
                (0..self.rules.len())
                    .filter(|&i| self.rules[i].admits(&kind))
                    .collect()
            })
        })
    }

    /// The output of the first rule to fire at `root`, whose kind
    /// [`Self::admits`], or `None` if none does.
    pub(crate) fn first_match(
        &self,
        edit: &mut EditFunction<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        let kind = *edit.node_kind(root);
        debug_assert!(!names_an_entity(&kind), "{kind:?}");
        for &i in self.admitted(kind).iter() {
            let fired = self.rules[i].run(edit, root)?;
            if fired.is_some() {
                return Ok(fired);
            }
        }
        Ok(None)
    }
}

/// One kind per variant [`names_an_entity`] accepts.
fn entity_samples() -> impl Iterator<Item = NodeKind> {
    use strider_ir::node::{InitialVnId, IntConstId, SwitchTableId};
    [
        NodeKind::IntConst(IntConstId::from_u32(0)),
        NodeKind::FloatConst(0),
        NodeKind::InitialVar(InitialVnId::from_u32(0)),
        NodeKind::Switch(SwitchTableId::from_u32(0)),
        NodeKind::Call { cc: None },
        NodeKind::CallOther { user_op_id: 0 },
    ]
    .into_iter()
}

/// Whether `kind`'s payload names a per-function entity, kept out of the
/// `by_kind` memo.
fn names_an_entity(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::IntConst(_)
            | NodeKind::FloatConst(_)
            | NodeKind::InitialVar(_)
            | NodeKind::Switch(_)
            | NodeKind::Call { .. }
            | NodeKind::CallOther { .. }
    )
}

/// Whether `from`'s cone reads `target`, walking no further past a producer
/// `stop` names, nor along an input `follow` refuses.
fn reads(
    f: &impl IRViewer,
    from: ValueId,
    target: ValueId,
    stop: impl Fn(NodeId) -> bool,
    follow: impl Fn(ValueId) -> bool,
) -> bool {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut stack = vec![from];
    while let Some(v) = stack.pop() {
        if v == target {
            return true;
        }
        if !seen.insert(v) {
            continue;
        }
        let producer = f.producer(v);
        if stop(producer) {
            continue;
        }
        stack.extend(f.node_inputs(producer).into_iter().filter(|&i| follow(i)));
    }
    false
}

/// Whether `value`'s early position dominates `replaced`'s. A `replaced` with
/// no position has no use on a path that runs.
fn available_at(edit: &EditFunction<'_>, value: ValueId, replaced: ValueId) -> bool {
    let tree = edit.control_dominator_tree();
    let at = |v| strider_ir::early_position(edit.function(), &tree, v);
    at(replaced).is_none_or(|used| at(value).is_some_and(|def| tree.dominates(def, used)))
}

/// An RHS capture the LHS does not bind on every match.
#[derive(Debug)]
pub struct UnboundCapture {
    pub capture: Capture,
    /// Bound on some alternation arms only.
    pub on_some_arms: bool,
}

impl UnboundCapture {
    /// The message, naming the capture `name`.
    pub fn message(&self, name: &str) -> String {
        if self.on_some_arms {
            format!(
                "RHS references {name} that the LHS binds only on SOME alternation \
                 arms; every arm must bind it, since only one arm fires per match"
            )
        } else {
            format!("RHS references {name} that the LHS does not bind")
        }
    }
}

impl std::fmt::Display for UnboundCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message(&format!("{:?}", self.capture)))
    }
}

impl std::error::Error for UnboundCapture {}

/// Checks both sides are unrefused and every capture the RHS references also
/// appears in the LHS, erring with an [`UnboundCapture`] where not.
fn check_capture_coverage(lhs: &Pattern, rhs: &Template) -> Result<()> {
    rhs.root()?;
    // Only one alternation arm fires, so a capture must be bound on every arm,
    // unless the RHS declared it optional, which needs only some arm.
    let lhs_caps = lhs.guaranteed_captures()?;
    let present: rustc_hash::FxHashSet<Capture> = lhs.bound_captures().collect();
    let optional: rustc_hash::FxHashSet<Capture> = rhs.optional_captures().collect();
    for capture in rhs.referenced_captures() {
        if !(lhs_caps.contains(&capture)
            || optional.contains(&capture) && present.contains(&capture))
        {
            return Err(UnboundCapture {
                capture,
                on_some_arms: present.contains(&capture),
            }
            .into());
        }
    }
    Ok(())
}

/// Applies the first matching rule at every reachable node in one walk (no
/// fixed point), returning the fire count. Refreshes the memory classes on
/// every exit, including the first rule error's.
pub fn apply_rules_count<R>(edit: &mut EditFunction<'_>, rules: &[R]) -> Result<usize>
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
{
    let applied = fire_rules(edit, rules);
    // A rule can rewire an address, staling its memory class.
    crate::refresh_memory_classes(edit);
    applied
}

fn fire_rules<R>(edit: &mut EditFunction<'_>, rules: &[R]) -> Result<usize>
where
    R: for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>,
{
    let candidates: Vec<NodeId> = edit.walk().collect();
    let mut applied: usize = 0;
    for node in candidates {
        // A fire can leave a later candidate dead, and a culled node still
        // matches an LHS that pins no input.
        if !edit.is_live(node) {
            continue;
        }
        for r in rules {
            if r(edit, node)?.is_some() {
                applied += 1;
                edit.clean();
                break;
            }
        }
    }
    Ok(applied)
}

/// A boxed rewrite rule.
pub type BoxedRule = Box<dyn for<'g> Fn(&mut EditFunction<'g>, NodeId) -> Result<Option<ValueId>>>;

#[cfg(test)]
mod tests {
    //! Every fixture builds a BUILT `Function` (entry set).

    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{EditFunction, IRBuilderExt, IRViewer, IntBinaryOp, UBig};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    use super::rewrite_rule;
    use strider_pattern::{
        Capture, CaptureExt, MatchPat, Matcher, int_add, int_const, int_const_with,
    };

    /// A capture bound through a cast is narrower than the root it would
    /// replace, so the site is declined.
    #[test]
    fn a_rewrite_may_not_replace_a_value_with_a_narrower_one() {
        narrower_replacement(false);
    }

    /// [`a_rewrite_may_not_replace_a_value_with_a_narrower_one`]'s rewrite,
    /// as a `built_in` table rule or a user one.
    fn narrower_replacement(built_in: bool) {
        use strider_ir::ExtendOp;
        use strider_pattern::{int_const as int_const_match, int_mul, var};

        let vn = reg_vn(0x1000, 8);
        let a = Capture::new();
        let bcap = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        // Register-derived, so the cast survives instead of const-folding away.
        let wide_in = b.read_variable(&vn).unwrap();
        let n = b.truncate_if_needed(wide_in, ValueType::int(32)).unwrap();
        let prod = b
            .build_int_binary_operation(n, n, IntBinaryOp::Mul, ValueType::int(32))
            .unwrap();
        let widened = b
            .extend_if_needed(prod, ValueType::int(64), ExtendOp::ZeroExtend)
            .unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(widened, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        let mut function = b.build().unwrap();

        // The `mul` sits below the zero-extend, so `a` binds at the pre-cast
        // width.
        let lhs = int_add(int_mul(var(a), var(bcap)), int_const_match(0u64))
            .into_pattern()
            .ignore_casts();
        let root = {
            let m = Matcher::new(&function);
            let hits = m.find_all(&lhs).unwrap();
            assert!(
                !hits.is_empty(),
                "the add must match through the zero-extend"
            );
            hits[0].root()
        };
        let rhs = strider_pattern::TemplatePat::into_template(var(a));
        if built_in {
            let rule = super::Rule::Rewrite(Box::new(super::Rewrite {
                built_in: true,
                ..super::Rewrite::checked(lhs, rhs).unwrap()
            }));
            let mut edit = EditFunction::new(&mut function);
            let _ = rule.try_at(&mut edit, root);
            return;
        }
        let rule = super::rewrite_rule_runtime(lhs, rhs).unwrap();

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let sum_value = edit.node_outputs(root)[0];
        assert_eq!(rule(&mut edit, root).unwrap(), None);
        assert_eq!(
            edit.value_uses(sum_value).count(),
            1,
            "the return still reads it"
        );
    }

    /// A built-in rule replacing its root at another width fails a test build.
    /// Mutation: decline it as a user rule's.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a built-in rule replaced")]
    fn a_built_in_rule_of_another_width_fails_a_test_build() {
        narrower_replacement(true);
    }

    /// `ret zext(n * n) + 0`, each node at its own address, with `n` a
    /// truncated register: the function, the `Mul` and the root `Add`.
    fn widened_square_fn() -> (strider_ir::Function, super::NodeId, super::NodeId) {
        use strider_ir::ExtendOp;
        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let wide_in = b.read_variable(&vn).unwrap();
        let n = b.truncate_if_needed(wide_in, ValueType::int(32)).unwrap();
        b.set_lift_addr(Some(0x20));
        let prod = b
            .build_int_binary_operation(n, n, IntBinaryOp::Mul, ValueType::int(32))
            .unwrap();
        b.set_lift_addr(Some(0x30));
        let widened = b
            .extend_if_needed(prod, ValueType::int(64), ExtendOp::ZeroExtend)
            .unwrap();
        b.set_lift_addr(Some(0x40));
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(widened, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0x50));
        b.build_return(Some(sum), &[]).unwrap();
        let function = b.build().unwrap();
        let (mul, add) = (function.producer(prod), function.producer(sum));
        (function, mul, add)
    }

    /// `add(mul(a, b), 0) -> mul:I32(a, b)` through the cast rebuilds the
    /// live `Mul`, so its build dedups onto it; the refused width must leave
    /// that node's fingerprint as it was. Mutation: union the footprint in at
    /// the build.
    #[test]
    fn a_refused_rewrite_leaves_the_node_its_build_reached_unattributed() {
        use strider_pattern::template::TemplateBuilder;
        use strider_pattern::{int_const as int_const_match, int_mul};

        let (mut function, mul, add) = widened_square_fn();
        let (a, bcap) = (Capture::new(), Capture::new());
        let lhs = int_add(int_mul(var(a), var(bcap)), int_const_match(0u64))
            .into_pattern()
            .ignore_casts();
        let mut tb = TemplateBuilder::new();
        let (ta, tbv) = (tb.capture(a), tb.capture(bcap));
        let mul_node = tb.node(strider_ir::node::NodeKind::IntBinaryOp(IntBinaryOp::Mul));
        tb.input(mul_node, 0, ta);
        tb.input(mul_node, 1, tbv);
        let product = tb.value_output(mul_node, 0);
        tb.set_value_ty(product, ValueType::int(32));
        let rule = super::rewrite_rule_runtime(lhs, tb.finish()).unwrap();

        let before = function.side_tables().asm_fingerprint(mul);
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let refused = rule(&mut edit, add);
        assert!(
            matches!(refused, Ok(None)),
            "an I32 Mul cannot replace an I64 Add: {refused:?}"
        );
        assert_eq!(edit.function().side_tables().asm_fingerprint(mul), before);
    }

    /// Through a cast, an untyped RHS node evaluated at the root's width
    /// over narrower captures declines as a typed one does. Mutation: err on
    /// the operand-width mismatch.
    #[test]
    fn an_inferred_width_mismatch_through_a_cast_declines() {
        use strider_pattern::{int_const as int_const_match, int_mul};

        let (mut function, _, add) = widened_square_fn();
        let (a, bcap) = (Capture::new(), Capture::new());
        let lhs = int_add(int_mul(var(a), var(bcap)), int_const_match(0u64))
            .into_pattern()
            .ignore_casts();
        let rhs = strider_pattern::TemplatePat::into_template(template::int_mul(var(a), var(bcap)));
        let rule = super::rewrite_rule_runtime(lhs, rhs).unwrap();
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let r = rule(&mut edit, add);
        assert!(matches!(r, Ok(None)), "{r:?}");
    }

    /// A root nothing reads is no site: neither a bare capture nor a build
    /// for it may attribute or create anything. Mutation: drop the use check.
    #[test]
    fn a_rewrite_of_an_unread_root_changes_nothing() {
        use strider_pattern::int_const as int_const_match;

        let (mut function, _, add) = widened_square_fn();
        let w = Capture::new();
        let lhs = || int_add(var(w), int_const_match(0u64));
        let rules = [
            rewrite_rule(lhs(), var(w)),
            rewrite_rule(lhs(), template::int_xor(var(w), int_const(0x5u128))),
        ];
        let mut edit = EditFunction::new(&mut function);
        // Not culled, so the unread root keeps its operands.
        let ret = edit
            .live_of_kind(|k| matches!(k, NodeKind::Return))
            .next()
            .unwrap();
        let slot = edit
            .node_input_id_at(ret, NodeKind::Return.input_head_len())
            .unwrap();
        let other = edit.build_int_const(7u64, ValueType::int(64)).unwrap();
        edit.update_input(slot, other);
        let zext = edit.producer(edit.node_inputs(add)[0]);
        let before = edit.function().side_tables().asm_fingerprint(zext);
        let nodes_before = edit.function().graph().all_node_ids().count();
        for rule in &rules {
            assert_eq!(rule(&mut edit, add).unwrap(), None);
        }
        assert_eq!(edit.function().side_tables().asm_fingerprint(zext), before);
        assert_eq!(edit.function().graph().all_node_ids().count(), nodes_before);
    }

    /// A match of fingerprint-exempt nodes only refuses to build; an RHS that
    /// skips or is a bare capture is no change.
    #[test]
    fn a_rewrite_of_an_exempt_only_match_builds_nothing() {
        use strider_pattern::initial_var_for;

        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).build_fn().unwrap();
        // Entry state carries no instruction address, as in a lift.
        b.set_lift_addr(None);
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let xv = b.read_variable(&vn).unwrap();
        b.set_lift_addr(Some(0x10));
        b.build_return(Some(xv), &[]).unwrap();
        let mut function = b.build().unwrap();
        let root = match_root(&function, initial_var_for(vn));
        assert!(matches!(function.node_kind(root), NodeKind::InitialVar(_)));
        assert!(function.side_tables().asm_fingerprint_is_empty(root));
        let nodes_before = function.graph().all_node_ids().count();

        let rule = rewrite_rule(initial_var_for(vn), int_const_with!([] => UBig::from(3u8)));
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let err = rule(&mut edit, root).expect_err("an unattributable build must be refused");
        assert!(
            format!("{err}").contains("instruction address"),
            "the error must say why: {err}"
        );
        assert_eq!(
            edit.function().graph().all_node_ids().count(),
            nodes_before,
            "the refusal creates no node"
        );
        drop(edit);
        strider_ir::validate::validate(&function).expect("the graph stays valid");

        // A declined RHS is no change, even one that builds before declining.
        let declined = || strider_pattern::int_const_with_fn(|_| Err(strider_pattern::skip()));
        let skips = rewrite_rule(initial_var_for(vn), declined());
        let skips_late = rewrite_rule(
            initial_var_for(vn),
            template::int_add(declined(), int_const_with!([] => UBig::ONE)),
        );
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert_eq!(skips(&mut edit, root).unwrap(), None, "a skip is no change");
        assert_eq!(
            super::apply_rules_count(&mut edit, &[skips_late]).unwrap(),
            0,
            "a skip after a built operand is no change"
        );
        assert_eq!(
            edit.function().graph().all_node_ids().count(),
            nodes_before,
            "a declined RHS creates no node"
        );
        drop(edit);

        let x = Capture::new();
        let keep = rewrite_rule(initial_var_for(vn).capture(x), var(x));
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert_eq!(
            keep(&mut edit, root).unwrap(),
            None,
            "a bare capture of the root itself is a no-op, not an error"
        );
    }

    /// An RHS node built directly on the graph by `instantiate` must still
    /// land in the cached live set.
    #[test]
    fn rewrite_rule_registers_fresh_node_in_live_set() {
        let c1 = Capture::new();
        let c2 = Capture::new();

        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let a = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let k = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(a, k, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        let mut function = b.build().unwrap();

        let add_root = {
            let m = Matcher::new(&function);
            let pat = int_add(int_const(c1), int_const(c2)).into_pattern();
            let hits = m.find_all(&pat).unwrap();
            assert!(!hits.is_empty(), "3 + 4 add must match");
            hits[0].root()
        };

        let rule = rewrite_rule(
            int_add(int_const(c1), int_const(c2)),
            int_const_with!([c1: uint, c2: uint] => c1 + c2),
        );

        // Mirror the pipeline construction path so the cached live/roots
        // state matches a real run.
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, add_root).unwrap();
        assert!(fired.is_some(), "3 + 4 fold must fire");
        let new_value = fired.unwrap();
        let new_node = edit.producer(new_value);

        assert!(
            matches!(edit.node_kind(new_node), NodeKind::IntConst(_))
                && edit.int_const_u128(new_value) == Some(7),
            "RHS built IntConst(7)"
        );
        assert!(
            edit.is_live(new_node),
            "freshly-instantiated RHS node must be registered live"
        );
        assert!(
            edit.live_of_kind(|k| matches!(k, NodeKind::IntConst(_)))
                .any(|n| n == new_node),
            "live_of_kind must surface the fresh node"
        );
        assert!(
            edit.is_root(new_node),
            "input-less fresh const must be a cached root"
        );
    }

    use strider_ir_test_utils::assert_live_matches_reachable;
    use strider_pattern::{template, var};

    fn match_root<L: MatchPat + 'static>(function: &strider_ir::Function, lhs: L) -> super::NodeId {
        let m = Matcher::new(function);
        let pat = lhs.into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "LHS must match exactly once");
        hits[0].root()
    }

    /// A one-region function at lift address `0x10` up to `(var + 1) + 2`:
    /// the builder and the `1`, `var + 1` and the outer sum.
    fn var_plus_1_plus_2() -> (strider_ir::FunctionBuilder, [super::ValueId; 3]) {
        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let ty = ValueType::int(64);
        let k1 = b.build_int_const(1u64, ty).unwrap();
        let inner = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ty)
            .unwrap();
        let outer = b
            .build_const_binop(2u64, inner, IntBinaryOp::Add, ty)
            .unwrap();
        (b, [k1, inner, outer])
    }

    /// Folding `(var + 1) + 2` to `var + 3` rewrites away the inner Add and
    /// its `IntConst(1)`; neither may linger in `live_nodes`.
    #[test]
    fn track_chain_fold_culls_dead_intermediate() {
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
        let (mut b, [k1, inner, outer]) = var_plus_1_plus_2();
        b.build_return(Some(outer), &[]).unwrap();
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let k1_node = function.producer(k1);
        let outer_node = function.producer(outer);

        let lhs = int_add(int_add(var(x), int_const(c1)), int_const(c2));
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer Add");

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        edit.clean();

        assert!(!edit.is_live(outer_node), "old outer Add culled");
        assert!(!edit.is_live(inner_node), "dead inner Add culled");
        assert!(!edit.is_live(k1_node), "dead IntConst(1) culled");
        let new_node = edit.producer(fired.unwrap());
        assert!(edit.is_live(new_node), "fresh var+3 Add is live");

        assert_live_matches_reachable(&edit);
    }

    /// AND-distribution, `((a & C1) | (b & C2)) & C3` to
    /// `(a & (C1&C3)) | (b & (C2&C3))`: every fresh node must be tracked and
    /// the old factored subtree culled.
    #[test]
    fn track_fresh_multi_node_subtree() {
        let a = reg_vn(0x1000, 8);
        let bb = reg_vn(0x1008, 8);
        let (ca, cb) = (Capture::new(), Capture::new());
        let (c1, c2, c3) = (Capture::new(), Capture::new(), Capture::new());

        let mut b = RegisterSet::new()
            .tracked(a)
            .tracked(bb)
            .arg(a)
            .arg(bb)
            .build_fn()
            .unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let av = b.read_variable(&a).unwrap();
        let bv = b.read_variable(&bb).unwrap();
        // C1 & C3 == 0 collapses a disjunct, which is what makes the rule
        // fire.
        let k1 = b.build_int_const(0xF0u64, ValueType::int(64)).unwrap();
        let k2 = b.build_int_const(0x0Cu64, ValueType::int(64)).unwrap();
        let k3 = b.build_int_const(0x0Fu64, ValueType::int(64)).unwrap();
        let a_and = b
            .build_int_binary_operation(av, k1, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        let b_and = b
            .build_int_binary_operation(bv, k2, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        let or = b
            .build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::int(64))
            .unwrap();
        let outer = b
            .build_int_binary_operation(or, k3, IntBinaryOp::And, ValueType::int(64))
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        let mut function = b.build().unwrap();

        let outer_node = function.producer(outer);
        let or_node = function.producer(or);
        let k3_node = function.producer(k3);

        use strider_pattern::{int_and, int_const, int_or as or_pat};
        let lhs = int_and(
            or_pat(
                int_and(var(ca), int_const(c1)),
                int_and(var(cb), int_const(c2)),
            ),
            int_const(c3),
        );
        let root = match_root(&function, lhs);
        assert_eq!(root, outer_node, "matched root is the outer And");

        let rule = rewrite_rule(
            int_and(
                or_pat(
                    int_and(var(ca), int_const(c1)),
                    int_and(var(cb), int_const(c2)),
                ),
                int_const(c3),
            ),
            template::int_or(
                template::int_and(var(ca), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
                template::int_and(var(cb), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
            ),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "AND-distribution must fire");
        edit.clean();

        assert!(!edit.is_live(outer_node), "old outer And culled");
        assert!(!edit.is_live(or_node), "old Or culled");
        assert!(!edit.is_live(k3_node), "old C3 const culled");
        let new_node = edit.producer(fired.unwrap());
        assert!(edit.is_live(new_node), "fresh Or is live");
        assert!(matches!(
            edit.node_kind(new_node),
            NodeKind::IntBinaryOp(IntBinaryOp::Or)
        ));

        assert_live_matches_reachable(&edit);
    }

    /// With a bare-capture RHS (`x + 0` to `x`) the old `Add` root dies and
    /// the captured `x` survives, since the Return still uses it.
    #[test]
    fn track_identity_fold_keeps_survivor() {
        let vn = reg_vn(0x1000, 8);
        let x = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let add_node_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(add_node_val), &[]).unwrap();
        let mut function = b.build().unwrap();

        let add_node = function.producer(add_node_val);
        let zero_node = function.producer(zero);
        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let lhs = int_add(var(x), int_const_match(0u64));
        let root = match_root(&function, lhs);
        assert_eq!(root, add_node, "matched root is the x+0 Add");

        let rule = rewrite_rule(int_add(var(x), int_const_match(0u64)), var(x));

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "x+0 fold must fire");
        edit.clean();

        assert!(!edit.is_live(add_node), "old Add culled");
        assert!(!edit.is_live(zero_node), "dead IntConst(0) culled");
        assert!(edit.is_live(x_node), "captured survivor x stays live");

        assert_live_matches_reachable(&edit);
    }

    /// A bare-capture identity fold carries the culled interior's
    /// asm-fingerprints onto the survivor.
    #[test]
    fn identity_fold_carries_interior_fingerprint_onto_survivor() {
        let vn = reg_vn(0x1000, 8);
        let x = Capture::new();

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        // Distinct addresses: interior zero 0xCC, root add 0xDD (which
        // replace_value absorbs either way).
        let xv = b.read_variable(&vn).unwrap();
        b.set_lift_addr(Some(0xCC));
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        b.set_lift_addr(Some(0xDD));
        let add_val = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0xEE));
        b.build_return(Some(add_val), &[]).unwrap();
        let mut function = b.build().unwrap();

        let x_node = function.producer(xv);

        use strider_pattern::int_const as int_const_match;
        let root = match_root(&function, int_add(var(x), int_const_match(0u64)));
        let rule = rewrite_rule(int_add(var(x), int_const_match(0u64)), var(x));

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        assert!(
            rule(&mut edit, root).unwrap().is_some(),
            "x+0 fold must fire"
        );
        edit.clean();

        let fp = edit.function().side_tables().asm_fingerprint(x_node);
        assert!(
            fp.contains(&0xCC),
            "survivor must absorb the culled interior const's fingerprint 0xCC: {fp:?}"
        );
        assert!(
            fp.contains(&0xDD),
            "survivor must absorb the culled root Add's fingerprint 0xDD: {fp:?}"
        );
    }

    /// When the RHS const dedup-hits an `IntConst(3)` already live in the
    /// graph, that node stays live (not double-counted) and the old root
    /// cone is still culled.
    #[test]
    fn track_dedup_hit_stays_live() {
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
        let (mut b, [_, inner, outer]) = var_plus_1_plus_2();
        // Kept live by a side-effecting Store so it survives independently
        // of the fold.
        let three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let store_addr = b.build_int_const(0x4000u64, ValueType::int(64)).unwrap();
        b.build_store(store_addr, three, rsleigh::VnSpace::RAM)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        let mut function = b.build().unwrap();

        let inner_node = function.producer(inner);
        let outer_node = function.producer(outer);
        let three_node = function.producer(three);

        let root = match_root(
            &function,
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some());
        let new_value = fired.unwrap();
        edit.clean();

        let new_add = edit.producer(new_value);
        let const_operand = edit.producer(edit.node_inputs(new_add)[1]);
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing IntConst(3)"
        );
        assert!(edit.is_live(three_node), "deduped const stays live");
        assert!(!edit.is_live(outer_node), "old outer Add culled");
        assert!(!edit.is_live(inner_node), "old inner Add culled");

        assert_live_matches_reachable(&edit);
    }

    /// Of two rules matching one node, only the first takes effect.
    #[test]
    fn apply_rules_count_keeps_the_first_fire_at_a_node() {
        let vn = reg_vn(0x1000, 8);
        let (x1, c1) = (Capture::new(), Capture::new());
        let (x2, c2) = (Capture::new(), Capture::new());

        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let k1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(xv, k1, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        let mut function = b.build().unwrap();

        // Both match `var + <const>`; only the first may take effect.
        let first = rewrite_rule(
            int_add(var(x1), int_const(c1)),
            int_const_with!([c1: uint] => c1 + UBig::from(10u8)),
        );
        let second = rewrite_rule(
            int_add(var(x2), int_const(c2)),
            int_const_with!([c2: uint] => c2 + UBig::from(20u8)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let fired = super::apply_rules_count(&mut edit, &[first, second]).unwrap();
        edit.clean();

        assert_eq!(fired, 1, "the second rule at that node redirects nothing");
        let returned = crate::test_support::return_value(edit.function().graph()).unwrap();
        assert_eq!(
            edit.function().int_const_u128(returned),
            Some(11),
            "the graph keeps the FIRST rule's result"
        );
    }

    /// `ret (x + 0) * 1` over a register `x`: the register, the sum and the
    /// product.
    fn add_then_mul_fn() -> (
        strider_ir::Function,
        strider_ir::node::ValueId,
        strider_ir::node::ValueId,
        strider_ir::node::ValueId,
    ) {
        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let xv = b.read_variable(&vn).unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(xv, zero, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let product = b
            .build_int_binary_operation(sum, one, IntBinaryOp::Mul, ValueType::int(64))
            .unwrap();
        b.build_return(Some(product), &[]).unwrap();
        (b.build().unwrap(), xv, sum, product)
    }

    /// A table hands each root kind, payload included, its own admitted
    /// rules: an `Add` seen first must not narrow what a `Mul` is offered.
    /// Mutation: key the memo by discriminant.
    #[test]
    fn rule_table_admits_per_root_kind_payload_included() {
        use super::{Rule, RuleTable};
        use strider_pattern::int_mul;
        let (x, y) = (Capture::new(), Capture::new());
        let (mut function, xv, sum, product) = add_then_mul_fn();

        let table = RuleTable::new(vec![
            Rule::rewrite(int_add(var(x), int_const(0u128)), var(x)),
            Rule::rewrite(int_mul(var(y), int_const(1u128)), var(y)),
        ]);
        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let add = edit.producer(sum);
        let mul = edit.producer(product);
        assert_eq!(table.first_match(&mut edit, add).unwrap(), Some(xv));
        assert_eq!(table.first_match(&mut edit, mul).unwrap(), Some(xv));
    }

    /// A kind naming one function's constant or variable gets no memo entry.
    /// Mutation: memoize every kind.
    #[test]
    fn rule_table_memo_stays_bounded_over_distinct_constants() {
        use super::{Rule, RuleTable};
        use cranelift_entity::EntityRef;
        use strider_ir::node::{IntConstId, NodeKind};
        let x = Capture::new();
        let table = RuleTable::new(vec![Rule::rewrite(
            int_add(var(x), int_const(0u128)),
            var(x),
        )]);
        for i in 0..1000 {
            assert!(!table.admits(&NodeKind::IntConst(IntConstId::new(i))));
            assert!(!table.admits(&NodeKind::FloatConst(i as u64)));
        }
        assert_eq!(table.by_kind.borrow().len(), 0);
    }

    /// A table refuses a rule rooted at a kind naming an entity.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a rule roots at an entity kind")]
    fn rule_table_refuses_an_entity_root() {
        use super::{Rule, RuleTable};
        let _ = RuleTable::new(vec![Rule::rewrite(int_const(0u128), int_const(1u128))]);
    }

    /// Once a rule fires, the rules after it must not even be tried.
    #[test]
    fn rule_table_stops_at_the_first_fire() {
        use super::{Rule, RuleTable};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LATER_TRIES: AtomicUsize = AtomicUsize::new(0);
        fn fires(
            edit: &mut EditFunction<'_>,
            root: strider_ir::node::NodeId,
        ) -> super::Result<Option<strider_ir::node::ValueId>> {
            Ok(Some(edit.node_outputs(root)[0]))
        }
        fn declines(
            _: &mut EditFunction<'_>,
            _: strider_ir::node::NodeId,
        ) -> super::Result<Option<strider_ir::node::ValueId>> {
            Ok(None)
        }
        fn counts(
            edit: &mut EditFunction<'_>,
            root: strider_ir::node::NodeId,
        ) -> super::Result<Option<strider_ir::node::ValueId>> {
            LATER_TRIES.fetch_add(1, Ordering::Relaxed);
            fires(edit, root)
        }

        let (mut function, _, sum, _) = add_then_mul_fn();
        let binary = |k: &strider_ir::node::NodeKind| {
            matches!(k, strider_ir::node::NodeKind::IntBinaryOp(_))
        };
        let table = RuleTable::new(vec![
            Rule::Fold {
                roots: binary,
                fold: declines,
            },
            Rule::Fold {
                roots: binary,
                fold: fires,
            },
            Rule::Fold {
                roots: binary,
                fold: counts,
            },
        ]);
        let mut edit = EditFunction::new(&mut function);
        let add = edit.producer(sum);
        assert_eq!(table.first_match(&mut edit, add).unwrap(), Some(sum));
        assert_eq!(
            LATER_TRIES.load(Ordering::Relaxed),
            0,
            "no rule may run after the first fire"
        );
    }

    /// After `apply_rules_count` plus `clean()`, no accumulated dead cone
    /// may linger in `live_nodes`.
    #[test]
    fn track_apply_rules_count_no_dead_cone() {
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
        // `((var+1)+2)` is itself an operand of a further `+ 4`, so apply
        // folds twice.
        let (mut b, [_, a1, a2]) = var_plus_1_plus_2();
        let k4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let a3 = b
            .build_int_binary_operation(a2, k4, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(a3), &[]).unwrap();
        let mut function = b.build().unwrap();

        let a1_node = function.producer(a1);
        let a2_node = function.producer(a2);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        // Each call walks once, so loop to a fixed point.
        loop {
            let fired =
                super::apply_rules_count(&mut edit, std::slice::from_ref(&rule)).unwrap() > 0;
            edit.clean();
            if !fired {
                break;
            }
        }

        assert!(!edit.is_live(a1_node), "intermediate Add a1 culled");
        assert!(!edit.is_live(a2_node), "intermediate Add a2 culled");
        assert_live_matches_reachable(&edit);
    }

    /// A multi-output template: the RHS root `Load` consumes a fresh
    /// `Store`'s memory output, and both fresh nodes must be tracked. The test
    /// threads the Store to the `Return` itself.
    #[test]
    fn track_multi_output_template_interior() {
        use super::rewrite_rule_runtime;
        use strider_ir::node::ValueType as VT;
        use strider_pattern::load;
        use strider_pattern::template::{TemplateBuilder, TemplateKind};

        // Rewrites `Load(mem, addr)` into
        // `Load(Store(InitialMemory, addr, 7), addr)`.
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x2000u64, VT::int(64)).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, VT::int(64))
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut function = b.build().unwrap();

        let load_node = function.producer(loaded);

        let addr_cap = Capture::new();
        let lhs = load()
            .addr(strider_pattern::anything().capture(addr_cap))
            .build();

        // Raw-builder RHS rooted at the Load, its single value output.
        let rhs = {
            let mut tb = TemplateBuilder::new();
            let a = tb.capture(addr_cap);
            let data = tb.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| Ok(UBig::from(7u8)))));
            tb.set_value_ty(data, VT::int(64));
            let init_mem_node = tb.node(NodeKind::InitialMemory);
            let init_mem = tb.memory_output(init_mem_node, 0);
            // Store(RAM): inputs [mem, addr, data], output [mem_out].
            let store_node = tb.node(NodeKind::Store(rsleigh::VnSpace::RAM));
            tb.input(store_node, 0, init_mem);
            tb.input(store_node, 1, a);
            tb.input(store_node, 2, data);
            let store_mem = tb.memory_output(store_node, 0);
            // Load(RAM): inputs [mem, addr], output [value].
            let load_n = tb.node(NodeKind::Load(rsleigh::VnSpace::RAM));
            let a2 = tb.capture(addr_cap);
            tb.input(load_n, 0, store_mem);
            tb.input(load_n, 1, a2);
            let out = tb.value_output(load_n, 0);
            tb.set_value_ty(out, VT::int(64));
            tb.finish()
        };

        let rule = rewrite_rule_runtime(lhs, rhs).unwrap();

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        let fired = rule(&mut edit, load_node).unwrap();
        assert!(fired.is_some(), "Load -> Load(Store) rewrite must fire");
        edit.clean();

        let new_load = edit.producer(fired.unwrap());
        assert!(matches!(edit.node_kind(new_load), NodeKind::Load(_)));
        assert!(edit.is_live(new_load), "fresh Load is live");

        let mem_in = edit.node_inputs(new_load)[NodeKind::ACCESS_MEM_SLOT];
        let store_node = edit.producer(mem_in);
        assert!(
            matches!(edit.node_kind(store_node), NodeKind::Store(_)),
            "Load's memory input is the fresh Store"
        );
        assert!(
            edit.is_live(store_node),
            "fresh interior Store is tracked live"
        );

        let init_mem_in = edit.node_inputs(store_node)[NodeKind::ACCESS_MEM_SLOT];
        let init_mem_node = edit.producer(init_mem_in);
        assert!(
            edit.is_root(init_mem_node),
            "fresh input-less InitialMemory is a cached root"
        );

        assert_live_matches_reachable(&edit);

        // The matched root's asm-fingerprint must reach every fresh interior
        // RHS node, memory and value alike, stamped at creation.
        let root_fp: Vec<u64> = edit
            .function()
            .side_tables()
            .asm_fingerprint(load_node)
            .into_iter()
            .collect();
        assert!(
            !root_fp.is_empty(),
            "fixture's matched root must carry a fingerprint"
        );
        for n in edit.live_of_kind(|k| {
            matches!(
                k,
                NodeKind::IntBinaryOp(_)
                    | NodeKind::IntConst(_)
                    | NodeKind::Store(_)
                    | NodeKind::Load(_)
            )
        }) {
            let fp = edit.function().side_tables().asm_fingerprint(n);
            assert!(
                root_fp.iter().all(|a| fp.contains(a)),
                "fresh RHS node {n:?} missing root fingerprint"
            );
        }

        // `Return` inputs are [control, mem, ..].
        let ret = edit
            .live_of_kind(|k| matches!(k, NodeKind::Return))
            .next()
            .unwrap();
        let mem_slot = edit.node_input_id_at(ret, 1).unwrap();
        let store_mem = edit.memory_output_of(store_node).unwrap();
        edit.redirect_input(mem_slot, store_mem);
        edit.clean();
        strider_ir::validate::validate(edit.function())
            .map_err(|e| e.report(edit.function()))
            .unwrap();
    }

    /// The RHS const dedup-REVIVES a node built earlier but culled as
    /// unreachable: the dedup-cache hit must re-enter `live_nodes`/`roots`.
    #[test]
    fn track_rhs_dedup_revives_culled_const() {
        let (x, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
        let (mut b, [_, _, outer]) = var_plus_1_plus_2();
        // Wired into nothing, so it starts unreachable and the initial cull
        // removes it. The fold's RHS const (1 + 2) dedups back onto it.
        let dangling_three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        let mut function = b.build().unwrap();

        let three_node = function.producer(dangling_three);
        let outer_node = function.producer(outer);

        let root = match_root(
            &function,
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
        );
        assert_eq!(root, outer_node);

        let rule = rewrite_rule(
            int_add(int_add(var(x), int_const(c1)), int_const(c2)),
            template::int_add(var(x), int_const_with!([c1: uint, c2: uint] => c1 + c2)),
        );

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();

        assert!(
            !edit.is_live(three_node),
            "dangling IntConst(3) culled as unreachable"
        );

        let fired = rule(&mut edit, root).unwrap();
        assert!(fired.is_some(), "(var+1)+2 fold must fire");
        edit.clean();

        let new_add = edit.producer(fired.unwrap());
        let const_operand = edit.producer(edit.node_inputs(new_add)[1]);
        assert_eq!(
            const_operand, three_node,
            "RHS const dedup-hit the pre-existing (culled) IntConst(3)"
        );
        assert!(
            edit.is_live(three_node),
            "dedup-revived const must be re-registered in live_nodes"
        );

        assert_live_matches_reachable(&edit);
    }

    /// `p = phi(x, b); r = p + 3; b = r + 1`. Replacing `r` by `b`, which the
    /// match bound through the phi, would make `b` read itself, though `b`
    /// sits in `r`'s own block, so the site is declined.
    #[test]
    fn a_rewrite_through_a_phi_may_not_close_a_cycle_its_back_edge_carries() {
        use strider_ir::IntCmpOp;
        use strider_pattern::{phi, var};

        let x = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(x).arg(x).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let head = b.create_region_all().unwrap();
        let exit = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_lift_addr(Some(0x10));
        b.set_region(entry);
        b.build_branch(head).unwrap();
        b.set_region(head);
        let p = b.read_variable(&x).unwrap();
        let three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let r = b
            .build_int_binary_operation(p, three, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let next = b
            .build_int_binary_operation(r, one, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.write_variable(&x, next).unwrap();
        let stop = b.build_int_const(9u64, ValueType::int(64)).unwrap();
        let done = b
            .build_int_cmp_operation(next, stop, IntCmpOp::Equal, ValueType::int(64))
            .unwrap();
        b.build_if(done, exit, head).unwrap();
        b.set_region(exit);
        b.build_return(Some(r), &[]).unwrap();
        let mut function = b.build().unwrap();
        let root = function.producer(r);

        let (init, back, c) = (Capture::new(), Capture::new(), Capture::new());
        let lhs = int_add(
            phi().phi_input(0, var(init)).phi_input(1, var(back)),
            var(c),
        );
        let rule = rewrite_rule(lhs, var(back));
        let mut edit = EditFunction::new(&mut function);
        assert_eq!(
            rule(&mut edit, root).unwrap(),
            None,
            "the back-edge value reads the root"
        );
        edit.clean();
        drop(edit);
        strider_ir::validate::validate(&function).unwrap();
    }

    /// `((v + 1) + 2) + 3` under `add(add(x, _), _) -> add(x, K)`: the fire at
    /// the outer `Add` leaves the middle one with only a dead consumer, so it
    /// is no site and is not counted.
    #[test]
    fn a_match_an_earlier_fire_left_dead_is_not_counted() {
        use super::apply_rules_count;
        use strider_ir::IRWalker;
        use strider_pattern::{anything, var};

        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let mut sum = b.read_variable(&vn).unwrap();
        for k in 1u64..=3 {
            let c = b.build_int_const(k, ValueType::int(64)).unwrap();
            sum = b
                .build_int_binary_operation(sum, c, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
        }
        b.build_return(Some(sum), &[]).unwrap();
        let mut function = b.build().unwrap();

        let x = Capture::new();
        let rule = rewrite_rule(
            int_add(int_add(var(x), anything()), anything()),
            int_add(var(x), int_const(0x77u128)),
        );
        let fired = {
            let mut edit = EditFunction::new(&mut function);
            apply_rules_count(&mut edit, std::slice::from_ref(&rule)).unwrap()
        };
        let sites = function
            .walk()
            .filter(|&n| {
                matches!(
                    function.node_kind(n),
                    NodeKind::IntBinaryOp(IntBinaryOp::Add)
                ) && function
                    .node_inputs(n)
                    .into_iter()
                    .any(|v| function.int_const_u128(v) == Some(0x77))
            })
            .count();
        assert_eq!((fired, sites), (1, 1));
        strider_ir::validate::validate(&function).unwrap();
    }

    /// `r = phi(.., r) + 1` in a loop no edge enters: `r` runs on no path, so
    /// replacing it through the phi needs no availability.
    #[test]
    fn a_rewrite_through_a_phi_in_code_that_never_runs_is_not_refused() {
        use strider_ir::IntCmpOp;
        use strider_pattern::{phi, var};

        let x = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(x).arg(x).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let dead = b.create_region_all().unwrap();
        let exit = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_lift_addr(Some(0x10));
        b.set_region(entry);
        b.build_branch(exit).unwrap();
        b.set_region(dead);
        let p = b.read_variable(&x).unwrap();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let r = b
            .build_int_binary_operation(p, one, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.write_variable(&x, r).unwrap();
        let stop = b.build_int_const(9u64, ValueType::int(64)).unwrap();
        let done = b
            .build_int_cmp_operation(r, stop, IntCmpOp::Equal, ValueType::int(64))
            .unwrap();
        b.build_if(done, exit, dead).unwrap();
        b.set_region(exit);
        let out = b.read_variable(&x).unwrap();
        b.build_return(Some(out), &[]).unwrap();
        let mut function = b.build().unwrap();
        let root = function.producer(r);
        assert!(matches!(function.kind_of_value(p), NodeKind::Phi));

        let (a, k) = (Capture::new(), Capture::new());
        let rule = rewrite_rule(int_add(phi().phi_input(0, var(a)), var(k)), var(k));
        let mut edit = EditFunction::new(&mut function);
        assert!(rule(&mut edit, root).unwrap().is_some());
        edit.clean();
        drop(edit);
        strider_ir::validate::validate(&function).unwrap();
    }

    /// `if (c) { x_i = i } ; *(0x100 + 8i) = x_i + 7` for eight registers:
    /// each store's sum matches through its phi, and the eight fires, which
    /// edit no control, share one dominator tree, a constant the RHS builds
    /// included. Mutation: build the tree per availability check; intern a
    /// built constant through `function_mut`.
    #[test]
    fn join_crossing_fires_share_one_dominator_tree() {
        use strider_pattern::{phi, var};
        let (a, k) = (Capture::new(), Capture::new());
        let lhs = || int_add(phi().phi_input(0, var(a)), var(k));
        join_crossing_fires_build_one_tree(&rewrite_rule(lhs(), var(k)));
        join_crossing_fires_build_one_tree(&rewrite_rule(
            lhs(),
            strider_pattern::template::int_add(var(k), int_const(0u128)),
        ));
    }

    fn join_crossing_fires_build_one_tree(rule: &super::BoxedRule) {
        use super::apply_rules_count;

        const K: u64 = 8;
        let flag = reg_vn(0x2000, 1);
        let regs: Vec<_> = (0..K).map(|i| reg_vn(0x1000 + 8 * i, 8)).collect();
        let mut set = RegisterSet::new().tracked(flag).arg(flag);
        for &r in &regs {
            set = set.tracked(r).arg(r);
        }
        let mut b = set.build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let then = b.create_region_all().unwrap();
        let join = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_lift_addr(Some(0x10));
        b.set_region(entry);
        let c = b.read_variable(&flag).unwrap();
        let c = b.truncate_if_needed(c, ValueType::I1).unwrap();
        b.build_if(c, then, join).unwrap();
        b.set_region(then);
        for (i, r) in regs.iter().enumerate() {
            let v = b.build_int_const(i as u64, ValueType::int(64)).unwrap();
            b.write_variable(r, v).unwrap();
        }
        b.build_branch(join).unwrap();
        b.set_region(join);
        for (i, r) in regs.iter().enumerate() {
            let x = b.read_variable(r).unwrap();
            let seven = b.build_int_const(7u64, ValueType::int(64)).unwrap();
            let sum = b
                .build_int_binary_operation(x, seven, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            let addr = b
                .build_int_const(0x100 + 8 * i as u64, ValueType::int(64))
                .unwrap();
            b.build_store(addr, sum, rsleigh::VnSpace::RAM).unwrap();
        }
        b.build_return(None, &[]).unwrap();
        let mut function = b.build().unwrap();

        let mut edit = EditFunction::new(&mut function);
        edit.cull_dead();
        let fired = apply_rules_count(&mut edit, std::slice::from_ref(rule)).unwrap();
        assert_eq!(fired, K as usize);
        assert_eq!(edit.control_dominator_builds(), 1);
    }

    /// `head: call; y = *p; x = y + 1; if (x < 9) goto head`. A match through
    /// the loop's `Region` binds the latch's `x`, which reads the root `y`
    /// with no phi between them: `y -> x` would make `x = x + 1`, so the site
    /// is declined.
    #[test]
    fn a_rewrite_through_a_region_may_not_close_a_cycle() {
        use strider_ir::IntCmpOp;
        use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86_64};
        use strider_pattern::{anything, call, if_else, int_cmp, load, region, var};

        let ptr = reg_vn(0x1000, 8);
        let mut b = sp_arg_frame(stack_vn_x86_64())
            .tracked(ptr)
            .arg(ptr)
            .build_fn()
            .unwrap();
        let entry = b.create_region_all().unwrap();
        let head = b.create_region_all().unwrap();
        let exit = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_lift_addr(Some(0x10));
        b.set_region(entry);
        b.build_branch(head).unwrap();
        b.set_region(head);
        let target = b.build_int_const(0x2000u64, ValueType::int(64)).unwrap();
        b.build_call(target, &[], &[], 0).unwrap();
        let addr = b.read_variable(&ptr).unwrap();
        let y = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
            .unwrap();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let x = b
            .build_int_binary_operation(y, one, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let nine = b.build_int_const(9u64, ValueType::int(64)).unwrap();
        let more = b
            .build_int_cmp_operation(x, nine, IntCmpOp::Sless, ValueType::int(64))
            .unwrap();
        b.build_if(more, head, exit).unwrap();
        b.set_region(exit);
        b.build_return(Some(x), &[]).unwrap();
        let mut function = b.build().unwrap();
        let root = function.producer(y);

        let c = Capture::new();
        let lhs = load().mem(call().ctrl(region().any_input(if_else().cond(int_cmp(
            IntCmpOp::Sless,
            var(c),
            anything(),
        )))));
        let rule = rewrite_rule(lhs, var(c));
        let mut edit = EditFunction::new(&mut function);
        assert_eq!(
            rule(&mut edit, root).unwrap(),
            None,
            "the latch's value reads the root"
        );
        edit.clean();
        drop(edit);
        strider_ir::validate::validate(&function).unwrap();
    }

    /// `if (c) x = g() else x = 7; return x + 1`. A match through the merge's
    /// `Phi` binds `g()`'s result, which only one arm computes, for the root
    /// `x + 1` past the merge, so the site is declined.
    #[test]
    fn a_rewrite_through_a_phi_may_not_read_one_arms_value() {
        use strider_ir_test_utils::{sp_arg_frame, stack_vn_x86_64};
        use strider_pattern::{anything, call, phi};

        let flag = reg_vn(0x1100, 1);
        let x = reg_vn(0x1200, 8);
        let mut b = sp_arg_frame(stack_vn_x86_64())
            .tracked(flag)
            .arg(flag)
            .tracked(x)
            .build_fn()
            .unwrap();
        let entry = b.create_region_all().unwrap();
        let then = b.create_region_all().unwrap();
        let other = b.create_region_all().unwrap();
        let join = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_lift_addr(Some(0x10));
        b.set_region(entry);
        let c = b.read_variable(&flag).unwrap();
        let c = b.truncate_if_needed(c, ValueType::I1).unwrap();
        b.build_if(c, then, other).unwrap();
        b.set_region(then);
        let g = b.build_int_const(0x3000u64, ValueType::int(64)).unwrap();
        let (_, outs) = b.build_call(g, &[], &[x], 0).unwrap();
        b.write_variable(&x, outs[0]).unwrap();
        b.build_branch(join).unwrap();
        b.set_region(other);
        let seven = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        b.write_variable(&x, seven).unwrap();
        b.build_branch(join).unwrap();
        b.set_region(join);
        let merged = b.read_variable(&x).unwrap();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let y = b
            .build_int_binary_operation(merged, one, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(y), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let root = function.producer(y);

        let cap = Capture::new();
        let lhs = int_add(phi().any_input(call().res().capture(cap)), anything());
        let rule = rewrite_rule(lhs, var(cap));
        let mut edit = EditFunction::new(&mut function);
        assert_eq!(
            rule(&mut edit, root).unwrap(),
            None,
            "one arm's value is unavailable"
        );
        edit.clean();
        drop(edit);
        strider_ir::validate::validate(&function).unwrap();
    }

    /// `x + one_of![x*C, x]`, with the coefficient read as an `Option`: `None`
    /// is the bare arm, which binds no constant.
    #[test]
    fn an_optional_capture_reads_whichever_arm_fired() {
        use strider_pattern::{int_mul, one_of, var};

        let (x, c1) = (Capture::new(), Capture::new());
        for (scale, want) in [(Some(3u64), 3u128), (None, 1u128)] {
            let vn = reg_vn(0x1000, 8);
            let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn().unwrap();
            let region = b.create_region_all().unwrap();
            b.set_entry_region_all(region).unwrap();
            b.set_region(region);
            b.set_lift_addr(Some(0x10));
            let v = b.read_variable(&vn).unwrap();
            let rhs = match scale {
                Some(k) => {
                    let k = b.build_int_const(k, ValueType::int(64)).unwrap();
                    b.build_int_binary_operation(v, k, IntBinaryOp::Mul, ValueType::int(64))
                        .unwrap()
                }
                None => v,
            };
            let sum = b
                .build_int_binary_operation(v, rhs, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            b.build_return(Some(sum), &[]).unwrap();
            let mut function = b.build().unwrap();
            let root = function.producer(sum);

            let rule = rewrite_rule(
                int_add(var(x), one_of![int_mul(var(x), int_const(c1)), var(x)]),
                int_const_with!([c1: uint?] => c1.unwrap_or(UBig::ONE)),
            );
            let mut edit = EditFunction::new(&mut function);
            let new = rule(&mut edit, root).unwrap().expect("the rule must fire");
            assert_eq!(edit.function().int_const_u128(new), Some(want));
            edit.clean();
            drop(edit);
            strider_ir::validate::validate(&function).unwrap();
        }
    }

    /// The relaxation is opt-in: the same partially binding capture read
    /// WITHOUT the `?` is still refused at construction.
    #[test]
    fn a_required_capture_bound_on_some_arms_is_still_refused() {
        use strider_pattern::{MatchPat, TemplatePat, int_mul, one_of, var};

        let (x, c1, absent) = (Capture::new(), Capture::new(), Capture::new());
        let lhs = || int_add(var(x), one_of![int_mul(var(x), int_const(c1)), var(x)]);
        let err = super::rewrite_rule_runtime(
            lhs().into_pattern(),
            TemplatePat::into_template(int_const_with!([c1: uint] => c1)),
        )
        .map(|_| ())
        .expect_err("a required capture must still be guaranteed");
        assert!(err.to_string().contains("only on SOME"), "{err}");

        // An optional capture no arm binds at all is still a typo, not a None.
        let err = super::rewrite_rule_runtime(
            lhs().into_pattern(),
            TemplatePat::into_template(
                int_const_with!([absent: uint?] => absent.unwrap_or(UBig::ZERO)),
            ),
        )
        .map(|_| ())
        .expect_err("an unmentioned capture must be refused even as optional");
        assert!(err.to_string().contains("does not bind"), "{err}");
    }

    /// The relaxation is per capture USE, not template-wide: one closure
    /// reading `c1` as an `Option` must not excuse a second use of `c1` that
    /// has no `None` to fall back to.
    #[test]
    fn an_optional_declaration_does_not_relax_a_required_use() {
        use strider_pattern::{
            MatchPat, TemplatePat, capture_typed, int_mul, one_of, template, var,
        };

        let (x, c1) = (Capture::new(), Capture::new());
        let lhs = || int_add(var(x), one_of![int_mul(var(x), int_const(c1)), var(x)]);
        let refused = |use_kind, rhs| {
            let err = match super::rewrite_rule_runtime(lhs().into_pattern(), rhs) {
                Ok(_) => panic!("{use_kind} must be refused"),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("only on SOME"),
                "{use_kind}: {err}"
            );
        };

        // A value vertex: `var(c1)` reuses the binding itself.
        refused(
            "a value vertex",
            TemplatePat::into_template(template::int_add(
                var(c1),
                int_const_with!([c1: uint?] => c1.unwrap_or(UBig::ONE)),
            )),
        );
        // An output type: `InheritBinding` reads `c1`'s WIDTH, with no Option.
        refused(
            "an InheritBinding output type",
            TemplatePat::into_template(capture_typed(
                c1,
                int_const_with!([c1: uint?] => c1.unwrap_or(UBig::ONE)),
            )),
        );
        // A second closure in the same template reading it without the `?`.
        refused(
            "a second closure's required read",
            TemplatePat::into_template(template::int_add(
                int_const_with!([c1: uint?] => c1.unwrap_or(UBig::ONE)),
                int_const_with!([c1: uint] => c1),
            )),
        );

        // The purely optional read stays accepted, so the three above are not
        // rejecting every template that mentions `c1`.
        assert!(
            super::rewrite_rule_runtime(
                lhs().into_pattern(),
                TemplatePat::into_template(int_const_with!([c1: uint?] => c1.unwrap_or(UBig::ONE))),
            )
            .is_ok(),
            "a capture read only as an Option must still be accepted"
        );
    }

    /// `store [p] = 5; return *p + 7` under `[load + k -> k, load -> 5]`: the
    /// first rule culls the load, and the second, which pins no input, would
    /// still match it and stamp its address onto the live `5`.
    #[test]
    fn a_candidate_an_earlier_fire_culled_is_not_tried() {
        use super::apply_rules_count;
        use strider_pattern::{load, var};

        let ptr = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(ptr).arg(ptr).build_fn().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(0x10));
        let addr = b.read_variable(&ptr).unwrap();
        let five = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        b.build_store(addr, five, rsleigh::VnSpace::RAM).unwrap();
        b.set_lift_addr(Some(0x20));
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0x30));
        let seven = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(loaded, seven, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        let mut function = b.build().unwrap();
        let five = function.producer(five);

        let k = Capture::new();
        let rules = [
            rewrite_rule(int_add(load(), var(k)), var(k)),
            rewrite_rule(load(), int_const(5u128)),
        ];
        let fired = {
            let mut edit = EditFunction::new(&mut function);
            apply_rules_count(&mut edit, &rules).unwrap()
        };
        assert_eq!(fired, 1);
        let stamped = function.side_tables().asm_fingerprint(five);
        assert!(!stamped.contains(&0x20), "{stamped:x?}");
        strider_ir::validate::validate(&function).unwrap();
    }
}
