// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

#[macro_export]
macro_rules! foo {
    () => {
        std::process::exit(1)
    };
}

#[macro_export]
macro_rules! macro_that_uses_unsafe {
    ($a:expr) => {
        let v = $a;
        let mut x = 0_u32;
        if v {
            x = unsafe { core::mem::transmute(-10_i32) };
        }
        x
    };
}

pub fn do_stuff() {
    let _ = include!(concat!(env!("OUT_DIR"), "/extra_code.rs"));
    foo!();
}
