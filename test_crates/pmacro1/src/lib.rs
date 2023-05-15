extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro]
pub fn create_write_to_file(_item: TokenStream) -> TokenStream {
    println!("{:?}", std::env::var("PWD"));
    r#"fn write_to_file(path: &str, text: &str) {
        std::fs::write(path, text).unwrap();
    }"#
    .parse()
    .unwrap()
}
