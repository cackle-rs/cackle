use crate::config::PermissionName;

pub(crate) const UNSAFE: PermissionName = PermissionName::new("unsafe");
pub(crate) const ERROR: PermissionName = PermissionName::new("error");

pub(crate) const ALL: &[PermissionName] = &[UNSAFE, ERROR];
