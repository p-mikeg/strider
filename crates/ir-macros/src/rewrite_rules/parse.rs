/// Parsed AST and code-generation for a `rewrite_rules!` rule.
///
/// Grammar subset:
/// ```text
/// Rules    := Rule (',' Rule)* ','?
/// Rule     := LhsPat ('where' Expr)? '=>' RhsExpr   -- pattern rule
///           | Ident '@' Ident                         -- escape rule
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
///
/// Escape rule: `label @ fn_name` — the `label` ident is purely documentary
/// (discarded at parse time) and `fn_name` is called directly as
/// `fn_name(fg, node)`.  No pattern matching or LHS/RHS is involved.
/// ```
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    Error, Expr, Ident, LitBool, LitInt, Result, Token,
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

pub(super) enum Rule {
    /// A pattern rule: `LhsPat [where Expr] => RhsExpr`.
    Pattern { lhs: Box<LhsPat>, where_guard: Option<Expr>, rhs: Box<RhsExpr> },
    /// An escape rule: `label @ fn_name` — calls `fn_name(fg, node)` directly.
    /// The `label` ident is discarded at parse time; it exists for documentation.
    Escape { fn_name: Ident },
}

impl Parse for Rule {
    fn parse(input: ParseStream) -> Result<Self> {
        // Peek for `Ident @ ...` — the escape-hatch form.
        if input.peek(Ident) && input.peek2(Token![@]) {
            input.parse::<Ident>()?; // consume the label (purely documentary)
            input.parse::<Token![@]>()?;
            let fn_name: Ident = input.parse()?;
            return Ok(Rule::Escape { fn_name });
        }
        // Fall through to the pattern-rule form.
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
        Ok(Rule::Pattern { lhs: Box::new(lhs), where_guard, rhs: Box::new(rhs) })
    }
}

impl Rule {
    pub fn codegen(&self, name: &proc_macro2::Ident) -> Result<TokenStream> {
        match self {
            Rule::Escape { fn_name } => Ok(quote! {
                #[allow(non_snake_case, clippy::needless_pass_by_ref_mut)]
                fn #name(
                    fg: &mut ir::BuiltFunctionGraph,
                    node: ir::node::NodeId,
                ) -> ::core::result::Result<opt::OptimizationResult, opt::Error> {
                    #fn_name(fg, node)
                }
            }),
            Rule::Pattern { lhs, where_guard, rhs } => {
                self.codegen_pattern(name, lhs, where_guard, rhs)
            }
        }
    }

    fn codegen_pattern(
        &self,
        name: &proc_macro2::Ident,
        lhs: &LhsPat,
        where_guard: &Option<Expr>,
        rhs: &RhsExpr,
    ) -> Result<TokenStream> {
        let mut caps = CaptureEnv::new();
        // Collect captures from LHS so the RHS emitter knows each name's kind.
        lhs.collect_captures(&mut caps);

        // ── Build the Pat expression ──────────────────────────────────────────
        // Walk the LhsPat tree and emit a ::pattern::... builder expression.
        // type_to_value maps InputType capture names → their value_name idents.
        let mut type_to_value: Vec<(Ident, Ident)> = Vec::new();

        // ── Static OnceLock Var allocations ──────────────────────────────────
        // One per capture that needs a Var (Output, IntConst, BoolConst,
        // FloatConst). InputType captures share the Var of their value_name.
        // We also always allocate __v_root for the where-guard's ty extraction.
        let var_statics: Vec<TokenStream> = caps.bindings.iter()
            .filter(|(_, k)| !matches!(k, CaptureKind::InputType))
            .map(|(id, _)| {
                let static_name = Ident::new(
                    &format!("__V_{}", id),
                    Span::call_site(),
                );
                let local_name = Ident::new(
                    &format!("__v_{}", id),
                    Span::call_site(),
                );
                quote! {
                    static #static_name: ::std::sync::OnceLock<::pattern::Var> =
                        ::std::sync::OnceLock::new();
                    let #local_name: ::pattern::Var =
                        *#static_name.get_or_init(::pattern::Var::new);
                }
            })
            .collect();

        // __v_root is needed for the where-guard's ty extraction.
        let v_root_static = quote! {
            static __V_root: ::std::sync::OnceLock<::pattern::Var> =
                ::std::sync::OnceLock::new();
            let __v_root: ::pattern::Var =
                *__V_root.get_or_init(::pattern::Var::new);
        };

        // ── Build guard closure body (shared between orderings if commutative) ─
        let guard_closure_body: Option<TokenStream> = where_guard.as_ref().map(|guard_expr| {
            let guard_bindings: Vec<TokenStream> = caps.bindings.iter()
                .map(|(id, kind)| {
                    let v_ident = Ident::new(&format!("__v_{}", id), Span::call_site());
                    let out_ident = Ident::new(
                        &format!("__guard_out_{}", id),
                        Span::call_site(),
                    );
                    match kind {
                        CaptureKind::Output => quote! {
                            let ::core::option::Option::Some(#out_ident) = __b.get(#v_ident)
                                else { return false; };
                            let #id: ir::node::NodeOutputId = #out_ident;
                        },
                        CaptureKind::IntConst => quote! {
                            let ::core::option::Option::Some(#out_ident) = __b.get(#v_ident)
                                else { return false; };
                            let ::core::option::Option::Some(#id) = fg.int_const_val(#out_ident)
                                else { return false; };
                        },
                        CaptureKind::BoolConst => quote! {
                            let ::core::option::Option::Some(#out_ident) = __b.get(#v_ident)
                                else { return false; };
                            let ::core::option::Option::Some(#id) = fg.bool_const_val(#out_ident)
                                else { return false; };
                        },
                        CaptureKind::FloatConst => quote! {
                            let ::core::option::Option::Some(#out_ident) = __b.get(#v_ident)
                                else { return false; };
                            let ::core::option::Option::Some(#id) = fg.float_const_val(#out_ident)
                                else { return false; };
                        },
                        CaptureKind::InputType => {
                            let value_name = type_to_value.iter()
                                .find(|(t, _)| t == id)
                                .map(|(_, v)| v.clone())
                                .unwrap_or_else(|| id.clone());
                            let value_v = Ident::new(
                                &format!("__v_{}", value_name),
                                Span::call_site(),
                            );
                            let val_out = Ident::new(
                                &format!("__guard_out_{}", value_name),
                                Span::call_site(),
                            );
                            quote! {
                                let ::core::option::Option::Some(#val_out) = __b.get(#value_v)
                                    else { return false; };
                                let ::core::option::Option::Some(#id) =
                                    fg.graph.output_kind(#val_out).as_value()
                                    else { return false; };
                            }
                        }
                    }
                })
                .collect();
            let ty_in_guard = quote! {
                let ::core::option::Option::Some(__root_out_g) = __b.get(__v_root)
                    else { return false; };
                let ::core::option::Option::Some(ty) =
                    fg.graph.output_kind(__root_out_g).as_value()
                    else { return false; };
            };
            quote! {
                #( #guard_bindings )*
                #ty_in_guard
                (#guard_expr) as bool
            }
        });

        // ── Build the final Pat tree(s) ───────────────────────────────────────
        // For commutative root ops with a where guard, we emit TWO ordered
        // patterns (one per operand ordering) and try them with `or_else`.
        // This ensures the guard sees each ordering and can accept/reject each.
        let match_ts = if let Some(guard_body) = &guard_closure_body {
            if lhs.root_is_commutative() {
                // Two ordered patterns (stated and swapped), each with the guard.
                let inner1 = emit_pat_builder(lhs, &mut type_to_value, false);
                let inner2 = emit_pat_builder(lhs, &mut type_to_value, true);
                quote! {
                    let __pat1: ::pattern::Pat = #inner1
                        .ordered()
                        .capture(__v_root)
                        .when_match(move |fg, __b| { #guard_body });
                    let __pat2: ::pattern::Pat = #inner2
                        .ordered()
                        .capture(__v_root)
                        .when_match(move |fg, __b| { #guard_body });
                    let __matched = {
                        let __matcher = ::pattern::Matcher::new(fg);
                        __matcher.match_at(node, &__pat1)
                            .or_else(|| __matcher.match_at(node, &__pat2))
                    };
                }
            } else {
                // Single pattern with guard (non-commutative root).
                let inner = emit_pat_builder(lhs, &mut type_to_value, false);
                quote! {
                    let __pat: ::pattern::Pat = <::pattern::Pat>::from(#inner)
                        .capture(__v_root)
                        .when_match(move |fg, __b| { #guard_body });
                    let __matched = {
                        let __matcher = ::pattern::Matcher::new(fg);
                        __matcher.match_at(node, &__pat)
                    };
                }
            }
        } else {
            // No guard — single pattern, commutative ops auto-retry both orderings.
            let inner = emit_pat_builder(lhs, &mut type_to_value, false);
            quote! {
                let __pat: ::pattern::Pat = <::pattern::Pat>::from(#inner);
                let __matched = {
                    let __matcher = ::pattern::Matcher::new(fg);
                    __matcher.match_at(node, &__pat)
                };
            }
        };

        // ── Capture extraction after successful match ─────────────────────────
        let extractions: Vec<TokenStream> = caps.bindings.iter()
            .map(|(id, kind)| {
                let v_ident = Ident::new(&format!("__v_{}", id), Span::call_site());
                match kind {
                    CaptureKind::Output => quote! {
                        let #id: ir::node::NodeOutputId = __m.get(#v_ident)
                            .ok_or_else(|| opt::Error::from(
                                opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                            ))?;
                    },
                    CaptureKind::IntConst => quote! {
                        let #id: u64 = __m.get_int_const(#v_ident, fg)
                            .ok_or_else(|| opt::Error::from(
                                opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                            ))?;
                    },
                    CaptureKind::BoolConst => quote! {
                        let #id: bool = __m.get_bool_const(#v_ident, fg)
                            .ok_or_else(|| opt::Error::from(
                                opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                            ))?;
                    },
                    CaptureKind::FloatConst => quote! {
                        let #id: u64 = __m.get_float_bits(#v_ident, fg)
                            .ok_or_else(|| opt::Error::from(
                                opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                            ))?;
                    },
                    CaptureKind::InputType => {
                        // Derive the type from the corresponding value capture's output.
                        let value_name = type_to_value.iter()
                            .find(|(t, _)| t == id)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_else(|| id.clone());
                        let value_v = Ident::new(
                            &format!("__v_{}", value_name),
                            Span::call_site(),
                        );
                        quote! {
                            let #id: ir::node::NodeOutputType = {
                                let __input_out = __m.get(#value_v)
                                    .ok_or_else(|| opt::Error::from(
                                        opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                                    ))?;
                                fg.graph.output_kind(__input_out).as_value()
                                    .ok_or_else(|| opt::Error::from(
                                        opt::ErrorKind::InternalCaptureMissing(stringify!(#id))
                                    ))?
                            };
                        }
                    }
                }
            })
            .collect();

        // ── Emit RHS expression ───────────────────────────────────────────────
        let rhs_ts = rhs.emit(&caps)?;

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
                // ── Stable Var identities per capture (one OnceLock per capture) ──
                #v_root_static
                #( #var_statics )*

                // ── Build the Pat tree and run the matcher ────────────────────
                #match_ts

                let ::core::option::Option::Some(__m) = __matched else {
                    return ::core::result::Result::Ok(opt::OptimizationResult::NoChange);
                };

                // ── Extract captures ──────────────────────────────────────────
                #( #extractions )*

                // ── Resolve `ty` (root output type; available in RHS) ─────────
                let [__root_out] = fg.graph.node_outputs_exact::<1>(node)?;
                let ::core::option::Option::Some(ty) =
                    fg.graph.output_kind(__root_out).as_value()
                else {
                    return ::core::result::Result::Ok(opt::OptimizationResult::NoChange);
                };
                #[allow(unused_variables)]
                let ty: ir::node::NodeOutputType = ty;

                // ── Emit RHS and apply rewrite ────────────────────────────────
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
    /// Bound to `NodeOutputType` (derived from an IntConst capture's output).
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

// ── LHS AST ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) enum IntBinOpKind {
    Add, Sub, Mul, Div, And, Or, Xor, Shl, Shr,
}

impl IntBinOpKind {
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


#[derive(Clone, Copy, Debug)]
pub(super) enum FloatBinOpKind {
    Add, Sub, Mul, Div,
}

impl FloatBinOpKind {
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

    /// Returns true if this pattern's root operator is commutative —
    /// i.e., the pattern crate's builder supports `.ordered()` and
    /// both operand orderings should be tried when a `where` guard is present.
    pub fn root_is_commutative(&self) -> bool {
        match self {
            // Commutative int binary ops
            LhsPat::IntBinaryOp { op, .. } => matches!(
                op,
                IntBinOpKind::Add | IntBinOpKind::Mul
                    | IntBinOpKind::And | IntBinOpKind::Or | IntBinOpKind::Xor
            ),
            // All bool binary ops are commutative
            LhsPat::BoolBinaryOp { .. } => true,
            // Commutative float binary ops
            LhsPat::FloatBinaryOp { op, .. } => {
                matches!(op, FloatBinOpKind::Add | FloatBinOpKind::Mul)
            }
            // Float cmp: Equal and NotEqual are commutative
            LhsPat::FloatCmpOp { op, .. } => {
                matches!(op, FloatCmpOpKind::Equal | FloatCmpOpKind::NotEqual)
            }
            // Int cmp: Equal, Carry, Scarry are commutative
            LhsPat::IntCmpOp { op, .. } => {
                matches!(op, IntCmpOpKind::Equal | IntCmpOpKind::Carry | IntCmpOpKind::Scarry)
            }
            _ => false,
        }
    }
}

// ── Pat builder emission ──────────────────────────────────────────────────────

/// Walk a `LhsPat` tree and emit a `::pattern::...` builder expression.
///
/// `type_to_value` accumulates `(type_name, value_name)` pairs for
/// `IntConstCaptureWithType` nodes — needed later during capture extraction
/// so we know which `Var` to read when resolving an `InputType` capture.
///
/// `swap_root` — when `true`, swaps the lhs/rhs of the top-level binary op.
/// Used to emit the "reversed operand ordering" pattern for commutative ops
/// with a `where` guard (so the guard sees each ordering independently).
/// Sub-patterns are always emitted in their stated order.
///
/// The emitted expression converts to `::pattern::Pat` via `.into()` for most
/// variants (binary op builders), or is already a `Pat` for leaf variants.
/// The caller wraps the whole thing in `<::pattern::Pat>::from(...)` or
/// chained `.capture(...).when_match(...)`.
fn emit_pat_builder(
    pat: &LhsPat,
    type_to_value: &mut Vec<(Ident, Ident)>,
    swap_root: bool,
) -> TokenStream {
    match pat {
        LhsPat::OutputCapture(name) => {
            let v = Ident::new(&format!("__v_{}", name), Span::call_site());
            quote! { ::pattern::var(#v) }
        }

        LhsPat::IntConstLiteral { value } => {
            quote! { ::pattern::int_const(#value) }
        }

        LhsPat::IntConstCapture { name } => {
            let v = Ident::new(&format!("__v_{}", name), Span::call_site());
            quote! { ::pattern::any_int_const(#v) }
        }

        LhsPat::IntConstCaptureWithType { value_name, type_name } => {
            // Register the type→value mapping for extraction.
            type_to_value.push((type_name.clone(), value_name.clone()));
            let v = Ident::new(&format!("__v_{}", value_name), Span::call_site());
            // The pattern only captures the value; type_name is derived from it.
            quote! { ::pattern::any_int_const(#v) }
        }

        LhsPat::BoolConstLiteral { value } => {
            quote! { ::pattern::bool_const(#value) }
        }

        LhsPat::BoolConstCapture { name } => {
            let v = Ident::new(&format!("__v_{}", name), Span::call_site());
            quote! { ::pattern::any_bool_const(#v) }
        }

        LhsPat::FloatConstLiteral { bits } => {
            quote! { ::pattern::float_const(#bits) }
        }

        LhsPat::FloatConstCapture { name } => {
            let v = Ident::new(&format!("__v_{}", name), Span::call_site());
            quote! { ::pattern::any_float_const(#v) }
        }

        LhsPat::IntBinaryOp { op, lhs, rhs } => {
            // When swap_root, emit rhs-var on the left and lhs-var on the right.
            // Child recursion always uses swap_root=false (only root swaps).
            let (first, second) = if swap_root {
                (emit_pat_builder(rhs, type_to_value, false),
                 emit_pat_builder(lhs, type_to_value, false))
            } else {
                (emit_pat_builder(lhs, type_to_value, false),
                 emit_pat_builder(rhs, type_to_value, false))
            };
            let (l, r) = (first, second);
            // Use shorthand free constructors; commutative ones auto-try both orderings.
            match op {
                IntBinOpKind::Add => quote! { ::pattern::add(#l, #r) },
                IntBinOpKind::Sub => quote! { ::pattern::sub(#l, #r) },
                IntBinOpKind::Mul => quote! { ::pattern::mul(#l, #r) },
                IntBinOpKind::Div => quote! { ::pattern::div(#l, #r) },
                IntBinOpKind::And => quote! { ::pattern::and(#l, #r) },
                IntBinOpKind::Or  => quote! { ::pattern::or(#l, #r)  },
                IntBinOpKind::Xor => quote! { ::pattern::xor(#l, #r) },
                IntBinOpKind::Shl => quote! { ::pattern::shl(#l, #r) },
                IntBinOpKind::Shr => quote! { ::pattern::shr(#l, #r) },
            }
        }

        LhsPat::BoolBinaryOp { op, lhs, rhs } => {
            let (first, second) = if swap_root {
                (emit_pat_builder(rhs, type_to_value, false),
                 emit_pat_builder(lhs, type_to_value, false))
            } else {
                (emit_pat_builder(lhs, type_to_value, false),
                 emit_pat_builder(rhs, type_to_value, false))
            };
            let (l, r) = (first, second);
            match op {
                BoolBinOpKind::And => quote! { ::pattern::bool_and(#l, #r) },
                BoolBinOpKind::Or  => quote! { ::pattern::bool_or(#l, #r)  },
                BoolBinOpKind::Xor => quote! { ::pattern::bool_xor(#l, #r) },
            }
        }

        LhsPat::FloatBinaryOp { op, lhs, rhs } => {
            let (first, second) = if swap_root {
                (emit_pat_builder(rhs, type_to_value, false),
                 emit_pat_builder(lhs, type_to_value, false))
            } else {
                (emit_pat_builder(lhs, type_to_value, false),
                 emit_pat_builder(rhs, type_to_value, false))
            };
            let (l, r) = (first, second);
            match op {
                FloatBinOpKind::Add => quote! { ::pattern::float_add(#l, #r) },
                FloatBinOpKind::Sub => quote! { ::pattern::float_sub(#l, #r) },
                FloatBinOpKind::Mul => quote! { ::pattern::float_mul(#l, #r) },
                FloatBinOpKind::Div => quote! { ::pattern::float_div(#l, #r) },
            }
        }

        LhsPat::FloatCmpOp { op, lhs, rhs } => {
            let (first, second) = if swap_root {
                (emit_pat_builder(rhs, type_to_value, false),
                 emit_pat_builder(lhs, type_to_value, false))
            } else {
                (emit_pat_builder(lhs, type_to_value, false),
                 emit_pat_builder(rhs, type_to_value, false))
            };
            let (l, r) = (first, second);
            match op {
                FloatCmpOpKind::Equal    => quote! { ::pattern::float_eq(#l, #r) },
                FloatCmpOpKind::NotEqual => quote! { ::pattern::float_ne(#l, #r) },
                FloatCmpOpKind::Less     => quote! { ::pattern::float_lt(#l, #r) },
                FloatCmpOpKind::LessEqual => quote! { ::pattern::float_le(#l, #r) },
            }
        }

        LhsPat::IntCmpOp { op, lhs, rhs } => {
            let (first, second) = if swap_root {
                (emit_pat_builder(rhs, type_to_value, false),
                 emit_pat_builder(lhs, type_to_value, false))
            } else {
                (emit_pat_builder(lhs, type_to_value, false),
                 emit_pat_builder(rhs, type_to_value, false))
            };
            let (l, r) = (first, second);
            match op {
                IntCmpOpKind::Equal      => quote! { ::pattern::int_eq(#l, #r)     },
                IntCmpOpKind::Less       => quote! { ::pattern::int_lt(#l, #r)     },
                IntCmpOpKind::LessEqual  => quote! { ::pattern::int_le(#l, #r)     },
                IntCmpOpKind::Sless      => quote! { ::pattern::int_slt(#l, #r)    },
                IntCmpOpKind::SlessEqual => quote! { ::pattern::int_sle(#l, #r)    },
                IntCmpOpKind::Carry      => quote! { ::pattern::int_carry(#l, #r)  },
                IntCmpOpKind::Borrow     => quote! {
                    ::pattern::int_cmp(::pattern::IntCmpOp::Borrow, #l, #r)
                },
                IntCmpOpKind::Scarry     => quote! { ::pattern::int_scarry(#l, #r) },
                IntCmpOpKind::Sborrow    => quote! { ::pattern::int_sborrow(#l, #r) },
            }
        }

        LhsPat::ExtendOp { kind, inner } => {
            let i = emit_pat_builder(inner, type_to_value, false);
            match kind {
                ExtendKind::SignExtend  => quote! { ::pattern::sign_extend(#i) },
                ExtendKind::ZeroExtend => quote! { ::pattern::zero_extend(#i) },
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
