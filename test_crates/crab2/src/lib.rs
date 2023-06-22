// Make sure that we don't add any unsafe to this crate. We want this crate to have no unsafe since
// it has an include_bytes! below and we want to make sure that include_bytes! of invalid UTF-8
// doesn't cause the unsafe token-based checker any problems. If we had some actual unsafe in this
// crate, then we'd need to allow it and then the unsafe-checker wouldn't run.
#![forbid(unsafe_code)]

use bar::write as woozle;
use fob::fs as bar;
use std as fob;

const DATA: &[u8] = include_bytes!("../../data/random.data");

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

        let total: u32 = crate::DATA.iter().cloned().map(|byte| byte as u32).sum();
        println!("{}", total);
    }
}
