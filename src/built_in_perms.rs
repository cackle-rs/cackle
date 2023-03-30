// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::config::PermissionName;

pub(crate) const UNSAFE: PermissionName = PermissionName::new("unsafe");
pub(crate) const ERROR: PermissionName = PermissionName::new("error");

pub(crate) const ALL: &[PermissionName] = &[UNSAFE, ERROR];
