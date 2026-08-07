//! The apply step in the OpaCluster controller.

use std::marker::PhantomData;

use serde_json::json;
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    cluster_resources::{ClusterResource, ClusterResourceApplyStrategy, ClusterResources},
    deep_merger::ObjectOverrides,
    k8s_openapi::api::apps::v1::DaemonSet,
    kube::ResourceExt,
    v2::cluster_resources::cluster_resources_new,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::controller::{
    Applied, KubernetesResources, Prepared, ValidatedCluster, controller_name, operator_name,
    product_name,
};

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("failed to apply Kubernetes resource"))]
    ApplyResource {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply legacy field-manager patch for DaemonSet {name}"))]
    ApplyPatchDaemonSet {
        source: stackable_operator::client::Error,
        name: String,
    },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphanedResources {
        source: stackable_operator::cluster_resources::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Applier for the Kubernetes resource specifications produced by this controller.
///
/// The implementation is not tied to this controller and could theoretically be moved to
/// stackable_operator if [`KubernetesResources`] would contain all possible resource types.
pub struct Applier<'a> {
    client: &'a Client,
    cluster_resources: ClusterResources<'a>,
}

impl<'a> Applier<'a> {
    pub fn new(
        client: &'a Client,
        cluster: &ValidatedCluster,
        apply_strategy: ClusterResourceApplyStrategy,
        object_overrides: &'a ObjectOverrides,
    ) -> Applier<'a> {
        let cluster_resources = cluster_resources_new(
            &product_name(),
            &operator_name(),
            &controller_name(),
            &cluster.name,
            &cluster.namespace,
            &cluster.uid,
            apply_strategy,
            object_overrides,
        );

        Applier {
            client,
            cluster_resources,
        }
    }

    /// Applies the given Kubernetes resources and marks them as applied.
    pub async fn apply(
        mut self,
        resources: KubernetesResources<Prepared>,
    ) -> Result<KubernetesResources<Applied>> {
        // Destructured without `..`, so adding a field to [`KubernetesResources`] fails to
        // compile here instead of silently never being applied.
        let KubernetesResources {
            daemon_sets,
            services,
            config_maps,
            service_accounts,
            role_bindings,
            status: _,
        } = resources;

        // Apply order is: DaemonSets last (a changed mounted ConfigMap must exist first, else the
        // Pods restart a second time -- commons-operator#111). The ServiceAccount comes first
        // because the Pods reference it at creation time.
        let service_accounts = self.add_resources(service_accounts).await?;
        let role_bindings = self.add_resources(role_bindings).await?;
        let services = self.add_resources(services).await?;
        let config_maps = self.add_resources(config_maps).await?;
        let daemon_sets = self.add_resources(daemon_sets).await?;

        self.remove_legacy_field_manager_scope(&daemon_sets).await?;

        self.cluster_resources
            .delete_orphaned_resources(self.client)
            .await
            .context(DeleteOrphanedResourcesSnafu)?;

        Ok(KubernetesResources {
            daemon_sets,
            services,
            config_maps,
            service_accounts,
            role_bindings,
            status: PhantomData,
        })
    }

    async fn add_resources<T: ClusterResource + Sync>(
        &mut self,
        resources: Vec<T>,
    ) -> Result<Vec<T>> {
        let mut applied_resources = vec![];

        for resource in resources {
            let applied_resource = self
                .cluster_resources
                .add(self.client, resource)
                .await
                .context(ApplyResourceSnafu)?;
            applied_resources.push(applied_resource);
        }

        Ok(applied_resources)
    }

    /// Relinquishes the fields still owned by the historical field manager scope "opacluster".
    ///
    /// A previous version of opa-operator used the field manager scope "opacluster" to write out a
    /// DaemonSet with the bundle-builder container called "opa-bundle-builder". During
    /// https://github.com/stackabletech/opa-operator/pull/420 it was renamed to "bundle-builder".
    /// As we are now using the field manager scope "opa.stackable.tech_opacluster", our old changes
    /// (with the old container) would stay valid. We have to use the old field manager scope and
    /// post an empty patch to get rid of it.
    /// https://github.com/stackabletech/issues/issues/390 will implement a proper fix, which also
    /// covers Services and ConfigMaps. For details see
    /// https://github.com/stackabletech/opa-operator/issues/444.
    async fn remove_legacy_field_manager_scope(&self, daemon_sets: &[DaemonSet]) -> Result<()> {
        for daemon_set in daemon_sets {
            tracing::trace!(
                "Removing old field manager scope \"opacluster\" of DaemonSet {daemonset_name} to remove the \"opa-bundle-builder\" container. \
                See https://github.com/stackabletech/opa-operator/issues/444 and https://github.com/stackabletech/issues/issues/390 for details.",
                daemonset_name = daemon_set.name_any()
            );

            self.client
                .apply_patch(
                    "opacluster",
                    daemon_set,
                    // We can hardcode this here, as https://github.com/stackabletech/issues/issues/390
                    // will solve the general problem and we always have created DaemonSets using
                    // the "apps/v1" version.
                    json!({"apiVersion": "apps/v1", "kind": "DaemonSet"}),
                )
                .await
                .context(ApplyPatchDaemonSetSnafu {
                    name: daemon_set.name_any(),
                })?;
        }

        Ok(())
    }
}
