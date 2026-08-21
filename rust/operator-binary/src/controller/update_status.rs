//! The update_status step in the OpaCluster controller.

use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{OPA_OPERATOR_NAME, OpaClusterStatus, v1alpha2};
use stackable_operator::{
    client::Client,
    status::condition::{
        compute_conditions, daemonset::DaemonSetConditionBuilder,
        operations::ClusterOperationsConditionBuilder,
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

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&opa.spec.cluster_operation);

    let status = OpaClusterStatus {
        conditions: compute_conditions(opa, &[&ds_cond_builder, &cluster_operation_cond_builder]),
    };

    client
        .apply_patch_status(OPA_OPERATOR_NAME, opa, &status)
        .await
        .context(ApplyStatusSnafu)?;

    Ok(())
}
