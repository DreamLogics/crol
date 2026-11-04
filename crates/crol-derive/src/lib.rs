//! Derive macros for CROL: `#[derive(Schema)]` and `#[derive(VariantName)]`,
//! re-exported by `crol` with the cargo feature `derive` (`SPEC.md` 8.1,
//! 10.9).

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Error, Expr, ExprLit, Fields, Lit, LitStr, Meta, Path, Token,
    Type, parse_macro_input,
};

/// Implements `crol::schema::Schema` from a type's fields and serde
/// attributes (`SPEC.md` 10.9).
///
/// Attributes, besides serde's own:
///
/// - `#[crol(defaults)]` on the type: record its default values;
/// - `#[crol(refers(Service, Volume))]` on an id field: the root types its
///   ids may name;
/// - `#[crol(schema = OtherType)]` or `#[crol(schema = any)]` on a field:
///   describe it as another type with the same serialized shape, or as
///   anything.
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

// ---- Attributes ----

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
    refers: Vec<Path>,
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
                            "unknown crol attribute; the attributes are `defaults`, `refers(...)`, `schema = ...` and `rename = \"...\"`",
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

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.qself.is_none() && p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

// ---- #[derive(Schema)] ----

fn schema(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let attrs = Attrs::parse(&input.attrs)?;
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

    let source = quote!((::std::string::String::from(::std::file!()), ::std::line!()));
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
                        let fields = named_fields(&variant.fields, None, false)?;
                        quote!(::crol::schema::VariantShape::Struct(#fields))
                    }
                };
                let variant_doc = &variant_attrs.doc;
                let aliases = &variant_attrs.aliases;
                variants.push(quote! {
                    (::std::string::String::from(#variant_name), ::crol::schema::VariantDef {
                        shape: #shape,
                        doc: ::std::string::String::from(#variant_doc),
                        aliases: ::std::vec![#(::std::string::String::from(#aliases)),*],
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

    Ok(quote! {
        impl #impl_generics ::crol::schema::Schema for #ident #ty_generics #where_clause {
            fn describe(registry: &mut ::crol::schema::Registry) -> ::crol::schema::TypeRef {
                let path = #path;
                registry.add(&path, |registry| {
                    let _ = &registry;
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
/// `defaults` records the values of fields' defaults (`#[crol(defaults)]`).
fn named_fields(fields: &Fields, rule: Option<&str>, defaults: bool) -> syn::Result<TokenStream2> {
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
            // serde makes a missing `Option` field `None`.
            (None, _) if is_option(&field.ty) => quote!(::crol::schema::none_value()),
            (None, _) => quote!(::crol::schema::FieldDefault::Required),
        };
        let doc = &attrs.doc;
        let aliases = &attrs.aliases;
        let flatten = attrs.flatten;
        entries.push(quote! {
            (::std::string::String::from(#name), ::crol::schema::Field {
                ty: #ty,
                doc: ::std::string::String::from(#doc),
                aliases: ::std::vec![#(::std::string::String::from(#aliases)),*],
                default: #default,
                flatten: #flatten,
                refers: ::std::vec::Vec::new(),
            })
        });
    }
    Ok(quote!(::std::iter::IntoIterator::into_iter([#(#entries),*]).collect()))
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
                ::crol::schema::TypeDef::Transparent { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner }
            }
        }
        Fields::Named(_) => {
            let fields_expr = named_fields(fields, attrs.rename_all.as_deref(), attrs.defaults)?;
            let deny = attrs.deny_unknown_fields;
            let default = if attrs.defaults && matches!(attrs.default, Some(None)) {
                quote!(::crol::schema::default_value(
                    &<Self as ::std::default::Default>::default()
                ))
            } else {
                quote!(::std::option::Option::None)
            };
            quote! {
                ::crol::schema::TypeDef::Struct {
                    name: #name_expr,
                    doc: #doc_expr,
                    source: #source,
                    fields: #fields_expr,
                    deny_unknown_fields: #deny,
                    default: #default,
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
                        ::crol::schema::TypeDef::Transparent { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner }
                    }
                }
                (1, false) => {
                    let inner = &types[0];
                    quote! {
                        ::crol::schema::TypeDef::Newtype { name: #name_expr, doc: #doc_expr, source: #source, inner: #inner }
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
    use super::rename;

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
