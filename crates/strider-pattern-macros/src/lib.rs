//! Proc-macro that emits Rust + PyO3 pattern builders from a single
//! annotated struct definition.
//!
//! See `EMISSION_SPEC.md` next to this file for the emission contract
//! and `crates/strider-py/src/pattern_reference.rs` for the
//! hand-written `PyStackStorePatV2` reference this macro must
//! replicate at the `.pyi` level.
//!
//! ## Usage
//!
//! ```ignore
//! use std::collections::BTreeSet;
//! use strider_pattern_macros::strider_pattern;
//!
//! #[strider_pattern(
//!     rust_name = "PyStackStorePatV2",
//!     py_name = "StackStorePatV2",
//!     py_module = "strider.pattern",
//!     base_builder = "stack_store",
//! )]
//! pub struct StackStorePatDef {
//!     #[field] offset: Option<i64>,
//!     #[field] offset_any: Option<BTreeSet<i64>>,
//!     #[field(accepts = "Pat")] data: Option<pattern::Pat>,
//!     #[field(accepts = "VnSpace")] space: Option<rsleigh::VnSpace>,
//! }
//! ```
//!
//! emits a `PyStackStorePatV2` `#[pyclass]` whose generated `.pyi`
//! matches the hand-written reference byte-for-byte.
//!
//! ## Field annotations
//!
//! - `#[field]` — primitive type (i64, u64, bool, BTreeSet<i64>, …):
//!   the Rust setter takes the type by value, the Python setter
//!   accepts the corresponding Python type via pyo3's `IntoPy`
//!   transparently.  The argument name in the generated method is the
//!   first letter of the field name unless overridden via `arg`.
//!
//! - `#[field(accepts = "Pat")]` — accepts a `PatLike<'py>` and calls
//!   `.into_pat()?`.  Returns `PyResult<PyRef<'py, Self>>` instead of
//!   plain `PyRef<'py, Self>`.
//!
//! - `#[field(accepts = "VnSpace")]` — accepts a `PyVnSpace` and reads
//!   `.inner` to get the underlying `rsleigh::VnSpace`.
//!
//! - `#[field(accepts = "Vn")]` — accepts a `PyVn` and reads
//!   `.inner` to get the underlying `rsleigh::Vn`.
//!
//! - `#[field(multi)]` (combine with `accepts = "Pat"`) — for
//!   accumulating setters that take `(idx, p)` and push onto a
//!   `Vec<(usize, T)>`.  Each call appends an entry; the inner-state
//!   field must be typed `Option<Vec<(usize, T)>>` (or
//!   `Option<Vec<(u32, T)>>`).  The generated `finalise()` walks the
//!   vec and applies `.<py_name>(idx, value)` in insertion order.
//!
//! - `#[field(alias = "py_name")]` — overrides the Python method name
//!   (default: same as the Rust field).
//!
//! - `#[field(arg = "k")]` — overrides the Python argument name in
//!   the generated method signature (default: first letter of the
//!   field name; some fields rename for clarity, e.g. `offset` ->
//!   `k`).
//!
//! - `#[field(doc = "...")]` — the docstring attached to the
//!   generated Python method.  If absent, defaults to a generic
//!   "Set the {name} field" line.
//!
//! ## Crate-attribute knobs
//!
//! - `rust_name = "..."` — the name of the emitted `#[pyclass]`
//!   struct (e.g. `"PyStackStorePatV2"`).
//! - `py_name = "..."` — the Python-visible name (e.g.
//!   `"StackStorePatV2"`).
//! - `py_module = "..."` — the Python module path (e.g.
//!   `"strider.pattern"`).
//! - `base_builder = "..."` — the `pattern::*` free function that
//!   produces an empty builder (e.g. `"stack_store"`).  The
//!   `finalise()` method starts from this builder and applies every
//!   set field.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, Attribute, Expr, ExprLit, Fields, Ident, ItemStruct, Lit, LitStr, Meta,
    Token, Type,
};

// ─── Crate-attribute parsing ────────────────────────────────────────

/// The `key = "value"` pairs supplied to `#[strider_pattern(...)]`.
struct CrateAttrs {
    rust_name: Ident,
    py_name: String,
    py_module: String,
    /// `pattern::*` free function returning the empty builder type.
    base_builder: Ident,
    /// Substituted into the `capture` method docstring's "matched
    /// {node_phrase}" slot — e.g. `"stack-store node"`.  Default:
    /// `"node"`.
    node_phrase: String,
}

impl Parse for CrateAttrs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut rust_name: Option<Ident> = None;
        let mut py_name: Option<String> = None;
        let mut py_module: Option<String> = None;
        let mut base_builder: Option<Ident> = None;
        let mut node_phrase: Option<String> = None;

        // Parse a comma-separated list of `key = "value"` pairs.
        let pairs: syn::punctuated::Punctuated<KeyValue, Token![,]> =
            input.parse_terminated(KeyValue::parse, Token![,])?;
        for kv in pairs {
            let key_str = kv.key.to_string();
            match key_str.as_str() {
                "rust_name" => {
                    rust_name = Some(format_ident!("{}", kv.value.value()));
                }
                "py_name" => {
                    py_name = Some(kv.value.value());
                }
                "py_module" => {
                    py_module = Some(kv.value.value());
                }
                "base_builder" => {
                    base_builder = Some(format_ident!("{}", kv.value.value()));
                }
                "node_phrase" => {
                    node_phrase = Some(kv.value.value());
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        kv.key,
                        format!(
                            "unknown #[strider_pattern] argument `{other}`; expected one of \
                             rust_name, py_name, py_module, base_builder, node_phrase",
                        ),
                    ));
                }
            }
        }

        Ok(Self {
            rust_name: rust_name.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[strider_pattern] requires `rust_name = \"...\"`",
                )
            })?,
            py_name: py_name.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[strider_pattern] requires `py_name = \"...\"`",
                )
            })?,
            py_module: py_module.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[strider_pattern] requires `py_module = \"...\"`",
                )
            })?,
            base_builder: base_builder.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[strider_pattern] requires `base_builder = \"...\"`",
                )
            })?,
            node_phrase: node_phrase.unwrap_or_else(|| "node".to_string()),
        })
    }
}

struct KeyValue {
    key: Ident,
    value: LitStr,
}

impl Parse for KeyValue {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        let _eq: Token![=] = input.parse()?;
        let value: LitStr = input.parse()?;
        Ok(Self { key, value })
    }
}

/// A `#[field(...)]` inner item — either a bare flag (`multi`) or a
/// `key = "value"` pair (`alias = "py_name"`).
enum FieldArg {
    Flag(Ident),
    KeyValue(KeyValue),
}

impl Parse for FieldArg {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if input.peek(Token![=]) {
            let _eq: Token![=] = input.parse()?;
            let value: LitStr = input.parse()?;
            Ok(FieldArg::KeyValue(KeyValue { key, value }))
        } else {
            Ok(FieldArg::Flag(key))
        }
    }
}

// ─── Per-field parsing ──────────────────────────────────────────────

/// The kind of value a field's Python setter accepts.
//
// `large_enum_variant` fires on the `MultiPat { idx_ty: Type }` arm
// because `syn::Type` is large; this enum lives one-per-field during
// macro expansion (a handful of values total) and never crosses a hot
// path, so the indirection isn't worth the noise.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum FieldKind {
    /// Plain primitive — passed by value, stored as `Some(v)`.  Covers
    /// numerics (`i64`, `u64`, `u32`, `bool`, …) as well as `String`
    /// (the finalise step clones via `Clone` because `String` isn't
    /// `Copy` and we can't partial-move out of a `MutexGuard`).
    Primitive,
    /// `PatLike` — calls `.into_pat()?`, returns `PyResult<...>`.
    PatLike,
    /// `PyVnSpace` — reads `.inner` to get the underlying space.
    VnSpace,
    /// `PyVn` — reads `.inner` to get the underlying `rsleigh::Vn`.
    Vn,
    /// `#[field(multi, accepts = "Pat")]` — accumulating `(idx, p)`
    /// setter that pushes onto a `Vec<(usize, Pat)>` (or
    /// `Vec<(u32, Pat)>`).  The carried type is the **tuple-inner**
    /// `T` (i.e. for `Option<Vec<(usize, Pat)>>` the field's
    /// `inner_ty` is `Vec<(usize, Pat)>` and `multi_idx_ty` is
    /// `usize`).
    MultiPat {
        /// The index type — `usize` or `u32`.
        idx_ty: Type,
    },
}

/// One field of the annotated `*Def` struct.  The macro emits one
/// PyO3 setter method per `Field`.
struct Field {
    /// Rust ident in the inner-state struct.
    rust_ident: Ident,
    /// Python method name (defaults to `rust_ident`).
    py_name: String,
    /// Argument name in the generated PyO3 setter signature.
    arg_name: Ident,
    /// Inner-state value type (the `T` inside the field's
    /// `Option<T>`).
    inner_ty: Type,
    /// The kind of setter shape to emit.
    kind: FieldKind,
    /// Docstring for the generated Python method.  `None` -> macro
    /// emits a generic "Set the {name} field" line.
    doc: Option<String>,
}

impl Field {
    fn parse(field: &syn::Field) -> syn::Result<Option<Self>> {
        // Skip fields without `#[field]` annotation entirely — they're
        // hidden state.  The reference uses `when` and `capture`
        // unannotated; we emit those universally.
        let Some(attr) = field
            .attrs
            .iter()
            .find(|a| a.path().is_ident("field"))
        else {
            return Ok(None);
        };

        // Extract `T` from `Option<T>` — every field must be `Option<...>`
        // because the inner-state struct uses `Option<T>` to track
        // "field set vs unset".
        let inner_ty = extract_option_inner(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &field.ty,
                "#[field] requires the type to be `Option<T>`",
            )
        })?;

        let rust_ident = field
            .ident
            .clone()
            .ok_or_else(|| syn::Error::new_spanned(field, "named fields only"))?;

        let mut alias: Option<String> = None;
        let mut arg: Option<String> = None;
        let mut accepts: Option<String> = None;
        let mut doc: Option<String> = None;
        let mut multi: bool = false;

        // Parse the inner attribute meta items.  Each item is either a
        // bare flag like `multi` or a `key = "value"` pair.  We accept
        // both shapes in any order, e.g. `#[field(multi, accepts =
        // "Pat", arg = "p")]`.
        if !matches!(attr.meta, Meta::Path(_)) {
            // `#[field(...)]` form.
            let nested = attr.parse_args_with(
                syn::punctuated::Punctuated::<FieldArg, Token![,]>::parse_terminated,
            )?;
            for item in nested {
                match item {
                    FieldArg::Flag(ident) => {
                        let s = ident.to_string();
                        match s.as_str() {
                            "multi" => multi = true,
                            other => {
                                return Err(syn::Error::new_spanned(
                                    &ident,
                                    format!(
                                        "unknown #[field(...)] flag `{other}`; expected `multi`",
                                    ),
                                ));
                            }
                        }
                    }
                    FieldArg::KeyValue(kv) => {
                        let key_str = kv.key.to_string();
                        match key_str.as_str() {
                            "alias" => alias = Some(kv.value.value()),
                            "arg" => arg = Some(kv.value.value()),
                            "accepts" => accepts = Some(kv.value.value()),
                            "doc" => doc = Some(kv.value.value()),
                            other => {
                                return Err(syn::Error::new_spanned(
                                    kv.key,
                                    format!(
                                        "unknown #[field(...)] key `{other}`; expected one of \
                                         alias, arg, accepts, doc, or the flag `multi`",
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Pull a Rust `///` docstring off the field if no `doc = "..."`
        // override was given.  Multi-line doc comments concatenate
        // with newlines (matches pyo3-stub-gen's emission).
        if doc.is_none() {
            doc = extract_rust_doc(&field.attrs);
        }

        let kind = if multi {
            // `multi` requires `accepts = "Pat"` and the field must be
            // typed `Option<Vec<(IDX, T)>>`.  Extract `IDX` from the
            // tuple so the generated setter has the right signature.
            match accepts.as_deref() {
                Some("Pat") => {
                    let idx_ty = extract_multi_idx_ty(&inner_ty).ok_or_else(|| {
                        syn::Error::new_spanned(
                            &field.ty,
                            "#[field(multi, accepts = \"Pat\")] requires \
                             `Option<Vec<(usize, T)>>` or `Option<Vec<(u32, T)>>`",
                        )
                    })?;
                    FieldKind::MultiPat { idx_ty }
                }
                Some(_) | None => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "`#[field(multi)]` currently requires `accepts = \"Pat\"`",
                    ));
                }
            }
        } else {
            match accepts.as_deref() {
                None => FieldKind::Primitive,
                Some("Pat") => FieldKind::PatLike,
                Some("VnSpace") => FieldKind::VnSpace,
                Some("Vn") => FieldKind::Vn,
                Some(other) => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        format!(
                            "unknown #[field(accepts = \"{other}\")] target; expected one of \
                             \"Pat\", \"VnSpace\", \"Vn\"",
                        ),
                    ));
                }
            }
        };

        let py_name = alias.unwrap_or_else(|| rust_ident.to_string());
        let arg_name = match arg {
            Some(s) => format_ident!("{}", s),
            // Default argument name: most setters use the same name
            // as the Rust field.  The reference's `offset(k: i64)`
            // overrides this via `#[field(arg = "k")]`.
            None => rust_ident.clone(),
        };

        Ok(Some(Self {
            rust_ident,
            py_name,
            arg_name,
            inner_ty,
            kind,
            doc,
        }))
    }
}

/// Extract the index type `IDX` from `Vec<(IDX, T)>`.  Returns `None`
/// if the type isn't a 2-tuple-typed `Vec`.  Used by the `multi`
/// field kind to wire the right `idx: usize` / `idx: u32` setter
/// signature.
fn extract_multi_idx_ty(ty: &Type) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    if last.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    for arg in &args.args {
        if let syn::GenericArgument::Type(Type::Tuple(tup)) = arg
            && tup.elems.len() == 2
        {
            return Some(tup.elems[0].clone());
        }
    }
    None
}

/// Unwrap `Option<T>` -> `T`.  Returns `None` if the type isn't an
/// `Option<...>` path.
fn extract_option_inner(ty: &Type) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    for arg in &args.args {
        if let syn::GenericArgument::Type(inner) = arg {
            return Some(inner.clone());
        }
    }
    None
}

/// Concatenate every `#[doc = "..."]` attribute into a single string
/// (newline-separated).  Trims the leading single space pyo3 + rustdoc
/// emit for `/// text` -> `#[doc = " text"]`.
fn extract_rust_doc(attrs: &[Attribute]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = &nv.value
        else {
            continue;
        };
        let raw = s.value();
        // `/// foo` lowers to `#[doc = " foo"]`; strip exactly one
        // leading space to match rustdoc canonical form.
        let trimmed = raw.strip_prefix(' ').unwrap_or(&raw).to_string();
        lines.push(trimmed);
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

// ─── Code emission ─────────────────────────────────────────────────

#[proc_macro_attribute]
pub fn strider_pattern(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as CrateAttrs);
    let input = parse_macro_input!(item as ItemStruct);

    let fields = match collect_fields(&input) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    let inner_struct = build_inner_struct(&input.ident, &fields);
    let inner_ident = format_ident!("{}Inner", &input.ident);
    let pyclass_struct = build_pyclass_struct(&attrs, &inner_ident, &input);
    let finalise_impl = build_finalise_impl(&attrs, &inner_ident, &fields);
    let pymethods_impl = build_pymethods_impl(&attrs, &inner_ident, &fields);

    let expanded = quote! {
        #inner_struct
        #pyclass_struct
        #finalise_impl
        #pymethods_impl
    };
    expanded.into()
}

fn collect_fields(input: &ItemStruct) -> syn::Result<Vec<Field>> {
    let Fields::Named(named) = &input.fields else {
        return Err(syn::Error::new_spanned(
            input,
            "#[strider_pattern] requires a struct with named fields",
        ));
    };
    let mut out = Vec::with_capacity(named.named.len());
    for field in &named.named {
        if let Some(f) = Field::parse(field)? {
            out.push(f);
        }
    }
    Ok(out)
}

/// Emit the `*Inner` struct that holds the builder's accumulated
/// state.  Each `#[field]` becomes an `Option<T>`; the universal
/// `capture` / `when` slots are always present.
fn build_inner_struct(def_ident: &Ident, fields: &[Field]) -> TokenStream2 {
    let inner_ident = format_ident!("{}Inner", def_ident);
    let field_decls = fields.iter().map(|f| {
        let ident = &f.rust_ident;
        let ty = &f.inner_ty;
        quote! { #ident: ::core::option::Option<#ty>, }
    });
    quote! {
        #[derive(::core::default::Default)]
        struct #inner_ident {
            #(#field_decls)*
            when: ::core::option::Option<::pyo3::PyObject>,
            capture: ::core::option::Option<::pattern::Capture>,
        }
    }
}

/// Emit the `#[gen_stub_pyclass] #[pyclass(...)]` struct with the
/// `Arc<Mutex<Inner>>` storage shape.
fn build_pyclass_struct(
    attrs: &CrateAttrs,
    inner_ident: &Ident,
    input: &ItemStruct,
) -> TokenStream2 {
    let rust_name = &attrs.rust_name;
    let py_name = &attrs.py_name;
    let py_module = &attrs.py_module;

    // Forward the original struct's documentation onto the emitted
    // pyclass so rustdoc + pyo3-stub-gen both see it.
    let docs: Vec<_> = input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .collect();

    // IMPORTANT: `#[gen_stub_pyclass]` walks the next attribute by
    // ident path; it only recognises a bare `pyclass` (not the
    // fully-qualified `::pyo3::pyclass`).  We emit both attributes
    // unqualified so the consumer must have `use pyo3::prelude::*;`
    // and `use pyo3_stub_gen::derive::{gen_stub_pyclass, …};` in
    // scope, which is the contract every PyO3 file already follows.
    quote! {
        #(#docs)*
        #[gen_stub_pyclass]
        #[pyclass(name = #py_name, module = #py_module)]
        pub struct #rust_name {
            inner: ::std::sync::Arc<::std::sync::Mutex<#inner_ident>>,
        }
    }
}

/// Emit the `impl PyXxx { pub(crate) fn finalise(...) -> pattern::Pat
/// { ... } }` block.  Locks the mutex once and applies every set
/// field via the base builder's typed setters.
fn build_finalise_impl(
    attrs: &CrateAttrs,
    _inner_ident: &Ident,
    fields: &[Field],
) -> TokenStream2 {
    let rust_name = &attrs.rust_name;
    let base_builder = &attrs.base_builder;

    let apply_fields = fields.iter().map(|f| {
        let rust_ident = &f.rust_ident;
        let py_name = format_ident!("{}", &f.py_name);
        match &f.kind {
            FieldKind::Primitive => {
                // BTreeSet fields need `.iter().copied().collect()`
                // so the v1 builder's `Vec<T>` API takes them by
                // value.  We detect this purely from the *Python*
                // method name: by convention every BTreeSet field
                // is exposed as `*_any` and the v1 builder method
                // matches that name.  Plain primitives forward by
                // value.
                if is_btreeset(&f.inner_ty) {
                    let inner_t = btreeset_inner(&f.inner_ty);
                    quote! {
                        if let ::core::option::Option::Some(ref set) = guard.#rust_ident {
                            b = b.#py_name(set.iter().copied().collect::<::std::vec::Vec<#inner_t>>());
                        }
                    }
                } else if is_vec(&f.inner_ty) {
                    // Vec<T> isn't Copy, so we can't move out of the
                    // MutexGuard; clone the inner vector instead.  The
                    // underlying builder takes `IntoIterator<Item = T>`,
                    // so a clone'd `Vec<T>` is accepted by value.
                    quote! {
                        if let ::core::option::Option::Some(ref v) = guard.#rust_ident {
                            b = b.#py_name(::core::clone::Clone::clone(v));
                        }
                    }
                } else if is_string(&f.inner_ty) {
                    // `String` isn't `Copy`, and we can't partial-move
                    // out of a `MutexGuard` (it derefs to `&mut T`).
                    // Clone the contents instead — the underlying
                    // builder typically takes `impl Into<String>` which
                    // accepts an owned `String` by value.
                    quote! {
                        if let ::core::option::Option::Some(ref s) = guard.#rust_ident {
                            b = b.#py_name(::core::clone::Clone::clone(s));
                        }
                    }
                } else {
                    quote! {
                        if let ::core::option::Option::Some(v) = guard.#rust_ident {
                            b = b.#py_name(v);
                        }
                    }
                }
            }
            FieldKind::PatLike => {
                quote! {
                    if let ::core::option::Option::Some(ref p) = guard.#rust_ident {
                        b = b.#py_name(::core::clone::Clone::clone(p));
                    }
                }
            }
            // `VnSpace` and `Vn` both stash a `Copy` rsleigh varnode in
            // the inner state.  The finalise body is identical — the
            // setter signature is the only difference, handled in
            // `emit_field_method`.
            FieldKind::VnSpace | FieldKind::Vn => {
                quote! {
                    if let ::core::option::Option::Some(v) = guard.#rust_ident {
                        b = b.#py_name(v);
                    }
                }
            }
            FieldKind::MultiPat { .. } => {
                // The vec entries are `(idx, Pat)`.  We can't move out
                // of the MutexGuard, so iterate-by-reference and clone
                // each Pat (cheap — `Pat` is `Arc`-backed).
                quote! {
                    if let ::core::option::Option::Some(ref entries) = guard.#rust_ident {
                        for &(idx, ref p) in entries.iter() {
                            b = b.#py_name(idx, ::core::clone::Clone::clone(p));
                        }
                    }
                }
            }
        }
    });

    quote! {
        impl #rust_name {
            /// Build the underlying [`pattern::Pat`] from the
            /// accumulated builder state.  Locks the inner `Mutex`
            /// once; recovers from a poisoned lock via
            /// `into_inner()` (parity with the v1 hand-written
            /// reference's `intern_table` recovery — keeps the type
            /// usable even after a future panicking method is added).
            pub(crate) fn finalise(&self) -> ::pattern::Pat {
                let guard = self
                    .inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let mut b = ::pattern::#base_builder();
                #(#apply_fields)*
                let mut pat: ::pattern::Pat = b.into();
                if let ::core::option::Option::Some(c) = guard.capture {
                    use ::pattern::IntoPat;
                    pat = pat.capture(c);
                }
                if let ::core::option::Option::Some(ref f) = guard.when {
                    let f_clone = ::pyo3::Python::with_gil(|py| f.clone_ref(py));
                    pat = crate::pattern::wrap_when(pat, f_clone);
                }
                pat
            }
        }
    }
}

/// `true` if `ty` is `Vec<...>` (any path ending in `Vec<...>`).
fn is_vec(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    last.ident == "Vec"
}

/// `true` if `ty` is `String` (path ends in `String`).
fn is_string(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    last.ident == "String"
}

/// `true` if `ty` is `BTreeSet<...>` (any path ending in
/// `BTreeSet<...>`).
fn is_btreeset(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    last.ident == "BTreeSet"
}

/// Extract `T` from `BTreeSet<T>` (call only when `is_btreeset`
/// returns true).
fn btreeset_inner(ty: &Type) -> Type {
    let Type::Path(path) = ty else {
        return syn::parse_quote! { () };
    };
    let Some(last) = path.path.segments.last() else {
        return syn::parse_quote! { () };
    };
    if let syn::PathArguments::AngleBracketed(args) = &last.arguments {
        for arg in &args.args {
            if let syn::GenericArgument::Type(t) = arg {
                return t.clone();
            }
        }
    }
    syn::parse_quote! { () }
}

/// Emit the `#[gen_stub_pymethods] #[pymethods] impl PyXxx { ... }`
/// block containing `#[new]`, every field setter, and the four
/// universal `capture` / `cap` / `when` / `into_pat` methods.
fn build_pymethods_impl(
    attrs: &CrateAttrs,
    _inner_ident: &Ident,
    fields: &[Field],
) -> TokenStream2 {
    let rust_name = &attrs.rust_name;
    let node_phrase = &attrs.node_phrase;

    // Build the capture-method docstring with the node-phrase
    // substituted in.  Mirrors the V2 reference's verbose form so
    // the macro-generated `.pyi` matches byte-for-byte when
    // `node_phrase = "stack-store node"`.
    let capture_doc_line1 = format!(" Capture the matched {node_phrase} under the given");
    let capture_doc_line2 = " [`Capture`].  Mirrors the v1 `pat_builder_finalise!`-emitted";
    let capture_doc_line3 = " `.capture(c)`.";

    let field_methods = fields.iter().map(|f| emit_field_method(rust_name, f));

    quote! {
        // Same path-recognition constraint as `gen_stub_pyclass` —
        // emit `pymethods` / `gen_stub_pymethods` unqualified.
        #[gen_stub_pymethods]
        #[pymethods]
        impl #rust_name {
            /// Construct an empty builder.  All fields default to
            /// `None`; `finalise()` produces the unconstrained
            /// pattern until a field is set.
            #[new]
            fn new() -> Self {
                Self {
                    inner: ::std::sync::Arc::new(::std::sync::Mutex::new(
                        ::core::default::Default::default(),
                    )),
                }
            }

            #(#field_methods)*

            #[doc = #capture_doc_line1]
            #[doc = #capture_doc_line2]
            #[doc = #capture_doc_line3]
            fn capture<'py>(
                slf: ::pyo3::PyRef<'py, Self>,
                c: ::pyo3::PyRef<'py, crate::pattern::PyCapture>,
            ) -> ::pyo3::PyRef<'py, Self> {
                let mut guard = slf
                    .inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                guard.capture = ::core::option::Option::Some(c.inner);
                ::core::mem::drop(guard);
                slf
            }

            /// Capture under a string name (auto-interned).  Reserved names
            /// (`"_"`, `"any_"`) raise `PatternError`.
            fn cap<'py>(
                slf: ::pyo3::PyRef<'py, Self>,
                name: &'py str,
            ) -> ::pyo3::PyResult<::pyo3::PyRef<'py, Self>> {
                let c = crate::pattern::intern_str(name)?;
                let mut guard = slf
                    .inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                guard.capture = ::core::option::Option::Some(c);
                ::core::mem::drop(guard);
                ::core::result::Result::Ok(slf)
            }

            /// Attach a Python predicate that runs after the match.
            /// See `PyPat::when` for the full predicate contract; the
            /// predicate receives a `PartialMatch` proxy and returns a bool.
            fn when(
                slf: ::pyo3::PyRef<'_, Self>,
                f: ::pyo3::PyObject,
            ) -> ::pyo3::PyRef<'_, Self> {
                let mut guard = slf
                    .inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                guard.when = ::core::option::Option::Some(f);
                ::core::mem::drop(guard);
                slf
            }

            /// Finalise into a [`Pat`].  Most call sites accept this builder
            /// directly via `PatLike`, so explicit `.into_pat()` is rarely
            /// needed.
            fn into_pat(&self) -> crate::pattern::PyPat {
                crate::pattern::PyPat::from_pat(self.finalise())
            }
        }
    }
}

/// Emit one PyO3 setter method for `field`.  The shape depends on
/// `field.kind` — `PatLike` needs `PyResult<...>`, others return
/// plain `PyRef`.
fn emit_field_method(_rust_name: &Ident, field: &Field) -> TokenStream2 {
    let py_name = format_ident!("{}", &field.py_name);
    let arg_name = &field.arg_name;
    let rust_ident = &field.rust_ident;

    // Doc-comment lines.  When `field.doc` is set, splice each line
    // as a separate `#[doc = "..."]` attribute so multi-line docs
    // round-trip through pyo3-stub-gen unchanged.
    let doc_attrs = field
        .doc
        .as_deref()
        .map(|d| {
            d.split('\n')
                .map(|line| {
                    // Prefix one leading space — rustdoc canonical form
                    // (matches what `///` produces).
                    let with_lead = format!(" {line}");
                    let lit = LitStr::new(&with_lead, proc_macro2::Span::call_site());
                    quote! { #[doc = #lit] }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    match &field.kind {
        FieldKind::Primitive => {
            let ty = &field.inner_ty;
            quote! {
                #(#doc_attrs)*
                fn #py_name(
                    slf: ::pyo3::PyRef<'_, Self>,
                    #arg_name: #ty,
                ) -> ::pyo3::PyRef<'_, Self> {
                    let mut guard = slf
                        .inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    guard.#rust_ident = ::core::option::Option::Some(#arg_name);
                    ::core::mem::drop(guard);
                    slf
                }
            }
        }
        FieldKind::PatLike => {
            quote! {
                #(#doc_attrs)*
                fn #py_name<'py>(
                    slf: ::pyo3::PyRef<'py, Self>,
                    #arg_name: crate::pattern::PatLike<'py>,
                ) -> ::pyo3::PyResult<::pyo3::PyRef<'py, Self>> {
                    let pat = #arg_name.into_pat()?;
                    let mut guard = slf
                        .inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    guard.#rust_ident = ::core::option::Option::Some(pat);
                    ::core::mem::drop(guard);
                    ::core::result::Result::Ok(slf)
                }
            }
        }
        FieldKind::VnSpace => {
            quote! {
                #(#doc_attrs)*
                fn #py_name(
                    slf: ::pyo3::PyRef<'_, Self>,
                    #arg_name: crate::sleigh::PyVnSpace,
                ) -> ::pyo3::PyRef<'_, Self> {
                    let mut guard = slf
                        .inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    guard.#rust_ident = ::core::option::Option::Some(#arg_name.inner);
                    ::core::mem::drop(guard);
                    slf
                }
            }
        }
        FieldKind::Vn => {
            quote! {
                #(#doc_attrs)*
                fn #py_name(
                    slf: ::pyo3::PyRef<'_, Self>,
                    #arg_name: crate::sleigh::PyVn,
                ) -> ::pyo3::PyRef<'_, Self> {
                    let mut guard = slf
                        .inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    guard.#rust_ident = ::core::option::Option::Some(#arg_name.inner);
                    ::core::mem::drop(guard);
                    slf
                }
            }
        }
        FieldKind::MultiPat { idx_ty } => {
            // Accumulating two-arg setter: `.method(idx, p)` pushes
            // `(idx, p)` onto the inner `Vec<(IDX, Pat)>`.  The Vec is
            // lazily allocated on first push so an unset field stays
            // `None` (matching the contract of non-multi fields).
            quote! {
                #(#doc_attrs)*
                fn #py_name<'py>(
                    slf: ::pyo3::PyRef<'py, Self>,
                    #arg_name: #idx_ty,
                    p: crate::pattern::PatLike<'py>,
                ) -> ::pyo3::PyResult<::pyo3::PyRef<'py, Self>> {
                    let pat = p.into_pat()?;
                    let mut guard = slf
                        .inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    guard
                        .#rust_ident
                        .get_or_insert_with(::std::vec::Vec::new)
                        .push((#arg_name, pat));
                    ::core::mem::drop(guard);
                    ::core::result::Result::Ok(slf)
                }
            }
        }
    }
}

