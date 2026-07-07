//! Builders that turn the [`ValidatedCluster`](crate::controller::ValidatedCluster) into
//! Kubernetes resources, one module per resource kind.

pub mod config_map;
pub mod daemonset;
pub mod discovery;
pub mod service;
