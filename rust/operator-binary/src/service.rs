use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{v1alpha1, APP_NAME};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    commons::product_image_selection::ResolvedProductImage,
    k8s_openapi::{
        api::core::v1::{Service, ServicePort, ServiceSpec},
        apimachinery::pkg::util::intstr::IntOrString,
    },
    kube::runtime::reflector::ObjectRef,
    kvp::{Label, LabelError, Labels},
    role_utils::RoleGroupRef,
};

use crate::controller::{build_recommended_labels, APP_PORT, APP_PORT_NAME, METRICS_PORT_NAME};

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object {opa} is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
        opa: ObjectRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("object has no name associated"))]
    NoName,

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build label"))]
    BuildLabel { source: LabelError },
}

pub struct ServiceConfig {
    pub name: String,
    pub internal_traffic_policy: String,
}

pub fn build_discoverable_services(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    service_configs: Vec<ServiceConfig>,
) -> Result<Vec<Service>> {
    let mut services = vec![];

    // discoverable role services
    for sc in service_configs {
        services.push(build_server_role_service(opa, resolved_product_image, sc)?);
    }

    Ok(services)
}

fn build_server_role_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    service_config: ServiceConfig,
) -> Result<Service> {
    let role_name = v1alpha1::OpaRole::Server.to_string();

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(service_config.name)
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu {
            opa: ObjectRef::from_obj(opa),
        })?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &role_name,
            "global",
        ))
        .context(ObjectMetaSnafu)?
        .build();

    let service_selector_labels =
        Labels::role_selector(opa, APP_NAME, &role_name).context(BuildLabelSnafu)?;

    let service_spec = ServiceSpec {
        type_: Some(opa.spec.cluster_config.listener_class.k8s_service_type()),
        ports: Some(vec![ServicePort {
            name: Some(APP_PORT_NAME.to_string()),
            port: APP_PORT.into(),
            protocol: Some("TCP".to_string()),
            ..ServicePort::default()
        }]),
        selector: Some(service_selector_labels.into()),
        internal_traffic_policy: Some(service_config.internal_traffic_policy),
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
pub fn build_rolegroup_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Service> {
    let prometheus_label =
        Label::try_from(("prometheus.io/scrape", "true")).context(BuildLabelSnafu)?;

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup.object_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu {
            opa: ObjectRef::from_obj(opa),
        })?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup.role,
            &rolegroup.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .with_label(prometheus_label)
        .build();

    let service_selector_labels =
        Labels::role_group_selector(opa, APP_NAME, &rolegroup.role, &rolegroup.role_group)
            .context(BuildLabelSnafu)?;

    let service_spec = ServiceSpec {
        // Internal communication does not need to be exposed
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(service_ports()),
        selector: Some(service_selector_labels.into()),
        publish_not_ready_addresses: Some(true),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

fn service_ports() -> Vec<ServicePort> {
    vec![
        ServicePort {
            name: Some(APP_PORT_NAME.to_string()),
            port: APP_PORT.into(),
            protocol: Some("TCP".to_string()),
            ..ServicePort::default()
        },
        ServicePort {
            name: Some(METRICS_PORT_NAME.to_string()),
            port: 9504, // Arbitrary port number, this is never actually used anywhere
            protocol: Some("TCP".to_string()),
            target_port: Some(IntOrString::String(APP_PORT_NAME.to_string())),
            ..ServicePort::default()
        },
    ]
}
