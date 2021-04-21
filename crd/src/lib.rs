use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::CustomResource;
use schemars::gen::SchemaGenerator;
use schemars::schema::Schema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{from_value, json};
use stackable_operator::Crd;
use std::collections::HashMap;

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

pub fn schema(_: &mut SchemaGenerator) -> Schema {
    from_value(json!({
      "description": "A label selector is a label query over a set of resources. The result of matchLabels and matchExpressions are ANDed. An empty label selector matches all objects. A null label selector matches no objects.",
      "properties": {
        "matchExpressions": {
          "description": "matchExpressions is a list of label selector requirements. The requirements are ANDed.",
          "items": {
            "description": "A label selector requirement is a selector that contains values, a key, and an operator that relates the key and values.",
            "properties": {
              "key": {
                "description": "key is the label key that the selector applies to.",
                "type": "string",
                "x-kubernetes-patch-merge-key": "key",
                "x-kubernetes-patch-strategy": "merge"
              },
              "operator": {
                "description": "operator represents a key's relationship to a set of values. Valid operators are In, NotIn, Exists and DoesNotExist.",
                "type": "string"
              },
              "values": {
                "description": "values is an array of string values. If the operator is In or NotIn, the values array must be non-empty. If the operator is Exists or DoesNotExist, the values array must be empty. This array is replaced during a strategic merge patch.",
                "items": {
                  "type": "string"
                },
                "type": "array"
              }
            },
            "required": [
              "key",
              "operator"
            ],
            "type": "object"
          },
          "type": "array"
        },
        "matchLabels": {
          "additionalProperties": {
            "type": "string"
          },
          "description": "matchLabels is a map of {key,value} pairs. A single {key,value} in the matchLabels map is equivalent to an element of matchExpressions, whose key field is \"key\", the operator is \"In\", and the values array contains only \"value\". The requirements are ANDed.",
          "type": "object"
        }
      },
      "type": "object"
    })).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::gen::SchemaGenerator;
    use stackable_operator::conditions::schema;

    #[test]
    fn print_crd() {
        let schema = OpenPolicyAgent::crd();
        let string_schema = serde_yaml::to_string(&schema).unwrap();
        println!("OPA CRD:\n{}\n", string_schema);
    }

    #[test]
    fn print_schema() {
        let schema = schema(&mut SchemaGenerator::default());

        let string_schema = serde_yaml::to_string(&schema).unwrap();
        println!("LabelSelector Schema:\n{}\n", string_schema);
    }
}
