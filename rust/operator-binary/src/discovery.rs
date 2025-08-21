use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::v1alpha1;
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    commons::product_image_selection::ResolvedProductImage,
    k8s_openapi::api::core::v1::{ConfigMap, Service},
    kube::{Resource, ResourceExt, runtime::reflector::ObjectRef},
    utils::cluster_info::KubernetesClusterInfo,
};

use crate::controller::{APP_PORT, build_recommended_labels};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object {} is missing metadata to build owner reference", opa))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
        opa: ObjectRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("object has no name associated"))]
    NoName,

    #[snafu(display("object has no namespace associated"))]
    NoNamespace,

    #[snafu(display("failed to build ConfigMap"))]
    BuildConfigMap {
        source: stackable_operator::builder::configmap::Error,
    },

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },
}

/// Builds discovery [`ConfigMap`]s for connecting to a [`v1alpha1::OpaCluster`] for all expected scenarios
pub fn build_discovery_configmaps(
    owner: &impl Resource<DynamicType = ()>,
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    svc: &Service,
    cluster_info: &KubernetesClusterInfo,
) -> Result<Vec<ConfigMap>, Error> {
    let name = owner.name_any();
    Ok(vec![build_discovery_configmap(
        &name,
        owner,
        opa,
        resolved_product_image,
        svc,
        cluster_info,
    )?])
}

/// Build a discovery [`ConfigMap`] containing information about how to connect to a certain [`v1alpha1::OpaCluster`]
fn build_discovery_configmap(
    name: &str,
    owner: &impl Resource<DynamicType = ()>,
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    svc: &Service,
    cluster_info: &KubernetesClusterInfo,
) -> Result<ConfigMap, Error> {
    let url = format!(
        "http://{name}.{namespace}.svc.{cluster_domain}:{port}/",
        name = svc.metadata.name.as_deref().context(NoNameSnafu)?,
        namespace = svc
            .metadata
            .namespace
            .as_deref()
            .context(NoNamespaceSnafu)?,
        cluster_domain = cluster_info.cluster_domain,
        port = APP_PORT
    );

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(name)
        .ownerreference_from_resource(owner, None, Some(true))
        .with_context(|_| ObjectMissingMetadataForOwnerRefSnafu {
            opa: ObjectRef::from_obj(opa),
        })?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label_value,
            &v1alpha1::OpaRole::Server.to_string(),
            "discovery",
        ))
        .context(ObjectMetaSnafu)?
        .build();

    ConfigMapBuilder::new()
        .metadata(metadata)
        .add_data("OPA", url)
        .build()
        .context(BuildConfigMapSnafu)
}
