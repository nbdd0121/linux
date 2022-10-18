use super::field::field_name_hash;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use syn::{
    punctuated::Punctuated, Data, DeriveInput, Error, Fields, GenericParam, Generics, Member,
    Result,
};

pub(crate) fn rcu_field(input: TokenStream) -> Result<TokenStream> {
    let DeriveInput {
        ident,
        generics,
        data,
        ..
    } = syn::parse2(input)?;

    // Check this is a struct, and extract inner.
    let data = match data {
        Data::Struct(v) => v,
        Data::Enum(v) => {
            return Err(Error::new(
                v.enum_token.span,
                "#[derive(Field)] cannot be applied to enum",
            ))
        }
        Data::Union(v) => {
            return Err(Error::new(
                v.union_token.span,
                "#[derive(Field)] cannot be applied to union",
            ))
        }
    };

    let fields = match data.fields {
        Fields::Named(v) => v.named,
        Fields::Unnamed(v) => v.unnamed,
        Fields::Unit => Punctuated::new(),
    };

    // Check if the field is `Rcu<...>`
    let is_rcu: Vec<_> = fields
        .iter()
        .map(|field| {
            let path = match &field.ty {
                syn::Type::Path(path) => path,
                _ => return false,
            };
            let segment = match path.path.segments.last() {
                Some(v) => v,
                _ => return false,
            };
            segment.ident == "Rcu"
        })
        .collect();

    let field_name: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(i, field)| match &field.ident {
            Some(v) => Member::Named(v.clone()),
            None => Member::Unnamed(i.into()),
        })
        .collect();

    // Extract generics and where clauses
    let Generics {
        params: generics,
        where_clause,
        ..
    } = generics;
    let generics: Vec<_> = generics
        .into_iter()
        .map(|mut x| {
            match &mut x {
                GenericParam::Lifetime(_) => (),
                GenericParam::Type(t) => t.default = None,
                GenericParam::Const(c) => c.default = None,
            }
            x
        })
        .collect();
    let ty_generics: Vec<_> = generics
        .iter()
        .map(|x| -> &dyn ToTokens {
            match x {
                GenericParam::Lifetime(l) => &l.lifetime,
                GenericParam::Type(t) => &t.ident,
                GenericParam::Const(c) => &c.ident,
            }
        })
        .collect();

    let mixed_site = Span::mixed_site();

    let mut builder = Vec::new();

    for i in 0..field_name.len() {
        let field_name_current = &field_name[i];
        let field_name_str = match field_name_current {
            Member::Named(v) => v.to_string(),
            Member::Unnamed(v) => v.index.to_string(),
        };
        let ty = &fields[i].ty;
        let field_name_hash = field_name_hash(&field_name_str);

        let wrapper_ty = if is_rcu[i] {
            quote_spanned!(mixed_site => &'__field_projection __FieldProjection)
        } else {
            quote_spanned!(mixed_site => &'__field_projection mut __FieldProjection)
        };

        builder.push(quote_spanned! {mixed_site =>
            unsafe impl<
                #(#generics,)*
            > kernel::sync::rcu_mutex::RcuGuardField<
                #ident<#(#ty_generics,)*>
            > for kernel::projection::FieldName<#field_name_hash> #where_clause
            {
                type Wrapper<'__field_projection, __FieldProjection: ?Sized + '__field_projection> = #wrapper_ty;
            }
        });

        if is_rcu[i] {
            builder.push(quote_spanned! {mixed_site =>
                unsafe impl<
                    #(#generics,)*
                > kernel::sync::rcu_mutex::RcuField<
                    #ident<#(#ty_generics,)*>
                > for kernel::projection::FieldName<#field_name_hash> #where_clause
                {}
            });
        }
    }

    let gen = quote!(#(#builder)*);
    Ok(gen)
}
