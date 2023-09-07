use pmacro_1::baz;
use pmacro_1::FooBar;

pmacro_1::create_write_to_file!();

pub trait FooBar {
    fn foo_bar() -> u32;
}

#[derive(FooBar)]
struct Foo {}

fn main() {
    let values = [1, 2, crab_6::add(40, 2)];
    // This unsafe is here to make sure that we handle unsafe code in packages with hyphens in their
    // name correctly. This is easy to mess up since the crate name passed to rustc will have an
    // underscore instead of a hyphen.
    let value = crab_1::crab_1(*unsafe { values.get_unchecked(2) });
    println!("{value}");
    non_mangled_function();
    println!("HOME: {:?}", crab_4::get_home());
    write_to_file("a.txt", "Hello");
    println!("pid={}", (crab_4::GET_PID[0])());
    crab_4::access_file();
    crab_7::do_something();
    crab_8::print_defaults();
    crab_3::run_process();
    res_1::print_something();
    assert_eq!(crab_2::res_b(), 42);
    assert_eq!(Foo::foo_bar(), 42);
    assert_eq!(function_with_custom_attr(), 40);
    // Note, the following call exits
    crab_2::stuff::do_stuff();
}

#[baz]
fn function_with_custom_attr() -> i32 {
    40
}

#[no_mangle]
fn non_mangled_function() {
    // Make sure we don't miss function references from non-mangled functions.
    println!("{:?}", std::env::var("HOME"));
    if std::env::var("SET_THIS_TO_ABORT").is_ok() {
        crab_1::inlined_abort();
    }
}
