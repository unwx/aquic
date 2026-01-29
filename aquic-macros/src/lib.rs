use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Item, ItemEnum, ItemFn, ItemStruct, ItemTrait, Meta, Token, parse_macro_input,
};

const QUIC_IMPL: [(&str, &str); 2] = [
    ("quiche", "[quiche](https://github.com/cloudflare/quiche)"),
    ("quinn", "[quinn-proto](https://github.com/quinn-rs/quinn)"),
];

/// Writes information about different QUIC implementations support
/// on the specified item.
///
/// Allowed identifiers:
/// - quiche.
/// - quinn (which is quinn-proto).
///
/// # Example:
/// ```no_run
/// #[supports]
/// struct Name {
///     #[supports(quiche, quinn)],
///     val specific_to_implementation_field: bool,
/// }
///
/// ```
///
/// Will be transformed into
/// ```no_run
/// struct Name {
///     /// Only the following QUIC implementations support this feature:
///     /// - [quiche](https://github.com/cloudflare/quiche)
///     /// - [quinn-proto](https://github.com/quinn-rs/quinn)
///     val specific_to_implementation_field: bool,
/// }
/// ```
///
/// **Note**: this macro doesn't make code conditional.
#[proc_macro_attribute]
pub fn supports(
    _args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let mut item = parse_macro_input!(input as Item);

    let result = match &mut item {
        Item::Struct(item_struct) => handle_struct(item_struct),
        Item::Trait(item_trait) => handle_trait(item_trait),
        Item::Enum(item_enum) => handle_enum(item_enum),
        Item::Fn(item_fn) => handle_fn(item_fn),
        _ => Ok(()),
    };

    if let Err(e) = result {
        return e.into();
    }

    quote! { #item }.into()
}

fn handle_struct(item_struct: &mut ItemStruct) -> Result<(), TokenStream> {
    for field in &mut item_struct.fields {
        append_docs(&mut field.attrs)?;
    }

    Ok(())
}

fn handle_trait(item_trait: &mut ItemTrait) -> Result<(), TokenStream> {
    for item in &mut item_trait.items {
        match item {
            syn::TraitItem::Fn(trait_item_fn) => append_docs(&mut trait_item_fn.attrs)?,
            _ => {}
        }
    }

    Ok(())
}

fn handle_enum(item_enum: &mut ItemEnum) -> Result<(), TokenStream> {
    for variant in &mut item_enum.variants {
        append_docs(&mut variant.attrs)?;
    }

    Ok(())
}

fn handle_fn(item_fn: &mut ItemFn) -> Result<(), TokenStream> {
    append_docs(&mut item_fn.attrs)
}


fn append_docs(attrs: &mut Vec<Attribute>) -> Result<(), TokenStream> {
    let attr = {
        let (mut supports_attrs, other_attrs): (Vec<_>, Vec<_>) = attrs
            .drain(..)
            .partition(|attr| attr.path().is_ident("supports"));

        *attrs = other_attrs;

        if supports_attrs.len() > 1 {
            return Err(syn::Error::new(
                Span::call_site(),
                "There is no need to include multiple #[supports] declarations on a single item. \
                You can combine them like this: #[supports(1, 2, 3)]",
            )
            .to_compile_error());
        }
        if supports_attrs.is_empty() {
            return Ok(());
        }

        supports_attrs.pop().unwrap()
    };

    let metas = attr
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .map_err(|e| TokenStream::from(e.into_compile_error()))?;

    if metas.is_empty() {
        return Ok(());
    }

    let mut lines = vec![
        "".to_string(),
        "# Limited Support".to_string(),
        "Only the following QUIC implementations support this feature:".to_string(),
    ];

    for meta in metas {
        if let Meta::Path(path) = meta {
            let ident = path.get_ident().unwrap().to_string();

            let Some((_, url)) = QUIC_IMPL.iter().find(|(name, _)| *name == ident.as_str()) else {
                return Err(syn::Error::new_spanned(
                    path,
                    format!(
                        "Unknown QUIC implementation: '{}', allowed values: {:?}",
                        ident,
                        QUIC_IMPL
                            .iter()
                            .map(|(name, _)| *name)
                            .collect::<Vec<&str>>()
                    ),
                )
                .to_compile_error());
            };

            lines.push(format!("- {url}"));
        }
    }

    for line in lines {
        attrs.push(syn::parse_quote!( #[doc = #line] ));
    }

    Ok(())
}
