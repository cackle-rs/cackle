use std::path::Path;

pub fn do_something() {}

/// This function runs before main. Make sure that we detect that it uses filesystem APIs.
extern "C" fn before_main() {
    println!("Does / exist?: {:?}", Path::new("/").exists());
}

#[link_section = ".init_array"]
#[used]
static INIT_ARRAY: [extern "C" fn(); 1] = [before_main];
