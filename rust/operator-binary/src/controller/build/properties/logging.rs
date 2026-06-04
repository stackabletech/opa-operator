//! Builds the Vector agent config file and maps log levels for the Stackable Rust sidecars.

use stackable_operator::{
    product_logging::{
        self,
        spec::{ContainerLogConfig, ContainerLogConfigChoice, LogLevel, Logging},
    },
    role_utils::RoleGroupRef,
};

use crate::crd::{Container, v1alpha2};

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

/// Builds the Vector agent config (`vector.yaml`) for the role group, or `None` when the Vector
/// agent is disabled.
pub fn build_vector_config(
    rolegroup: &RoleGroupRef<v1alpha2::OpaCluster>,
    logging: &Logging<Container>,
) -> Option<String> {
    if !logging.enable_vector_agent {
        return None;
    }

    let vector_log_config = if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = logging.containers.get(&Container::Vector)
    {
        Some(log_config)
    } else {
        None
    };

    Some(product_logging::framework::create_vector_config(
        rolegroup,
        vector_log_config,
    ))
}
