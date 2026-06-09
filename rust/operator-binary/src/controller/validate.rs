//! The validate step in the OpaCluster controller
//!
//! Synchronously merges and validates the cluster spec into the typed
//! [`ValidatedCluster`] consumed by the rest of `reconcile_opa`. No Kubernetes
//! client is required.

use std::{collections::BTreeMap, str::FromStr};

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    kube::{Resource, api::ObjectMeta},
    role_utils::RoleGroup,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        builder::pod::container::{EnvVarName, EnvVarSet},
        controller_utils::{get_cluster_name, get_namespace, get_uid},
        role_utils::{GenericCommonConfig, RoleGroupConfig, with_validated_config},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::ClusterName,
        },
    },
};
use strum::IntoEnumIterator;

use crate::crd::{
    OpaConfig, OpaConfigOverrides, OpaRole, user_info_fetcher,
    v1alpha2::{self, OpaTls},
};

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

/// The validated [`v1alpha2::OpaCluster`].
///
/// The output of the validate step: config fragments and `configOverrides` merged and validated
/// for every role group, ready to be turned into Kubernetes resources without touching the raw
/// `OpaCluster` spec again (except for owner references).
pub struct ValidatedCluster {
    /// Object metadata (name, namespace, UID) of the owning `OpaCluster`, built from the validated
    /// fields below. Lets [`ValidatedCluster`] implement [`Resource`] so the build steps can derive
    /// owner references and object metadata without touching the raw `OpaCluster` spec.
    metadata: ObjectMeta,
    pub name: ClusterName,
    pub namespace: NamespaceName,
    pub uid: Uid,
    pub image: ResolvedProductImage,
    pub cluster_config: ValidatedClusterConfig,
    pub role_group_configs: BTreeMap<OpaRole, BTreeMap<String, OpaRoleGroupConfig>>,
}

impl ValidatedCluster {
    /// The name of the role-level load-balanced Kubernetes `Service`, as used in the discovery URL.
    pub fn server_role_service_name(&self) -> String {
        format!("{name}-{role}", name = self.name, role = OpaRole::Server)
    }
}

impl HasName for ValidatedCluster {
    fn to_name(&self) -> String {
        self.name.to_string()
    }
}

impl HasUid for ValidatedCluster {
    fn to_uid(&self) -> Uid {
        self.uid.clone()
    }
}

impl NameIsValidLabelValue for ValidatedCluster {
    fn to_label_value(&self) -> String {
        self.name.to_label_value()
    }
}

impl Resource for ValidatedCluster {
    type DynamicType = <v1alpha2::OpaCluster as Resource>::DynamicType;
    type Scope = <v1alpha2::OpaCluster as Resource>::Scope;

    fn kind(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::kind(dt)
    }

    fn group(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::group(dt)
    }

    fn version(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::version(dt)
    }

    fn plural(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::plural(dt)
    }

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }
}

/// Cluster-wide settings resolved once during validation, so the build steps no longer need the
/// raw `OpaCluster` to render config (except for owner references).
pub struct ValidatedClusterConfig {
    pub user_info: Option<user_info_fetcher::v1alpha2::Config>,
    pub tls: Option<OpaTls>,
}

/// The validated configuration of a single role group.
///
/// All override kinds (`config`, `configOverrides`, `envOverrides`, `cliOverrides`, `podOverrides`)
/// are merged once by [`with_validated_config`], with the role group winning over the role, which
/// wins over the operator defaults.
///
/// Note: `replicas` is carried by the framework type but unused here — OPA runs as a `DaemonSet`
/// (one Pod per node).
pub type OpaRoleGroupConfig = RoleGroupConfig<OpaConfig, GenericCommonConfig, OpaConfigOverrides>;

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
            super::CONTAINER_IMAGE_BASE_NAME,
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

    let metadata = ObjectMeta {
        name: Some(name.to_string()),
        namespace: Some(namespace.to_string()),
        uid: Some(uid.to_string()),
        ..ObjectMeta::default()
    };

    Ok(ValidatedCluster {
        metadata,
        name,
        namespace,
        uid,
        image,
        cluster_config: ValidatedClusterConfig {
            user_info: opa.spec.cluster_config.user_info.clone(),
            tls: opa.spec.cluster_config.tls.clone(),
        },
        role_group_configs,
    })
}
