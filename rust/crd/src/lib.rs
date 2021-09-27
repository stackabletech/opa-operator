pub mod error;
pub mod util;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};
use stackable_operator::identity::PodToNodeMapping;
use stackable_operator::product_config_utils::{ConfigError, Configuration};
use stackable_operator::role_utils::Role;
use stackable_operator::status::{Conditions, Status, Versioned};
use stackable_operator::versioning::{ProductVersion, Versioning, VersioningState};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use strum_macros::EnumIter;
use tracing::error;

pub const APP_NAME: &str = "opa";
pub const MANAGED_BY: &str = "opa-operator";

pub const CONFIG_FILE: &str = "config.yaml";
pub const REPO_RULE_REFERENCE: &str = "repoRuleReference";
pub const PORT: &str = "port";

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "opa.stackable.tech",
    version = "v1alpha1",
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

impl Status<OpaStatus> for OpenPolicyAgent {
    fn status(&self) -> &Option<OpaStatus> {
        &self.status
    }
    fn status_mut(&mut self) -> &mut Option<OpaStatus> {
        &mut self.status
    }
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
    #[serde(rename = "0.28.0")]
    #[strum(serialize = "0.28.0")]
    v0_28_0,
}

impl Versioning for OpaVersion {
    fn versioning_state(&self, other: &Self) -> VersioningState {
        let from_version = match Version::parse(&self.to_string()) {
            Ok(v) => v,
            Err(e) => {
                return VersioningState::Invalid(format!(
                    "Could not parse [{}] to SemVer: {}",
                    self.to_string(),
                    e.to_string()
                ))
            }
        };

        let to_version = match Version::parse(&other.to_string()) {
            Ok(v) => v,
            Err(e) => {
                return VersioningState::Invalid(format!(
                    "Could not parse [{}] to SemVer: {}",
                    other.to_string(),
                    e.to_string()
                ))
            }
        };

        match to_version.cmp(&from_version) {
            Ordering::Greater => VersioningState::ValidUpgrade,
            Ordering::Less => VersioningState::ValidDowngrade,
            Ordering::Equal => VersioningState::NoOp,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<ProductVersion<OpaVersion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<PodToNodeMapping>,
}

impl Versioned<OpaVersion> for OpaStatus {
    fn version(&self) -> &Option<ProductVersion<OpaVersion>> {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Option<ProductVersion<OpaVersion>> {
        &mut self.version
    }
}

impl Conditions for OpaStatus {
    fn conditions(&self) -> &[Condition] {
        self.conditions.as_slice()
    }
    fn conditions_mut(&mut self) -> &mut Vec<Condition> {
        &mut self.conditions
    }
}

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

#[cfg(test)]
mod tests {
    use crate::OpaVersion;
    use stackable_operator::versioning::{Versioning, VersioningState};
    use std::str::FromStr;

    #[test]
    fn test_zookeeper_version_versioning() {
        assert_eq!(
            OpaVersion::v0_27_1.versioning_state(&OpaVersion::v0_28_0),
            VersioningState::ValidUpgrade
        );
        assert_eq!(
            OpaVersion::v0_28_0.versioning_state(&OpaVersion::v0_27_1),
            VersioningState::ValidDowngrade
        );
        assert_eq!(
            OpaVersion::v0_27_1.versioning_state(&OpaVersion::v0_27_1),
            VersioningState::NoOp
        );
    }

    #[test]
    fn test_version_conversion() {
        OpaVersion::from_str("0.27.1").unwrap();
        OpaVersion::from_str("0.28.0").unwrap();
        OpaVersion::from_str("1.2.3").unwrap_err();
    }
}
