//! Iris 引擎过程宏：为类型自动实现引擎标记 trait。

use proc_macro::TokenStream;
use quote::quote;

/// 为类型实现 `iris_core::component::Component`。
#[proc_macro_derive(Component)]
pub fn derive_component(input: TokenStream) -> TokenStream {
    impl_marker(input, quote!(::iris_core::component::Component))
}

/// 为类型实现 `iris_core::resource::Resource`。
#[proc_macro_derive(Resource)]
pub fn derive_resource(input: TokenStream) -> TokenStream {
    impl_marker(input, quote!(::iris_core::resource::Resource))
}

/// 为类型实现 `iris_core::event::Event`。
#[proc_macro_derive(Event)]
pub fn derive_event(input: TokenStream) -> TokenStream {
    impl_marker(input, quote!(::iris_core::event::Event))
}

fn impl_marker(input: TokenStream, marker_trait: proc_macro2::TokenStream) -> TokenStream {
    let ast = syn::parse::<syn::DeriveInput>(input)
        .expect("Iris 派生宏只能用于类型定义");
    let name = &ast.ident;
    let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();
    quote! {
        impl #impl_generics #marker_trait for #name #ty_generics #where_clause {}
    }
    .into()
}
