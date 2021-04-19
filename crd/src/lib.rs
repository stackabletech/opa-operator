use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::Crd;

#[derive(
    Clone, CustomResource, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize,
)]
#[kube(
    group = "authz.stackable.tech",
    version = "v1",
    kind = "OpenPolicyAgent",
    shortname = "opa",
    namespaced
)]
pub struct OpaSpec {
    pub selectors: Vec<OpaNodeSelector>,
}

#[derive(Clone, Debug, Hash, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaNodeSelector {
    pub repo_rule_operator_ref: String,
    pub port: Option<u16>,
}

impl Crd for OpenPolicyAgent {
    const RESOURCE_NAME: &'static str = "openpolicyagents.authz.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/server.opa.crd.yaml");
}
