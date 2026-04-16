#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

use proc_macro::TokenStream;
use proc_macro2::{Literal, Span, TokenStream as TokenStream2};
use quote::{ToTokens, quote};
use syn::{
    Block, Ident, Pat, Result, Token, bracketed,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    token,
};

enum BindingKind {
    Node,
    Value,
}

struct Binding {
    kind: BindingKind,
    name: Ident,
}

struct NodeMatch {
    kind_pat: Pat,
    inputs: Option<Punctuated<GraphPat, Token![,]>>,
}

struct GraphPat {
    binding: Option<Binding>,
    node_match: Option<NodeMatch>,
}

struct ValueMatch {
    pat: GraphPat,
    ctx: Ident,
    val: Ident,
    body: Block,
}

fn parse_binding(input: ParseStream) -> Result<Option<Binding>> {
    let Ok(ident) = input.fork().parse::<Ident>() else {
        return Ok(None);
    };

    let kind = if ident == "node" {
        BindingKind::Node
    } else if ident == "val" {
        BindingKind::Value
    } else {
        return Ok(None);
    };

    input.parse::<Ident>()?;
    let name = input.parse()?;

    Ok(Some(Binding { kind, name }))
}

impl Parse for NodeMatch {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut kind_pat = Pat::parse_single(input)?;

        // Drop unnecessary parenthesization, but keep or-patterns inside parens.
        if let Pat::Paren(inner) = kind_pat {
            kind_pat = if matches!(*inner.pat, Pat::Or(_)) {
                *inner.pat
            } else {
                Pat::Paren(inner)
            };
        }

        let inputs = if input.peek(token::Bracket) {
            let content;
            bracketed!(content in input);
            let inputs = content.parse_terminated(GraphPat::parse, Token![,])?;
            Some(inputs)
        } else {
            None
        };

        Ok(Self { kind_pat, inputs })
    }
}

impl Parse for GraphPat {
    fn parse(input: ParseStream) -> Result<Self> {
        let binding = parse_binding(input)?;
        let node_match = match binding {
            Some(_) => {
                if input.peek(Token![@]) {
                    input.parse::<Token![@]>()?;
                    Some(input.parse()?)
                } else {
                    None
                }
            }
            None => Some(input.parse()?),
        };

        Ok(Self {
            binding,
            node_match,
        })
    }
}

impl Parse for ValueMatch {
    fn parse(input: ParseStream) -> Result<Self> {
        input.parse::<Token![if]>()?;
        input.parse::<Token![let]>()?;

        let pat = input.parse()?;

        input.parse::<Token![=]>()?;

        let ctx = input.parse()?;

        input.parse::<Token![,]>()?;

        let val = input.parse()?;
        let body = input.parse()?;

        Ok(ValueMatch {
            pat,
            ctx,
            val,
            body,
        })
    }
}

fn fresh_tmp_var(var_idx: &mut usize) -> Ident {
    let var_name = Ident::new(&format!("__{}", *var_idx), Span::mixed_site());
    *var_idx += 1;
    var_name
}

fn build_match_code(
    var_idx: &mut usize,
    pat: GraphPat,
    ctx: &Ident,
    val: &Ident,
    mut body: TokenStream2,
) -> TokenStream2 {
    // Wrap body with a `val` binding (binds the NodeOutputId to a name).
    if let Some(Binding {
        kind: BindingKind::Value,
        name,
    }) = &pat.binding
    {
        body = quote! {
            {
                let #name = #val;
                #body
            }
        };
    }

    // Determine whether we need a node temp variable.
    let need_node = matches!(
        pat.binding,
        Some(Binding {
            kind: BindingKind::Node,
            ..
        })
    ) || pat.node_match.is_some();

    let node_tmp = if need_node {
        Some(fresh_tmp_var(var_idx))
    } else {
        None
    };

    // Wrap body with a `node` binding (binds the NodeId to a name).
    if let Some(Binding {
        kind: BindingKind::Node,
        name,
    }) = pat.binding
    {
        body = quote! {
            {
                let #name = #node_tmp;
                #body
            }
        };
    }

    if let Some(NodeMatch { kind_pat, inputs }) = pat.node_match {
        // `need_node` (computed above) is true whenever `pat.node_match.is_some()`,
        // so `node_tmp` is unconditionally `Some` here. This `unwrap` runs at
        // proc-macro expansion time; a panic would produce a compile error on a
        // broken invariant, never at runtime.
        #[allow(clippy::unwrap_used)]
        let node_tmp = node_tmp.as_ref().unwrap();

        let input_match = match inputs {
            Some(inputs) => {
                let inner_varnames: Vec<_> =
                    inputs.iter().map(|_| fresh_tmp_var(var_idx)).collect();

                if inner_varnames.is_empty() {
                    // `[]` with no patterns — no inputs check, just run body.
                    body
                } else {
                    let mut inner_match = body;
                    for (inner_pat, inner_var) in inputs.into_iter().zip(&inner_varnames) {
                        inner_match =
                            build_match_code(var_idx, inner_pat, ctx, inner_var, inner_match)
                    }

                    // Emit: if let Ok([__0, __1, ...]) = ctx.node_inputs_exact::<N>(node) { ... }
                    // The const generic N is the number of input patterns.
                    let n = Literal::usize_unsuffixed(inner_varnames.len());
                    quote! {
                        {
                            if let Ok([#(#inner_varnames),*]) =
                                #ctx.node_inputs_exact::<#n>(#node_tmp)
                            {
                                #inner_match
                            }
                        }
                    }
                }
            }
            // No `[...]` — no inputs check.
            None => body,
        };

        // Emit: if let KindPat = *ctx.node_kind(node) { input_match }
        // The `*` dereference is needed because node_kind() returns &NodeKind (which is Copy).
        body = quote! {
            if let #kind_pat = *#ctx.node_kind(#node_tmp) #input_match
        };
    }

    // Emit: let node_tmp = ctx.get_node_from_output(val); { body }
    // Unlike the reference valmatch (which uses value_def and checks output index == 0),
    // we simply resolve the NodeOutputId to its defining NodeId without an index check.
    if let Some(node_tmp) = node_tmp {
        body = quote! {
            {
                let #node_tmp = #ctx.get_node_from_output(#val);
                #body
            }
        };
    }

    body
}

/// Pattern-matches an IR graph value against a structural node pattern.
///
/// # Syntax
///
/// ```text
/// match_value! {
///     if let PATTERN = CTX, VAL {
///         BODY
///     }
/// }
/// ```
///
/// `CTX` is an identifier referring to an IR graph (must have `get_node_from_output`,
/// `node_kind`, and `node_inputs_exact::<N>` methods). `VAL` is a `NodeOutputId`.
///
/// `PATTERN` is a `GraphPat`:
/// - `val name` — bind `VAL` as a `NodeOutputId` to `name`
/// - `node name` — bind the `NodeId` of `VAL`'s defining node to `name`
/// - `val name @ NodeMatch` — bind + structurally match
/// - `node name @ NodeMatch` — bind node + structurally match
/// - `NodeMatch` — structural match only
///
/// `NodeMatch` is `KindPat` or `KindPat[input0, input1, ...]` where each input
/// is itself a `GraphPat` (recursive).
///
/// # Generated Code
///
/// For `match_value! { if let NodeKind::IntBinaryOp(op)[val a, val b] = graph, x { body } }`:
///
/// ```rust,ignore
/// {
///     let __node0 = graph.get_node_from_output(x);
///     if let NodeKind::IntBinaryOp(op) = *graph.node_kind(__node0) {
///         if let Ok([__in0, __in1]) = graph.node_inputs_exact::<2>(__node0) {
///             let a = __in0;
///             let b = __in1;
///             { body }
///         }
///     }
/// }
/// ```
#[proc_macro]
pub fn match_value(input: TokenStream) -> TokenStream {
    let val_match = parse_macro_input!(input as ValueMatch);

    let mut var_idx = 0;
    build_match_code(
        &mut var_idx,
        val_match.pat,
        &val_match.ctx,
        &val_match.val,
        val_match.body.to_token_stream(),
    )
    .into()
}
