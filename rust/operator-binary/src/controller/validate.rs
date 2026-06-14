//! The validate step in the OpaCluster controller
//!
//! Synchronously merges and validates the cluster spec into the typed
//! [`ValidatedCluster`] consumed by the rest of `reconcile_opa`. No Kubernetes
//! client is required.

use std::{collections::BTreeMap, str::FromStr};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    product_logging::spec::Logging,
    role_utils::RoleGroup,
    v2::{
        builder::pod::container::{EnvVarName, EnvVarSet},
        controller_utils::{get_cluster_name, get_namespace, get_uid},
        product_logging::framework::{
            VectorContainerLogConfig, validate_logging_configuration_for_container,
        },
        role_utils::with_validated_config,
        types::{kubernetes::ConfigMapName, operator::RoleGroupName},
    },
};
use strum::IntoEnumIterator;

use super::{OpaRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig, ValidatedRoleGroup};
use crate::crd::{Container, OpaConfig, OpaRole, v1alpha2};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("failed to get the cluster name"))]
    GetClusterName {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to get the cluster namespace"))]
    GetNamespace {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to get the cluster UID"))]
    GetUid {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to merge and validate config for role group {role_group:?}"))]
    ValidateRoleGroupConfig {
        source: stackable_operator::config::fragment::ValidationError,
        role_group: String,
    },

    #[snafu(display("failed to parse environment variable name {name:?}"))]
    ParseEnvVarName {
        source: stackable_operator::v2::builder::pod::container::Error,
        name: String,
    },

    #[snafu(display("the role group name {role_group:?} is invalid"))]
    ParseRoleGroupName {
        source: <RoleGroupName as FromStr>::Err,
        role_group: String,
    },

    #[snafu(display("failed to validate the logging configuration"))]
    ValidateLoggingConfig {
        source: stackable_operator::v2::product_logging::framework::Error,
    },

    #[snafu(display(
        "the Vector agent is enabled but no Vector aggregator discovery ConfigMap name is set"
    ))]
    MissingVectorAggregatorConfigMapName,

    #[snafu(display("the Vector aggregator discovery ConfigMap name is invalid"))]
    ParseVectorAggregatorConfigMapName {
        source: stackable_operator::v2::macros::attributed_string_type::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Validated logging configuration for a role group.
///
/// Produced up-front by [`validate_logging`] (mirroring hive/opensearch) so that a missing or
/// invalid Vector aggregator discovery ConfigMap name fails reconciliation during validation
/// rather than at resource-build time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedLogging {
    /// The validated Vector container config, or `None` when the Vector agent is disabled.
    pub vector_container: Option<VectorContainerLogConfig>,
    pub enable_vector_agent: bool,
}

/// Validates the logging configuration for the (optional) Vector container of a role group.
///
/// `vector_aggregator_config_map_name` is the discovery ConfigMap name of the Vector aggregator;
/// it is required (and validated) only when the Vector agent is enabled.
fn validate_logging(
    logging: &Logging<Container>,
    vector_aggregator_config_map_name: &Option<ConfigMapName>,
) -> Result<ValidatedLogging> {
    let vector_container = if logging.enable_vector_agent {
        let vector_aggregator_config_map_name = vector_aggregator_config_map_name
            .clone()
            .context(MissingVectorAggregatorConfigMapNameSnafu)?;
        Some(VectorContainerLogConfig {
            log_config: validate_logging_configuration_for_container(logging, &Container::Vector)
                .context(ValidateLoggingConfigSnafu)?,
            vector_aggregator_config_map_name,
        })
    } else {
        None
    };

    Ok(ValidatedLogging {
        vector_container,
        enable_vector_agent: logging.enable_vector_agent,
    })
}

/// Validates the cluster spec and produces a [`ValidatedCluster`].
pub fn validate(
    opa: &v1alpha2::OpaCluster,
    operator_environment: &OperatorEnvironmentOptions,
) -> Result<ValidatedCluster> {
    // Wrap the metadata in fail-safe v2 types so the build steps never have to re-check it.
    let name = get_cluster_name(opa).context(GetClusterNameSnafu)?;
    let namespace = get_namespace(opa).context(GetNamespaceSnafu)?;
    let uid = get_uid(opa).context(GetUidSnafu)?;

    let image = opa
        .spec
        .image
        .resolve(
            crate::opa_controller::CONTAINER_IMAGE_BASE_NAME,
            &operator_environment.image_repository,
            crate::built_info::PKG_VERSION,
        )
        .context(ResolveProductImageSnafu)?;

    // The Vector aggregator discovery ConfigMap name (validated here so an invalid name fails
    // up-front). It is only required when the Vector agent is enabled for a role group.
    let vector_aggregator_config_map_name = opa
        .spec
        .cluster_config
        .vector_aggregator_config_map_name
        .as_deref()
        .map(ConfigMapName::from_str)
        .transpose()
        .context(ParseVectorAggregatorConfigMapNameSnafu)?;

    let mut role_group_configs = BTreeMap::new();
    for opa_role in OpaRole::iter() {
        let role = opa.role(&opa_role);

        let mut group_configs = BTreeMap::new();
        for (role_group_name, role_group) in &role.role_groups {
            // Merge default <- role <- role group and validate the config fragment, plus merge all
            // four override kinds (config/env/cli/pod) in one shot. Role group wins over role wins
            // over defaults.
            let merged: RoleGroup<OpaConfig, _, _> =
                with_validated_config(role_group, role, &OpaConfig::default_config()).context(
                    ValidateRoleGroupConfigSnafu {
                        role_group: role_group_name.clone(),
                    },
                )?;

            // The framework keeps `envOverrides` as a `HashMap<String, String>`; lift it into the
            // type-safe `EnvVarSet` so the build step matches opensearch/hive.
            let mut env_overrides = EnvVarSet::new();
            for (name, value) in merged.config.env_overrides {
                env_overrides = env_overrides.with_value(
                    &EnvVarName::from_str(&name)
                        .context(ParseEnvVarNameSnafu { name: name.clone() })?,
                    value,
                );
            }

            // Validate the logging configuration up-front (borrows the merged config before it is
            // moved into the `OpaRoleGroupConfig` below).
            let logging = validate_logging(
                &merged.config.config.logging,
                &vector_aggregator_config_map_name,
            )?;

            // Validate the role group name against the upstream `RoleGroupName` newtype (RFC 1123
            // label, length-bounded) so the typed key is guaranteed to produce valid resource names.
            let role_group_name =
                RoleGroupName::from_str(role_group_name).context(ParseRoleGroupNameSnafu {
                    role_group: role_group_name.clone(),
                })?;

            group_configs.insert(
                role_group_name,
                ValidatedRoleGroup {
                    config: OpaRoleGroupConfig {
                        // Unused for a DaemonSet, but the framework type requires it.
                        replicas: merged.replicas.unwrap_or(0),
                        config: merged.config.config,
                        config_overrides: merged.config.config_overrides,
                        env_overrides,
                        cli_overrides: merged.config.cli_overrides,
                        pod_overrides: merged.config.pod_overrides,
                        product_specific_common_config: merged
                            .config
                            .product_specific_common_config,
                    },
                    logging,
                },
            );
        }

        role_group_configs.insert(opa_role, group_configs);
    }

    Ok(ValidatedCluster::new(
        name,
        namespace,
        uid,
        image,
        ValidatedClusterConfig {
            user_info: opa.spec.cluster_config.user_info.clone(),
            tls: opa.spec.cluster_config.tls.clone(),
            listener_class: opa.spec.cluster_config.listener_class.clone(),
        },
        role_group_configs,
    ))
}
