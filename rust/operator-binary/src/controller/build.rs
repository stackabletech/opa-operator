//! Build steps that turn the [`ValidatedCluster`](super::ValidatedCluster) into
//! Kubernetes resource specifications.

use std::str::FromStr;

use crate::controller::RoleGroupName;

pub mod properties;
pub mod resource;

// Placeholder role-group name for the recommended labels of the role-level `Service`, which is not
// bound to a single role group. `global` matches the historical `app.kubernetes.io/role-group`
// value.
stackable_operator::constant!(pub(crate) PLACEHOLDER_ROLE_LEVEL_ROLE_GROUP: RoleGroupName = "global");

// Placeholder role-group name for the recommended labels of the discovery `ConfigMap`, which is a
// cluster-level object not bound to a single role group.
stackable_operator::constant!(pub(crate) PLACEHOLDER_DISCOVERY_ROLE_GROUP: RoleGroupName = "discovery");
