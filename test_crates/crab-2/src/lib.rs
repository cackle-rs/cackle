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
        let crab_2_env = env!("CRAB_2_ENV");
        assert_eq!(crab_2_env, "42");

        crab_3::macro_that_uses_unsafe!({
            crate::woozle("/tmp/foo.bar", [42]).unwrap();
            true
        });
        crab_3::do_stuff();

        // This is here as a way to allow cackle's integration test to trigger a rebuild of this
        // file (by changing the environment variable).
        println!("CRAB_2_EXT_ENV: {:?}", option_env!("CRAB_2_EXT_ENV"));

        let total: u32 = crate::DATA.iter().cloned().map(|byte| byte as u32).sum();
        println!("{}", total);
    }
}

#[inline(always)]
pub fn res_b() -> u64 {
    crab_3::res_a()
}
