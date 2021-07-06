mod error;
pub mod util;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::product_config_utils::{ConfigError, Configuration};
use stackable_operator::role_utils::Role;
use stackable_operator::Crd;
use std::collections::BTreeMap;
use strum_macros::EnumIter;
use tracing::error;

pub const APP_NAME: &str = "opa";
pub const MANAGED_BY: &str = "stackable-opa";

pub const CONFIG_FILE: &str = "config.yaml";
pub const REPO_RULE_REFERENCE: &str = "repoRuleReference";
pub const PORT: &str = "port";

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
    pub servers: Role<OpaConfig>,
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

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaConfig {
    pub port: Option<u16>,
    pub repo_rule_reference: String,
}

impl Configuration for OpaConfig {
    type Configurable = OpenPolicyAgent;

    fn compute_env(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        Ok(BTreeMap::new())
    }

    fn compute_cli(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        let mut cli = BTreeMap::new();
        cli.insert(PORT.to_string(), self.port.map(|p| p.to_string()));
        Ok(cli)
    }

    fn compute_files(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
        file: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        let mut config = BTreeMap::new();

        if file == CONFIG_FILE {
            config.insert(
                REPO_RULE_REFERENCE.to_string(),
                Some(self.repo_rule_reference.clone()),
            );
        } else {
            error!(
                "Did not find any properties matching config file [{}]. This should not happen.",
                CONFIG_FILE
            );
        }

        Ok(config)
    }
}

#[derive(
    EnumIter,
    Clone,
    Debug,
    Hash,
    Deserialize,
    Eq,
    JsonSchema,
    PartialEq,
    Serialize,
    strum_macros::Display,
    strum_macros::EnumString,
)]
pub enum OpaRole {
    #[serde(rename = "server")]
    #[strum(serialize = "server")]
    Server,
}

impl Crd for OpenPolicyAgent {
    const RESOURCE_NAME: &'static str = "openpolicyagents.authz.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/server.opa.crd.yaml");
}
