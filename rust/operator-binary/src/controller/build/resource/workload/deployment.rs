//! Builds the rolegroup [`Deployment`] that runs a fixed number of OPA replicas.

use stackable_operator::k8s_openapi::{
    api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy, RollingUpdateDeployment},
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};

// The shared building blocks (constants, `Error`, the Pod template, the start-command helpers) all
// live in the parent module; a glob keeps this wrapper from restating the whole list.
use super::*;
use crate::controller::build::role_group_selector;

/// Runs a fixed number of replicas, unlike [`daemonset`](super::daemonset), which covers every
/// node. The Pods therefore do not cover every node and the role Service has to route to any of
/// them rather than to a node-local one.
#[allow(clippy::too_many_arguments)]
pub fn build_server_rolegroup_deployment(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    role_group: &OpaRoleGroupConfig,
    opa_bundle_builder_image: &str,
    user_info_fetcher_image: &str,
    resource_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<Deployment> {
    let pod_template = build_server_rolegroup_pod_template(
        cluster,
        role_group_name,
        role_group,
        opa_bundle_builder_image,
        user_info_fetcher_image,
        resource_info_fetcher_image,
        cluster_info,
    )?;

    let metadata = build::object_meta(
        cluster,
        cluster
            .role_group_resource_names(role_group_name)
            .deployment_name()
            .to_string(),
        recommended_labels_for_role_group_resources(cluster, &OpaRole::Server, role_group_name),
    )
    .build();

    let deployment_spec = DeploymentSpec {
        // Left unset so Kubernetes applies its default of one, rather than the operator inventing
        // a replica count.
        replicas: role_group.replicas.map(i32::from),
        selector: LabelSelector {
            match_labels: Some(
                role_group_selector(cluster, &OpaRole::Server, role_group_name).into(),
            ),
            ..LabelSelector::default()
        },
        template: pod_template,
        strategy: Some(DeploymentStrategy {
            type_: Some("RollingUpdate".to_string()),
            rolling_update: Some(RollingUpdateDeployment {
                max_surge: Some(IntOrString::Int(1)),
                max_unavailable: Some(IntOrString::Int(0)),
            }),
        }),
        ..DeploymentSpec::default()
    };

    Ok(Deployment {
        metadata,
        spec: Some(deployment_spec),
        status: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stackable_opa_operator::crd::OpaRole;
    use stackable_operator::commons::networking::DomainName;

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    fn build(cluster: &ValidatedCluster) -> Deployment {
        let (role_group_name, role_group) = cluster.role_group_configs[&OpaRole::Server]
            .iter()
            .next()
            .expect("the default role group should exist");
        build_server_rolegroup_deployment(
            cluster,
            role_group_name,
            role_group,
            "bundle-builder-image",
            "user-info-fetcher-image",
            "resource-info-fetcher-image",
            &cluster_info(),
        )
        .expect("the deployment should build")
    }

    /// Named like the DaemonSet it replaces, so switching `workloadKind` swaps like for like.
    #[test]
    fn deployment_has_expected_name_and_rolling_update_strategy() {
        let deployment = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": {} },
            },
        })));

        assert_eq!(
            deployment.metadata.name.as_deref(),
            Some("test-opa-server-default")
        );
        let strategy = deployment.spec.as_ref().unwrap().strategy.as_ref().unwrap();
        assert_eq!(strategy.type_.as_deref(), Some("RollingUpdate"));
        let rolling_update = strategy.rolling_update.as_ref().unwrap();
        // OPA sits in the products' hot path, so a rollout must never reduce the ready Pod count.
        assert_eq!(rolling_update.max_unavailable, Some(IntOrString::Int(0)));
        assert_eq!(rolling_update.max_surge, Some(IntOrString::Int(1)));
    }

    /// `replicas` is what a Deployment adds over a DaemonSet, so it has to reach the spec.
    #[test]
    fn deployment_takes_the_replicas_of_its_role_group() {
        let deployment = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": { "replicas": 3 } },
            },
        })));

        assert_eq!(deployment.spec.as_ref().unwrap().replicas, Some(3));
    }

    /// An unset `replicas` stays unset, leaving the Kubernetes default of one in place.
    #[test]
    fn deployment_without_replicas_leaves_them_unset() {
        let deployment = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": {} },
            },
        })));

        assert_eq!(deployment.spec.as_ref().unwrap().replicas, None);
    }

    /// The Pod template is shared with the DaemonSet and covered by its tests; this only checks that
    /// it is wrapped and selected the same way.
    #[test]
    fn deployment_wraps_the_shared_pod_template() {
        let deployment = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": {} },
            },
        })));

        let spec = deployment.spec.as_ref().unwrap();
        let containers: Vec<&str> = spec
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers
            .iter()
            .map(|container| container.name.as_str())
            .collect();
        assert!(containers.contains(&"opa"));
        assert!(containers.contains(&"bundle-builder"));

        let match_labels = spec.selector.match_labels.as_ref().unwrap();
        assert_eq!(
            match_labels
                .get("app.kubernetes.io/role-group")
                .map(String::as_str),
            Some("default")
        );
    }

    /// Replicas are only worth having if they are deployed on different nodes, so the default anti-affinity
    /// has to survive the config merge into the Pod template.
    #[test]
    fn deployment_pods_are_spread_across_nodes_by_default() {
        let deployment = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": { "replicas": 3 } },
            },
        })));

        let anti_affinity = deployment
            .spec
            .and_then(|spec| spec.template.spec)
            .and_then(|pod_spec| pod_spec.affinity)
            .and_then(|affinity| affinity.pod_anti_affinity)
            .expect("the default affinity spreads the role's Pods");

        let preferred = anti_affinity
            .preferred_during_scheduling_ignored_during_execution
            .expect("the spread is a soft term");
        assert_eq!(preferred.len(), 1);
        assert_eq!(preferred[0].weight, 70);
        assert_eq!(
            preferred[0].pod_affinity_term.topology_key,
            "kubernetes.io/hostname"
        );
    }
}
