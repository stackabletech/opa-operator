use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{Container, OpaCluster};
use stackable_operator::product_logging::spec::LogLevel;
use stackable_operator::{
    builder::ConfigMapBuilder,
    client::Client,
    k8s_openapi::api::core::v1::ConfigMap,
    kube::ResourceExt,
    product_logging::{
        self,
        spec::{ContainerLogConfig, ContainerLogConfigChoice, Logging},
    },
    role_utils::RoleGroupRef,
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object has no namespace"))]
    ObjectHasNoNamespace,
    #[snafu(display("failed to retrieve the ConfigMap [{cm_name}]"))]
    ConfigMapNotFound {
        source: stackable_operator::error::Error,
        cm_name: String,
    },
    #[snafu(display("failed to retrieve the entry [{entry}] for ConfigMap [{cm_name}]"))]
    MissingConfigMapEntry {
        entry: &'static str,
        cm_name: String,
    },
    #[snafu(display("vectorAggregatorConfigMapName must be set"))]
    MissingVectorAggregatorAddress,
}

type Result<T, E = Error> = std::result::Result<T, E>;

const VECTOR_AGGREGATOR_CM_ENTRY: &str = "ADDRESS";

#[derive(strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum OpaLogLevel {
    Debug,
    Info,
    Error,
}

impl From<LogLevel> for OpaLogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::TRACE | LogLevel::DEBUG => Self::Debug,
            LogLevel::INFO => Self::Info,
            LogLevel::WARN | LogLevel::ERROR | LogLevel::FATAL | LogLevel::NONE => Self::Error,
        }
    }
}

/// Return the address of the Vector aggregator if the corresponding ConfigMap name is given in the
/// cluster spec
pub async fn resolve_vector_aggregator_address(
    opa: &OpaCluster,
    client: &Client,
) -> Result<Option<String>> {
    let vector_aggregator_address = if let Some(vector_aggregator_config_map_name) =
        &opa.spec.vector_aggregator_config_map_name
    {
        let vector_aggregator_address = client
            .get::<ConfigMap>(
                vector_aggregator_config_map_name,
                opa.namespace()
                    .as_deref()
                    .context(ObjectHasNoNamespaceSnafu)?,
            )
            .await
            .context(ConfigMapNotFoundSnafu {
                cm_name: vector_aggregator_config_map_name.to_string(),
            })?
            .data
            .and_then(|mut data| data.remove(VECTOR_AGGREGATOR_CM_ENTRY))
            .context(MissingConfigMapEntrySnafu {
                entry: VECTOR_AGGREGATOR_CM_ENTRY,
                cm_name: vector_aggregator_config_map_name.to_string(),
            })?;
        Some(vector_aggregator_address)
    } else {
        None
    };

    Ok(vector_aggregator_address)
}

/// Extend the role group ConfigMap with logging and Vector configurations
pub fn extend_role_group_config_map(
    rolegroup: &RoleGroupRef<OpaCluster>,
    vector_aggregator_address: Option<&str>,
    logging: &Logging<Container>,
    cm_builder: &mut ConfigMapBuilder,
) -> Result<()> {
    let vector_log_config = if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = logging.containers.get(&Container::Vector)
    {
        Some(log_config)
    } else {
        None
    };

    if logging.enable_vector_agent {
        cm_builder.add_data(
            product_logging::framework::VECTOR_CONFIG_FILE,
            product_logging::framework::create_vector_config(
                rolegroup,
                vector_aggregator_address.context(MissingVectorAggregatorAddressSnafu)?,
                vector_log_config,
            ),
        );
    }

    Ok(())
}

pub fn opa_capture_shell_output(log_dir: &str, container: &str, log_file: &str) -> String {
    let log_file_dir = format!("{log_dir}/{container}");

    vec![
        format!("mkdir --parents {log_file_dir}"),
        format!("exec > >(tee {log_file_dir}/{log_file})"),
    ]
    .join(" && ")
}
