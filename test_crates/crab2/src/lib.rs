use bar::write as woozle;
use fob::fs as bar;
use std as fob;

pub mod stuff {
    pub fn do_stuff() {
        crab3::macro_that_uses_unsafe!({
            crate::woozle("/tmp/foo.bar", &[42]).unwrap();
            true
        });
    }
}
