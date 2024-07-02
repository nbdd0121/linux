// SPDX-License-Identifier: GPL-2.0

use std::collections::HashSet;

use proc_macro2::{Ident, TokenStream};
use quote::ToTokens;
use syn::{parse_quote, Error, ImplItem, Item, ItemImpl, ItemTrait, Result, TraitItem};

fn handle_trait(mut item: ItemTrait) -> Result<ItemTrait> {
    let mut functions = Vec::new();
    let mut consts = HashSet::new();

    let mut items = Vec::new();

    for mut item in item.items.into_iter() {
        match item {
            TraitItem::Fn(ref fn_item) => {
                functions.push(fn_item.sig.ident.clone());
            }
            TraitItem::Const(ref mut const_item) => {
                consts.insert(const_item.ident.clone());

                // Check for `#[unique]` constants, we have special treatment.
                if const_item
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("unique"))
                {
                    // Remove the attribute.
                    const_item
                        .attrs
                        .retain(|attr| !attr.path().is_ident("unique"));

                    let syn::Type::Reference(syn::TypeReference {
                        lifetime: Some(lifetime),
                        mutability: None,
                        elem: ty,
                        ..
                    }) = &const_item.ty
                    else {
                        Err(Error::new_spanned(
                            &const_item.ty,
                            "`#[unique]` item must have a `&'static` type",
                        ))?
                    };

                    if lifetime.ident != "static" {
                        Err(Error::new_spanned(
                            &const_item.ty,
                            "`#[unique]` item must have a `&'static` type",
                        ))?
                    }

                    let Some((
                        _,
                        syn::Expr::Reference(syn::ExprReference {
                            mutability: None,
                            expr: default,
                            ..
                        }),
                    )) = const_item.default.take()
                    else {
                        Err(Error::new_spanned(
                            const_item,
                            "`#[unique]` item must have a default value and it must be a reference",
                        ))?
                    };

                    let gen_const_impl_name = Ident::new(
                        &format!("{}_IMPL", const_item.ident.clone()),
                        const_item.ident.span(),
                    );
                    let gen_const_use_unique_attr_name = Ident::new(
                        &format!("{}_USE_UNIQUE_ATTR", const_item.ident.clone()),
                        const_item.ident.span(),
                    );

                    items.push(parse_quote! {
                         /// A marker to prevent implementors from forgetting to use `#[unique]`
                         /// attribute when implementing this trait.
                         const #gen_const_use_unique_attr_name: ();
                    });
                    items.push(parse_quote! {
                        /// TODO
                        const #gen_const_impl_name: #ty = #default;
                    });
                }
            }
            _ => {}
        }
        items.push(item);
    }

    items.push(parse_quote! {
         /// A marker to prevent implementors from forgetting to use [`#[vtable]`](vtable)
         /// attribute when implementing this trait.
         const USE_VTABLE_ATTR: ();
    });

    for name in functions {
        let gen_const_name = Ident::new(
            &format!("HAS_{}", name.to_string().to_uppercase()),
            name.span(),
        );
        // Skip if it's declared already -- this allows user override.
        if consts.contains(&gen_const_name) {
            continue;
        }
        // We don't know on the implementation-site whether a method is required or provided
        // so we have to generate a const for all methods.
        let comment = format!("Indicates if the `{name}` method is overridden by the implementor.");
        items.push(parse_quote! {
           #[doc = #comment]
            const #gen_const_name: bool = false;
        });
        consts.insert(gen_const_name);
    }

    item.items = items;
    Ok(item)
}

fn handle_impl(mut impl_: ItemImpl) -> Result<ItemImpl> {
    // `#[vtable]` must be used on a trait impl.
    let Some((_, trait_, _)) = &impl_.trait_ else {
        Err(Error::new_spanned(
            impl_,
            "`#[vtable]` cannot be used on inherent impl",
        ))?
    };

    let mut functions = Vec::new();
    let mut consts = HashSet::new();

    let mut items = Vec::new();

    for mut item in impl_.items.into_iter() {
        match item {
            ImplItem::Fn(ref fn_item) => {
                functions.push(fn_item.sig.ident.clone());
            }
            ImplItem::Const(ref mut const_item) => {
                consts.insert(const_item.ident.clone());

                // Check for `#[unique]` constants, we have special treatment.
                if const_item
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("unique"))
                {
                    // Remove the attribute.
                    const_item
                        .attrs
                        .retain(|attr| !attr.path().is_ident("unique"));

                    let syn::Type::Reference(syn::TypeReference {
                        lifetime: Some(lifetime),
                        mutability: None,
                        elem: ty,
                        ..
                    }) = &const_item.ty
                    else {
                        Err(Error::new_spanned(
                            &const_item.ty,
                            "`#[unique]` item must have a `&'static` type",
                        ))?
                    };

                    if lifetime.ident != "static" {
                        Err(Error::new_spanned(
                            &const_item.ty,
                            "`#[unique]` item must have a `&'static` type",
                        ))?
                    }

                    let gen_const_impl_name = Ident::new(
                        &format!("{}_IMPL", const_item.ident.clone()),
                        const_item.ident.span(),
                    );
                    let gen_const_use_unique_attr_name = Ident::new(
                        &format!("{}_USE_UNIQUE_ATTR", const_item.ident.clone()),
                        const_item.ident.span(),
                    );

                    if !impl_.generics.params.is_empty() {
                        return Err(Error::new_spanned(
                            impl_.generics,
                            "`#[unique]` cannot be used when impl is generic",
                        ));
                    }
                    let self_ty = &impl_.self_ty;

                    const_item.expr = parse_quote! {{
                        static IMPL: #ty = <#self_ty as #trait_>::#gen_const_impl_name;
                        &IMPL
                    }};

                    items.push(parse_quote! {
                         const #gen_const_use_unique_attr_name: () = ();
                    });
                }
            }
            _ => {}
        }
        items.push(item);
    }

    items.push(parse_quote! {
         const USE_VTABLE_ATTR: () = ();
    });

    for name in functions {
        let gen_const_name = Ident::new(
            &format!("HAS_{}", name.to_string().to_uppercase()),
            name.span(),
        );
        // Skip if it's declared already -- this allows user override.
        if consts.contains(&gen_const_name) {
            continue;
        }
        items.push(parse_quote! {
            const #gen_const_name: bool = true;
        });
        consts.insert(gen_const_name);
    }

    impl_.items = items;
    Ok(impl_)
}

pub(crate) fn vtable(input: Item) -> Result<TokenStream> {
    match input {
        Item::Trait(item) => Ok(handle_trait(item)?.into_token_stream()),
        Item::Impl(item) => Ok(handle_impl(item)?.into_token_stream()),
        _ => Err(Error::new_spanned(
            input,
            "`#[vtable]` expects a `trait` or `impl`.",
        ))?,
    }
}
