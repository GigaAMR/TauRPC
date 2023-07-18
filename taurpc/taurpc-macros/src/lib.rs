use generator::ProceduresGenerator;
use proc::{Procedures, RpcMethod};
use proc_macro::{self, TokenStream};
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse_macro_input, parse_quote, parse_quote_spanned, spanned::Spanned, Ident, ImplItem,
    ImplItemFn, ImplItemType, ItemImpl, ItemStruct, ReturnType, Type,
};

mod args;
mod attrs;
mod generator;
mod proc;

use once_cell::sync::Lazy;
use std::sync::Mutex;

use crate::attrs::ProceduresAttrs;

/// https://github.com/google/tarpc/blob/master/plugins/src/lib.rs#L29
/// Accumulates multiple errors into a result.
/// Only use this for recoverable errors, i.e. non-parse errors. Fatal errors should early exit to
/// avoid further complications.
macro_rules! extend_errors {
    ($errors: ident, $e: expr) => {
        match $errors {
            Ok(_) => $errors = Err($e),
            Err(ref mut errors) => errors.extend($e),
        }
    };
}
pub(crate) use extend_errors;

static STRUCT_NAMES: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(vec![]));

/// Add this macro to all structs used inside the procedures arguments or return types.
/// This macro is necessary for serialization and TS type generation.
#[proc_macro_attribute]
pub fn rpc_struct(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);

    STRUCT_NAMES.lock().unwrap().push(input.ident.to_string());

    quote! {
        #[derive(taurpc::Serialize, taurpc::Deserialize, taurpc::TS, Clone)]
        #input
    }
    .into()
}

/// Generates the necessary structs and enums for handling calls and generating TS-types.
#[proc_macro_attribute]
pub fn procedures(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let procedures_attrs = parse_macro_input!(attrs as ProceduresAttrs);

    let Procedures {
        ref ident,
        ref methods,
        ref vis,
        ref generics,
        ref attrs,
    } = parse_macro_input!(item as Procedures);

    let struct_idents = STRUCT_NAMES.lock().unwrap();
    let unit_type: &Type = &parse_quote!(());

    ProceduresGenerator {
        trait_ident: ident,
        handler_ident: &format_ident!("TauRpc{}Handler", ident),
        event_trigger_ident: &procedures_attrs
            .event_trigger_ident
            .unwrap_or(format_ident!("TauRpc{}EventTrigger", ident)),
        inputs_ident: &format_ident!("TauRpc{}Inputs", ident),
        outputs_ident: &format_ident!("TauRpc{}Outputs", ident),
        output_types_ident: &format_ident!("TauRpc{}OutputTypes", ident),
        output_futures_ident: &format_ident!("TauRpc{}OutputFutures", ident),
        methods,
        method_output_types: &methods
            .iter()
            .map(|RpcMethod { output, .. }| match output {
                ReturnType::Type(_, ref ty) => ty,
                ReturnType::Default => unit_type,
            })
            .collect::<Vec<_>>(),
        alias_method_idents: &methods
            .into_iter()
            .map(|RpcMethod { ident, attrs, .. }| {
                attrs
                    .alias
                    .as_ref()
                    .map(|alias| Ident::new(alias, ident.span()))
                    .unwrap_or(ident.clone())
            })
            .collect::<Vec<_>>(),
        struct_idents: &struct_idents
            .iter()
            .map(|name| format_ident!("{}", name))
            .collect::<Vec<_>>(),
        vis,
        generics,
        attrs,
    }
    .into_token_stream()
    .into()
}

/// Transforms all methods to return Pin<Box<Future<Output = ...>>>, async traits are not supported.
#[proc_macro_attribute]
pub fn resolvers(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut item = syn::parse_macro_input!(item as ItemImpl);
    let mut types: Vec<ImplItemType> = Vec::new();

    for inner in &mut item.items {
        match inner {
            ImplItem::Fn(method) => {
                if method.sig.asyncness.is_some() {
                    types.push(transform_method(method));
                }
            }
            _ => {}
        }
    }

    // add the type declarations into the impl block
    for t in types.into_iter() {
        item.items.push(syn::ImplItem::Type(t));
    }

    quote!(#item).into()
}

// Transform an async method into a sync one that returns a Pin<Box<Future<Output = ...  >> .
fn transform_method(method: &mut ImplItemFn) -> ImplItemType {
    method.sig.asyncness = None;

    let ret = match &method.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ret) => quote!(#ret),
    };

    let fut_ident = method_fut_ident(&method.sig.ident);

    method.sig.output = parse_quote! {
        -> ::core::pin::Pin<Box<
                dyn ::core::future::Future<Output = #ret> + ::core::marker::Send
            >>
    };

    // transform the body of the method into Box::pin(async move { body }).
    let block = method.block.clone();
    method.block = parse_quote_spanned! {method.span()=>{
        Box::pin(async move #block)
    }};

    // generate and return type declaration for return type.
    let t = parse_quote! {
        type #fut_ident = ::core::pin::Pin<Box<dyn ::core::future::Future<Output = #ret> + ::core::marker::Send>>;
    };

    t
}

pub(crate) fn format_method_name(method: &Ident) -> Ident {
    format_ident!("TauRPC__{}", method)
}

fn method_fut_ident(ident: &Ident) -> Ident {
    format_ident!("{}Fut", ident)
}
