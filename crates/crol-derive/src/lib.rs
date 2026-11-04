//! Derive macros for CROL: `#[derive(Schema)]`, `#[derive(VariantName)]` and
//! `#[derive(View)]`, re-exported by `crol` with the cargo feature `derive`
//! (`SPEC.md` 8.1, 10.9, 10.11).

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, ExprLit, Fields, Lit, LitBool, LitStr, Meta, Path,
    Token, Type, parse_macro_input,
};

/// Implements `crol::schema::Schema` from a type's fields and serde
/// attributes (`SPEC.md` 10.9).
///
/// Attributes, besides serde's own:
///
/// - `#[crol(defaults)]` on the type: record its default values;
/// - `#[crol(explicit_defaults)]` or `#[crol(explicit_defaults = false)]`
///   on a struct, an enum (for its struct variants) or a struct variant:
///   whether fields left out take their defaults only with `..`;
/// - `#[crol(refers(Service, Volume))]` on an id field: the root types its
///   ids may name;
/// - `#[crol(schema = OtherType)]` or `#[crol(schema = any)]` on a field:
///   describe it as another type with the same serialized shape, or as
///   anything;
/// - `#[crol(constraint = "name")]` on a field, or on a newtype or transparent
///   struct, and `#[crol(key_constraint = "name")]` on a map field: the
///   constraint on its value, or on the map's keys;
/// - `#[crol(constraints(name = Min(0)))]` on a type: define sealed
///   constraints, and `#[crol(open_constraints(...))]` ones that a
///   `schema.crod` may change;
/// - `#[crol(sealed)]` on a field, or on a type for all of its fields: only
///   the layer that defines a value can set it.
#[proc_macro_derive(Schema, attributes(crol))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    schema(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Implements `crol::VariantName` for an enum without fields: each variant
/// is named in snake case (`ProdEu` is `prod_eu`), unless
/// `#[crol(rename = "...")]` names it (`SPEC.md` 8.1).
#[proc_macro_derive(VariantName, attributes(crol))]
pub fn derive_variant_name(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    variant_name(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Implements `crol::View`: each field reads the value's field of its own
/// name, or the path in `#[crol(path = "combat.range.max")]`, from a struct
/// with named fields (`SPEC.md` 10.11). An `Option` is `None` where the path
/// is missing, and `#[crol(default)]` on a field takes its `Default` there.
#[proc_macro_derive(View, attributes(crol))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    view(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn view(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            input,
            "View is derived for a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            input,
            "View is derived for a struct with named fields",
        ));
    };
    let mut reads = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("a named field");
        let mut path = ident.to_string();
        let mut default = false;
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("crol")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("path") {
                    path = meta.value()?.parse::<LitStr>()?.value();
                    Ok(())
                } else if meta.path.is_ident("default") {
                    default = true;
                    Ok(())
                } else {
                    Err(meta.error("expected `path = \"...\"` or `default`"))
                }
            })?;
        }
        let steps = path.split('.');
        let ty = &field.ty;
        let read = if let Some(inner) = option_inner(ty) {
            quote!(source.optional::<#inner>(&[#(#steps),*])?)
        } else if default {
            quote!(source.optional::<#ty>(&[#(#steps),*])?.unwrap_or_default())
        } else {
            quote!(source.field::<#ty>(&[#(#steps),*])?)
        };
        reads.push(quote!(#ident: #read));
    }
    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::crol::View for #name #type_generics #where_clause {
            fn read(source: &::crol::ViewSource<'_>) -> ::std::result::Result<Self, ::crol::Error> {
                ::std::result::Result::Ok(Self { #(#reads),* })
            }
        }
    })
}

/// `T` of an `Option<T>`.
fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let last = path.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

// ---- Attributes ----

/// A `Constraint` from what `#[crol(constraints(...))]` writes: `Min(0)`,
/// `Max(10)`, `Range(0, 10)`, `OneOf(["a", "b"])`, `Refers(Unit, Tech)`.
fn constraint_expr(expr: &Expr) -> syn::Result<TokenStream2> {
    let wrong = || {
        Error::new(
            expr.span(),
            "expected `Min(n)`, `Max(n)`, `Range(a, b)`, `OneOf([...])` or `Refers(Type, ...)`",
        )
    };
    let Expr::Call(call) = expr else {
        return Err(wrong());
    };
    let Expr::Path(function) = &*call.func else {
        return Err(wrong());
    };
    let name = function.path.get_ident().ok_or_else(wrong)?.to_string();
    let args: Vec<&Expr> = call.args.iter().collect();
    Ok(match (name.as_str(), args.as_slice()) {
        ("Min", [n]) => quote!(::crol::schema::Constraint::Min((#n) as f64)),
        ("Max", [n]) => quote!(::crol::schema::Constraint::Max((#n) as f64)),
        ("Range", [a, b]) => {
            quote!(::crol::schema::Constraint::Range((#a) as f64, (#b) as f64))
        }
        ("OneOf", [values]) => quote!(::crol::schema::Constraint::one_of(#values)),
        ("Refers", types) if !types.is_empty() => {
            let names = types
                .iter()
                .map(|ty| match ty {
                    Expr::Path(path) => path
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .ok_or_else(wrong),
                    _ => Err(wrong()),
                })
                .collect::<syn::Result<Vec<String>>>()?;
            quote!(::crol::schema::Constraint::Refers(
                ::std::vec![#(::std::string::String::from(#names)),*]
            ))
        }
        _ => return Err(wrong()),
    })
}

/// The serde and crol attributes of a type, variant or field.
#[derive(Default)]
struct Attrs {
    rename: Option<String>,
    rename_all: Option<String>,
    aliases: Vec<String>,
    /// `#[serde(default)]`: `Some(None)`; `#[serde(default = "path")]`:
    /// `Some(Some(path))`.
    default: Option<Option<Path>>,
    skip: bool,
    flatten: bool,
    transparent: bool,
    untagged: bool,
    tag: Option<String>,
    content: Option<String>,
    deny_unknown_fields: bool,
    with: bool,
    defaults: bool,
    explicit_defaults: Option<bool>,
    refers: Vec<Path>,
    /// `#[crol(constraint = "...")]`
    constraint: Option<String>,
    /// `#[crol(key_constraint = "...")]`
    key_constraint: Option<String>,
    /// `#[crol(sealed)]`
    sealed: bool,
    /// `#[crol(constraints(...))]` and `#[crol(open_constraints(...))]`:
    /// the name, the `Constraint` expression, and whether it is sealed.
    constraints: Vec<(String, TokenStream2, bool)>,
    /// `#[crol(schema = T)]`: `Some(Some(T))`; `#[crol(schema = any)]`:
    /// `Some(None)`.
    schema: Option<Option<Type>>,
    doc: String,
}

fn string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value: LitStr = meta.value()?.parse()?;
    Ok(value.value())
}

impl Attrs {
    fn parse(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut out = Attrs::default();
        let mut doc = Vec::new();
        for attr in attrs {
            if attr.path().is_ident("doc") {
                if let Meta::NameValue(nv) = &attr.meta
                    && let Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) = &nv.value
                {
                    doc.push(s.value());
                }
            } else if attr.path().is_ident("serde") {
                attr.parse_nested_meta(|meta| {
                    let path = &meta.path;
                    if path.is_ident("rename") {
                        if meta.input.peek(Token![=]) {
                            out.rename = Some(string_value(&meta)?);
                        } else {
                            // `rename(serialize = "...", deserialize = "...")`
                            meta.parse_nested_meta(|inner| {
                                if inner.path.is_ident("deserialize") {
                                    out.rename = Some(string_value(&inner)?);
                                } else {
                                    let _ = string_value(&inner)?;
                                }
                                Ok(())
                            })?;
                        }
                    } else if path.is_ident("rename_all") {
                        if meta.input.peek(Token![=]) {
                            out.rename_all = Some(string_value(&meta)?);
                        } else {
                            meta.parse_nested_meta(|inner| {
                                if inner.path.is_ident("deserialize") {
                                    out.rename_all = Some(string_value(&inner)?);
                                } else {
                                    let _ = string_value(&inner)?;
                                }
                                Ok(())
                            })?;
                        }
                    } else if path.is_ident("alias") {
                        out.aliases.push(string_value(&meta)?);
                    } else if path.is_ident("default") {
                        if meta.input.peek(Token![=]) {
                            let function: LitStr = meta.value()?.parse()?;
                            out.default = Some(Some(function.parse()?));
                        } else {
                            out.default = Some(None);
                        }
                    } else if path.is_ident("skip") || path.is_ident("skip_deserializing") {
                        out.skip = true;
                    } else if path.is_ident("flatten") {
                        out.flatten = true;
                    } else if path.is_ident("transparent") {
                        out.transparent = true;
                    } else if path.is_ident("untagged") {
                        out.untagged = true;
                    } else if path.is_ident("tag") {
                        out.tag = Some(string_value(&meta)?);
                    } else if path.is_ident("content") {
                        out.content = Some(string_value(&meta)?);
                    } else if path.is_ident("deny_unknown_fields") {
                        out.deny_unknown_fields = true;
                    } else if path.is_ident("with") || path.is_ident("deserialize_with") {
                        out.with = true;
                        let _ = string_value(&meta)?;
                    } else if meta.input.peek(Token![=]) {
                        // Other serde attributes do not change the schema.
                        let _: Expr = meta.value()?.parse()?;
                    } else if meta.input.peek(syn::token::Paren) {
                        meta.parse_nested_meta(|inner| {
                            if inner.input.peek(Token![=]) {
                                let _: Expr = inner.value()?.parse()?;
                            }
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
            } else if attr.path().is_ident("crol") {
                attr.parse_nested_meta(|meta| {
                    let path = &meta.path;
                    if path.is_ident("defaults") {
                        out.defaults = true;
                    } else if path.is_ident("explicit_defaults") {
                        out.explicit_defaults = Some(if meta.input.peek(Token![=]) {
                            meta.value()?.parse::<LitBool>()?.value
                        } else {
                            true
                        });
                    } else if path.is_ident("constraint") {
                        out.constraint = Some(string_value(&meta)?);
                    } else if path.is_ident("key_constraint") {
                        out.key_constraint = Some(string_value(&meta)?);
                    } else if path.is_ident("sealed") {
                        out.sealed = true;
                    } else if path.is_ident("constraints") || path.is_ident("open_constraints") {
                        let sealed = path.is_ident("constraints");
                        meta.parse_nested_meta(|inner| {
                            let name = inner
                                .path
                                .get_ident()
                                .ok_or_else(|| inner.error("expected the constraint's name"))?
                                .to_string();
                            let expr: Expr = inner.value()?.parse()?;
                            out.constraints.push((name, constraint_expr(&expr)?, sealed));
                            Ok(())
                        })?;
                    } else if path.is_ident("refers") {
                        let content;
                        syn::parenthesized!(content in meta.input);
                        let types: Punctuated<Path, Token![,]> = content.parse_terminated(Path::parse_mod_style, Token![,])?;
                        out.refers.extend(types);
                    } else if path.is_ident("schema") {
                        let ty: Type = meta.value()?.parse()?;
                        let any = matches!(&ty, Type::Path(p) if p.path.is_ident("any"));
                        out.schema = Some(if any { None } else { Some(ty) });
                    } else if path.is_ident("rename") {
                        out.rename = Some(string_value(&meta)?);
                    } else {
                        return Err(meta.error(
                            "unknown crol attribute; the attributes are `defaults`, `explicit_defaults`, `refers(...)`, `schema = ...` and `rename = \"...\"`",
                        ));
                    }
                    Ok(())
                })?;
            }
        }
        // Doc comments: one leading space removed from each line.
        out.doc = doc
            .iter()
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        Ok(out)
    }
}

/// Converts an identifier as serde's `rename_all` does. `variant` says
/// whether it is a variant name (PascalCase) or a field name (snake_case).
fn rename(name: &str, rule: &str, variant: bool) -> syn::Result<String> {
    let words: Vec<String> = if variant {
        let mut words = Vec::new();
        let mut current = String::new();
        for c in name.chars() {
            if c.is_uppercase() && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(c);
        }
        if !current.is_empty() {
            words.push(current);
        }
        words.into_iter().map(|w| w.to_lowercase()).collect()
    } else {
        name.split('_').map(str::to_owned).collect()
    };
    let capitalize = |w: &str| {
        let mut chars = w.chars();
        chars
            .next()
            .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
    };
    Ok(match rule {
        "lowercase" => name.to_lowercase(),
        "UPPERCASE" => name.to_uppercase(),
        "PascalCase" => words.iter().map(|w| capitalize(w)).collect(),
        "camelCase" => {
            let pascal: String = words.iter().map(|w| capitalize(w)).collect();
            let mut chars = pascal.chars();
            chars
                .next()
                .map_or_else(String::new, |c| c.to_lowercase().chain(chars).collect())
        }
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),
        other => {
            return Err(Error::new(
                Span::call_site(),
                format!("unknown rename_all rule `{other}`"),
            ));
        }
    })
}

fn serde_name(
    ident: &str,
    attrs: &Attrs,
    rule: Option<&str>,
    variant: bool,
) -> syn::Result<String> {
    if let Some(name) = &attrs.rename {
        return Ok(name.clone());
    }
    let ident = ident.strip_prefix("r#").unwrap_or(ident);
    match rule {
        Some(rule) => rename(ident, rule, variant),
        None => Ok(ident.to_owned()),
    }
}

/// `#[crol(explicit_defaults)]` goes where the deserializer checks that a
/// struct is complete; elsewhere it could not do what it says.
fn check_explicit_defaults(input: &DeriveInput, attrs: &Attrs) -> syn::Result<()> {
    let wrong = |span: Span, message: &str| Err(Error::new(span, message));
    let no_field_has_it = |fields: &Fields| -> syn::Result<()> {
        for field in fields {
            if Attrs::parse(&field.attrs)?.explicit_defaults.is_some() {
                return wrong(
                    field.span(),
                    "`explicit_defaults` goes on a struct, an enum or a struct variant, not on a field",
                );
            }
        }
        Ok(())
    };
    match &input.data {
        Data::Struct(data) => {
            no_field_has_it(&data.fields)?;
            if attrs.explicit_defaults.is_some()
                && (attrs.transparent || !matches!(data.fields, Fields::Named(_)))
            {
                return wrong(input.span(), "`explicit_defaults` needs named fields");
            }
            if attrs.explicit_defaults == Some(true) {
                for field in &data.fields {
                    if Attrs::parse(&field.attrs)?.flatten {
                        return wrong(
                            field.span(),
                            "a struct with `#[serde(flatten)]` deserializes as a map, which cannot ask for `..`",
                        );
                    }
                }
            }
        }
        Data::Enum(data) => {
            let mut annotated = attrs.explicit_defaults.is_some();
            for variant in &data.variants {
                no_field_has_it(&variant.fields)?;
                if Attrs::parse(&variant.attrs)?.explicit_defaults.is_some() {
                    annotated = true;
                    if !matches!(variant.fields, Fields::Named(_)) {
                        return wrong(variant.span(), "`explicit_defaults` needs named fields");
                    }
                }
            }
            if annotated && (attrs.untagged || attrs.tag.is_some()) {
                return wrong(
                    input.span(),
                    "a tagged or untagged enum goes through serde's `any`, which cannot ask for `..`",
                );
            }
        }
        Data::Union(_) => {}
    }
    Ok(())
}

/// An `Option<bool>` as an expression.
fn option_bool(value: Option<bool>) -> TokenStream2 {
    match value {
        Some(value) => quote!(::std::option::Option::Some(#value)),
        None => quote!(::std::option::Option::None),
    }
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.qself.is_none() && p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

// ---- #[derive(Schema)] ----

fn schema(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let attrs = Attrs::parse(&input.attrs)?;
    check_explicit_defaults(input, &attrs)?;
    let ident = &input.ident;
    let name = serde_name(&ident.to_string(), &attrs, None, false)?;
    let doc = &attrs.doc;

    // Generic types: each instantiation is a type of its own, named with
    // its arguments (`SPEC.md` 10.10).
    let mut generics = input.generics.clone();
    let type_params: Vec<_> = generics.type_params().map(|p| p.ident.clone()).collect();
    for param in generics.type_params_mut() {
        param.bounds.push(syn::parse_quote!(::crol::schema::Schema));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let path = if type_params.is_empty() {
        quote!(::std::string::String::from(
            ::std::concat!(::std::module_path!(), "::", #name)
        ))
    } else {
        quote! {{
            let args: ::std::vec::Vec<::std::string::String> = ::std::vec![
                #(::crol::schema::type_name(&<#type_params as ::crol::schema::Schema>::describe(registry))),*
            ];
            ::std::format!("{}::{}<{}>", ::std::module_path!(), #name, args.join(", "))
        }}
    };

    // `/` separators on every platform, so that schema files are the same
    // wherever they are generated.
    let source = quote!((::std::file!().replace('\\', "/"), ::std::line!()));
    let definition = match &input.data {
        Data::Struct(data) => struct_definition(&attrs, &name, doc, &source, &data.fields)?,
        Data::Enum(data) => {
            let repr = match (&attrs.tag, &attrs.content, attrs.untagged) {
                (_, _, true) => quote!(::crol::schema::Repr::Untagged),
                (Some(tag), Some(content), _) => quote!(::crol::schema::Repr::Adjacent {
                    tag: ::std::string::String::from(#tag),
                    content: ::std::string::String::from(#content),
                }),
                (Some(tag), None, _) => {
                    quote!(::crol::schema::Repr::Internal { tag: ::std::string::String::from(#tag) })
                }
                _ => quote!(::crol::schema::Repr::External),
            };
            let mut variants = Vec::new();
            for variant in &data.variants {
                let variant_attrs = Attrs::parse(&variant.attrs)?;
                if variant_attrs.skip {
                    continue;
                }
                let variant_name = serde_name(
                    &variant.ident.to_string(),
                    &variant_attrs,
                    attrs.rename_all.as_deref(),
                    true,
                )?;
                let shape = match &variant.fields {
                    Fields::Unit => quote!(::crol::schema::VariantShape::Unit),
                    Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                        let ty = field_type(
                            &Attrs::parse(&fields.unnamed[0].attrs)?,
                            &fields.unnamed[0].ty,
                        )?;
                        quote!(::crol::schema::VariantShape::Newtype(#ty))
                    }
                    Fields::Unnamed(fields) => {
                        let mut types = Vec::new();
                        for field in &fields.unnamed {
                            types.push(field_type(&Attrs::parse(&field.attrs)?, &field.ty)?);
                        }
                        quote!(::crol::schema::VariantShape::Tuple(
                            ::std::vec![#(#types),*]
                        ))
                    }
                    Fields::Named(_) => {
                        let fields =
                            named_fields(&variant.fields, None, false, false, attrs.sealed)?;
                        quote!(::crol::schema::VariantShape::Struct(#fields))
                    }
                };
                let variant_doc = &variant_attrs.doc;
                let aliases = &variant_attrs.aliases;
                // The enum's setting is for its struct variants.
                let explicit_defaults = option_bool(
                    variant_attrs
                        .explicit_defaults
                        .or(attrs.explicit_defaults)
                        .filter(|_| matches!(variant.fields, Fields::Named(_))),
                );
                variants.push(quote! {
                    (::std::string::String::from(#variant_name), ::crol::schema::VariantDef {
                        shape: #shape,
                        doc: ::std::string::String::from(#variant_doc),
                        aliases: ::std::vec![#(::std::string::String::from(#aliases)),*],
                        explicit_defaults: #explicit_defaults,
                    })
                });
            }
            quote! {
                ::crol::schema::TypeDef::Enum {
                    name: ::std::string::String::from(#name),
                    doc: ::std::string::String::from(#doc),
                    source: #source,
                    repr: #repr,
                    variants: ::std::iter::IntoIterator::into_iter([#(#variants),*]).collect(),
                }
            }
        }
        Data::Union(_) => {
            return Err(Error::new(
                input.span(),
                "a union cannot be part of a schema",
            ));
        }
    };

    let constraints = attrs.constraints.iter().map(
        |(name, constraint, sealed)| quote!(registry.constraint(#name, #constraint, #sealed);),
    );

    Ok(quote! {
        impl #impl_generics ::crol::schema::Schema for #ident #ty_generics #where_clause {
            fn describe(registry: &mut ::crol::schema::Registry) -> ::crol::schema::TypeRef {
                let path = #path;
                registry.add(&path, |registry| {
                    let _ = &registry;
                    #(#constraints)*
                    #definition
                })
            }
        }
    })
}

/// The type expression of a field, from `#[crol(schema = ...)]` or its
/// type, with `#[crol(refers(...))]` applied.
fn field_type(attrs: &Attrs, ty: &Type) -> syn::Result<TokenStream2> {
    let base = match &attrs.schema {
        Some(None) => quote!(::crol::schema::TypeExpr::Any),
        Some(Some(other)) => quote!(<#other as ::crol::schema::Schema>::describe(registry)),
        None if attrs.with => {
            return Err(Error::new(
                ty.span(),
                "a field with `#[serde(with = ...)]` needs `#[crol(schema = Type)]` or `#[crol(schema = any)]`, since its serialized shape is not its type's",
            ));
        }
        None => quote!(<#ty as ::crol::schema::Schema>::describe(registry)),
    };
    if attrs.refers.is_empty() {
        return Ok(base);
    }
    let refers = &attrs.refers;
    Ok(quote! {
        ::crol::schema::with_refers(#base, ::std::vec![
            #(::crol::schema::type_name(&<#refers as ::crol::schema::Schema>::describe(registry))),*
        ])
    })
}

/// The fields of a struct or struct variant, as an `IndexMap` expression.
/// `defaults` records the values of fields' defaults (`#[crol(defaults)]`);
/// `struct_default` says the struct has `#[serde(default)]`, so that every
/// field has one.
fn named_fields(
    fields: &Fields,
    rule: Option<&str>,
    defaults: bool,
    struct_default: bool,
    sealed: bool,
) -> syn::Result<TokenStream2> {
    let mut entries = Vec::new();
    for field in fields {
        let attrs = Attrs::parse(&field.attrs)?;
        if attrs.skip {
            continue;
        }
        let ident = field.ident.as_ref().expect("named fields").to_string();
        let name = serde_name(&ident, &attrs, rule, false)?;
        let ty = field_type(&attrs, &field.ty)?;
        let field_ty = &field.ty;
        let default = match (&attrs.default, defaults) {
            (Some(None), true) => quote!(::crol::schema::recorded_default(
                &<#field_ty as ::std::default::Default>::default()
            )),
            (Some(Some(function)), true) => quote!(::crol::schema::recorded_default(&#function())),
            (Some(_), false) => quote!(::crol::schema::FieldDefault::Unrecorded),
            // Its value is the field's in the struct's default.
            (None, _) if struct_default => quote!(::crol::schema::FieldDefault::Unrecorded),
            // serde makes a missing `Option` field `None`.
            (None, _) if is_option(&field.ty) => quote!(::crol::schema::none_value()),
            (None, _) => quote!(::crol::schema::FieldDefault::Required),
        };
        let doc = &attrs.doc;
        let aliases = &attrs.aliases;
        let flatten = attrs.flatten;
        let constraint = option_string(attrs.constraint.as_deref());
        let key_constraint = option_string(attrs.key_constraint.as_deref());
        let field_sealed = sealed || attrs.sealed;
        entries.push(quote! {
            (::std::string::String::from(#name), ::crol::schema::Field {
                ty: #ty,
                doc: ::std::string::String::from(#doc),
                aliases: ::std::vec![#(::std::string::String::from(#aliases)),*],
                default: #default,
                flatten: #flatten,
                refers: ::std::vec::Vec::new(),
                constraint: #constraint,
                key_constraint: #key_constraint,
                sealed: #field_sealed,
            })
        });
    }
    Ok(quote!(::std::iter::IntoIterator::into_iter([#(#entries),*]).collect()))
}

/// `Some(String::from(text))` or `None`, as an expression.
fn option_string(text: Option<&str>) -> TokenStream2 {
    match text {
        Some(text) => {
            quote!(::std::option::Option::Some(::std::string::String::from(#text)))
        }
        None => quote!(::std::option::Option::None),
    }
}

fn struct_definition(
    attrs: &Attrs,
    name: &str,
    doc: &str,
    source: &TokenStream2,
    fields: &Fields,
) -> syn::Result<TokenStream2> {
    let name_expr = quote!(::std::string::String::from(#name));
    let doc_expr = quote!(::std::string::String::from(#doc));
    let constraint = option_string(attrs.constraint.as_deref());
    let wraps = attrs.transparent || matches!(fields, Fields::Unnamed(f) if f.unnamed.len() == 1);
    if attrs.constraint.is_some() && !wraps {
        return Err(Error::new(
            Span::call_site(),
            "`constraint` on a type goes on a newtype or transparent struct; on another type, put it on its fields",
        ));
    }
    Ok(match fields {
        Fields::Unit => quote! {
            ::crol::schema::TypeDef::UnitStruct { name: #name_expr, doc: #doc_expr, source: #source }
        },
        Fields::Named(named) if attrs.transparent => {
            let field = named
                .named
                .iter()
                .find(|f| Attrs::parse(&f.attrs).is_ok_and(|a| !a.skip))
                .ok_or_else(|| Error::new(named.span(), "a transparent struct needs a field"))?;
            let inner = field_type(&Attrs::parse(&field.attrs)?, &field.ty)?;
            quote! {
                ::crol::schema::TypeDef::Transparent { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner, constraint: #constraint }
            }
        }
        Fields::Named(_) => {
            let fields_expr = named_fields(
                fields,
                attrs.rename_all.as_deref(),
                attrs.defaults,
                attrs.default.is_some(),
                attrs.sealed,
            )?;
            let deny = attrs.deny_unknown_fields;
            let default = if attrs.defaults && matches!(attrs.default, Some(None)) {
                quote!(::crol::schema::default_value(
                    &<Self as ::std::default::Default>::default()
                ))
            } else {
                quote!(::std::option::Option::None)
            };
            let explicit_defaults = option_bool(attrs.explicit_defaults);
            quote! {
                ::crol::schema::TypeDef::Struct {
                    name: #name_expr,
                    doc: #doc_expr,
                    source: #source,
                    fields: #fields_expr,
                    deny_unknown_fields: #deny,
                    default: #default,
                    explicit_defaults: #explicit_defaults,
                }
            }
        }
        Fields::Unnamed(unnamed) => {
            let mut types = Vec::new();
            for field in &unnamed.unnamed {
                let field_attrs = Attrs::parse(&field.attrs)?;
                if !field_attrs.skip {
                    types.push(field_type(&field_attrs, &field.ty)?);
                }
            }
            match (types.len(), attrs.transparent) {
                (1, true) => {
                    let inner = &types[0];
                    quote! {
                        ::crol::schema::TypeDef::Transparent { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner, constraint: #constraint }
                    }
                }
                (1, false) => {
                    let inner = &types[0];
                    quote! {
                        ::crol::schema::TypeDef::Newtype { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner, constraint: #constraint }
                    }
                }
                _ => quote! {
                    ::crol::schema::TypeDef::TupleStruct {
                        name: #name_expr,
                        doc: #doc_expr,
                        source: #source,
                        fields: ::std::vec![#(#types),*],
                    }
                },
            }
        }
    })
}

// ---- #[derive(VariantName)] ----

fn variant_name(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Enum(data) = &input.data else {
        return Err(Error::new(
            input.span(),
            "`VariantName` is derived for enums",
        ));
    };
    let ident = &input.ident;
    let mut names = Vec::new();
    let mut variants = Vec::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(Error::new(
                variant.span(),
                "`VariantName` is derived for enums whose variants have no fields",
            ));
        }
        let attrs = Attrs::parse(&variant.attrs)?;
        let name = match &attrs.rename {
            Some(name) => name.clone(),
            None => rename(&variant.ident.to_string(), "snake_case", true)?,
        };
        names.push(name);
        variants.push(variant.ident.clone());
    }
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::crol::VariantName for #ident #ty_generics #where_clause {
            fn name(&self) -> &str {
                match self {
                    #(Self::#variants => #names,)*
                }
            }

            fn from_name(name: &str) -> ::std::option::Option<Self> {
                match name {
                    #(#names => ::std::option::Option::Some(Self::#variants),)*
                    _ => ::std::option::Option::None,
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{rename, schema};
    use syn::parse_quote;

    #[track_caller]
    fn rejected(input: syn::DeriveInput, message: &str) {
        let error = schema(&input).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }

    #[test]
    fn explicit_defaults_goes_where_it_can_hold() {
        for input in [
            parse_quote!(
                #[crol(explicit_defaults)]
                struct S {
                    a: u8,
                }
            ),
            parse_quote!(
                #[crol(explicit_defaults = false)]
                struct S {
                    a: u8,
                }
            ),
            parse_quote!(
                #[crol(explicit_defaults = false)]
                struct S {
                    #[serde(flatten)]
                    a: T,
                }
            ),
            parse_quote!(
                #[crol(explicit_defaults = false)]
                enum E {
                    A { a: u8 },
                    B(u8),
                }
            ),
            parse_quote!(
                enum E {
                    #[crol(explicit_defaults)]
                    A { a: u8 },
                }
            ),
        ] {
            schema(&input).unwrap();
        }
        rejected(
            parse_quote!(
                struct S {
                    #[crol(explicit_defaults)]
                    a: u8,
                }
            ),
            "not on a field",
        );
        rejected(
            parse_quote!(
                #[crol(explicit_defaults)]
                struct S(u8, u8);
            ),
            "needs named fields",
        );
        rejected(
            parse_quote!(
                #[crol(explicit_defaults)]
                #[serde(transparent)]
                struct S {
                    a: u8,
                }
            ),
            "needs named fields",
        );
        rejected(
            parse_quote!(
                enum E {
                    #[crol(explicit_defaults)]
                    A(u8),
                }
            ),
            "needs named fields",
        );
        rejected(
            parse_quote!(
                #[crol(explicit_defaults)]
                struct S {
                    #[serde(flatten)]
                    a: T,
                }
            ),
            "deserializes as a map",
        );
        rejected(
            parse_quote!(
                #[crol(explicit_defaults)]
                #[serde(tag = "t")]
                enum E {
                    A { a: u8 },
                }
            ),
            "tagged or untagged",
        );
        rejected(
            parse_quote!(
                #[serde(untagged)]
                enum E {
                    #[crol(explicit_defaults)]
                    A { a: u8 },
                }
            ),
            "tagged or untagged",
        );
        rejected(
            parse_quote!(
                #[crol(explicit_defaults = "no")]
                struct S {
                    a: u8,
                }
            ),
            "expected boolean literal",
        );
    }

    #[test]
    fn rename_rules_follow_serde() {
        assert_eq!(rename("ProdEu", "snake_case", true).unwrap(), "prod_eu");
        assert_eq!(rename("ProdEu", "kebab-case", true).unwrap(), "prod-eu");
        assert_eq!(
            rename("ProdEu", "SCREAMING_SNAKE_CASE", true).unwrap(),
            "PROD_EU"
        );
        assert_eq!(rename("memory_mb", "camelCase", false).unwrap(), "memoryMb");
        assert_eq!(
            rename("memory_mb", "PascalCase", false).unwrap(),
            "MemoryMb"
        );
        assert_eq!(
            rename("memory_mb", "SCREAMING-KEBAB-CASE", false).unwrap(),
            "MEMORY-MB"
        );
        assert_eq!(rename("ProdEu", "lowercase", true).unwrap(), "prodeu");
    }
}
