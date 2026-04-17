/// Parsed AST and code-generation for a `rewrite_rules!` rule.
///
/// Grammar subset for the spike (three rules):
/// ```text
/// Rules    := Rule (',' Rule)* ','?
/// Rule     := LhsPat '=>' RhsExpr
/// LhsPat   := '(' LhsPat BinOpSym LhsPat ')'
///           | 'Extend' '::' '<' ExtendKind '>' '(' LhsPat ')'
///           | 'IntConst' '(' IntConstPat ')'
///           | Ident
/// IntConstPat := IntLit | Ident ':' Ident | Ident
/// RhsExpr  := 'int_const' '(' RhsValExpr ',' Ident ')'
///           | RhsAtom ('&' RhsAtom)*
/// ```
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    Error, Ident, LitInt, Result, Token,
    parse::{Parse, ParseStream},
};

// ── Public entry points ───────────────────────────────────────────────────────

pub(super) struct Rules {
    pub rules: Vec<Rule>,
}

impl Parse for Rules {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut rules = Vec::new();
        while !input.is_empty() {
            rules.push(input.parse::<Rule>()?);
            if input.is_empty() { break; }
            input.parse::<Token![,]>()?;
        }
        Ok(Rules { rules })
    }
}

pub(super) struct Rule {
    pub lhs: LhsPat,
    pub rhs: RhsExpr,
}

impl Parse for Rule {
    fn parse(input: ParseStream) -> Result<Self> {
        let lhs = input.parse::<LhsPat>()?;
        input.parse::<Token![=>]>()?;
        let rhs = input.parse::<RhsExpr>()?;
        Ok(Rule { lhs, rhs })
    }
}

impl Rule {
    pub fn codegen(&self, name: &proc_macro2::Ident) -> Result<TokenStream> {
        let mut caps = CaptureEnv::new();
        // collect captures from LHS
        self.lhs.collect_captures(&mut caps);
        // declare option captures for loop
        let cap_decls = caps.decl_options();
        // emit LHS check (returns None on fail, sets options on success)
        let lhs_check = self.lhs.emit_check(&caps)?;
        // unwrap captures after check
        let cap_unwraps = caps.unwrap_options();
        // emit RHS expression
        let rhs_ts = self.rhs.emit(&caps)?;

        Ok(quote! {
            #[allow(
                non_snake_case,
                unused_variables,
                clippy::needless_pass_by_ref_mut,
                clippy::type_complexity,
            )]
            fn #name(
                fg: &mut ir::BuiltFunctionGraph,
                node: ir::node::NodeId,
            ) -> ::core::result::Result<opt::OptimizationResult, opt::Error> {
                // `ty` is the output type of the matched root node; available in RHS expressions.
                #[allow(unused_variables)]
                let ty: ir::node::NodeOutputType = fg.graph
                    .output_kind(fg.graph.node_outputs(node)[0])
                    .as_value()
                    .unwrap_or(ir::node::NodeOutputType::U64);

                #cap_decls
                #lhs_check
                #cap_unwraps

                let [__root_out] = fg.graph.node_outputs_exact::<1>(node)?;
                let __new_out: ir::node::NodeOutputId = #rhs_ts;
                let __changed = fg.replace_all_uses(__root_out, __new_out)?;
                ::core::result::Result::Ok(opt::OptimizationResult::from_changed(__changed))
            }
        })
    }
}

// ── Capture environment ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) enum CaptureKind {
    /// Bound to `NodeOutputId`.
    Output,
    /// Bound to `u64`.
    IntConst,
    /// Bound to `NodeOutputType`.
    InputType,
}

pub(super) struct CaptureEnv {
    pub bindings: Vec<(Ident, CaptureKind)>,
}

impl CaptureEnv {
    pub fn new() -> Self { CaptureEnv { bindings: Vec::new() } }

    pub fn bind(&mut self, ident: Ident, kind: CaptureKind) {
        if !self.bindings.iter().any(|(id, _)| id == &ident) {
            self.bindings.push((ident, kind));
        }
    }

    pub fn kind_of(&self, name: &Ident) -> Option<CaptureKind> {
        self.bindings.iter().find(|(id, _)| id == name).map(|(_, k)| *k)
    }

    /// Emit `let mut __cap_X: Option<T> = None;` for each capture.
    pub fn decl_options(&self) -> TokenStream {
        self.bindings.iter().map(|(id, kind)| {
            let opt_name = opt_ident(id);
            let ty = kind_type(*kind);
            quote! { let mut #opt_name: ::core::option::Option<#ty> = None; }
        }).collect()
    }

    /// Emit `let X = __cap_X.unwrap();` for each capture.
    pub fn unwrap_options(&self) -> TokenStream {
        self.bindings.iter().map(|(id, _)| {
            let opt_name = opt_ident(id);
            quote! {
                #[allow(clippy::unwrap_used)]
                let #id = #opt_name.unwrap();
            }
        }).collect()
    }
}

fn opt_ident(id: &Ident) -> Ident {
    Ident::new(&format!("__cap_{}", id), Span::call_site())
}

fn kind_type(kind: CaptureKind) -> TokenStream {
    match kind {
        CaptureKind::Output    => quote! { ir::node::NodeOutputId },
        CaptureKind::IntConst  => quote! { u64 },
        CaptureKind::InputType => quote! { ir::node::NodeOutputType },
    }
}

// ── LHS AST ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) enum IntBinOpKind {
    Add, Sub, Mul, Div, And, Or, Xor, Shl, Shr,
}

impl IntBinOpKind {
    pub fn is_commutative(self) -> bool {
        matches!(self, Self::Add | Self::Mul | Self::And | Self::Or | Self::Xor)
    }
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::Add => "Add",  Self::Sub => "Sub",  Self::Mul => "Mul",
            Self::Div => "Div",  Self::And => "And",  Self::Or  => "Or",
            Self::Xor => "Xor",  Self::Shl => "ShiftLeft", Self::Shr => "ShiftRight",
        }, Span::call_site())
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ExtendKind { SignExtend, ZeroExtend }

impl ExtendKind {
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::SignExtend => "SignExtend",
            Self::ZeroExtend => "ZeroExtend",
        }, Span::call_site())
    }
}

#[derive(Clone)]
pub(super) enum LhsPat {
    OutputCapture(Ident),
    IntConstLiteral { value: u64 },
    IntConstCapture { name: Ident },
    IntConstCaptureWithType { value_name: Ident, type_name: Ident },
    IntBinaryOp { op: IntBinOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
    ExtendOp { kind: ExtendKind, inner: Box<LhsPat> },
}

impl Parse for LhsPat {
    fn parse(input: ParseStream) -> Result<Self> { parse_lhs(input) }
}

/// Parse an LhsPat, including optional `: type_name` suffix.
///
/// The `: type_name` suffix is written after a sub-pattern at the same level,
/// e.g. `IntConst(v) : in_ty` where `: in_ty` is parsed here, not inside
/// `IntConst(...)`.  It is only valid on `IntConst(capture)` patterns.
fn parse_lhs(input: ParseStream) -> Result<LhsPat> {
    let pat = parse_lhs_inner(input)?;

    // Check for `: type_name` suffix (input-type binding).
    if input.peek(Token![:]) {
        input.parse::<Token![:]>()?;
        let type_name: Ident = input.parse()?;
        // Only valid on IntConstCapture.
        return match pat {
            LhsPat::IntConstCapture { name } => {
                Ok(LhsPat::IntConstCaptureWithType { value_name: name, type_name })
            }
            _other => Err(Error::new(
                type_name.span(),
                format!(
                    "`: {type_name}` type annotation is only valid on an `IntConst(capture)` pattern"
                ),
            )),
        };
    }

    Ok(pat)
}

fn parse_lhs_inner(input: ParseStream) -> Result<LhsPat> {
    if input.peek(syn::token::Paren) { return parse_grouped(input); }
    let ident: Ident = input.parse()?;
    if ident == "IntConst" { return parse_int_const(input); }
    if ident == "Extend" {
        input.parse::<Token![::]>()?;
        input.parse::<Token![<]>()?;
        let k: Ident = input.parse()?;
        input.parse::<Token![>]>()?;
        let kind = match k.to_string().as_str() {
            "SignExtend" => ExtendKind::SignExtend,
            "ZeroExtend" => ExtendKind::ZeroExtend,
            other => return Err(Error::new(k.span(),
                format!("unknown Extend kind `{other}`; expected SignExtend or ZeroExtend"))),
        };
        let content; syn::parenthesized!(content in input);
        // Parse the inner pattern, which may include `: type_name` suffix.
        let inner = parse_lhs(&content)?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens inside Extend(...)"));
        }
        return Ok(LhsPat::ExtendOp { kind, inner: Box::new(inner) });
    }
    Ok(LhsPat::OutputCapture(ident))
}

fn parse_int_const(input: ParseStream) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    if content.peek(LitInt) {
        let lit: LitInt = content.parse()?;
        let value: u64 = lit.base10_parse()?;
        return Ok(LhsPat::IntConstLiteral { value });
    }
    let name: Ident = content.parse()?;
    // Note: `: type_name` is NOT parsed here — it's at the outer level.
    // (The `content` stream is the inside of `IntConst(...)` parentheses.)
    Ok(LhsPat::IntConstCapture { name })
}

fn parse_grouped(input: ParseStream) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    let lhs = parse_lhs(&content)?;
    let op = parse_binop(&content)?;
    let rhs = parse_lhs(&content)?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside grouped LHS pattern"));
    }
    Ok(LhsPat::IntBinaryOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
}

fn parse_binop(input: ParseStream) -> Result<IntBinOpKind> {
    if input.peek(Token![+])  { input.parse::<Token![+]>()?;  return Ok(IntBinOpKind::Add); }
    if input.peek(Token![-])  { input.parse::<Token![-]>()?;  return Ok(IntBinOpKind::Sub); }
    if input.peek(Token![*])  { input.parse::<Token![*]>()?;  return Ok(IntBinOpKind::Mul); }
    if input.peek(Token![/])  { input.parse::<Token![/]>()?;  return Ok(IntBinOpKind::Div); }
    if input.peek(Token![&])  { input.parse::<Token![&]>()?;  return Ok(IntBinOpKind::And); }
    if input.peek(Token![|])  { input.parse::<Token![|]>()?;  return Ok(IntBinOpKind::Or);  }
    if input.peek(Token![^])  { input.parse::<Token![^]>()?;  return Ok(IntBinOpKind::Xor); }
    if input.peek(Token![<<]) { input.parse::<Token![<<]>()?; return Ok(IntBinOpKind::Shl); }
    if input.peek(Token![>>]) { input.parse::<Token![>>]>()?; return Ok(IntBinOpKind::Shr); }
    Err(input.error("expected binary operator (+, -, *, /, &, |, ^, <<, >>)"))
}

impl LhsPat {
    /// Walk the pattern tree and register all captures into `caps`.
    pub fn collect_captures(&self, caps: &mut CaptureEnv) {
        match self {
            LhsPat::OutputCapture(name) => caps.bind(name.clone(), CaptureKind::Output),
            LhsPat::IntConstLiteral { .. } => {}
            LhsPat::IntConstCapture { name } => caps.bind(name.clone(), CaptureKind::IntConst),
            LhsPat::IntConstCaptureWithType { value_name, type_name } => {
                caps.bind(value_name.clone(), CaptureKind::IntConst);
                caps.bind(type_name.clone(), CaptureKind::InputType);
            }
            LhsPat::IntBinaryOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::ExtendOp { inner, .. } => inner.collect_captures(caps),
        }
    }

    /// Emit LHS matching code directly into the rule function body.
    ///
    /// On mismatch, emits `return Ok(OptimizationResult::NoChange)`.
    /// On match, all `__cap_X` options are set and control falls through.
    pub fn emit_check(&self, _caps: &CaptureEnv) -> Result<TokenStream> {
        let no_change = quote! {
            return ::core::result::Result::Ok(opt::OptimizationResult::NoChange);
        };
        match self {
            LhsPat::IntBinaryOp { op, lhs, rhs } if op.is_commutative() => {
                let variant = op.variant_ident();

                // Collect all captures from lhs/rhs for reset-between-orderings.
                let mut inner_caps = CaptureEnv::new();
                lhs.collect_captures(&mut inner_caps);
                rhs.collect_captures(&mut inner_caps);
                let resets: TokenStream = inner_caps.bindings.iter().map(|(id, _)| {
                    let opt = opt_ident(id);
                    quote! { #opt = None; }
                }).collect();

                // Inside the loop we `continue 'orderings` on mismatch.
                let lhs_body = lhs.emit_sub(&quote! { __ord_l }, &quote! { continue 'orderings; })?;
                let rhs_body = rhs.emit_sub(&quote! { __ord_r }, &quote! { continue 'orderings; })?;

                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::IntBinaryOp;
                        let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    let mut __found_ordering = false;
                    'orderings: for (__ord_l, __ord_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                        #resets
                        #lhs_body
                        #rhs_body
                        __found_ordering = true;
                        break 'orderings;
                    }
                    if !__found_ordering { #no_change }
                })
            }

            LhsPat::IntBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let lhs_body = lhs.emit_sub(&quote! { __root_in0 }, &no_change)?;
                let rhs_body = rhs.emit_sub(&quote! { __root_in1 }, &no_change)?;
                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::IntBinaryOp;
                        let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    #lhs_body
                    #rhs_body
                })
            }

            LhsPat::ExtendOp { kind, inner } => {
                let variant = kind.variant_ident();
                let inner_body = inner.emit_sub(&quote! { __ext_inner }, &no_change)?;
                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::ExtendOp;
                        let NodeKind::Extend(ExtendOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__ext_inner] = match fg.graph.node_inputs_exact::<1>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    #inner_body
                })
            }

            other => {
                // Simple root pattern: get the node's output and delegate.
                let no_change_ref = &no_change;
                let sub = other.emit_sub(&quote! { __root_val }, no_change_ref)?;
                Ok(quote! {
                    let __root_val = fg.graph.node_outputs(node)[0];
                    #sub
                })
            }
        }
    }

    /// Emit match code for this sub-pattern where `val_ts` is a `NodeOutputId` expression.
    ///
    /// `fail_ts` is the token stream to emit on mismatch (e.g. `return Ok(NoChange);`
    /// or `continue 'orderings;`).
    fn emit_sub(&self, val_ts: &TokenStream, fail_ts: &TokenStream) -> Result<TokenStream> {
        match self {
            LhsPat::OutputCapture(name) => {
                let opt = opt_ident(name);
                Ok(quote! { #opt = Some(#val_ts); })
            }

            LhsPat::IntConstLiteral { value } => Ok(quote! {
                {
                    let Some(__cv) = fg.int_const_val(#val_ts) else { #fail_ts };
                    if __cv != #value { #fail_ts }
                }
            }),

            LhsPat::IntConstCapture { name } => {
                let opt = opt_ident(name);
                Ok(quote! {
                    {
                        let Some(__cv) = fg.int_const_val(#val_ts) else { #fail_ts };
                        #opt = Some(__cv);
                    }
                })
            }

            LhsPat::IntConstCaptureWithType { value_name, type_name } => {
                let opt_v = opt_ident(value_name);
                let opt_t = opt_ident(type_name);
                Ok(quote! {
                    {
                        let Some(__cv) = fg.int_const_val(#val_ts) else { #fail_ts };
                        let Some(__ct) = fg.graph.output_kind(#val_ts).as_value() else { #fail_ts };
                        #opt_v = Some(__cv);
                        #opt_t = Some(__ct);
                    }
                })
            }

            LhsPat::IntBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let node_tmp = Ident::new("__sub_node", Span::call_site());

                if op.is_commutative() {
                    // Nested commutative: inner loop with `continue '__nested_ord`.
                    let mut sub_caps = CaptureEnv::new();
                    lhs.collect_captures(&mut sub_caps);
                    rhs.collect_captures(&mut sub_caps);
                    let resets: TokenStream = sub_caps.bindings.iter().map(|(id, _)| {
                        let opt = opt_ident(id);
                        quote! { #opt = None; }
                    }).collect();

                    let lhs_body = lhs.emit_sub(&quote! { __nested_l }, &quote! { continue '__nested_ord; })?;
                    let rhs_body = rhs.emit_sub(&quote! { __nested_r }, &quote! { continue '__nested_ord; })?;

                    Ok(quote! {
                        let #node_tmp = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntBinaryOp;
                            let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(#node_tmp) else { #fail_ts };
                        }
                        let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(#node_tmp) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        {
                            let mut __nested_found = false;
                            '__nested_ord: for (__nested_l, __nested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                                #resets
                                #lhs_body
                                #rhs_body
                                __nested_found = true;
                                break '__nested_ord;
                            }
                            if !__nested_found { #fail_ts }
                        }
                    })
                } else {
                    let lhs_body = lhs.emit_sub(&quote! { __sub_lhs }, fail_ts)?;
                    let rhs_body = rhs.emit_sub(&quote! { __sub_rhs }, fail_ts)?;
                    Ok(quote! {
                        let #node_tmp = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntBinaryOp;
                            let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(#node_tmp) else { #fail_ts };
                        }
                        let [__sub_lhs, __sub_rhs] = match fg.graph.node_inputs_exact::<2>(#node_tmp) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        #lhs_body
                        #rhs_body
                    })
                }
            }

            LhsPat::ExtendOp { kind, inner } => {
                let variant = kind.variant_ident();
                let node_tmp = Ident::new("__sub_ext_node", Span::call_site());
                let inner_body = inner.emit_sub(&quote! { __sub_ext_inner }, fail_ts)?;
                Ok(quote! {
                    let #node_tmp = fg.graph.get_node_from_output(#val_ts);
                    {
                        use ir::node::NodeKind;
                        use ir::ExtendOp;
                        let NodeKind::Extend(ExtendOp::#variant) = *fg.graph.node_kind(#node_tmp) else { #fail_ts };
                    }
                    let [__sub_ext_inner] = match fg.graph.node_inputs_exact::<1>(#node_tmp) {
                        Ok(v) => v,
                        Err(_) => { #fail_ts }
                    };
                    #inner_body
                })
            }
        }
    }
}

// ── RHS AST ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(super) enum RhsExpr {
    /// `x` — a captured NodeOutputId (or u64, context-dependent).
    Ident(Ident),
    /// `int_const(<val_expr>, <ty>)` — builds a new IntConst node.
    IntConstBuilder { val_expr: RhsValExpr, ty_ident: Ident },
    /// `lhs & rhs` — creates a new IntBinaryOp(And) node.
    BinOp { op: IntBinOpKind, lhs: Box<RhsExpr>, rhs: Box<RhsExpr> },
}

/// An expression that always evaluates to a plain Rust value (not a NodeOutputId).
#[derive(Clone)]
pub(super) enum RhsValExpr {
    Ident(Ident),
    /// `receiver.method(arg)` — e.g. `in_ty.sign_extend(v)`.
    MethodCall { receiver: Ident, method: Ident, arg: Ident },
    /// `lhs & rhs` — plain Rust `&`.
    BinOp { op: IntBinOpKind, lhs: Box<RhsValExpr>, rhs: Box<RhsValExpr> },
}

impl Parse for RhsExpr {
    fn parse(input: ParseStream) -> Result<Self> { parse_rhs(input) }
}

fn parse_rhs(input: ParseStream) -> Result<RhsExpr> {
    let lhs = parse_rhs_atom(input)?;
    if input.peek(Token![&]) {
        input.parse::<Token![&]>()?;
        let rhs = parse_rhs_atom(input)?;
        return Ok(RhsExpr::BinOp { op: IntBinOpKind::And, lhs: Box::new(lhs), rhs: Box::new(rhs) });
    }
    Ok(lhs)
}

fn parse_rhs_atom(input: ParseStream) -> Result<RhsExpr> {
    if !input.peek(Ident) {
        return Err(input.error("expected identifier or `int_const(...)` in RHS expression"));
    }
    // Fork to peek without consuming.
    let fork = input.fork();
    let head: Ident = fork.parse()?;
    if head == "int_const" && fork.peek(syn::token::Paren) {
        input.parse::<Ident>()?; // consume "int_const"
        let content; syn::parenthesized!(content in input);
        let val_expr = parse_rhs_val(&content)?;
        content.parse::<Token![,]>()?;
        let ty_ident: Ident = content.parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens in int_const(...)"));
        }
        return Ok(RhsExpr::IntConstBuilder { val_expr, ty_ident });
    }
    input.parse::<Ident>().map(RhsExpr::Ident)
}

fn parse_rhs_val(input: ParseStream) -> Result<RhsValExpr> {
    let lhs = parse_rhs_val_atom(input)?;
    if input.peek(Token![&]) {
        input.parse::<Token![&]>()?;
        let rhs = parse_rhs_val_atom(input)?;
        return Ok(RhsValExpr::BinOp { op: IntBinOpKind::And, lhs: Box::new(lhs), rhs: Box::new(rhs) });
    }
    Ok(lhs)
}

fn parse_rhs_val_atom(input: ParseStream) -> Result<RhsValExpr> {
    let ident: Ident = input.parse()?;
    if input.peek(Token![.]) {
        input.parse::<Token![.]>()?;
        let method: Ident = input.parse()?;
        let content; syn::parenthesized!(content in input);
        let arg: Ident = content.parse()?;
        return Ok(RhsValExpr::MethodCall { receiver: ident, method, arg });
    }
    Ok(RhsValExpr::Ident(ident))
}

impl RhsValExpr {
    pub fn emit(&self) -> TokenStream {
        match self {
            RhsValExpr::Ident(n) => quote! { #n },
            RhsValExpr::MethodCall { receiver, method, arg } =>
                // sign_extend returns Option<u64>; default to 0 on None (shouldn't happen
                // when the pattern already validated the integer type).
                quote! { #receiver.#method(#arg).unwrap_or(0) },
            RhsValExpr::BinOp { op, lhs, rhs } => {
                let l = lhs.emit(); let r = rhs.emit();
                match op {
                    IntBinOpKind::And => quote! { (#l & #r) },
                    IntBinOpKind::Or  => quote! { (#l | #r) },
                    IntBinOpKind::Xor => quote! { (#l ^ #r) },
                    IntBinOpKind::Add => quote! { #l.wrapping_add(#r) },
                    IntBinOpKind::Sub => quote! { #l.wrapping_sub(#r) },
                    IntBinOpKind::Mul => quote! { #l.wrapping_mul(#r) },
                    _ => quote! { compile_error!("unsupported value-level RHS op") },
                }
            }
        }
    }
}

impl RhsExpr {
    pub fn is_output(&self, caps: &CaptureEnv) -> bool {
        match self {
            RhsExpr::Ident(n) => matches!(caps.kind_of(n), Some(CaptureKind::Output)),
            RhsExpr::IntConstBuilder { .. } => true,
            RhsExpr::BinOp { lhs, rhs, .. } => lhs.is_output(caps) || rhs.is_output(caps),
        }
    }

    pub fn emit(&self, caps: &CaptureEnv) -> Result<TokenStream> {
        match self {
            RhsExpr::Ident(n) => Ok(quote! { #n }),
            RhsExpr::IntConstBuilder { val_expr, ty_ident } => {
                let v = val_expr.emit();
                Ok(quote! { fg.make_int_const(#v, #ty_ident)? })
            }
            RhsExpr::BinOp { op, lhs, rhs } => {
                if lhs.is_output(caps) || rhs.is_output(caps) {
                    let lt = lhs.emit(caps)?;
                    let rt = rhs.emit(caps)?;
                    let variant = op.variant_ident();
                    // Evaluate each operand into a temporary first to avoid
                    // multiple simultaneous `&mut fg` borrows in one expression.
                    Ok(quote! {
                        {
                            let __rhs_lhs: ir::node::NodeOutputId = #lt;
                            let __rhs_rhs: ir::node::NodeOutputId = #rt;
                            fg.make_value_node(
                                ir::node::NodeKind::IntBinaryOp(ir::IntBinaryOp::#variant),
                                [__rhs_lhs, __rhs_rhs],
                                ty,
                            )?
                        }
                    })
                } else {
                    Err(Error::new(
                        Span::call_site(),
                        "RHS `&` between two non-output captures is not supported at top level; \
                         use `int_const(c1 & c2, ty)` instead",
                    ))
                }
            }
        }
    }
}
