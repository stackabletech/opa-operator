//! The update_status step in the OpaCluster controller.

use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{OPA_OPERATOR_NAME, OpaClusterStatus, v1alpha2};
use stackable_operator::{
    client::Client,
    status::condition::{
        ConditionBuilder, compute_conditions, daemonset::DaemonSetConditionBuilder,
        deployment::DeploymentConditionBuilder, operations::ClusterOperationsConditionBuilder,
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::controller::{Applied, KubernetesResources};

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::client::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Computes the cluster status from the applied resources and patches it onto the
/// [`v1alpha2::OpaCluster`]. Takes [`KubernetesResources<Applied>`] so the type system proves the
/// status derives from applied resources, not merely built ones.
pub async fn update_status(
    client: &Client,
    opa: &v1alpha2::OpaCluster,
    applied: &KubernetesResources<Applied>,
) -> Result<()> {
    let mut ds_cond_builder = DaemonSetConditionBuilder::default();
    for daemon_set in &applied.daemon_sets {
        ds_cond_builder.add(daemon_set.clone());
    }

    let mut deployment_cond_builder = DeploymentConditionBuilder::default();
    for deployment in &applied.deployments {
        deployment_cond_builder.add(deployment.clone());
    }

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&opa.spec.cluster_operation);

    // The workload kind is configured per role, so only one of the two workload lists is filled.
    // Only builders with resources to judge are passed on.
    let status = {
        let mut condition_builders: Vec<&dyn ConditionBuilder> = Vec::new();
        if !applied.daemon_sets.is_empty() {
            condition_builders.push(&ds_cond_builder);
        }
        if !applied.deployments.is_empty() {
            condition_builders.push(&deployment_cond_builder);
        }
        condition_builders.push(&cluster_operation_cond_builder);

        OpaClusterStatus {
            conditions: compute_conditions(opa, &condition_builders),
        }
    };

    client
        .apply_patch_status(OPA_OPERATOR_NAME, opa, &status)
        .await
        .context(ApplyStatusSnafu)?;

    Ok(())
}
