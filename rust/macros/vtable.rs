// SPDX-License-Identifier: GPL-2.0

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::{
    parse::{Parse, ParseStream},
    parse_quote, Error, ImplItem, Item, ItemImpl, ItemTrait, Result, TraitItem,
};

pub(crate) enum TraitOrImpl {
    Trait(ItemTrait),
    Impl(ItemImpl),
    ItemError,
}

impl Parse for TraitOrImpl {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        match input.parse() {
            Ok(Item::Trait(trait_)) => Ok(TraitOrImpl::Trait(trait_)),
            Ok(Item::Impl(impl_)) => Ok(TraitOrImpl::Impl(impl_)),
            Ok(other) => Err(Error::new_spanned(
                other,
                "`#[vtable]` expects a `trait` or `impl`.",
            )),
            Err(_) => Ok(TraitOrImpl::ItemError),
        }
    }
}

pub(crate) fn vtable(input: TraitOrImpl) -> TokenStream {
    match input {
        TraitOrImpl::Impl(impl_) => handle_impl(impl_),
        TraitOrImpl::Trait(trait_) => handle_trait(trait_),
        // Should not be supplied.
        TraitOrImpl::ItemError => unreachable!(),
    }
}

fn handle_impl(mut impl_: ItemImpl) -> TokenStream {
    let mut functions = Vec::new();
    let mut consts = HashSet::new();
    for item in &impl_.items {
        match item {
            ImplItem::Fn(fn_) => functions.push(fn_.sig.ident.to_string()),
            ImplItem::Const(const_) => {
                consts.insert(const_.ident.to_string());
            }
            _ => {}
        }
    }
    impl_.items.push(parse_quote!(
        const USE_VTABLE_ATTR: () = ();
    ));
    for func in functions {
        let gen_const_name = format_ident!("HAS_{}", func.to_uppercase());
        if consts.contains(&format!("{gen_const_name}")) {
            continue;
        }
        impl_
            .items
            .push(parse_quote!(const #gen_const_name: bool = true;));
        consts.insert(format!("{gen_const_name}"));
    }
    quote! { #impl_ }
}

fn handle_trait(mut trait_: ItemTrait) -> TokenStream {
    let mut functions = Vec::new();
    let mut consts = HashSet::new();
    for item in &trait_.items {
        match item {
            TraitItem::Fn(fn_) => functions.push(fn_.sig.ident.to_string()),
            TraitItem::Const(const_) => {
                consts.insert(const_.ident.to_string());
            }
            _ => {}
        }
    }
    trait_.items.push(parse_quote!(
        /// A marker to prevent implementors from forgetting to use the [`#[vtable]`](vtable)
        /// attribute macro when implementing this trait.
        const USE_VTABLE_ATTR: () = ();
    ));
    for func in functions {
        let gen_const_name = format_ident!("HAS_{}", func.to_uppercase());
        // Skip if it's declared already -- this allows user override.
        if consts.contains(&format!("{gen_const_name}")) {
            continue;
        }
        // We don't know on the implementation-site whether a method is required or provided
        // so we have to generate a const for all methods.
        trait_.items.push(parse_quote!(
            /// Indicates if the `#func` method is overridden by the implementor.
            const #gen_const_name: bool = false;
        ));
        consts.insert(format!("{gen_const_name}"));
    }
    quote! { #trait_ }
}
