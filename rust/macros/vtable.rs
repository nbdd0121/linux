// SPDX-License-Identifier: GPL-2.0

use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::collections::HashSet;
use syn::{parse_quote, Error, ImplItem, Item, ItemImpl, ItemTrait, Result, TraitItem};

pub(crate) fn vtable(input: Item) -> Result<TokenStream> {
    match input {
        Item::Impl(impl_) => Ok(handle_impl(impl_)),
        Item::Trait(trait_) => Ok(handle_trait(trait_)),
        other => Err(Error::new_spanned(
            other,
            "`#[vtable]` expects a `trait` or `impl`.",
        )),
    }
}

fn handle_impl(mut impl_: ItemImpl) -> TokenStream {
    let mut functions = Vec::new();
    let mut consts = HashSet::new();
    for item in &impl_.items {
        match item {
            ImplItem::Fn(fn_) => functions.push(fn_.sig.ident.clone()),
            ImplItem::Const(const_) => {
                consts.insert(const_.ident.clone());
            }
            _ => {}
        }
    }
    impl_.items.push(parse_quote!(
        const USE_VTABLE_ATTR: () = ();
    ));
    for func in functions {
        let gen_const_name = Ident::new(
            &format!("HAS_{}", func.to_string().to_uppercase()),
            func.span(),
        );
        if consts.contains(&gen_const_name) {
            continue;
        }
        impl_
            .items
            .push(parse_quote!(const #gen_const_name: bool = true;));
        consts.insert(gen_const_name);
    }
    quote! { #impl_ }
}

fn handle_trait(mut trait_: ItemTrait) -> TokenStream {
    let mut functions = Vec::new();
    let mut consts = HashSet::new();
    for item in &trait_.items {
        match item {
            TraitItem::Fn(fn_) => functions.push(fn_.sig.ident.clone()),
            TraitItem::Const(const_) => {
                consts.insert(const_.ident.clone());
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
        let gen_const_name = Ident::new(
            &format!("HAS_{}", func.to_string().to_uppercase()),
            func.span(),
        );
        // Skip if it's declared already -- this allows user override.
        if consts.contains(&gen_const_name) {
            continue;
        }
        // We don't know on the implementation-site whether a method is required or provided
        // so we have to generate a const for all methods.
        let comment = format!("Indicates if the `{func}` method is overridden by the implementor.");
        trait_.items.push(parse_quote!(
            #[doc = #comment]
            const #gen_const_name: bool = false;
        ));
        consts.insert(gen_const_name);
    }
    quote! { #trait_ }
}
