use pmacro1::baz;
use pmacro1::FooBar;

pmacro1::create_write_to_file!();

pub trait FooBar {
    fn foo_bar() -> u32;
}

#[derive(FooBar)]
struct Foo {}

fn main() {
    let values = [1, 2, crab6::add(40, 2)];
    // This unsafe is here to make sure that we handle unsafe code in packages with hyphens in their
    // name correctly. This is easy to mess up since the crate name passed to rustc will have an
    // underscore instead of a hyphen.
    let value = crab1::crab1(*unsafe { values.get_unchecked(2) });
    println!("{value}");
    non_mangled_function();
    println!("HOME: {:?}", crab4::get_home());
    write_to_file("a.txt", "Hello");
    println!("pid={}", (crab4::GET_PID[0])());
    crab4::access_file();
    crab7::do_something();
    crab8::print_defaults();
    crab3::run_process();
    res1::print_something();
    assert_eq!(Foo::foo_bar(), 42);
    assert_eq!(function_with_custom_attr(), 40);
    // Note, the following call exits
    crab2::stuff::do_stuff();
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
        crab1::inlined_abort();
    }
}
