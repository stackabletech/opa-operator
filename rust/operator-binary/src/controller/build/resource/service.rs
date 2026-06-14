use std::str::FromStr;

use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::{Annotations, Labels},
    v2::builder::meta::ownerreference_from_resource,
};

use crate::{
    controller::{RoleGroupName, ValidatedCluster},
    crd::v1alpha2,
};

pub const APP_PORT: u16 = 8081;
pub const APP_TLS_PORT: u16 = 8443;
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
pub(crate) fn build_server_role_service(
    opa: &v1alpha2::OpaCluster,
    cluster: &ValidatedCluster,
) -> Service {
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .name(cluster.server_role_service_name())
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(cluster.recommended_labels(&role_level_role_group_name()))
        .build();

    let service_spec = ServiceSpec {
        type_: Some(opa.spec.cluster_config.listener_class.k8s_service_type()),
        ports: Some(data_service_ports(cluster.cluster_config.tls.is_some())),
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
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .name(
            cluster
                .resource_names(role_group_name)
                .headless_service_name()
                .to_string(),
        )
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(cluster.recommended_labels(role_group_name))
        .build();

    let service_spec = ServiceSpec {
        // Currently we don't offer listener-exposition of OPA mostly due to security concerns.
        // OPA is currently public within the Kubernetes (without authentication).
        // Opening it up to outside of Kubernetes might worsen things.
        // We are open to implement listener-integration, but this needs to be thought through before
        // implementing it.
        // Note: We have kind of similar situations for HMS and Zookeeper, as the authentication
        // options there are non-existent (mTLS still opens plain port) or suck (Kerberos).
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(data_service_ports(cluster.cluster_config.tls.is_some())),
        selector: Some(cluster.role_group_selector(role_group_name).into()),
        publish_not_ready_addresses: Some(true),
        ..ServiceSpec::default()
    };

    Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    }
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and has the
/// prometheus.io/scrape label.
pub(crate) fn build_rolegroup_metrics_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    let tls_enabled = cluster.cluster_config.tls.is_some();
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .name(metrics_service_name(cluster, role_group_name))
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(cluster.recommended_labels(role_group_name))
        .with_labels(prometheus_labels())
        .with_annotations(prometheus_annotations(tls_enabled))
        .build();

    let service_spec = ServiceSpec {
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(vec![metrics_service_port(tls_enabled)]),
        selector: Some(cluster.role_group_selector(role_group_name).into()),
        ..ServiceSpec::default()
    };

    Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    }
}

/// The metrics [`Service`] name, `<cluster>-<role>-<rolegroup>-metrics`.
///
/// [`ResourceNames`](stackable_operator::v2::role_group_utils::ResourceNames) has no metrics
/// service helper, so the `-metrics` suffix is appended to the qualified role-group name (which is
/// also the StatefulSet/DaemonSet name).
pub(crate) fn metrics_service_name(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> String {
    format!(
        "{qualified}-metrics",
        qualified = cluster.resource_names(role_group_name).stateful_set_name()
    )
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
