mod error;
pub mod util;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::label_selector::schema;
use stackable_operator::Crd;
use std::collections::HashMap;

pub const APP_NAME: &str = "opa";
pub const MANAGED_BY: &str = "stackable-opa";

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "authz.stackable.tech",
    version = "v1",
    kind = "OpenPolicyAgent",
    shortname = "opa",
    namespaced
)]
#[kube(status = "OpaStatus")]
#[serde(rename_all = "camelCase")]
pub struct OpaSpec {
    pub version: OpaVersion,
    pub servers: NodeGroup<OpaConfig>,
}

#[allow(non_camel_case_types)]
#[derive(
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    JsonSchema,
    PartialEq,
    Serialize,
    strum_macros::Display,
    strum_macros::EnumString,
)]
pub enum OpaVersion {
    #[serde(rename = "0.27.1")]
    #[strum(serialize = "0.27.1")]
    v0_27_1,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct OpaStatus {}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeGroup<T> {
    pub selectors: HashMap<String, SelectorAndConfig<T>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectorAndConfig<T> {
    pub instances: u16,
    pub instances_per_node: u8,
    pub config: T,
    #[schemars(schema_with = "schema")]
    pub selector: Option<LabelSelector>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaConfig {
    pub port: Option<u16>,
    pub repo_rule_reference: String,
}

impl Crd for OpenPolicyAgent {
    const RESOURCE_NAME: &'static str = "openpolicyagents.authz.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/server.opa.crd.yaml");
}
