//! Builds the discovery [`ConfigMap`] clients use to connect to an `OpaCluster`.
//!
//! The content comes entirely from the [`ValidatedCluster`] (plus the externally-resolved role
//! service and `cluster_info`).

use std::str::FromStr;

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    utils::cluster_info::KubernetesClusterInfo,
    v2::builder::meta::ownerreference_from_resource,
};

use super::service::{APP_PORT, APP_TLS_PORT};
use crate::controller::{RoleGroupName, ValidatedCluster};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build ConfigMap"))]
    BuildConfigMap {
        source: stackable_operator::builder::configmap::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds the discovery [`ConfigMap`] containing the URL (and, when TLS is enabled, the secret
/// class) clients need to connect to the cluster.
pub fn build_discovery_config_map(
    cluster: &ValidatedCluster,
    cluster_info: &KubernetesClusterInfo,
) -> Result<ConfigMap> {
    let (scheme, port) = if cluster.cluster_config.tls.is_some() {
        ("https", APP_TLS_PORT)
    } else {
        ("http", APP_PORT)
    };

    let url = format!(
        "{scheme}://{service_name}.{namespace}.svc.{cluster_domain}:{port}/",
        service_name = cluster.server_role_service_name(),
        namespace = cluster.namespace,
        cluster_domain = cluster_info.cluster_domain,
    );

    // Discovery is a cluster-level object (named after the cluster); `discovery` is used as a
    // placeholder role-group name for the recommended labels.
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(cluster.recommended_labels(
            &RoleGroupName::from_str("discovery").expect("'discovery' is a valid role group name"),
        ))
        .build();

    let mut cm_builder = ConfigMapBuilder::new();
    cm_builder.metadata(metadata).add_data("OPA", url);

    if let Some(tls) = &cluster.cluster_config.tls {
        cm_builder.add_data("OPA_SECRET_CLASS", &tls.server_secret_class);
    }

    cm_builder.build().context(BuildConfigMapSnafu)
}

#[cfg(test)]
mod tests {
    use stackable_operator::{
        commons::networking::DomainName, utils::cluster_info::KubernetesClusterInfo,
    };

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    #[test]
    fn renders_http_url_without_tls() {
        let (_opa, validated) = validated_cluster_from_spec(serde_json::json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));

        let cm = build_discovery_config_map(&validated, &cluster_info()).unwrap();
        let data = cm.data.unwrap();

        assert_eq!(
            data.get("OPA").map(String::as_str),
            Some("http://test-opa-server.default.svc.cluster.local:8081/")
        );
        assert!(!data.contains_key("OPA_SECRET_CLASS"));
    }

    #[test]
    fn renders_https_url_and_secret_class_with_tls() {
        let (_opa, validated) = validated_cluster_from_spec(serde_json::json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": { "tls": { "serverSecretClass": "tls" } },
            "servers": { "roleGroups": { "default": {} } },
        }));

        let cm = build_discovery_config_map(&validated, &cluster_info()).unwrap();
        let data = cm.data.unwrap();

        assert_eq!(
            data.get("OPA").map(String::as_str),
            Some("https://test-opa-server.default.svc.cluster.local:8443/")
        );
        assert_eq!(
            data.get("OPA_SECRET_CLASS").map(String::as_str),
            Some("tls")
        );
    }
}
