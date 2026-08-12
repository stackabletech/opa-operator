//! Builds the [`PodDisruptionBudget`] that limits how many OPA Pods of a role a voluntary
//! disruption (a node drain, say) may take down at once.

use stackable_opa_operator::crd::{OpaRole, v1alpha2};
use stackable_operator::{
    k8s_openapi::api::policy::v1::PodDisruptionBudget,
    v2::builder::pdb::pod_disruption_budget_builder_with_role,
};

use crate::controller::{ValidatedCluster, controller_name, operator_name, product_name};

const DEFAULT_MAX_UNAVAILABLE: u16 = 1;

/// The role-level [`PodDisruptionBudget`], or `None` when the role has it disabled.
///
/// One per role rather than per role group. 
pub fn build_role_pod_disruption_budget(
    cluster: &ValidatedCluster,
    opa_role: &OpaRole,
    role_config: &v1alpha2::OpaRoleConfig,
) -> Option<PodDisruptionBudget> {
    if !role_config.pod_disruption_budget_enabled() {
        return None;
    }

    let max_unavailable = role_config
        .pod_disruption_budget
        .max_unavailable
        .unwrap_or(DEFAULT_MAX_UNAVAILABLE);

    Some(
        pod_disruption_budget_builder_with_role(
            cluster,
            &product_name(),
            &opa_role.clone().into(),
            &operator_name(),
            &controller_name(),
        )
        .with_max_unavailable(max_unavailable)
        .build(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn build(spec: serde_json::Value) -> Option<PodDisruptionBudget> {
        let cluster = validated_cluster_from_spec(spec);
        build_role_pod_disruption_budget(
            &cluster,
            &OpaRole::Server,
            cluster.role_config(&OpaRole::Server),
        )
    }

    /// A DaemonSet covers every node and `kubectl drain` skips its Pods, so a budget would protect
    /// nothing. This is the default, so existing installations gain no new object.
    #[test]
    fn daemonset_gets_no_pod_disruption_budget() {
        assert!(
            build(json!({
                "image": { "productVersion": "1.2.3" },
                "servers": { "roleGroups": { "default": {} } },
            }))
            .is_none()
        );
    }

    /// A Deployment's Pods are evictable, so the budget is created by default.
    #[test]
    fn deployment_gets_a_pod_disruption_budget_selecting_the_whole_role() {
        let pdb = build(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": { "workloadKind": "Deployment" },
                "roleGroups": { "default": {}, "other": {} },
            },
        }))
        .expect("a Deployment is protected by default");

        assert_eq!(pdb.metadata.name.as_deref(), Some("test-opa-server"));
        let spec = pdb.spec.expect("the builder always sets a spec");
        assert_eq!(
            spec.max_unavailable,
            Some(
                stackable_operator::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                    1
                )
            )
        );
        // Selecting the role rather than a role group is what lets one budget cover both role
        // groups configured above.
        let match_labels = spec
            .selector
            .and_then(|selector| selector.match_labels)
            .expect("the builder always sets a role selector");
        assert_eq!(
            match_labels
                .get("app.kubernetes.io/component")
                .map(String::as_str),
            Some("server")
        );
        assert!(!match_labels.contains_key("app.kubernetes.io/role-group"));
    }

    /// `enabled` is an explicit override, so a DaemonSet gets a budget when the administrator asks
    /// for one, even though it will not do much.
    #[test]
    fn explicitly_enabling_it_wins_over_the_workload_kind_default() {
        assert!(
            build(json!({
                "image": { "productVersion": "1.2.3" },
                "servers": {
                    "roleConfig": { "podDisruptionBudget": { "enabled": true } },
                    "roleGroups": { "default": {} },
                },
            }))
            .is_some()
        );
    }

    /// ...and disabling it opts a Deployment out.
    #[test]
    fn explicitly_disabling_it_opts_a_deployment_out() {
        assert!(
            build(json!({
                "image": { "productVersion": "1.2.3" },
                "servers": {
                    "roleConfig": {
                        "workloadKind": "Deployment",
                        "podDisruptionBudget": { "enabled": false },
                    },
                    "roleGroups": { "default": {} },
                },
            }))
            .is_none()
        );
    }

    #[test]
    fn configured_max_unavailable_overrides_the_default() {
        let pdb = build(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "roleConfig": {
                    "workloadKind": "Deployment",
                    "podDisruptionBudget": { "maxUnavailable": 2 },
                },
                "roleGroups": { "default": {} },
            },
        }))
        .expect("a Deployment is protected by default");

        assert_eq!(
            pdb.spec.unwrap().max_unavailable,
            Some(
                stackable_operator::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                    2
                )
            )
        );
    }
}
