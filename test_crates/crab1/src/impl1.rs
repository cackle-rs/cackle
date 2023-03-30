// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

pub fn crab1(v: u32) -> u32 {
    unsafe {
        let mut v2: [u8; 4] = core::mem::transmute(v);
        v2.reverse();
        core::mem::transmute(v2)
    }
}
