//! Renders the Vector agent config (`vector.yaml`) assembled into the rolegroup `ConfigMap`, and
//! maps log levels for the Stackable Rust sidecars.

use stackable_operator::product_logging::spec::LogLevel;

/// The Vector agent configuration (`vector.yaml`).
const VECTOR_CONFIG: &str = include_str!("vector.yaml");

/// Returns the Vector agent config (`vector.yaml`) content added to the rolegroup `ConfigMap`.
pub fn vector_config_file_content() -> String {
    VECTOR_CONFIG.to_owned()
}

/// The log level passed to the Stackable Rust sidecars (bundle-builder, user-info-fetcher) via the
/// `CONSOLE_LOG_LEVEL`/`FILE_LOG_LEVEL` environment variables.
#[derive(strum::Display)]
#[strum(serialize_all = "UPPERCASE")]
pub enum BundleBuilderLogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for BundleBuilderLogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::TRACE => Self::Trace,
            LogLevel::DEBUG => Self::Debug,
            LogLevel::INFO => Self::Info,
            LogLevel::WARN => Self::Warn,
            LogLevel::ERROR | LogLevel::FATAL | LogLevel::NONE => Self::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_config_file_content_has_opa_sources() {
        let content = vector_config_file_content();
        assert!(!content.is_empty());
        // OPA's own JSON logs and the Rust sidecar (tracing-rs) logs must both be handled.
        assert!(content.contains("files_opa_json"));
        assert!(content.contains("files_tracing_rs"));
    }

    #[test]
    fn bundle_builder_log_level_maps_and_renders_uppercase() {
        // The sidecar only understands these five levels, so FATAL and NONE collapse onto ERROR.
        let cases = [
            (LogLevel::TRACE, "TRACE"),
            (LogLevel::DEBUG, "DEBUG"),
            (LogLevel::INFO, "INFO"),
            (LogLevel::WARN, "WARN"),
            (LogLevel::ERROR, "ERROR"),
            (LogLevel::FATAL, "ERROR"),
            (LogLevel::NONE, "ERROR"),
        ];
        for (level, expected) in cases {
            assert_eq!(BundleBuilderLogLevel::from(level).to_string(), expected);
        }
    }
}
