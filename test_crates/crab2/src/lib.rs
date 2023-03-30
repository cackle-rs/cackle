// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

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
