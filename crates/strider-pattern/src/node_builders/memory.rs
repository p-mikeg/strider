//! Slot conventions, per the IR `expected_signature`:
//!
//! * `Load` inputs `[mem(0), addr(1)]`, output the loaded value at slot 0.
//! * `Store` inputs `[mem(0), addr(1), data(2)]`, output the new memory token
//!   at slot 0.
//!
//! `Load` is value-producing and nests as a value operand; `Store` is a
//! memory-token root exposing its token via [`MemPat`].

use strider_ir::node::{NodeId, NodeKind};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};

use super::MemPat;
use super::node_pat::{KindCheck, NodePat, variant_kind};

/// SP-relative match state shared by `LoadPat` and `StorePat`.
#[derive(Default)]
struct StackAccessSpec {
    stack_offset_filter: Option<i128>,
    /// When `true`, rejects matches where `Function::stack_offset` is `None`.
    stack_only: bool,
}

impl StackAccessSpec {
    fn active(&self) -> bool {
        self.stack_offset_filter.is_some() || self.stack_only
    }

    fn check(&self, function: &strider_ir::Function, node: NodeId) -> bool {
        if !self.active() {
            return true;
        }
        let Some((_base, offset)) = function.stack_offset(node) else {
            return false;
        };
        if let Some(k) = self.stack_offset_filter
            && offset != k
        {
            return false;
        }
        true
    }

    /// A no-op when inactive.
    fn apply(self, n: NodePat) -> NodePat {
        if !self.active() {
            return n;
        }
        n.with_node_predicate(move || {
            Box::new(move |matcher, node| self.check(matcher.function(), node))
        })
    }
}

/// Variant-agnostic without a space constraint; with one, pins the exact
/// `VnSpace`.
fn load_store_kind(exemplar: NodeKind, space: Option<rsleigh::VnSpace>) -> KindSpec {
    let discriminant = std::mem::discriminant(&exemplar);
    let is_load = matches!(exemplar, NodeKind::Load(_));
    let check = space.map(|s| {
        let check: KindCheck = Box::new(move |k| {
            matches!((is_load, k),
                (true, NodeKind::Load(actual)) | (false, NodeKind::Store(actual))
                    if *actual == s)
        });
        check
    });
    variant_kind(discriminant, check)
}

/// What `LoadPat` and `StorePat` share verbatim, embedded as `common`.
#[derive(Default)]
struct MemAccessSpec {
    space: Option<rsleigh::VnSpace>,
    mem_in: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    /// Applied in order; each call adds a separate constraint.
    any_input: Vec<Box<dyn FnOnce(NodePat) -> NodePat>>,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
    capture: Option<Capture>,
}

impl MemAccessSpec {
    fn wire_mem_and_capture(&mut self, mut n: NodePat) -> NodePat {
        if let Some(m) = self.mem_in.take() {
            n = m(n);
        }
        for f in self.any_input.drain(..) {
            n = f(n);
        }
        if let Some(c) = self.capture {
            n = n.capture(c);
        }
        n
    }

    fn apply_stack(self, n: NodePat) -> NodePat {
        self.stack.apply(n)
    }
}

/// Inputs `[mem(0), addr(1)]`, single output the loaded value.
#[derive(Default)]
pub struct LoadPat {
    common: MemAccessSpec,
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
}

impl LoadPat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.common.space = Some(s);
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.common.mem_in = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
        self
    }

    /// Candidates are mem and addr. A typed value sub binds only addr;
    /// `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.common
            .any_input
            .push(Box::new(move |n: NodePat| n.input_any(p)));
        self
    }

    /// Pins the value output's width.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.common.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.common.stack.stack_offset_filter = Some(k);
        self
    }

    /// Rejects matches where `Function::stack_offset(node)` is `None`.
    pub fn stack_only(mut self) -> Self {
        self.common.stack.stack_only = true;
        self
    }

    /// Binds the value output.
    pub fn capture(mut self, c: Capture) -> Self {
        self.common.capture = Some(c);
        self
    }

    fn configured(self) -> NodePat {
        let LoadPat { mut common, addr } = self;
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        // The loaded value lives at output slot 0.
        let mut n = NodePat::value(load_store_kind(exemplar, common.space), 0);
        n = common.wire_mem_and_capture(n);
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(w) = common.bit_width {
            // The loaded value IS the anchor output, so pin it there.
            n = n.with_output_width(w);
        }
        common.apply_stack(n)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for LoadPat {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

pub fn load() -> LoadPat {
    LoadPat::default()
}

/// Inputs `[mem(0), addr(1), data(2)]`, single output the new memory token.
#[derive(Default)]
pub struct StorePat {
    common: MemAccessSpec,
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    data: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
}

impl StorePat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.common.space = Some(s);
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// The stored value, `inputs[2]`.
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.data = Some(Box::new(move |n: NodePat| n.input(2, p)));
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.common.mem_in = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
        self
    }

    /// Candidates are mem, addr and data. A typed value sub binds only addr
    /// or data; `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.common
            .any_input
            .push(Box::new(move |n: NodePat| n.input_any(p)));
        self
    }

    /// Pins the width of the data input, `inputs[2]`.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.common.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.common.stack.stack_offset_filter = Some(k);
        self
    }

    /// Rejects matches where `Function::stack_offset(node)` is `None`.
    pub fn stack_only(mut self) -> Self {
        self.common.stack.stack_only = true;
        self
    }

    pub fn capture(mut self, c: Capture) -> Self {
        self.common.capture = Some(c);
        self
    }

    fn configured(self) -> NodePat {
        let StorePat {
            mut common,
            addr,
            data,
        } = self;
        let exemplar = NodeKind::Store(rsleigh::VnSpace::RAM);
        // The new memory token lives at output slot 0.
        let mut n = NodePat::node(load_store_kind(exemplar, common.space)).with_mem_value(0);
        n = common.wire_mem_and_capture(n);
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(d) = data {
            n = d(n);
        } else if common.bit_width.is_some() {
            // A width constraint needs SOME wired producer output at the data
            // slot to pin itself on.
            n = n.input(2, crate::typed::wildcards::any());
        }
        if let Some(w) = common.bit_width {
            // The stored value is an input, not an output, so the width pins
            // on that input's producer output.
            n = n.with_input_width(2, w);
        }
        common.apply_stack(n)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MemPat for StorePat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

pub fn store() -> StorePat {
    StorePat::default()
}
