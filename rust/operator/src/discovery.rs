use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{OpaRole, OpenPolicyAgent, APP_NAME};
use stackable_operator::{
    builder::{ConfigMapBuilder, ObjectMetaBuilder},
    k8s_openapi::api::core::v1::{ConfigMap, Service},
    kube::{runtime::reflector::ObjectRef, Resource, ResourceExt},
};

use crate::opa_controller::{opa_version, APP_PORT};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object {} is missing metadata to build owner reference", opa))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::error::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
    #[snafu(display("object has no name associated"))]
    NoName,
    #[snafu(display("object has no namespace associated"))]
    NoNamespace,
    #[snafu(display("failed to build ConfigMap"))]
    BuildConfigMap {
        source: stackable_operator::error::Error,
    },
}

/// Builds discovery [`ConfigMap`]s for connecting to a [`OpenPolicyAgent`] for all expected scenarios
pub fn build_discovery_configmaps(
    owner: &impl Resource<DynamicType = ()>,
    opa: &OpenPolicyAgent,
    svc: &Service,
) -> Result<Vec<ConfigMap>, Error> {
    let name = owner.name();
    Ok(vec![build_discovery_configmap(&name, owner, opa, svc)?])
}

/// Build a discovery [`ConfigMap`] containing information about how to connect to a certain [`OpenPolicyAgent`]
///
/// `hosts` will usually come from either [`pod_hosts`] or [`nodeport_hosts`].
fn build_discovery_configmap(
    name: &str,
    owner: &impl Resource<DynamicType = ()>,
    opa: &OpenPolicyAgent,
    svc: &Service,
) -> Result<ConfigMap, Error> {
    let url = format!(
        "http://{}.{}.svc.cluster.local:{}/",
        svc.metadata.name.as_deref().context(NoName)?,
        svc.metadata.namespace.as_deref().context(NoNamespace)?,
        APP_PORT
    );
    ConfigMapBuilder::new()
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(opa)
                .name(name)
                .ownerreference_from_resource(owner, None, Some(true))
                .with_context(|| ObjectMissingMetadataForOwnerRef {
                    opa: ObjectRef::from_obj(opa),
                })?
                .with_recommended_labels(
                    opa,
                    APP_NAME,
                    opa_version(opa).as_deref().unwrap_or("unknown"),
                    &OpaRole::Server.to_string(),
                    "discovery",
                )
                .build(),
        )
        .add_data("OPA", url)
        .build()
        .context(BuildConfigMap)
}
