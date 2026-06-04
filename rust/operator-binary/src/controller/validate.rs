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
    kube::{ResourceExt, runtime::reflector::ObjectRef},
    role_utils::RoleGroupRef,
    v2::types::operator::ClusterName,
};
use strum::IntoEnumIterator;

use crate::crd::{OpaConfig, OpaRole, v1alpha2};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("invalid cluster name"))]
    InvalidClusterName {
        source: stackable_operator::v2::macros::attributed_string_type::Error,
    },

    #[snafu(display("failed to resolve and merge config for role and role group"))]
    FailedToResolveConfig { source: crate::crd::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// The validated [`v1alpha2::OpaCluster`].
///
/// The output of the validate step: config fragments merged and validated for every role group,
/// ready to be turned into Kubernetes resources without touching the raw `OpaCluster` spec again
/// (except for owner references).
pub struct ValidatedCluster {
    // TODO: consumed by the config_map build step in a follow-up commit (Step 3).
    #[allow(dead_code)]
    pub name: ClusterName,
    pub image: ResolvedProductImage,
    pub role_group_configs: BTreeMap<OpaRole, BTreeMap<String, OpaRoleGroupConfig>>,
}

/// The validated configuration of a single role group.
pub struct OpaRoleGroupConfig {
    pub merged_config: OpaConfig,
}

/// Validates the cluster spec and produces a [`ValidatedCluster`].
pub fn validate(
    opa: &v1alpha2::OpaCluster,
    operator_environment: &OperatorEnvironmentOptions,
) -> Result<ValidatedCluster> {
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
        for role_group_name in role.role_groups.keys() {
            let rolegroup_ref = RoleGroupRef {
                cluster: ObjectRef::from_obj(opa),
                role: opa_role.to_string(),
                role_group: role_group_name.clone(),
            };
            let merged_config = opa
                .merged_config(&opa_role, &rolegroup_ref)
                .context(FailedToResolveConfigSnafu)?;

            group_configs.insert(role_group_name.clone(), OpaRoleGroupConfig { merged_config });
        }

        role_group_configs.insert(opa_role, group_configs);
    }

    Ok(ValidatedCluster {
        name: ClusterName::from_str(&opa.name_any()).context(InvalidClusterNameSnafu)?,
        image,
        role_group_configs,
    })
}
