use std::{collections::BTreeMap, sync::Arc};

use const_format::concatcp;
use serde_json::json;
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    cluster_resources::ClusterResourceApplyStrategy,
    commons::rbac::build_rbac_resources,
    kube::{
        ResourceExt,
        core::{DeserializeGuard, error_boundary},
        runtime::controller::Action,
    },
    kvp::LabelError,
    logging::controller::ReconcilerError,
    shared::time::Duration,
    status::condition::{
        compute_conditions, daemonset::DaemonSetConditionBuilder,
        operations::ClusterOperationsConditionBuilder,
    },
    utils::cluster_info::KubernetesClusterInfo,
    v2::cluster_resources::cluster_resources_new,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    controller::{
        RoleGroupName, build,
        build::resource::service::{
            build_rolegroup_headless_service, build_rolegroup_metrics_service,
            build_server_role_service,
        },
        controller_name, operator_name, product_name, validate,
    },
    crd::{APP_NAME, OPERATOR_NAME, OpaClusterStatus, OpaRole, v1alpha2},
};

pub const OPA_CONTROLLER_NAME: &str = "opacluster";
pub const OPA_FULL_CONTROLLER_NAME: &str = concatcp!(OPA_CONTROLLER_NAME, '.', OPERATOR_NAME);

pub(crate) const CONTAINER_IMAGE_BASE_NAME: &str = "opa";

pub const OPA_STACKABLE_SERVICE_NAME: &str = "stackable";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub opa_bundle_builder_image: String,
    pub user_info_fetcher_image: String,
    pub cluster_info: KubernetesClusterInfo,
    pub operator_environment: OperatorEnvironmentOptions,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("OpaCluster object is invalid"))]
    InvalidOpaCluster {
        // boxed because otherwise Clippy warns about a large enum variant
        #[snafu(source(from(error_boundary::InvalidObject, Box::new)))]
        source: Box<error_boundary::InvalidObject>,
    },

    #[snafu(display("failed to apply role Service"))]
    ApplyRoleService {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply Service for [{rolegroup}]"))]
    ApplyRoleGroupService {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build ConfigMap for [{rolegroup}]"))]
    BuildRoleGroupConfig {
        source: build::resource::config_map::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build DaemonSet for [{rolegroup}]"))]
    BuildRoleGroupDaemonSet {
        source: build::resource::daemonset::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to apply ConfigMap for [{rolegroup}]"))]
    ApplyRoleGroupConfig {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to apply DaemonSet for [{rolegroup}]"))]
    ApplyRoleGroupDaemonSet {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to apply patch for DaemonSet for [{rolegroup}]"))]
    ApplyPatchRoleGroupDaemonSet {
        source: stackable_operator::client::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to patch service account"))]
    ApplyServiceAccount {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to patch role binding"))]
    ApplyRoleBinding {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to build discovery ConfigMap"))]
    BuildDiscoveryConfig {
        source: build::resource::discovery::Error,
    },

    #[snafu(display("failed to apply discovery ConfigMap"))]
    ApplyDiscoveryConfig {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphans {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to build RBAC resources"))]
    BuildRbacResources {
        source: stackable_operator::commons::rbac::Error,
    },

    #[snafu(display("failed to build label"))]
    BuildLabel { source: LabelError },

    #[snafu(display("failed to validate cluster"))]
    ValidateCluster { source: validate::Error },
}
type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_opa(
    opa: Arc<DeserializeGuard<v1alpha2::OpaCluster>>,
    ctx: Arc<Ctx>,
) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let opa = opa
        .0
        .as_ref()
        .map_err(error_boundary::InvalidObject::clone)
        .context(InvalidOpaClusterSnafu)?;

    let client = &ctx.client;

    let validated_cluster =
        validate::validate(opa, &ctx.operator_environment).context(ValidateClusterSnafu)?;

    let opa_role = OpaRole::Server;

    let mut cluster_resources = cluster_resources_new(
        &product_name(),
        &operator_name(),
        &controller_name(),
        &validated_cluster.name,
        &validated_cluster.namespace,
        &validated_cluster.uid,
        ClusterResourceApplyStrategy::from(&opa.spec.cluster_operation),
        &opa.spec.object_overrides,
    );

    let empty_role_group_configs = BTreeMap::new();
    let role_group_configs = validated_cluster
        .role_group_configs
        .get(&opa_role)
        .unwrap_or(&empty_role_group_configs);

    let server_role_service = build_server_role_service(&validated_cluster);
    cluster_resources
        .add(client, server_role_service)
        .await
        .context(ApplyRoleServiceSnafu)?;

    let required_labels = cluster_resources
        .get_required_labels()
        .context(BuildLabelSnafu)?;

    let (rbac_sa, rbac_rolebinding) =
        build_rbac_resources(opa, APP_NAME, required_labels).context(BuildRbacResourcesSnafu)?;

    let rbac_sa = cluster_resources
        .add(client, rbac_sa)
        .await
        .context(ApplyServiceAccountSnafu)?;
    cluster_resources
        .add(client, rbac_rolebinding)
        .await
        .context(ApplyRoleBindingSnafu)?;

    let mut ds_cond_builder = DaemonSetConditionBuilder::default();

    for (rolegroup_name, rolegroup) in role_group_configs {
        let rg_configmap = build::resource::config_map::build_rolegroup_config_map(
            &validated_cluster,
            rolegroup_name,
            rolegroup,
        )
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup_name.clone(),
        })?;
        let rg_service = build_rolegroup_headless_service(&validated_cluster, rolegroup_name);
        let rg_metrics_service =
            build_rolegroup_metrics_service(&validated_cluster, rolegroup_name);
        let rg_daemonset = build::resource::daemonset::build_server_rolegroup_daemonset(
            &validated_cluster,
            rolegroup_name,
            rolegroup,
            &ctx.opa_bundle_builder_image,
            &ctx.user_info_fetcher_image,
            &rbac_sa,
            &ctx.cluster_info,
        )
        .with_context(|_| BuildRoleGroupDaemonSetSnafu {
            rolegroup: rolegroup_name.clone(),
        })?;

        cluster_resources
            .add(client, rg_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup_name.clone(),
            })?;
        cluster_resources
            .add(client, rg_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup_name.clone(),
            })?;
        cluster_resources
            .add(client, rg_metrics_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup_name.clone(),
            })?;
        ds_cond_builder.add(
            cluster_resources
                .add(client, rg_daemonset.clone())
                .await
                .with_context(|_| ApplyRoleGroupDaemonSetSnafu {
                    rolegroup: rolegroup_name.clone(),
                })?,
        );

        // Previous version of opa-operator used the field manager scope "opacluster" to write out a DaemonSet with the bundle-builder container called "opa-bundle-builder".
        // During https://github.com/stackabletech/opa-operator/pull/420 it was renamed to "bundle-builder".
        // As we are now using the field manager scope "opa.stackable.tech_opacluster", our old changes (with the old container) will stay valid.
        // We have to use the old field manager scope and post an empty path to get rid of it
        // https://github.com/stackabletech/issues/issues/390 will implement a proper fix, e.g. also fixing Services and ConfigMaps
        // For details see https://github.com/stackabletech/opa-operator/issues/444
        tracing::trace!(
            "Removing old field manager scope \"opacluster\" of DaemonSet {daemonset_name} to remove the \"opa-bundle-builder\" container. \
            See https://github.com/stackabletech/opa-operator/issues/444 and https://github.com/stackabletech/issues/issues/390 for details.",
            daemonset_name = rg_daemonset.name_any()
        );
        client
            .apply_patch(
                "opacluster",
                &rg_daemonset,
                // We can hardcode this here, as https://github.com/stackabletech/issues/issues/390 will solve the general problem and we always have created DaemonSets using the "apps/v1" version
                json!({"apiVersion": "apps/v1", "kind": "DaemonSet"}),
            )
            .await
            .context(ApplyPatchRoleGroupDaemonSetSnafu {
                rolegroup: rolegroup_name.clone(),
            })?;
    }

    let discovery_cm = build::resource::discovery::build_discovery_config_map(
        &validated_cluster,
        &client.kubernetes_cluster_info,
    )
    .context(BuildDiscoveryConfigSnafu)?;
    cluster_resources
        .add(client, discovery_cm)
        .await
        .context(ApplyDiscoveryConfigSnafu)?;

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&opa.spec.cluster_operation);

    let status = OpaClusterStatus {
        conditions: compute_conditions(opa, &[&ds_cond_builder, &cluster_operation_cond_builder]),
    };

    client
        .apply_patch_status(OPERATOR_NAME, opa, &status)
        .await
        .context(ApplyStatusSnafu)?;

    cluster_resources
        .delete_orphaned_resources(client)
        .await
        .context(DeleteOrphansSnafu)?;

    Ok(Action::await_change())
}

pub fn error_policy(
    _obj: Arc<DeserializeGuard<v1alpha2::OpaCluster>>,
    error: &Error,
    _ctx: Arc<Ctx>,
) -> Action {
    match error {
        // root object is invalid, will be requeued when modified anyway
        Error::InvalidOpaCluster { .. } => Action::await_change(),

        _ => Action::requeue(*Duration::from_secs(10)),
    }
}
