use std::sync::Arc;

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
    controller::{build, controller_name, operator_name, product_name, validate},
    crd::{APP_NAME, OPERATOR_NAME, OpaClusterStatus, v1alpha2},
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

    #[snafu(display("failed to validate cluster"))]
    ValidateCluster { source: validate::Error },

    #[snafu(display("failed to build the Kubernetes resources"))]
    BuildResources { source: build::Error },

    #[snafu(display("failed to apply Kubernetes resource"))]
    ApplyResource {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply legacy field-manager patch for DaemonSet {name}"))]
    ApplyPatchDaemonSet {
        source: stackable_operator::client::Error,
        name: String,
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

    let required_labels = cluster_resources
        .get_required_labels()
        .context(BuildLabelSnafu)?;

    let (rbac_sa, rbac_rolebinding) =
        build_rbac_resources(opa, APP_NAME, required_labels).context(BuildRbacResourcesSnafu)?;

    // The ServiceAccount name is deterministic on the built object, so the build step does not
    // depend on the applied ServiceAccount.
    let service_account_name = rbac_sa.name_any();

    cluster_resources
        .add(client, rbac_sa)
        .await
        .context(ApplyServiceAccountSnafu)?;
    cluster_resources
        .add(client, rbac_rolebinding)
        .await
        .context(ApplyRoleBindingSnafu)?;

    let resources = build::build(
        &validated_cluster,
        &service_account_name,
        &ctx.opa_bundle_builder_image,
        &ctx.user_info_fetcher_image,
        &ctx.cluster_info,
    )
    .context(BuildResourcesSnafu)?;

    let mut ds_cond_builder = DaemonSetConditionBuilder::default();

    // Apply order: DaemonSets last, so a changed mounted ConfigMap already exists before the Pods
    // (that would otherwise restart) are updated (commons-operator#111).
    for service in resources.services {
        cluster_resources
            .add(client, service)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for config_map in resources.config_maps {
        cluster_resources
            .add(client, config_map)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for daemon_set in resources.daemon_sets {
        ds_cond_builder.add(
            cluster_resources
                .add(client, daemon_set.clone())
                .await
                .context(ApplyResourceSnafu)?,
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
            daemonset_name = daemon_set.name_any()
        );
        client
            .apply_patch(
                "opacluster",
                &daemon_set,
                // We can hardcode this here, as https://github.com/stackabletech/issues/issues/390 will solve the general problem and we always have created DaemonSets using the "apps/v1" version
                json!({"apiVersion": "apps/v1", "kind": "DaemonSet"}),
            )
            .await
            .context(ApplyPatchDaemonSetSnafu {
                name: daemon_set.name_any(),
            })?;
    }

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
