use proc_macro::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{
    parse_macro_input, punctuated::Punctuated, DeriveInput, Expr, ExprLit, ItemTrait, Lit, Meta,
    Token,
};

/// Derives the standard set of traits for any type that crosses the
/// frontend/backend boundary: serde for the wire, ts-rs for the TypeScript
/// counterpart. Per-type ts-rs bindings land in `target/schema-bindings/_per_type/`
/// where `schema-codegen` inlines them into per-namespace TS files.
#[proc_macro_attribute]
pub fn cinema_type(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    quote! {
        #[derive(Debug, Clone, ::serde::Serialize, ::serde::Deserialize, ::ts_rs::TS)]
        #[ts(export, export_to = "_per_type/")]
        #input
    }
    .into()
}

fn parse_namespace(attr: TokenStream, macro_name: &str) -> Result<String, TokenStream> {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let metas = match parser.parse(attr) {
        Ok(m) => m,
        Err(e) => return Err(e.to_compile_error().into()),
    };

    let mut namespace: Option<String> = None;
    for meta in metas {
        if let Meta::NameValue(nv) = meta {
            if nv.path.is_ident("namespace") {
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) = nv.value
                {
                    namespace = Some(s.value());
                }
            }
        }
    }

    namespace.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("#[{macro_name}] requires `namespace = \"...\"`"),
        )
        .to_compile_error()
        .into()
    })
}

/// On a `trait`: declares an API namespace (requires `namespace = "..."`) and
/// injects `#[async_trait]`. On an `impl ... for AppContext`: no args, just
/// shorthand for `#[async_trait]`. The codegen reads the namespace from source.
#[proc_macro_attribute]
pub fn cinema_api(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::Item);
    match input {
        syn::Item::Trait(t) => {
            if let Err(ts) = parse_namespace(attr, "cinema_api") {
                return ts;
            }
            quote! {
                #[::async_trait::async_trait]
                #t
            }
            .into()
        }
        syn::Item::Impl(i) => quote! {
            #[::async_trait::async_trait]
            #i
        }
        .into(),
        other => syn::Error::new_spanned(other, "#[cinema_api] expects a trait or impl block")
            .to_compile_error()
            .into(),
    }
}

/// Declares a namespace of backend→frontend events. The annotated trait is a
/// pure manifest consumed by the codegen — never implemented, erased at compile
/// time. Codegen reads the trait from source, not from expansion.
#[proc_macro_attribute]
pub fn cinema_events(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Err(ts) = parse_namespace(attr, "cinema_events") {
        return ts;
    }
    let _input = parse_macro_input!(item as ItemTrait);
    TokenStream::new()
}
