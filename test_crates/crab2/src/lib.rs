use bar::write as woozle;
use fob::fs as bar;
use std as fob;

pub mod stuff {
    #[link(name = "nothing")]
    extern "C" {
        fn nothing_to_see_here();
    }

    pub fn do_stuff() {
        crab3::macro_that_uses_unsafe!({
            crate::woozle("/tmp/foo.bar", &[42]).unwrap();
            true
        });
        // Unfortunately (or fortunately?) this doesn't seem to be sufficient to cause our external
        // library's constructor from running.
        std::hint::black_box(&nothing_to_see_here);
        crab3::do_stuff();
    }
}
