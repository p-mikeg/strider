//! `rewrite_rules!` proc-macro DSL.
//!
//! Emits a closure
//! `|fg: &mut BuiltFunctionGraph, node: NodeId| -> Result<OptimizationResult, opt::Error>`
//! that applies a list of pattern-rewrite rules in declaration order.
//!
//! Grammar and semantics: see
//! `docs/superpowers/specs/2026-04-17-code-quality-refactor-design.md`.

use proc_macro2::TokenStream;
use quote::quote;

mod parse;

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    match try_expand(input) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error(),
    }
}

fn try_expand(input: TokenStream) -> syn::Result<TokenStream> {
    let rules: parse::Rules = syn::parse2(input)?;

    let rule_fns: Vec<TokenStream> = rules
        .rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let name = quote::format_ident!("__rewrite_rule_{}", i);
            r.codegen(&name)
        })
        .collect::<syn::Result<_>>()?;

    let dispatch_arms: Vec<TokenStream> = (0..rules.rules.len())
        .map(|i| {
            let name = quote::format_ident!("__rewrite_rule_{}", i);
            quote! {
                if let opt::OptimizationResult::Changed = #name(fg, node)? {
                    __any_changed = true;
                }
            }
        })
        .collect();

    Ok(quote! {
        {
            // ── per-rule functions emitted by `rewrite_rules!` ──────────────
            #( #rule_fns )*

            // ── dispatcher: calls each rule in order, returns aggregate result
            #[allow(non_snake_case)]
            |fg: &mut ir::BuiltFunctionGraph, node: ir::node::NodeId|
                -> ::core::result::Result<opt::OptimizationResult, opt::Error>
            {
                let mut __any_changed = false;
                #( #dispatch_arms )*
                ::core::result::Result::Ok(opt::OptimizationResult::from_changed(__any_changed))
            }
        }
    })
}
