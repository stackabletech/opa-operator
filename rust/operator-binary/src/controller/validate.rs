//! The validate step in the OpaCluster controller
//!
//! Synchronously merges and validates the cluster spec into the typed
//! [`ValidatedCluster`] consumed by the rest of `reconcile_opa`. No Kubernetes
//! client is required.

use std::{collections::BTreeMap, str::FromStr};

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    role_utils::RoleGroup,
    v2::{
        builder::pod::container::{EnvVarName, EnvVarSet},
        controller_utils::{get_cluster_name, get_namespace, get_uid},
        role_utils::with_validated_config,
    },
};
use strum::IntoEnumIterator;

use super::{OpaRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig};
use crate::crd::{OpaConfig, OpaRole, v1alpha2};

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
}

type Result<T, E = Error> = std::result::Result<T, E>;

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

            group_configs.insert(
                role_group_name.clone(),
                OpaRoleGroupConfig {
                    // Unused for a DaemonSet, but the framework type requires it.
                    replicas: merged.replicas.unwrap_or(0),
                    config: merged.config.config,
                    config_overrides: merged.config.config_overrides,
                    env_overrides,
                    cli_overrides: merged.config.cli_overrides,
                    pod_overrides: merged.config.pod_overrides,
                    product_specific_common_config: merged.config.product_specific_common_config,
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
        },
        role_group_configs,
    ))
}
