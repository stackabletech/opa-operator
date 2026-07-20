//! Build steps that turn the [`ValidatedCluster`](super::ValidatedCluster) into
//! Kubernetes resource specifications.

use std::str::FromStr;

use snafu::{ResultExt, Snafu};
use stackable_operator::{utils::cluster_info::KubernetesClusterInfo, v2::types::common::Port};

use crate::controller::{
    KubernetesResources, RoleGroupName, ValidatedCluster,
    build::resource::{
        config_map::build_rolegroup_config_map,
        daemonset::build_server_rolegroup_daemonset,
        discovery::build_discovery_config_map,
        rbac::{build_role_binding, build_service_account},
        service::{
            build_rolegroup_headless_service, build_rolegroup_metrics_service,
            build_server_role_service,
        },
    },
};

pub mod properties;
pub mod resource;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build ConfigMap for role group {role_group}"))]
    ConfigMap {
        source: resource::config_map::Error,
        role_group: RoleGroupName,
    },

    #[snafu(display("failed to build DaemonSet for role group {role_group}"))]
    DaemonSet {
        source: resource::daemonset::Error,
        role_group: RoleGroupName,
    },

    #[snafu(display("failed to build the discovery ConfigMap"))]
    Discovery { source: resource::discovery::Error },
}

/// Builds every Kubernetes resource for the given validated cluster.
///
/// Does not need a Kubernetes client: every reference to another Kubernetes resource is already
/// dereferenced and validated by this point, so the errors returned here are resource-assembly
/// failures only.
///
/// `service_account_name` is the name of the RBAC `ServiceAccount` the Pods run under (the RBAC
/// resources are built and applied separately, in the reconcile step). `cluster_info` supplies the
/// Kubernetes cluster domain used in the discovery URL and the sidecar environment.
pub fn build(
    cluster: &ValidatedCluster,
    opa_bundle_builder_image: &str,
    user_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<KubernetesResources, Error> {
    let mut daemon_sets = vec![];
    let mut services = vec![];
    let mut config_maps = vec![];

    // The role-level load-balanced Service, which is not bound to a single role group.
    services.push(build_server_role_service(cluster));

    for role_group_configs in cluster.role_group_configs.values() {
        for (role_group_name, role_group) in role_group_configs {
            config_maps.push(
                build_rolegroup_config_map(cluster, role_group_name, role_group).context(
                    ConfigMapSnafu {
                        role_group: role_group_name.clone(),
                    },
                )?,
            );
            services.push(build_rolegroup_headless_service(cluster, role_group_name));
            services.push(build_rolegroup_metrics_service(cluster, role_group_name));
            daemon_sets.push(
                build_server_rolegroup_daemonset(
                    cluster,
                    role_group_name,
                    role_group,
                    opa_bundle_builder_image,
                    user_info_fetcher_image,
                    cluster_info,
                )
                .context(DaemonSetSnafu {
                    role_group: role_group_name.clone(),
                })?,
            );
        }
    }

    // The cluster-level discovery ConfigMap.
    config_maps.push(build_discovery_config_map(cluster, cluster_info).context(DiscoverySnafu)?);

    Ok(KubernetesResources {
        daemon_sets,
        services,
        config_maps,
        service_accounts: vec![build_service_account(cluster)],
        role_bindings: vec![build_role_binding(cluster)],
    })
}

/// The port the bundle-builder sidecar listens on, and which OPA connects to for bundle polling.
pub(crate) const BUNDLE_BUILDER_PORT: Port = Port(3030);

// Placeholder role-group name for the recommended labels of the role-level `Service`, which is not
// bound to a single role group. `global` matches the historical `app.kubernetes.io/role-group`
// value.
stackable_operator::constant!(pub(crate) PLACEHOLDER_ROLE_LEVEL_ROLE_GROUP: RoleGroupName = "global");

// Placeholder role-group name for the recommended labels of the discovery `ConfigMap`, which is a
// cluster-level object not bound to a single role group.
stackable_operator::constant!(pub(crate) PLACEHOLDER_DISCOVERY_ROLE_GROUP: RoleGroupName = "discovery");

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use stackable_operator::{
        commons::networking::DomainName, kube::Resource, utils::cluster_info::KubernetesClusterInfo,
    };

    use super::build;
    use crate::controller::{
        ValidatedCluster, build::properties::test_support::validated_cluster_from_spec,
    };

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    fn sorted_names(resources: &[impl Resource]) -> Vec<&str> {
        let mut names: Vec<&str> = resources
            .iter()
            .filter_map(|resource| resource.meta().name.as_deref())
            .collect();
        names.sort();
        names
    }

    /// A validated single-role-group cluster with no TLS or extra sidecars.
    fn cluster() -> ValidatedCluster {
        validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }))
    }

    #[test]
    fn build_produces_expected_resource_names() {
        let resources = build(
            &cluster(),
            "bundle-builder-image",
            "user-info-fetcher-image",
            &cluster_info(),
        )
        .expect("build succeeds");

        // One DaemonSet per role group.
        assert_eq!(
            sorted_names(&resources.daemon_sets),
            ["test-opa-server-default"]
        );
        // The role-level Service plus a headless and a metrics Service per role group.
        assert_eq!(
            sorted_names(&resources.services),
            [
                "test-opa-server",
                "test-opa-server-default-headless",
                "test-opa-server-default-metrics",
            ]
        );
        // The per-role-group ConfigMap plus the cluster-level discovery ConfigMap (`test-opa`).
        assert_eq!(
            sorted_names(&resources.config_maps),
            ["test-opa", "test-opa-server-default"]
        );
    }

    /// Locks the RBAC resource names, the roleRef, and the recommended label set against
    /// accidental drift. The fixture's cluster name deliberately differs from the product name so
    /// that swapped `name`/`instance` label values cannot pass unnoticed.
    #[test]
    fn build_produces_rbac() {
        let resources = build(
            &cluster(),
            "bundle-builder-image",
            "user-info-fetcher-image",
            &cluster_info(),
        )
        .expect("build succeeds");

        assert_eq!(
            sorted_names(&resources.service_accounts),
            ["test-opa-serviceaccount"]
        );
        assert_eq!(
            sorted_names(&resources.role_bindings),
            ["test-opa-rolebinding"]
        );

        let expected_labels = BTreeMap::from(
            [
                ("app.kubernetes.io/component", "none"),
                ("app.kubernetes.io/instance", "test-opa"),
                (
                    "app.kubernetes.io/managed-by",
                    "opa.stackable.tech_opacluster",
                ),
                ("app.kubernetes.io/name", "opa"),
                ("app.kubernetes.io/role-group", "none"),
                ("app.kubernetes.io/version", "1.2.3-stackable0.0.0-dev"),
                ("stackable.tech/vendor", "Stackable"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        let service_account = resources
            .service_accounts
            .first()
            .expect("a ServiceAccount is built");
        assert_eq!(
            service_account.metadata.labels,
            Some(expected_labels.clone())
        );

        let role_binding = resources
            .role_bindings
            .first()
            .expect("a RoleBinding is built");
        assert_eq!(role_binding.metadata.labels, Some(expected_labels));
        assert_eq!(role_binding.role_ref.name, "opa-clusterrole");
    }
}
