//! Builds the OPA `user-info-fetcher.json` file.

use snafu::{ResultExt, Snafu};

use crate::crd::user_info_fetcher;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to serialize user info fetcher configuration"))]
    SerializeUserInfoFetcherConfig { source: serde_json::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Serializes the user-info-fetcher configuration into the `user-info-fetcher.json` file content.
pub fn build(user_info: &user_info_fetcher::v1alpha2::Config) -> Result<String> {
    serde_json::to_string_pretty(user_info).context(SerializeUserInfoFetcherConfigSnafu)
}
