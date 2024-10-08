use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let fields = if let syn::Data::Struct(data) = &input.data {
        &data.fields
    } else {
        unimplemented!("NetEncode can only be derived for structs");
    };

    let encode_fields = fields.iter().map(|field| {
        let field_name = field.ident.as_ref().unwrap();
        quote! {
            // TODO: see if we need to pass options here
            <self.#field_name as ferrumc_net_codec::encode::NetEncode>::encode(&self.#field_name, writer, ferrumc_net_codec::encode::NetEncodeOptions::None);
        }
    });

    let expanded = quote! {
        impl ferrumc_net_codec::encode::NetEncode for #name {
            // TODO: see if we need to use options here.
            fn encode<W: Write>(&self, writer: &mut W, opts: &NetEncodeOpts) -> NetEncodeResult<()> {
                #(#encode_fields)*
            }
        }
    };
    
    TokenStream::from(expanded)
}