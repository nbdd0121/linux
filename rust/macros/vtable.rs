// SPDX-License-Identifier: GPL-2.0

use std::{
    collections::HashSet,
    iter::Extend, //
};

use proc_macro2::{
    Ident,
    TokenStream, //
};
use quote::{
    format_ident,
    ToTokens, //
};
use syn::{
    parse_quote,
    Error,
    ImplItem,
    Item,
    ItemImpl,
    ItemTrait,
    Result,
    TraitItem,
    TraitItemConst,
    Type, //
};

fn require_static_ref_ty(ty: &Type) -> Result<&Type> {
    let syn::Type::Reference(syn::TypeReference {
        lifetime: Some(lifetime),
        mutability: None,
        elem,
        ..
    }) = ty
    else {
        Err(Error::new_spanned(
            ty,
            "`#[unique]` item must have a `&'static` type",
        ))?
    };

    if lifetime.ident != "static" {
        Err(Error::new_spanned(
            ty,
            "`#[unique]` item must have a `&'static` type",
        ))?
    }

    Ok(elem)
}

fn handle_trait(mut item: ItemTrait) -> Result<ItemTrait> {
    let mut gen_items = Vec::new();

    gen_items.push(parse_quote! {
         /// A marker to prevent implementors from forgetting to use [`#[vtable]`](vtable)
         /// attribute when implementing this trait.
         const USE_VTABLE_ATTR: ();
    });

    for item in &mut item.items {
        match item {
            TraitItem::Fn(fn_item) => {
                let name = &fn_item.sig.ident;
                let gen_const_name = Ident::new(
                    &format!("HAS_{}", name.to_string().to_uppercase()),
                    name.span(),
                );

                // We don't know on the implementation-site whether a method is required or provided
                // so we have to generate a const for all methods.
                let cfg_attrs = crate::helpers::gather_cfg_attrs(&fn_item.attrs);
                let comment =
                    format!("Indicates if the `{name}` method is overridden by the implementor.");
                gen_items.push(parse_quote! {
                    #(#cfg_attrs)*
                    #[doc = #comment]
                    const #gen_const_name: bool = false;
                });
            }
            TraitItem::Const(const_item) => {
                // Check for constants with `#[unique]` attribute,, we have special treatment.
                let attr_len = const_item.attrs.len();
                const_item
                    .attrs
                    .retain(|attr| !attr.path().is_ident("unique"));
                if const_item.attrs.len() != attr_len {
                    let ty = require_static_ref_ty(&const_item.ty)?;

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

                    let gen_const_impl_name = format_ident!("{}_IMPL", const_item.ident);
                    let gen_const_use_unique_attr_name =
                        format_ident!("{}_USE_UNIQUE_ATTR", const_item.ident);

                    gen_items.push(parse_quote! {
                        /// A marker to prevent implementors from forgetting to use `#[unique]`
                        /// attribute when implementing this trait.
                        const #gen_const_use_unique_attr_name: ();
                    });
                    // This is the implementation detail of this attribute.
                    gen_items.push(parse_quote! {
                        #[doc(hidden)]
                        const #gen_const_impl_name: #ty = #default;
                    });
                }
            }
            _ => (),
        }
    }

    item.items.extend(gen_items);
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

    let mut gen_items = Vec::new();
    let mut defined_consts = HashSet::new();

    // Iterate over all user-defined constants to gather any possible explicit overrides.
    for item in &impl_.items {
        if let ImplItem::Const(const_item) = item {
            defined_consts.insert(const_item.ident.clone());
        }
    }

    gen_items.push(parse_quote! {
        const USE_VTABLE_ATTR: () = ();
    });

    for item in &mut impl_.items {
        match item {
            ImplItem::Fn(fn_item) => {
                let name = &fn_item.sig.ident;
                let gen_const_name = Ident::new(
                    &format!("HAS_{}", name.to_string().to_uppercase()),
                    name.span(),
                );
                // Skip if it's declared already -- this allows user override.
                if defined_consts.contains(&gen_const_name) {
                    continue;
                }
                let cfg_attrs = crate::helpers::gather_cfg_attrs(&fn_item.attrs);
                gen_items.push(parse_quote! {
                    #(#cfg_attrs)*
                    const #gen_const_name: bool = true;
                });
            }

            ImplItem::Const(const_item) => {
                // `#[unique]` constants are defined on the trait side and impls are not allowed
                // to override them. Therefore, impl side has
                // `#[unique] const FOO: &'static Bar;` syntax which is not a valid impl item.
                // This is handled in the verbatim arm instead.
                let attr_len = const_item.attrs.len();
                const_item
                    .attrs
                    .retain(|attr| !attr.path().is_ident("unique"));
                if const_item.attrs.len() != attr_len {
                    Err(Error::new_spanned(
                        &const_item.expr,
                        "`#[unique]` item must not have a value",
                    ))?
                }
            }

            ImplItem::Verbatim(item) => {
                // `#[unique] const FOO: &'static Bar;` is not a valid impl item (although still accepted
                // by the parser), so we receive a verbatim item instead.
                // Parse it as trait item which is the desired syntax.
                let mut const_item: TraitItemConst = syn::parse2(std::mem::take(item))?;

                // `#[unique]` constants are defined on the trait side and impls are not allowed
                // to override them. Therefore, impl side has
                // `#[unique] const FOO: &'static Bar;` syntax which is not a valid impl item.
                // This is handled in the verbatim arm instead.
                let attr_len = const_item.attrs.len();
                const_item
                    .attrs
                    .retain(|attr| !attr.path().is_ident("unique"));
                if const_item.attrs.len() == attr_len {
                    Err(Error::new_spanned(
                        &const_item,
                        "`#[unique]` not applied to const item",
                    ))?
                }

                if !impl_.generics.params.is_empty() {
                    return Err(Error::new_spanned(
                        impl_.generics,
                        "`#[unique]` cannot be used when impl is generic",
                    ));
                }

                let ty = require_static_ref_ty(&const_item.ty)?;
                let gen_const_impl_name = format_ident!("{}_IMPL", const_item.ident);
                let gen_const_use_unique_attr_name =
                    format_ident!("{}_USE_UNIQUE_ATTR", const_item.ident);

                gen_items.push(parse_quote! {
                    const #gen_const_use_unique_attr_name: () = ();
                });

                let self_ty = &impl_.self_ty;
                const_item.default = Some((
                    Default::default(),
                    parse_quote! {{
                        static IMPL: #ty = <#self_ty as #trait_>::#gen_const_impl_name;
                        &IMPL
                    }},
                ));
                *item = const_item.into_token_stream();
            }

            _ => (),
        }
    }

    impl_.items.extend(gen_items);
    Ok(impl_)
}

pub(crate) fn vtable(input: Item) -> Result<TokenStream> {
    match input {
        Item::Trait(item) => Ok(handle_trait(item)?.into_token_stream()),
        Item::Impl(item) => Ok(handle_impl(item)?.into_token_stream()),
        _ => Err(Error::new_spanned(
            input,
            "`#[vtable]` attribute should only be applied to trait or impl block",
        ))?,
    }
}
