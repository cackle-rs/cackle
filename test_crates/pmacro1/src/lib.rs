extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro]
pub fn create_write_to_file(_item: TokenStream) -> TokenStream {
    if std::env::var("PWD").as_deref() == Ok("/foo/bar") {
        println!("This seems unlikely");
    }
    r#"fn write_to_file(path: &str, text: &str) {
        std::fs::write(path, text).unwrap();
    }"#
    .parse()
    .unwrap()
}

#[proc_macro_derive(FooBar, attributes(marker))]
pub fn derive_foo_bar(_item: TokenStream) -> TokenStream {
    "impl FooBar for Foo { fn foo_bar() -> u32 { 42 } }"
        .parse()
        .unwrap()
}

#[proc_macro_attribute]
pub fn baz(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
