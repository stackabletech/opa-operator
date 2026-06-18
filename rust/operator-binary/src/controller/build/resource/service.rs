use std::{collections::BTreeMap, str::FromStr};

use stackable_operator::{
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::{Annotations, Labels},
    v2::types::common::Port,
};

use crate::controller::{RoleGroupName, ValidatedCluster};

pub const APP_PORT: Port = Port(8081);
pub const APP_TLS_PORT: Port = Port(8443);
pub const APP_PORT_NAME: &str = "http";
pub const APP_TLS_PORT_NAME: &str = "https";
pub const METRICS_PORT_NAME: &str = "metrics";

/// The role-level `Service` and the discovery `ConfigMap` are not bound to a single role group, but
/// the recommended labels require one. `global` is used as a placeholder, matching the historical
/// `app.kubernetes.io/role-group` value.
fn role_level_role_group_name() -> RoleGroupName {
    RoleGroupName::from_str("global").expect("'global' is a valid role group name")
}

/// The server-role service is the primary endpoint that should be used by clients that do not perform internal load balancing,
/// including targets outside of the cluster.
pub(crate) fn build_server_role_service(cluster: &ValidatedCluster) -> Service {
    let metadata = cluster
        .object_meta(
            cluster.server_role_service_name(),
            &role_level_role_group_name(),
        )
        .build();

    let service_spec = ServiceSpec {
        type_: Some(cluster.cluster_config.listener_class.k8s_service_type()),
        ports: Some(data_service_ports(cluster.is_tls_enabled())),
        selector: Some(cluster.role_selector().into()),
        // This ensures that products (e.g. Trino) on a node always talk to the OPA pod on the
        // same node, avoiding cross-node latency. The downside is that if the local OPA pod is
        // unavailable, requests fail instead of falling back to another node.
        // TODO: Once our minimum supported Kubernetes version is 1.35, use
        // `trafficDistribution: PreferSameNode` instead, which prefers the local node but
        // gracefully falls back to other nodes if the local pod is unavailable.
        internal_traffic_policy: Some("Local".to_string()),
        ..ServiceSpec::default()
    };

    Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    }
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
pub(crate) fn build_rolegroup_headless_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    let metadata = cluster
        .object_meta(
            cluster
                .resource_names(role_group_name)
                .headless_service_name()
                .to_string(),
            role_group_name,
        )
        .build();

    // Currently we don't offer listener-exposition of OPA mostly due to security concerns.
    // OPA is currently public within the Kubernetes (without authentication).
    // Opening it up to outside of Kubernetes might worsen things.
    // We are open to implement listener-integration, but this needs to be thought through before
    // implementing it.
    // Note: We have kind of similar situations for HMS and Zookeeper, as the authentication
    // options there are non-existent (mTLS still opens plain port) or suck (Kerberos).
    let service_spec = headless_cluster_ip_service_spec(
        data_service_ports(cluster.is_tls_enabled()),
        cluster.role_group_selector(role_group_name).into(),
        true,
    );

    Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    }
}

/// A headless (`ClusterIP: None`) [`ServiceSpec`] for the given ports and role-group `selector`.
fn headless_cluster_ip_service_spec(
    ports: Vec<ServicePort>,
    selector: BTreeMap<String, String>,
    publish_not_ready_addresses: bool,
) -> ServiceSpec {
    ServiceSpec {
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(ports),
        selector: Some(selector),
        publish_not_ready_addresses: publish_not_ready_addresses.then_some(true),
        ..ServiceSpec::default()
    }
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and has the
/// prometheus.io/scrape label.
pub(crate) fn build_rolegroup_metrics_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    let tls_enabled = cluster.is_tls_enabled();
    let metadata = cluster
        .object_meta(
            cluster
                .resource_names(role_group_name)
                .metrics_service_name()
                .to_string(),
            role_group_name,
        )
        .with_labels(prometheus_labels())
        .with_annotations(prometheus_annotations(tls_enabled))
        .build();

    let service_spec = headless_cluster_ip_service_spec(
        vec![metrics_service_port(tls_enabled)],
        cluster.role_group_selector(role_group_name).into(),
        false,
    );

    Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    }
}

fn data_service_ports(tls_enabled: bool) -> Vec<ServicePort> {
    let (port_name, port) = if tls_enabled {
        (APP_TLS_PORT_NAME, APP_TLS_PORT)
    } else {
        (APP_PORT_NAME, APP_PORT)
    };

    vec![ServicePort {
        name: Some(port_name.to_string()),
        port: port.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}

fn metrics_service_port(tls_enabled: bool) -> ServicePort {
    let port = if tls_enabled { APP_TLS_PORT } else { APP_PORT };

    ServicePort {
        name: Some(METRICS_PORT_NAME.to_string()),
        // The metrics are served on the same port as the HTTP/HTTPS traffic
        port: port.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }
}

/// Common labels for Prometheus
fn prometheus_labels() -> Labels {
    Labels::try_from([("prometheus.io/scrape", "true")]).expect("should be a valid label")
}

/// Common annotations for Prometheus
///
/// These annotations can be used in a ServiceMonitor.
///
/// see also <https://github.com/prometheus-community/helm-charts/blob/prometheus-27.32.0/charts/prometheus/values.yaml#L983-L1036>
fn prometheus_annotations(tls_enabled: bool) -> Annotations {
    let (port, scheme) = if tls_enabled {
        (APP_TLS_PORT, "https")
    } else {
        (APP_PORT, "http")
    };

    Annotations::try_from([
        ("prometheus.io/path".to_owned(), "/metrics".to_owned()),
        ("prometheus.io/port".to_owned(), port.to_string()),
        ("prometheus.io/scheme".to_owned(), scheme.to_owned()),
        ("prometheus.io/scrape".to_owned(), "true".to_owned()),
    ])
    .expect("should be valid annotations")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        controller::build::properties::test_support::validated_cluster_from_spec, crd::OpaRole,
    };

    const ROLE_GROUP_LABEL: &str = "app.kubernetes.io/role-group";

    fn cluster(tls: bool) -> ValidatedCluster {
        let mut spec = json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        });
        if tls {
            spec["clusterConfig"] = json!({ "tls": { "serverSecretClass": "tls" } });
        }
        validated_cluster_from_spec(spec)
    }

    fn default_role_group(cluster: &ValidatedCluster) -> RoleGroupName {
        cluster.role_group_configs[&OpaRole::Server]
            .keys()
            .next()
            .expect("the default role group should exist")
            .clone()
    }

    /// Returns the `(name, port)` of the single data port of `service`.
    fn single_port(service: &Service) -> (String, i32) {
        let ports = service.spec.as_ref().unwrap().ports.as_ref().unwrap();
        assert_eq!(ports.len(), 1, "expected exactly one port");
        (ports[0].name.clone().unwrap(), ports[0].port)
    }

    #[test]
    fn role_service_is_cluster_internal_with_node_local_traffic() {
        let cluster = cluster(false);
        let service = build_server_role_service(&cluster);
        let spec = service.spec.unwrap();

        assert_eq!(service.metadata.name.as_deref(), Some("test-opa-server"));
        // Default listener class `cluster-internal` maps to a `ClusterIP` Service.
        assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));
        assert_eq!(spec.internal_traffic_policy.as_deref(), Some("Local"));
        // The role-level service selects the whole role, so it must not pin a role group.
        assert!(!spec.selector.unwrap().contains_key(ROLE_GROUP_LABEL));
    }

    #[test]
    fn role_service_port_follows_tls() {
        assert_eq!(
            single_port(&build_server_role_service(&cluster(false))),
            ("http".to_owned(), 8081)
        );
        assert_eq!(
            single_port(&build_server_role_service(&cluster(true))),
            ("https".to_owned(), 8443)
        );
    }

    #[test]
    fn headless_service_is_headless_and_role_group_scoped() {
        let cluster = cluster(false);
        let rg = default_role_group(&cluster);
        let service = build_rolegroup_headless_service(&cluster, &rg);
        let spec = service.spec.unwrap();

        assert_eq!(
            service.metadata.name.as_deref(),
            Some("test-opa-server-default-headless")
        );
        assert_eq!(spec.cluster_ip.as_deref(), Some("None"));
        assert_eq!(spec.publish_not_ready_addresses, Some(true));
        assert!(spec.selector.unwrap().contains_key(ROLE_GROUP_LABEL));
    }

    #[test]
    fn metrics_service_has_prometheus_metadata() {
        let cluster = cluster(false);
        let rg = default_role_group(&cluster);
        let service = build_rolegroup_metrics_service(&cluster, &rg);
        let spec = service.spec.as_ref().unwrap();

        assert_eq!(
            service.metadata.name.as_deref(),
            Some("test-opa-server-default-metrics")
        );
        assert_eq!(spec.cluster_ip.as_deref(), Some("None"));
        assert_eq!(single_port(&service), ("metrics".to_owned(), 8081));

        let annotations = service.metadata.annotations.unwrap();
        assert_eq!(
            annotations.get("prometheus.io/scrape").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            annotations.get("prometheus.io/scheme").map(String::as_str),
            Some("http")
        );
        assert_eq!(
            annotations.get("prometheus.io/port").map(String::as_str),
            Some("8081")
        );
    }
}
