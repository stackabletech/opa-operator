use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{APP_NAME, v1alpha1};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    commons::product_image_selection::ResolvedProductImage,
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::{Annotations, LabelError, Labels},
    role_utils::RoleGroupRef,
};

use crate::controller::build_recommended_labels;

pub const APP_PORT: u16 = 8081;
pub const APP_PORT_NAME: &str = "http";
pub const METRICS_PORT_NAME: &str = "metrics";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build label"))]
    BuildLabel { source: LabelError },

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },
}

/// The server-role service is the primary endpoint that should be used by clients that do not perform internal load balancing,
/// including targets outside of the cluster.
pub(crate) fn build_server_role_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service, Error> {
    let role_name = v1alpha1::OpaRole::Server.to_string();

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(opa.server_role_service_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label_value,
            &role_name,
            "global",
        ))
        .context(ObjectMetaSnafu)?
        .build();

    let service_selector_labels =
        Labels::role_selector(opa, APP_NAME, &role_name).context(BuildLabelSnafu)?;

    let service_spec = ServiceSpec {
        type_: Some(opa.spec.cluster_config.listener_class.k8s_service_type()),
        ports: Some(data_service_ports()),
        selector: Some(service_selector_labels.into()),
        internal_traffic_policy: Some("Local".to_string()),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
pub(crate) fn build_rolegroup_headless_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Service, Error> {
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup.rolegroup_headless_service_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label_value,
            &rolegroup.role,
            &rolegroup.role_group,
        ))
        .context(ObjectMetaSnafu)?
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
        ports: Some(data_service_ports()),
        selector: Some(role_group_selector_labels(opa, rolegroup)?.into()),
        publish_not_ready_addresses: Some(true),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and has the
/// prometheus.io/scrape label.
pub(crate) fn build_rolegroup_metrics_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Service, Error> {
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup.rolegroup_metrics_service_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label_value,
            &rolegroup.role,
            &rolegroup.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .with_labels(prometheus_labels())
        .with_annotations(prometheus_annotations())
        .build();

    let service_spec = ServiceSpec {
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(vec![metrics_service_port()]),
        selector: Some(role_group_selector_labels(opa, rolegroup)?.into()),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// Returns the [`Labels`] that can be used to select all Pods that are part of the roleGroup.
fn role_group_selector_labels(
    opa: &v1alpha1::OpaCluster,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Labels, Error> {
    Labels::role_group_selector(opa, APP_NAME, &rolegroup.role, &rolegroup.role_group)
        .context(BuildLabelSnafu)
}

fn data_service_ports() -> Vec<ServicePort> {
    // Currently only HTTP is exposed
    vec![ServicePort {
        name: Some(APP_PORT_NAME.to_string()),
        port: APP_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}

fn metrics_service_port() -> ServicePort {
    ServicePort {
        name: Some(METRICS_PORT_NAME.to_string()),
        // The metrics are served on the same port as the HTTP traffic
        port: APP_PORT.into(),
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
fn prometheus_annotations() -> Annotations {
    Annotations::try_from([
        ("prometheus.io/path".to_owned(), "/metrics".to_owned()),
        ("prometheus.io/port".to_owned(), APP_PORT.to_string()),
        ("prometheus.io/scheme".to_owned(), "http".to_owned()),
        ("prometheus.io/scrape".to_owned(), "true".to_owned()),
    ])
    .expect("should be valid annotations")
}
