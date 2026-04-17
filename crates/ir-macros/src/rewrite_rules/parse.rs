/// Parsed AST and code-generation for a `rewrite_rules!` rule.
///
/// Grammar subset:
/// ```text
/// Rules    := Rule (',' Rule)* ','?
/// Rule     := LhsPat ('where' Expr)? '=>' RhsExpr
/// LhsPat   := '(' LhsPat BinOpSym LhsPat ')'
///           | 'Extend' '::' '<' ExtendKind '>' '(' LhsPat ')'
///           | 'IntConst' '(' IntConstPat ')'
///           | 'BoolConst' '(' BoolConstPat ')'
///           | 'FloatConst' '(' FloatConstPat ')'
///           | BoolOpName '(' LhsPat ',' LhsPat ')'
///           | FloatBinOpName '(' LhsPat ',' LhsPat ')'
///           | FloatCmpOpName '(' LhsPat ',' LhsPat ')'
///           | IntCmpOpName '(' LhsPat ',' LhsPat ')'
///           | Ident
/// IntConstPat  := IntLit | Ident ':' Ident | Ident
/// BoolConstPat := 'true' | 'false' | Ident
/// FloatConstPat := IntLit | Ident
/// BoolOpName      := 'BAnd' | 'BOr' | 'BXor'
/// FloatBinOpName  := 'FAdd' | 'FSub' | 'FMul' | 'FDiv'
/// FloatCmpOpName  := 'FEq' | 'FNe' | 'FLt' | 'FLe'
/// IntCmpOpName    := 'IntEq' | 'IntLt' | 'IntLe' | 'IntSlt' | 'IntSle'
///                  | 'IntCarry' | 'IntBorrow' | 'IntScarry' | 'IntSborrow'
/// RhsExpr  := 'int_const' '(' RhsValExpr ',' Ident ')'
///           | 'float_const' '(' RhsValExpr ',' RhsTyExpr ')'
///           | 'bool_const' '(' Expr ')'
///           | RhsAtom ('&' RhsAtom)*
/// ```
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    Error, Expr, Ident, LitBool, LitInt, Lifetime, Result, Token,
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
    /// Optional `where <Expr>` guard; evaluated after LHS captures are bound.
    pub where_guard: Option<Expr>,
    pub rhs: RhsExpr,
}

impl Parse for Rule {
    fn parse(input: ParseStream) -> Result<Self> {
        let lhs = input.parse::<LhsPat>()?;
        // Optional `where <Expr>` clause before `=>`.
        let where_guard = if input.peek(Token![where]) {
            input.parse::<Token![where]>()?;
            // Parse until `=>` — use `Expr::parse` which stops before `=>`.
            let guard: Expr = input.parse()?;
            Some(guard)
        } else {
            None
        };
        input.parse::<Token![=>]>()?;
        let rhs = input.parse::<RhsExpr>()?;
        Ok(Rule { lhs, where_guard, rhs })
    }
}

impl Rule {
    pub fn codegen(&self, name: &proc_macro2::Ident) -> Result<TokenStream> {
        let mut caps = CaptureEnv::new();
        // Collect captures from LHS so the RHS emitter knows each name's kind.
        self.lhs.collect_captures(&mut caps);
        // Emit LHS matching code. On success, each capture is bound as a
        // plain `let` binding in scope; on failure the emitted code diverges
        // via `return Ok(NoChange);`.
        let mut ctx = EmitCtx::new();
        let lhs_check = self.lhs.emit_check(&caps, &mut ctx, self.where_guard.as_ref())?;
        // Emit RHS expression (uses captures as live bindings).
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

                #lhs_check

                let [__root_out] = fg.graph.node_outputs_exact::<1>(node)?;
                let __new_out: ir::node::NodeOutputId = #rhs_ts;
                let __changed = fg.replace_all_uses(__root_out, __new_out)?;
                ::core::result::Result::Ok(opt::OptimizationResult::from_changed(__changed))
            }
        })
    }
}

// ── Emit context ─────────────────────────────────────────────────────────────

/// Per-rule code-emission state: supplies fresh identifiers/labels so nested
/// commutative scopes don't collide.
pub(super) struct EmitCtx {
    counter: usize,
}

impl EmitCtx {
    pub fn new() -> Self { EmitCtx { counter: 0 } }

    fn fresh(&mut self, prefix: &str) -> (Ident, Lifetime) {
        let n = self.counter;
        self.counter += 1;
        let ident = Ident::new(&format!("{prefix}_{n}"), Span::call_site());
        let lifetime = Lifetime::new(&format!("'{prefix}_lbl_{n}"), Span::call_site());
        (ident, lifetime)
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
    /// Bound to `bool`.
    BoolConst,
    /// Bound to `u64` (raw float bits).
    FloatConst,
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
}

fn kind_type(kind: CaptureKind) -> TokenStream {
    match kind {
        CaptureKind::Output     => quote! { ir::node::NodeOutputId },
        CaptureKind::IntConst   => quote! { u64 },
        CaptureKind::InputType  => quote! { ir::node::NodeOutputType },
        CaptureKind::BoolConst  => quote! { bool },
        CaptureKind::FloatConst => quote! { u64 },
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
pub(super) enum BoolBinOpKind {
    And, Or, Xor,
}

impl BoolBinOpKind {
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::And => "And",
            Self::Or  => "Or",
            Self::Xor => "Xor",
        }, Span::call_site())
    }

    /// Parse a `BAnd` / `BOr` / `BXor` head ident into the corresponding kind.
    pub fn from_ident(ident: &Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "BAnd" => Some(Self::And),
            "BOr"  => Some(Self::Or),
            "BXor" => Some(Self::Xor),
            _ => None,
        }
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

#[derive(Clone, Copy, Debug)]
pub(super) enum FloatBinOpKind {
    Add, Sub, Mul, Div,
}

impl FloatBinOpKind {
    pub fn is_commutative(self) -> bool {
        matches!(self, Self::Add | Self::Mul)
    }
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::Add => "Add",
            Self::Sub => "Sub",
            Self::Mul => "Mul",
            Self::Div => "Div",
        }, Span::call_site())
    }
    /// Parse `FAdd` / `FSub` / `FMul` / `FDiv` head ident.
    pub fn from_ident(ident: &Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "FAdd" => Some(Self::Add),
            "FSub" => Some(Self::Sub),
            "FMul" => Some(Self::Mul),
            "FDiv" => Some(Self::Div),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum FloatCmpOpKind {
    Equal, NotEqual, Less, LessEqual,
}

impl FloatCmpOpKind {
    pub fn is_commutative(self) -> bool {
        matches!(self, Self::Equal | Self::NotEqual)
    }
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::Equal    => "Equal",
            Self::NotEqual => "NotEqual",
            Self::Less     => "Less",
            Self::LessEqual => "LessEqual",
        }, Span::call_site())
    }
    /// Parse `FEq` / `FNe` / `FLt` / `FLe` head ident.
    pub fn from_ident(ident: &Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "FEq" => Some(Self::Equal),
            "FNe" => Some(Self::NotEqual),
            "FLt" => Some(Self::Less),
            "FLe" => Some(Self::LessEqual),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum IntCmpOpKind {
    Equal, Less, LessEqual, Sless, SlessEqual, Carry, Borrow, Scarry, Sborrow,
}

impl IntCmpOpKind {
    pub fn is_commutative(self) -> bool {
        matches!(self, Self::Equal | Self::Carry | Self::Scarry)
    }
    pub fn variant_ident(self) -> Ident {
        Ident::new(match self {
            Self::Equal      => "Equal",
            Self::Less       => "Less",
            Self::LessEqual  => "LessEqual",
            Self::Sless      => "Sless",
            Self::SlessEqual => "SlessEqual",
            Self::Carry      => "Carry",
            Self::Borrow     => "Borrow",
            Self::Scarry     => "Scarry",
            Self::Sborrow    => "Sborrow",
        }, Span::call_site())
    }
    /// Parse `IntEq` / `IntLt` / `IntLe` / `IntSlt` / `IntSle` /
    /// `IntCarry` / `IntBorrow` / `IntScarry` / `IntSborrow` head ident.
    pub fn from_ident(ident: &Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "IntEq"      => Some(Self::Equal),
            "IntLt"      => Some(Self::Less),
            "IntLe"      => Some(Self::LessEqual),
            "IntSlt"     => Some(Self::Sless),
            "IntSle"     => Some(Self::SlessEqual),
            "IntCarry"   => Some(Self::Carry),
            "IntBorrow"  => Some(Self::Borrow),
            "IntScarry"  => Some(Self::Scarry),
            "IntSborrow" => Some(Self::Sborrow),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(super) enum LhsPat {
    OutputCapture(Ident),
    IntConstLiteral { value: u64 },
    IntConstCapture { name: Ident },
    IntConstCaptureWithType { value_name: Ident, type_name: Ident },
    BoolConstLiteral { value: bool },
    BoolConstCapture { name: Ident },
    FloatConstLiteral { bits: u64 },
    FloatConstCapture { name: Ident },
    IntBinaryOp { op: IntBinOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
    BoolBinaryOp { op: BoolBinOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
    FloatBinaryOp { op: FloatBinOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
    FloatCmpOp { op: FloatCmpOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
    IntCmpOp { op: IntCmpOpKind, lhs: Box<LhsPat>, rhs: Box<LhsPat> },
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
    if ident == "IntConst"   { return parse_int_const(input); }
    if ident == "BoolConst"  { return parse_bool_const(input); }
    if ident == "FloatConst" { return parse_float_const(input); }
    if let Some(bool_op) = BoolBinOpKind::from_ident(&ident) {
        return parse_bool_bin_op(input, bool_op);
    }
    if let Some(float_op) = FloatBinOpKind::from_ident(&ident) {
        return parse_float_bin_op(input, float_op);
    }
    if let Some(float_cmp) = FloatCmpOpKind::from_ident(&ident) {
        return parse_float_cmp_op(input, float_cmp);
    }
    if let Some(int_cmp) = IntCmpOpKind::from_ident(&ident) {
        return parse_int_cmp_op(input, int_cmp);
    }
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

fn parse_bool_const(input: ParseStream) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    if content.peek(LitBool) {
        let lit: LitBool = content.parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens inside BoolConst(...)"));
        }
        return Ok(LhsPat::BoolConstLiteral { value: lit.value });
    }
    let name: Ident = content.parse()?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside BoolConst(...)"));
    }
    Ok(LhsPat::BoolConstCapture { name })
}

fn parse_float_const(input: ParseStream) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    if content.peek(LitInt) {
        let lit: LitInt = content.parse()?;
        let bits: u64 = lit.base10_parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens inside FloatConst(...)"));
        }
        return Ok(LhsPat::FloatConstLiteral { bits });
    }
    let name: Ident = content.parse()?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside FloatConst(...)"));
    }
    Ok(LhsPat::FloatConstCapture { name })
}

fn parse_bool_bin_op(input: ParseStream, op: BoolBinOpKind) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    let lhs = parse_lhs(&content)?;
    content.parse::<Token![,]>()?;
    let rhs = parse_lhs(&content)?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside BAnd/BOr/BXor(...)"));
    }
    Ok(LhsPat::BoolBinaryOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
}

fn parse_float_bin_op(input: ParseStream, op: FloatBinOpKind) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    let lhs = parse_lhs(&content)?;
    content.parse::<Token![,]>()?;
    let rhs = parse_lhs(&content)?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside FAdd/FSub/FMul/FDiv(...)"));
    }
    Ok(LhsPat::FloatBinaryOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
}

fn parse_float_cmp_op(input: ParseStream, op: FloatCmpOpKind) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    let lhs = parse_lhs(&content)?;
    content.parse::<Token![,]>()?;
    let rhs = parse_lhs(&content)?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens inside FEq/FNe/FLt/FLe(...)"));
    }
    Ok(LhsPat::FloatCmpOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
}

fn parse_int_cmp_op(input: ParseStream, op: IntCmpOpKind) -> Result<LhsPat> {
    let content; syn::parenthesized!(content in input);
    let lhs = parse_lhs(&content)?;
    content.parse::<Token![,]>()?;
    let rhs = parse_lhs(&content)?;
    if !content.is_empty() {
        return Err(content.error(
            "unexpected tokens inside IntEq/IntLt/IntLe/IntSlt/IntSle/IntCarry/IntBorrow/IntScarry/IntSborrow(...)"
        ));
    }
    Ok(LhsPat::IntCmpOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
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
            LhsPat::BoolConstLiteral { .. } => {}
            LhsPat::BoolConstCapture { name } => caps.bind(name.clone(), CaptureKind::BoolConst),
            LhsPat::FloatConstLiteral { .. } => {}
            LhsPat::FloatConstCapture { name } => caps.bind(name.clone(), CaptureKind::FloatConst),
            LhsPat::IntBinaryOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::BoolBinaryOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::FloatBinaryOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::FloatCmpOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::IntCmpOp { lhs, rhs, .. } => {
                lhs.collect_captures(caps);
                rhs.collect_captures(caps);
            }
            LhsPat::ExtendOp { inner, .. } => inner.collect_captures(caps),
        }
    }

    /// Emit LHS matching code at the rule's top level.
    ///
    /// On mismatch, the emitted code diverges via `return Ok(NoChange);`.
    /// On match, all captures in `caps` are bound as live `let` bindings in
    /// the surrounding scope, and control falls through to the RHS.
    ///
    /// `guard` is an optional `where <Expr>` guard evaluated after captures are
    /// bound. For commutative patterns a failed guard `continue`s to try the
    /// other ordering; for non-commutative / leaf patterns it returns `NoChange`.
    pub fn emit_check(
        &self,
        _caps: &CaptureEnv,
        ctx: &mut EmitCtx,
        guard: Option<&Expr>,
    ) -> Result<TokenStream> {
        let no_change = quote! {
            return ::core::result::Result::Ok(opt::OptimizationResult::NoChange);
        };

        // Helper: emit the guard check that diverges with `fail_ts` on failure.
        // Returns an empty stream when there is no guard.
        let guard_check = |fail_ts: &TokenStream| -> TokenStream {
            match guard {
                None => quote! {},
                Some(g) => quote! { if !(#g) { #fail_ts } },
            }
        };

        match self {
            LhsPat::IntBinaryOp { op, lhs, rhs } if op.is_commutative() => {
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("ord");

                // Collect captures introduced by each side, in pattern order
                // (lhs, then rhs), so we can destructure them after the loop.
                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __ord_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __ord_r }, &quote! { continue; }, ctx)?;
                let guard_ts = guard_check(&quote! { continue; });

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
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__ord_l, __ord_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                            #lhs_body
                            #rhs_body
                            #guard_ts
                            break #label #cap_tuple;
                        }
                        // No ordering matched: fall through to fail.
                        #no_change
                    };
                })
            }

            LhsPat::IntBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let lhs_body = lhs.emit_sub(&quote! { __root_in0 }, &no_change, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __root_in1 }, &no_change, ctx)?;
                let guard_ts = guard_check(&no_change);
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
                    #guard_ts
                })
            }

            LhsPat::BoolBinaryOp { op, lhs, rhs } => {
                // All `BoolBinaryOp` variants (`And`, `Or`, `Xor`) are commutative.
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("bord");

                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __bord_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __bord_r }, &quote! { continue; }, ctx)?;
                let guard_ts = guard_check(&quote! { continue; });

                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::BoolBinaryOp;
                        let NodeKind::BoolBinaryOp(BoolBinaryOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__bord_l, __bord_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                            #lhs_body
                            #rhs_body
                            #guard_ts
                            break #label #cap_tuple;
                        }
                        #no_change
                    };
                })
            }

            LhsPat::FloatBinaryOp { op, lhs, rhs } if op.is_commutative() => {
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("ford");

                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __ford_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __ford_r }, &quote! { continue; }, ctx)?;
                let guard_ts = guard_check(&quote! { continue; });

                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::FloatBinaryOp;
                        let NodeKind::FloatBinaryOp(FloatBinaryOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__ford_l, __ford_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                            #lhs_body
                            #rhs_body
                            #guard_ts
                            break #label #cap_tuple;
                        }
                        #no_change
                    };
                })
            }

            LhsPat::FloatBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let lhs_body = lhs.emit_sub(&quote! { __root_in0 }, &no_change, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __root_in1 }, &no_change, ctx)?;
                let guard_ts = guard_check(&no_change);
                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::FloatBinaryOp;
                        let NodeKind::FloatBinaryOp(FloatBinaryOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    #lhs_body
                    #rhs_body
                    #guard_ts
                })
            }

            LhsPat::FloatCmpOp { op, lhs, rhs } if op.is_commutative() => {
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("fcord");

                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __fcord_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __fcord_r }, &quote! { continue; }, ctx)?;
                let guard_ts = guard_check(&quote! { continue; });

                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::FloatCmpOp;
                        let NodeKind::FloatCmpOp(FloatCmpOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__fcord_l, __fcord_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                            #lhs_body
                            #rhs_body
                            #guard_ts
                            break #label #cap_tuple;
                        }
                        #no_change
                    };
                })
            }

            LhsPat::FloatCmpOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let lhs_body = lhs.emit_sub(&quote! { __root_in0 }, &no_change, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __root_in1 }, &no_change, ctx)?;
                let guard_ts = guard_check(&no_change);
                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::FloatCmpOp;
                        let NodeKind::FloatCmpOp(FloatCmpOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    #lhs_body
                    #rhs_body
                    #guard_ts
                })
            }

            LhsPat::IntCmpOp { op, lhs, rhs } if op.is_commutative() => {
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("icord");

                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __icord_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __icord_r }, &quote! { continue; }, ctx)?;
                let guard_ts = guard_check(&quote! { continue; });

                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::IntCmpOp;
                        let NodeKind::IntCmpOp(IntCmpOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__icord_l, __icord_r) in [(__root_in0, __root_in1), (__root_in1, __root_in0)] {
                            #lhs_body
                            #rhs_body
                            #guard_ts
                            break #label #cap_tuple;
                        }
                        #no_change
                    };
                })
            }

            LhsPat::IntCmpOp { op, lhs, rhs } => {
                let variant = op.variant_ident();
                let lhs_body = lhs.emit_sub(&quote! { __root_in0 }, &no_change, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __root_in1 }, &no_change, ctx)?;
                let guard_ts = guard_check(&no_change);
                Ok(quote! {
                    {
                        use ir::node::NodeKind;
                        use ir::IntCmpOp;
                        let NodeKind::IntCmpOp(IntCmpOp::#variant) = *fg.graph.node_kind(node) else {
                            #no_change
                        };
                    }
                    let [__root_in0, __root_in1] = match fg.graph.node_inputs_exact::<2>(node) {
                        Ok(v) => v,
                        Err(_) => { #no_change }
                    };
                    #lhs_body
                    #rhs_body
                    #guard_ts
                })
            }

            LhsPat::ExtendOp { kind, inner } => {
                let variant = kind.variant_ident();
                let inner_body = inner.emit_sub(&quote! { __ext_inner }, &no_change, ctx)?;
                let guard_ts = guard_check(&no_change);
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
                    #guard_ts
                })
            }

            other => {
                // Simple root pattern: get the node's output and delegate.
                let guard_ts = guard_check(&no_change);
                let no_change_ref = &no_change;
                let sub = other.emit_sub(&quote! { __root_val }, no_change_ref, ctx)?;
                Ok(quote! {
                    let __root_val = fg.graph.node_outputs(node)[0];
                    #sub
                    #guard_ts
                })
            }
        }
    }

    /// Emit match code for this sub-pattern where `val_ts` is a `NodeOutputId` expression.
    ///
    /// `fail_ts` is the token stream to emit on mismatch (e.g. `return Ok(NoChange);`
    /// or `continue;`).  It must be a diverging statement.
    ///
    /// On success, the emitted code introduces all captures of this
    /// sub-pattern as live `let` bindings in the surrounding scope.
    fn emit_sub(
        &self,
        val_ts: &TokenStream,
        fail_ts: &TokenStream,
        ctx: &mut EmitCtx,
    ) -> Result<TokenStream> {
        match self {
            LhsPat::OutputCapture(name) => {
                // Bind the capture directly: `let x = #val_ts;`.
                Ok(quote! { let #name: ir::node::NodeOutputId = #val_ts; })
            }

            LhsPat::IntConstLiteral { value } => Ok(quote! {
                {
                    let Some(__cv) = fg.int_const_val(#val_ts) else { #fail_ts };
                    if __cv != #value { #fail_ts }
                }
            }),

            LhsPat::IntConstCapture { name } => Ok(quote! {
                let Some(#name): ::core::option::Option<u64> = fg.int_const_val(#val_ts) else { #fail_ts };
            }),

            LhsPat::IntConstCaptureWithType { value_name, type_name } => Ok(quote! {
                let Some(#value_name): ::core::option::Option<u64> = fg.int_const_val(#val_ts) else { #fail_ts };
                let Some(#type_name): ::core::option::Option<ir::node::NodeOutputType> =
                    fg.graph.output_kind(#val_ts).as_value() else { #fail_ts };
            }),

            LhsPat::BoolConstLiteral { value } => Ok(quote! {
                {
                    let Some(__bcv) = fg.bool_const_val(#val_ts) else { #fail_ts };
                    if __bcv != #value { #fail_ts }
                }
            }),

            LhsPat::BoolConstCapture { name } => Ok(quote! {
                let Some(#name): ::core::option::Option<bool> = fg.bool_const_val(#val_ts) else { #fail_ts };
            }),

            LhsPat::FloatConstLiteral { bits } => Ok(quote! {
                {
                    let Some(__fcv) = fg.float_const_val(#val_ts) else { #fail_ts };
                    if __fcv != #bits { #fail_ts }
                }
            }),

            LhsPat::FloatConstCapture { name } => Ok(quote! {
                let Some(#name): ::core::option::Option<u64> = fg.float_const_val(#val_ts) else { #fail_ts };
            }),

            LhsPat::BoolBinaryOp { op, lhs, rhs } => {
                // All `BoolBinaryOp` variants (`And`, `Or`, `Xor`) are commutative.
                let variant = op.variant_ident();
                let (_, label) = ctx.fresh("bnord");

                let mut sub_caps = CaptureEnv::new();
                lhs.collect_captures(&mut sub_caps);
                rhs.collect_captures(&mut sub_caps);
                let cap_tuple = cap_tuple_tokens(&sub_caps);
                let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                let lhs_body = lhs.emit_sub(&quote! { __bnested_l }, &quote! { continue; }, ctx)?;
                let rhs_body = rhs.emit_sub(&quote! { __bnested_r }, &quote! { continue; }, ctx)?;

                Ok(quote! {
                    let __sub_node = fg.graph.get_node_from_output(#val_ts);
                    {
                        use ir::node::NodeKind;
                        use ir::BoolBinaryOp;
                        let NodeKind::BoolBinaryOp(BoolBinaryOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                    }
                    let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                        Ok(v) => v,
                        Err(_) => { #fail_ts }
                    };
                    let #cap_tuple: #cap_tuple_ty = #label: loop {
                        for (__bnested_l, __bnested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                            #lhs_body
                            #rhs_body
                            break #label #cap_tuple;
                        }
                        #fail_ts
                    };
                })
            }

            LhsPat::IntBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();

                if op.is_commutative() {
                    let (_, label) = ctx.fresh("nord");

                    let mut sub_caps = CaptureEnv::new();
                    lhs.collect_captures(&mut sub_caps);
                    rhs.collect_captures(&mut sub_caps);
                    let cap_tuple = cap_tuple_tokens(&sub_caps);
                    let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                    let lhs_body = lhs.emit_sub(&quote! { __nested_l }, &quote! { continue; }, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __nested_r }, &quote! { continue; }, ctx)?;

                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntBinaryOp;
                            let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        let #cap_tuple: #cap_tuple_ty = #label: loop {
                            for (__nested_l, __nested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                                #lhs_body
                                #rhs_body
                                break #label #cap_tuple;
                            }
                            #fail_ts
                        };
                    })
                } else {
                    let lhs_body = lhs.emit_sub(&quote! { __sub_lhs }, fail_ts, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __sub_rhs }, fail_ts, ctx)?;
                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntBinaryOp;
                            let NodeKind::IntBinaryOp(IntBinaryOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_lhs, __sub_rhs] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        #lhs_body
                        #rhs_body
                    })
                }
            }

            LhsPat::FloatBinaryOp { op, lhs, rhs } => {
                let variant = op.variant_ident();

                if op.is_commutative() {
                    let (_, label) = ctx.fresh("fnord");

                    let mut sub_caps = CaptureEnv::new();
                    lhs.collect_captures(&mut sub_caps);
                    rhs.collect_captures(&mut sub_caps);
                    let cap_tuple = cap_tuple_tokens(&sub_caps);
                    let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                    let lhs_body = lhs.emit_sub(&quote! { __fnested_l }, &quote! { continue; }, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __fnested_r }, &quote! { continue; }, ctx)?;

                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::FloatBinaryOp;
                            let NodeKind::FloatBinaryOp(FloatBinaryOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        let #cap_tuple: #cap_tuple_ty = #label: loop {
                            for (__fnested_l, __fnested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                                #lhs_body
                                #rhs_body
                                break #label #cap_tuple;
                            }
                            #fail_ts
                        };
                    })
                } else {
                    let lhs_body = lhs.emit_sub(&quote! { __sub_flhs }, fail_ts, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __sub_frhs }, fail_ts, ctx)?;
                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::FloatBinaryOp;
                            let NodeKind::FloatBinaryOp(FloatBinaryOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_flhs, __sub_frhs] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        #lhs_body
                        #rhs_body
                    })
                }
            }

            LhsPat::FloatCmpOp { op, lhs, rhs } => {
                let variant = op.variant_ident();

                if op.is_commutative() {
                    let (_, label) = ctx.fresh("fcnord");

                    let mut sub_caps = CaptureEnv::new();
                    lhs.collect_captures(&mut sub_caps);
                    rhs.collect_captures(&mut sub_caps);
                    let cap_tuple = cap_tuple_tokens(&sub_caps);
                    let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                    let lhs_body = lhs.emit_sub(&quote! { __fcnested_l }, &quote! { continue; }, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __fcnested_r }, &quote! { continue; }, ctx)?;

                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::FloatCmpOp;
                            let NodeKind::FloatCmpOp(FloatCmpOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        let #cap_tuple: #cap_tuple_ty = #label: loop {
                            for (__fcnested_l, __fcnested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                                #lhs_body
                                #rhs_body
                                break #label #cap_tuple;
                            }
                            #fail_ts
                        };
                    })
                } else {
                    let lhs_body = lhs.emit_sub(&quote! { __sub_fclhs }, fail_ts, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __sub_fcrhs }, fail_ts, ctx)?;
                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::FloatCmpOp;
                            let NodeKind::FloatCmpOp(FloatCmpOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_fclhs, __sub_fcrhs] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        #lhs_body
                        #rhs_body
                    })
                }
            }

            LhsPat::IntCmpOp { op, lhs, rhs } => {
                let variant = op.variant_ident();

                if op.is_commutative() {
                    let (_, label) = ctx.fresh("icnord");

                    let mut sub_caps = CaptureEnv::new();
                    lhs.collect_captures(&mut sub_caps);
                    rhs.collect_captures(&mut sub_caps);
                    let cap_tuple = cap_tuple_tokens(&sub_caps);
                    let cap_tuple_ty = cap_tuple_ty_tokens(&sub_caps);

                    let lhs_body = lhs.emit_sub(&quote! { __icnested_l }, &quote! { continue; }, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __icnested_r }, &quote! { continue; }, ctx)?;

                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntCmpOp;
                            let NodeKind::IntCmpOp(IntCmpOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_ni0, __sub_ni1] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
                            Ok(v) => v,
                            Err(_) => { #fail_ts }
                        };
                        let #cap_tuple: #cap_tuple_ty = #label: loop {
                            for (__icnested_l, __icnested_r) in [(__sub_ni0, __sub_ni1), (__sub_ni1, __sub_ni0)] {
                                #lhs_body
                                #rhs_body
                                break #label #cap_tuple;
                            }
                            #fail_ts
                        };
                    })
                } else {
                    let lhs_body = lhs.emit_sub(&quote! { __sub_iclhs }, fail_ts, ctx)?;
                    let rhs_body = rhs.emit_sub(&quote! { __sub_icrhs }, fail_ts, ctx)?;
                    Ok(quote! {
                        let __sub_node = fg.graph.get_node_from_output(#val_ts);
                        {
                            use ir::node::NodeKind;
                            use ir::IntCmpOp;
                            let NodeKind::IntCmpOp(IntCmpOp::#variant) = *fg.graph.node_kind(__sub_node) else { #fail_ts };
                        }
                        let [__sub_iclhs, __sub_icrhs] = match fg.graph.node_inputs_exact::<2>(__sub_node) {
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
                let inner_body = inner.emit_sub(&quote! { __sub_ext_inner }, fail_ts, ctx)?;
                Ok(quote! {
                    let __sub_ext_node = fg.graph.get_node_from_output(#val_ts);
                    {
                        use ir::node::NodeKind;
                        use ir::ExtendOp;
                        let NodeKind::Extend(ExtendOp::#variant) = *fg.graph.node_kind(__sub_ext_node) else { #fail_ts };
                    }
                    let [__sub_ext_inner] = match fg.graph.node_inputs_exact::<1>(__sub_ext_node) {
                        Ok(v) => v,
                        Err(_) => { #fail_ts }
                    };
                    #inner_body
                })
            }
        }
    }
}

/// Emit `(c1, c2, ...)` tokens for the captures in `env`.
fn cap_tuple_tokens(env: &CaptureEnv) -> TokenStream {
    let elems: Vec<TokenStream> = env.bindings.iter().map(|(id, _)| quote! { #id }).collect();
    // Use trailing comma so single-element tuples parse correctly as tuples,
    // not parenthesised expressions.
    quote! { ( #( #elems, )* ) }
}

/// Emit the type `(T1, T2, ...)` tokens matching `cap_tuple_tokens`.
fn cap_tuple_ty_tokens(env: &CaptureEnv) -> TokenStream {
    let tys: Vec<TokenStream> = env.bindings.iter().map(|(_, k)| kind_type(*k)).collect();
    quote! { ( #( #tys, )* ) }
}

// ── RHS AST ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(super) enum RhsExpr {
    /// `x` — a captured NodeOutputId (or u64, context-dependent).
    Ident(Ident),
    /// `int_const(<val_expr>, <ty>)` — builds a new IntConst node.
    IntConstBuilder { val_expr: RhsValExpr, ty_ident: Ident },
    /// `float_const(<val_expr>, <ty_expr>)` — builds a new FloatConst node.
    FloatConstBuilder { val_expr: RhsValExpr, ty_expr: Expr },
    /// `bool_const(<expr>)` — builds a BoolConst node; `<expr>` is any Rust
    /// expression that evaluates to `bool` (e.g. `true`, `false`, `l == r`).
    BoolConstBuilder { value: Expr },
    /// `lhs & rhs` — creates a new IntBinaryOp(And) node.
    BinOp { op: IntBinOpKind, lhs: Box<RhsExpr>, rhs: Box<RhsExpr> },
}

/// An expression that always evaluates to a plain Rust value (not a NodeOutputId).
#[derive(Clone)]
pub(super) enum RhsValExpr {
    Ident(Ident),
    /// An integer literal such as `0u64`.
    Lit(LitInt),
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
        return Err(input.error("expected identifier or `int_const(...)`/`float_const(...)`/`bool_const(...)` in RHS expression"));
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
    if head == "float_const" && fork.peek(syn::token::Paren) {
        input.parse::<Ident>()?; // consume "float_const"
        let content; syn::parenthesized!(content in input);
        let val_expr = parse_rhs_val(&content)?;
        content.parse::<Token![,]>()?;
        let ty_expr: Expr = content.parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens in float_const(...)"));
        }
        return Ok(RhsExpr::FloatConstBuilder { val_expr, ty_expr });
    }
    if head == "bool_const" && fork.peek(syn::token::Paren) {
        input.parse::<Ident>()?; // consume "bool_const"
        let content; syn::parenthesized!(content in input);
        // Accept any Rust expression that evaluates to `bool`
        // (e.g. `true`, `false`, `l == r`, `a && b`).
        let expr: Expr = content.parse()?;
        if !content.is_empty() {
            return Err(content.error("unexpected tokens in bool_const(...)"));
        }
        return Ok(RhsExpr::BoolConstBuilder { value: expr });
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
    if input.peek(Token![+]) {
        input.parse::<Token![+]>()?;
        let rhs = parse_rhs_val_atom(input)?;
        return Ok(RhsValExpr::BinOp { op: IntBinOpKind::Add, lhs: Box::new(lhs), rhs: Box::new(rhs) });
    }
    if input.peek(Token![-]) {
        input.parse::<Token![-]>()?;
        let rhs = parse_rhs_val_atom(input)?;
        return Ok(RhsValExpr::BinOp { op: IntBinOpKind::Sub, lhs: Box::new(lhs), rhs: Box::new(rhs) });
    }
    Ok(lhs)
}

fn parse_rhs_val_atom(input: ParseStream) -> Result<RhsValExpr> {
    // Accept integer literals (e.g. `0u64`) as plain value expressions.
    if input.peek(LitInt) {
        let lit: LitInt = input.parse()?;
        return Ok(RhsValExpr::Lit(lit));
    }
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
            RhsValExpr::Lit(lit) => quote! { #lit },
            RhsValExpr::MethodCall { receiver, method, arg } => {
                // `sign_extend` returns `Option<u64>` because the underlying
                // `NodeOutputType` covers widths (`Bool`, `U128`, `U256`, floats)
                // that can't be represented as a sign-extended `u64`. Propagate
                // that via `?` — the LHS has only checked that the value is an
                // `IntConst`, not that its declared output type is an integer
                // `<= 64 bits`, so a mismatched type is a legitimate (if
                // unexpected) error, not something to silently hide.
                quote! {
                    #receiver.#method(#arg)
                        .ok_or_else(|| opt::Error::from(
                            opt::ErrorKind::ExpectedIntegerType(#receiver)
                        ))?
                }
            }
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
            RhsExpr::FloatConstBuilder { .. } => true,
            RhsExpr::BoolConstBuilder { .. } => true,
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
            RhsExpr::FloatConstBuilder { val_expr, ty_expr } => {
                let v = val_expr.emit();
                Ok(quote! { fg.make_float_const(#v, #ty_expr)? })
            }
            RhsExpr::BoolConstBuilder { value } => {
                Ok(quote! { fg.make_bool_const(#value)? })
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
