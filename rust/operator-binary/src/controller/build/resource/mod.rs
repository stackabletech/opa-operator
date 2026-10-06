//! Builders that turn the [`ValidatedCluster`](crate::controller::ValidatedCluster) into
//! Kubernetes resources, one module per resource kind.

pub mod config_map;
pub mod discovery;
pub mod pdb;
pub mod rbac;
pub mod service;
pub mod workload;
