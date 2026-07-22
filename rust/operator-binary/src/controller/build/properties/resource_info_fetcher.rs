//! Builds the OPA `resource-info-fetcher.json` file.

use snafu::{ResultExt, Snafu};

use crate::crd::resource_info_fetcher;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to serialize resource info fetcher configuration"))]
    SerializeResourceInfoFetcherConfig { source: serde_json::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Serializes the resource-info-fetcher configuration into the `resource-info-fetcher.json` file content.
pub fn build(resource_info: &resource_info_fetcher::v1alpha1::Config) -> Result<String> {
    serde_json::to_string_pretty(resource_info).context(SerializeResourceInfoFetcherConfigSnafu)
}
