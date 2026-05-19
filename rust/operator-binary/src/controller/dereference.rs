//! The dereference step in the OpaCluster controller
//!
//! Fetches all Kubernetes objects referenced by the OpaCluster spec and returns them in
//! [`DereferencedObjects`]. This is currently scaffolding — no objects are dereferenced yet.
//! Follow-up work might move existing inline lookups (e.g. the `user_info_fetcher` Secrets and
//! SecretClasses) through here.

use snafu::Snafu;
use stackable_operator::client::Client;

use crate::crd::v1alpha2;

#[derive(Snafu, Debug)]
pub enum Error {}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Kubernetes objects referenced from the OpaCluster spec, already fetched.
pub struct DereferencedObjects {}

/// Fetches all Kubernetes objects referenced from the [`v1alpha2::OpaCluster`] spec.
pub async fn dereference(
    _client: &Client,
    _opa: &v1alpha2::OpaCluster,
) -> Result<DereferencedObjects> {
    Ok(DereferencedObjects {})
}
