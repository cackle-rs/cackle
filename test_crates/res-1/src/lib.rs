use crab_6::impl_foo;
use crab_6::Foo;

impl_foo!(Res);

pub fn print_something() {
    let r = Res {};
    r.foo2();
    crab_6::debug!();
}
