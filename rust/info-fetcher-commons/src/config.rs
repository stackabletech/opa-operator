use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};

#[derive(Snafu, Debug)]
pub enum ConfigError {
    #[snafu(display("failed to open config file from {path:?}"))]
    OpenFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("unable to read config file from {path:?}"))]
    ParseConfigFile {
        source: serde_json::Error,
        path: PathBuf,
    },
}

pub fn read_config_file<C>(path: &Path) -> Result<C, ConfigError>
where
    C: DeserializeOwned,
{
    let file = File::open(path).with_context(|_| OpenFileSnafu { path })?;
    let reader = BufReader::new(file);

    serde_json::from_reader(reader).with_context(|_| ParseConfigFileSnafu { path })
}
