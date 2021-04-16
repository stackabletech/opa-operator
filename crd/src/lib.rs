use kube_derive::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::Crd;

#[derive(
    Clone, CustomResource, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize,
)]
#[kube(
    group = "opa.stackable.tech",
    version = "v1",
    kind = "OpaServer",
    shortname = "os",
    namespaced
)]
pub struct OpaServerSpec {
    pub port: Option<u16>,
}

impl Crd for OpaServer {
    const RESOURCE_NAME: &'static str = "server.opa.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/server.opa.crd.yaml");
}
