use proc_macro::TokenStream;

mod error;

#[proc_macro_derive(fmt_err, attributes(err_code_prefix, error))]
pub fn derive_fmt_err(input: TokenStream) -> TokenStream {
    error::derive::<error::FmtVariant>(input, "fmt_err")
}

#[proc_macro_derive(raw_err, attributes(err_code_prefix, error))]
pub fn derive_raw_err(input: TokenStream) -> TokenStream {
    error::derive::<error::RawVariant>(input, "raw_err")
}
