//! Macro helpers for the builder types in [`crate::pat`].
//!
//! The pattern DSL has a dozen Option-field builders whose bodies are nearly
//! identical: a private `new()` initializing every field to `None`/empty, one
//! fluent setter per field that wraps the argument in `Some`/pushes to a `Vec`,
//! plus a `From<Builder> for Pat` that forwards every field into the
//! corresponding `PatKind` variant.  [`define_pat_builder!`] collapses that
//! boilerplate.
//!
//! ### Field flavors
//!
//! | Syntax                                  | Storage                 | Setter signature                                              |
//! |-----------------------------------------|-------------------------|---------------------------------------------------------------|
//! | `opt name: Ty;`                         | `Option<Ty>`            | `fn name(self, v: Ty) -> Self`                                |
//! | `opt name: Ty as setter;`               | `Option<Ty>`            | `fn setter(self, v: Ty) -> Self`                              |
//! | `pat name;`                             | `Option<Pat>`           | `fn name(self, p: impl Into<Pat>) -> Self`                    |
//! | `pat name as setter;`                   | `Option<Pat>`           | `fn setter(self, p: impl Into<Pat>) -> Self`                  |
//! | `vec_pat name push setter;`             | `Vec<(usize, Pat)>`     | `fn setter(self, idx: usize, p: impl Into<Pat>) -> Self`      |
//!
//! Every flavor accepts doc-comment attributes before its spec — they attach
//! to the generated setter.
//!
//! ### Struct / variant names
//!
//! The variant field names on `PatKind::$Variant` must match the struct field
//! names exactly — the macro moves them through by name.  For the one builder
//! whose `PatKind` field name differs (`ret_vals`), define the builder field
//! with that same name.
//!
//! ### Extras
//!
//! An optional trailing `extra { … }` block is pasted verbatim into an
//! additional inherent impl — use it for sugar that can't be expressed as a
//! plain setter (e.g. `CallPat::at` delegating through `.target(...)`, or
//! `StackStorePhiPat::offsets` with sort-on-set semantics).

/// Generates a `Pat` builder struct, its fluent setters, and the
/// `From<Builder> for Pat` glue.  See the module-level docs for the field
/// syntax.
macro_rules! define_pat_builder {
    (
        $(#[$struct_attr:meta])*
        $vis:vis struct $Name:ident => PatKind::$Variant:ident {
            $($body:tt)*
        }
        $( extra { $($extra:tt)* } )?
    ) => {
        $crate::pat_macros::__pat_builder_parse! {
            @parse
            meta       = [ $(#[$struct_attr])* ],
            vis        = [$vis],
            name       = [$Name],
            variant    = [$Variant],
            extra      = [ $($( $extra )*)? ],
            fields     = [],
            inits      = [],
            setters    = [],
            moves      = [],
            remaining  = { $($body)* },
        }
    };
}

/// Internal TT-muncher that walks the field-spec list one entry at a time,
/// accumulating struct declarations, `new()` initializers, setter fns, and
/// `From` field moves, then emits the full definition on the terminal arm.
///
/// This macro is a crate-internal implementation detail of
/// [`define_pat_builder!`].
macro_rules! __pat_builder_parse {
    // ── Terminal: emit the full definition ──────────────────────────────────
    (@parse
        meta       = [$($meta:tt)*],
        vis        = [$vis:vis],
        name       = [$Name:ident],
        variant    = [$Variant:ident],
        extra      = [$($extra:tt)*],
        fields     = [$($fields:tt)*],
        inits      = [$($inits:tt)*],
        setters    = [$($setters:tt)*],
        moves      = [$($moves:tt)*],
        remaining  = { },
    ) => {
        $($meta)*
        $vis struct $Name {
            $($fields)*
        }

        impl $Name {
            pub(crate) fn new() -> Self {
                Self { $($inits)* }
            }
            $($setters)*
            $($extra)*
        }

        impl From<$Name> for $crate::pat::Pat {
            fn from(b: $Name) -> $crate::pat::Pat {
                let $Name { $($moves)* } = b;
                $crate::pat::Pat::new($crate::pat::PatKind::$Variant { $($moves)* })
            }
        }
    };

    // ── `opt_field NAME: TY;` — Option<TY> with no auto-setter (extra { } provides one) ─
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { opt_field $f:ident : $ty:ty ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::option::Option<$ty>,],
            inits   = [$($is)* $f: ::std::option::Option::None,],
            setters = [$($ss)*],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };

    // ── `opt NAME: TY;` — setter name == field name ─────────────────────────
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { $(#[$fa:meta])* opt $f:ident : $ty:ty ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::option::Option<$ty>,],
            inits   = [$($is)* $f: ::std::option::Option::None,],
            setters = [$($ss)*
                $(#[$fa])*
                pub fn $f(mut self, v: $ty) -> Self {
                    self.$f = ::std::option::Option::Some(v);
                    self
                }
            ],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };

    // ── `opt NAME: TY as SETTER;` — renamed setter ──────────────────────────
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { $(#[$fa:meta])* opt $f:ident : $ty:ty as $s:ident ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::option::Option<$ty>,],
            inits   = [$($is)* $f: ::std::option::Option::None,],
            setters = [$($ss)*
                $(#[$fa])*
                pub fn $s(mut self, v: $ty) -> Self {
                    self.$f = ::std::option::Option::Some(v);
                    self
                }
            ],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };

    // ── `pat NAME;` — Option<Pat>, setter takes impl Into<Pat> ──────────────
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { $(#[$fa:meta])* pat $f:ident ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::option::Option<$crate::pat::Pat>,],
            inits   = [$($is)* $f: ::std::option::Option::None,],
            setters = [$($ss)*
                $(#[$fa])*
                pub fn $f(mut self, p: impl ::std::convert::Into<$crate::pat::Pat>) -> Self {
                    self.$f = ::std::option::Option::Some(p.into());
                    self
                }
            ],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };

    // ── `pat NAME as SETTER;` — Option<Pat> with renamed setter ─────────────
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { $(#[$fa:meta])* pat $f:ident as $s:ident ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::option::Option<$crate::pat::Pat>,],
            inits   = [$($is)* $f: ::std::option::Option::None,],
            setters = [$($ss)*
                $(#[$fa])*
                pub fn $s(mut self, p: impl ::std::convert::Into<$crate::pat::Pat>) -> Self {
                    self.$f = ::std::option::Option::Some(p.into());
                    self
                }
            ],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };

    // ── `vec_pat NAME push SETTER;` — Vec<(usize, Pat)> push setter ─────────
    (@parse
        meta = [$($meta:tt)*], vis = [$vis:vis], name = [$N:ident], variant = [$V:ident],
        extra = [$($ex:tt)*],
        fields = [$($fs:tt)*], inits = [$($is:tt)*], setters = [$($ss:tt)*], moves = [$($ms:tt)*],
        remaining = { $(#[$fa:meta])* vec_pat $f:ident push $s:ident ; $($rest:tt)* },
    ) => {
        $crate::pat_macros::__pat_builder_parse! { @parse
            meta = [$($meta)*], vis = [$vis], name = [$N], variant = [$V],
            extra = [$($ex)*],
            fields  = [$($fs)* $f: ::std::vec::Vec<(usize, $crate::pat::Pat)>,],
            inits   = [$($is)* $f: ::std::vec::Vec::new(),],
            setters = [$($ss)*
                $(#[$fa])*
                pub fn $s(mut self, idx: usize, p: impl ::std::convert::Into<$crate::pat::Pat>) -> Self {
                    self.$f.push((idx, p.into()));
                    self
                }
            ],
            moves   = [$($ms)* $f,],
            remaining = { $($rest)* },
        }
    };
}

pub(crate) use {__pat_builder_parse, define_pat_builder};
