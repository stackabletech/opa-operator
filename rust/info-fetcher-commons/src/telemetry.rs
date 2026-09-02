//! Startup helpers around [`stackable_operator::telemetry`].

use std::path::PathBuf;

use snafu::{ResultExt, Snafu};
use stackable_operator::telemetry::tracing::TelemetryOptions;

#[derive(Snafu, Debug)]
#[snafu(display("failed to create the file log directory {path:?}"))]
pub struct CreateFileLogDirectoryError {
    source: std::io::Error,
    path: PathBuf,
}

/// Creates the file log directory, if file logging is enabled at all.
///
/// `tracing-appender` prunes old log files *before* it writes the first one, and that pruning pass
/// prints `Error reading the log directory/files: No such file or directory` straight to stderr when
/// the directory does not exist yet. It then creates the directory itself and logging works, so the
/// message is harmless, but it is unstructured stderr output that reads like a startup failure and
/// reaches the log collector as an unparsable line.
///
/// The other containers avoid it because their bash entrypoint runs `mkdir -p` first. The
/// info-fetcher sidecars exec their binary directly, with no shell to do that for them, so they
/// create the directory here instead.
pub fn create_file_log_directory(
    options: &TelemetryOptions,
) -> Result<(), CreateFileLogDirectoryError> {
    let Some(path) = &options.file_log_directory else {
        return Ok(());
    };

    std::fs::create_dir_all(path).context(CreateFileLogDirectorySnafu { path })
}
