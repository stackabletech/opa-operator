//! Builds the discovery [`ConfigMap`] clients use to connect to an `OpaCluster`.
//!
//! The content comes entirely from the [`ValidatedCluster`] (plus the externally-resolved role
//! [`Service`] and `cluster_info`); the owner object is only used for the owner reference and
//! object metadata.

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::{ConfigMap, Service},
    kube::runtime::reflector::ObjectRef,
    utils::cluster_info::KubernetesClusterInfo,
};

use crate::{
    controller::{build_recommended_labels, validate::ValidatedCluster},
    crd::{OpaRole, v1alpha2},
    service::{APP_PORT, APP_TLS_PORT},
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object {} is missing metadata to build owner reference", opa))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
        opa: ObjectRef<v1alpha2::OpaCluster>,
    },

    #[snafu(display("the role Service has no name associated"))]
    NoServiceName,

    #[snafu(display("the role Service has no namespace associated"))]
    NoServiceNamespace,

    #[snafu(display("failed to build ConfigMap"))]
    BuildConfigMap {
        source: stackable_operator::builder::configmap::Error,
    },

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds the discovery [`ConfigMap`] containing the URL (and, when TLS is enabled, the secret
/// class) clients need to connect to the cluster.
pub fn build_discovery_config_map(
    cluster: &ValidatedCluster,
    svc: &Service,
    cluster_info: &KubernetesClusterInfo,
    owner: &v1alpha2::OpaCluster,
) -> Result<ConfigMap> {
    let name = cluster.name.to_string();

    let (scheme, port) = if cluster.cluster_config.tls.is_some() {
        ("https", APP_TLS_PORT)
    } else {
        ("http", APP_PORT)
    };

    let url = format!(
        "{scheme}://{service_name}.{namespace}.svc.{cluster_domain}:{port}/",
        service_name = svc.metadata.name.as_deref().context(NoServiceNameSnafu)?,
        namespace = svc
            .metadata
            .namespace
            .as_deref()
            .context(NoServiceNamespaceSnafu)?,
        cluster_domain = cluster_info.cluster_domain,
    );

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(owner)
        .name(&name)
        .ownerreference_from_resource(owner, None, Some(true))
        .with_context(|_| ObjectMissingMetadataForOwnerRefSnafu {
            opa: ObjectRef::from_obj(owner),
        })?
        .with_recommended_labels(&build_recommended_labels(
            owner,
            &cluster.image.app_version_label_value,
            &OpaRole::Server.to_string(),
            "discovery",
        ))
        .context(ObjectMetaSnafu)?
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
        commons::networking::DomainName,
        k8s_openapi::api::core::v1::{Service, ServiceSpec},
        kube::api::ObjectMeta,
        utils::cluster_info::KubernetesClusterInfo,
    };

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn role_service() -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some("test-opa-server".to_owned()),
                namespace: Some("default".to_owned()),
                ..ObjectMeta::default()
            },
            spec: Some(ServiceSpec::default()),
            status: None,
        }
    }

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    #[test]
    fn renders_http_url_without_tls() {
        let (opa, validated) = validated_cluster_from_spec(serde_json::json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));

        let cm =
            build_discovery_config_map(&validated, &role_service(), &cluster_info(), &opa).unwrap();
        let data = cm.data.unwrap();

        assert_eq!(
            data.get("OPA").map(String::as_str),
            Some("http://test-opa-server.default.svc.cluster.local:8081/")
        );
        assert!(!data.contains_key("OPA_SECRET_CLASS"));
    }

    #[test]
    fn renders_https_url_and_secret_class_with_tls() {
        let (opa, validated) = validated_cluster_from_spec(serde_json::json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": { "tls": { "serverSecretClass": "tls" } },
            "servers": { "roleGroups": { "default": {} } },
        }));

        let cm =
            build_discovery_config_map(&validated, &role_service(), &cluster_info(), &opa).unwrap();
        let data = cm.data.unwrap();

        assert_eq!(
            data.get("OPA").map(String::as_str),
            Some("https://test-opa-server.default.svc.cluster.local:8443/")
        );
        assert_eq!(data.get("OPA_SECRET_CLASS").map(String::as_str), Some("tls"));
    }
}
