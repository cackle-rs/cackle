use bar::write as woozle;
use fob::fs as bar;
use std as fob;

pub mod stuff {
    #[link(
        name = "nothing",
        kind = "static",
        modifiers = "-bundle,+whole-archive"
    )]
    extern "C" {}

    pub fn do_stuff() {
        crab3::macro_that_uses_unsafe!({
            crate::woozle("/tmp/foo.bar", [42]).unwrap();
            true
        });
        crab3::do_stuff();
    }
}
