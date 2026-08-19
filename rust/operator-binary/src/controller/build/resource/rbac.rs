//! Builds the RBAC resources (ServiceAccount + RoleBinding) shared by all role groups.

use stackable_operator::{
    k8s_openapi::api::{core::v1::ServiceAccount, rbac::v1::RoleBinding},
    v2::rbac,
};

use crate::controller::{ValidatedCluster, build::recommended_labels_for_cluster_resources};

/// Builds the [`ServiceAccount`] that the role-group Pods run under.
pub fn build_service_account(cluster: &ValidatedCluster) -> ServiceAccount {
    rbac::build_service_account(
        cluster,
        &cluster.cluster_resource_names(),
        recommended_labels_for_cluster_resources(cluster),
    )
}

/// Builds the [`RoleBinding`] that binds the [`ServiceAccount`] from [`build_service_account`] to
/// the operator-deployed ClusterRole.
pub fn build_role_binding(cluster: &ValidatedCluster) -> RoleBinding {
    rbac::build_role_binding(
        cluster,
        &cluster.cluster_resource_names(),
        recommended_labels_for_cluster_resources(cluster),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::controller::build::properties::test_support::{
        app_version_label, validated_cluster_from_spec,
    };

    // `test-opa` vs `opa`: see the swap-guard note on `validated_cluster_from_spec`.
    fn cluster() -> ValidatedCluster {
        validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }))
    }

    #[test]
    fn test_service_account() {
        let service_account = build_service_account(&cluster());

        assert_eq!(
            json!({
                "apiVersion": "v1",
                "kind": "ServiceAccount",
                "metadata": {
                    // The RBAC resources are cluster-shared, so they carry the cluster-level
                    // recommended labels (no role or role-group label).
                    "labels": {
                        "app.kubernetes.io/instance": "test-opa",
                        "app.kubernetes.io/managed-by": "opa.stackable.tech_opacluster",
                        "app.kubernetes.io/name": "opa",
                        "app.kubernetes.io/version": app_version_label("1.2.3"),
                        "stackable.tech/vendor": "Stackable"
                    },
                    "name": "test-opa-serviceaccount",
                    "namespace": "default",
                    "ownerReferences": [
                        {
                            "apiVersion": "opa.stackable.tech/v1alpha2",
                            "controller": true,
                            "kind": "OpaCluster",
                            "name": "test-opa",
                            "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd"
                        }
                    ]
                }
            }),
            serde_json::to_value(service_account).expect("must be serializable")
        );
    }

    #[test]
    fn test_role_binding() {
        let role_binding = build_role_binding(&cluster());

        assert_eq!(
            json!({
                "apiVersion": "rbac.authorization.k8s.io/v1",
                "kind": "RoleBinding",
                "metadata": {
                    "labels": {
                        "app.kubernetes.io/instance": "test-opa",
                        "app.kubernetes.io/managed-by": "opa.stackable.tech_opacluster",
                        "app.kubernetes.io/name": "opa",
                        "app.kubernetes.io/version": app_version_label("1.2.3"),
                        "stackable.tech/vendor": "Stackable"
                    },
                    "name": "test-opa-rolebinding",
                    "namespace": "default",
                    "ownerReferences": [
                        {
                            "apiVersion": "opa.stackable.tech/v1alpha2",
                            "controller": true,
                            "kind": "OpaCluster",
                            "name": "test-opa",
                            "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd"
                        }
                    ]
                },
                "roleRef": {
                    "apiGroup": "rbac.authorization.k8s.io",
                    "kind": "ClusterRole",
                    "name": "opa-clusterrole"
                },
                "subjects": [
                    {
                        "kind": "ServiceAccount",
                        "name": "test-opa-serviceaccount",
                        "namespace": "default"
                    }
                ]
            }),
            serde_json::to_value(role_binding).expect("must be serializable")
        );
    }
}
