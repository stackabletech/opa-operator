use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};

#[derive(Snafu, Debug)]
pub enum ConfigError {
    #[snafu(display("failed to read config file from {path:?}"))]
    ReadFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("unable to read config file from {path:?}"))]
    ParseConfigFile {
        source: serde_json::Error,
        path: PathBuf,
    },
}

pub async fn read_config_file<C>(path: &Path) -> Result<C, ConfigError>
where
    C: DeserializeOwned,
{
    let contents = tokio::fs::read_to_string(path)
        .await
        .with_context(|_| ReadFileSnafu { path })?;

    serde_json::from_str(&contents).with_context(|_| ParseConfigFileSnafu { path })
}
