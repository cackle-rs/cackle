use crab6::impl_foo;
use crab6::Foo;

impl_foo!(Res);

pub fn print_something() {
    let r = Res {};
    r.foo2();
    crab6::debug!();
}
