mod doc_support;

/// Writes information about different QUIC implementations support
/// on the specified item.
///
/// Allowed identifiers:
/// - quiche.
/// - quinn (which is quinn-proto).
///
/// # Example:
/// ```no_run
/// #[doc_support]
/// struct Name {
///     #[doc_support(quiche, quinn)],
///     val specific_to_implementation_field: bool,
/// }
///
/// ```
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
pub fn doc_support(
    _args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    doc_support::doc_support(input)
}
