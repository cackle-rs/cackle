use std::path::Path;

pub fn do_something() {}

/// This function runs before main. Make sure that we detect that it uses filesystem APIs.
extern "C" fn before_main() {
    // We avoid actually calling filesystem APIs before main, since Rust doesn't really support
    // pre-main activities and with recent versions doing so causes an assertion failure. We can
    // apparently still get away with the environment variable check.
    if std::env::var("FOO").is_ok() {
        println!("Does / exist?: {:?}", Path::new("/").exists());
    }
}

#[link_section = ".init_array"]
#[used]
static INIT_ARRAY: [extern "C" fn(); 1] = [before_main];
