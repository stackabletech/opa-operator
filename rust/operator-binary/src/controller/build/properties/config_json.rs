//! Builds the OPA `config.json` file.

use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    product_logging::spec::{ContainerLogConfig, ContainerLogConfigChoice, LogLevel},
    v2::config_overrides::JsonConfigOverrides,
};

use super::ConfigFileName;
use crate::{
    crd::{Container, OpaConfig, OpaConfigOverrides},
    opa_controller::OPA_STACKABLE_SERVICE_NAME,
};

/// Decision logging is disabled by default. It is enabled when the `decision` logger is set to a
/// level other than `NONE`.
const DEFAULT_DECISION_LOGGING_ENABLED: bool = false;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to serialize config file {file:?}"))]
    SerializeConfigFile {
        source: serde_json::Error,
        file: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds the OPA `config.json` from the operator defaults and the merged user `configOverrides`.
pub fn build(merged_config: &OpaConfig, config_overrides: &OpaConfigOverrides) -> Result<String> {
    let mut decision_logging_enabled = DEFAULT_DECISION_LOGGING_ENABLED;

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Opa)
        && let Some(config) = log_config.loggers.get("decision")
    {
        decision_logging_enabled = config.level != LogLevel::NONE;
    }

    let decision_logging = if decision_logging_enabled {
        Some(OpaClusterConfigDecisionLog { console: true })
    } else {
        None
    };

    let config = OpaClusterConfigFile::new(decision_logging);

    let config_value =
        serde_json::to_value(&config).with_context(|_| SerializeConfigFileSnafu {
            file: ConfigFileName::ConfigJson.to_string(),
        })?;

    // Apply the merged user `configOverrides`. The merge built a sequence that applies the
    // role-level patch first, then the role-group-level patch on top. `apply` is infallible; an
    // invalid patch is logged and skipped.
    let config_json = JsonConfigOverrides::from(config_overrides.config_json.clone());
    let config_value = config_json.apply(&config_value);

    serde_json::to_string_pretty(&config_value).with_context(|_| SerializeConfigFileSnafu {
        file: ConfigFileName::ConfigJson.to_string(),
    })
}

#[derive(Serialize, Deserialize)]
pub struct OpaClusterConfigFile {
    services: Vec<OpaClusterConfigService>,
    bundles: OpaClusterBundle,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_logs: Option<OpaClusterConfigDecisionLog>,
    status: Option<OpaClusterConfigStatus>,
}

impl OpaClusterConfigFile {
    pub fn new(decision_logging: Option<OpaClusterConfigDecisionLog>) -> Self {
        Self {
            services: vec![OpaClusterConfigService {
                name: OPA_STACKABLE_SERVICE_NAME.to_owned(),
                url: "http://localhost:3030/opa/v1".to_owned(),
            }],
            bundles: OpaClusterBundle {
                stackable: OpaClusterBundleConfig {
                    service: OPA_STACKABLE_SERVICE_NAME.to_owned(),
                    resource: "opa/bundle.tar.gz".to_owned(),
                    persist: true,
                    polling: OpaClusterBundleConfigPolling {
                        min_delay_seconds: 10,
                        max_delay_seconds: 20,
                    },
                },
            },
            decision_logs: decision_logging,
            // Enable more Prometheus metrics, such as bundle loads
            // See https://www.openpolicyagent.org/docs/monitoring#status-metrics
            status: Some(OpaClusterConfigStatus { prometheus: true }),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct OpaClusterConfigService {
    name: String,
    url: String,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundle {
    stackable: OpaClusterBundleConfig,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundleConfig {
    service: String,
    resource: String,
    persist: bool,
    polling: OpaClusterBundleConfigPolling,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundleConfigPolling {
    min_delay_seconds: i32,
    max_delay_seconds: i32,
}

#[derive(Serialize, Deserialize)]
pub struct OpaClusterConfigDecisionLog {
    console: bool,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterConfigStatus {
    prometheus: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        controller::build::properties::test_support::validated_cluster_from_spec, crd::OpaRole,
    };

    /// Renders `config.json` for the `default` server role group of an `OpaCluster` built from
    /// `spec`, and parses it back into a [`Value`].
    fn config_json_for(spec: Value) -> Value {
        let (_, validated) = validated_cluster_from_spec(spec);
        let rg = validated.role_group_configs[&OpaRole::Server]
            .values()
            .next()
            .expect("the default role group should exist");
        let rendered =
            build(&rg.config.config, &rg.config.config_overrides).expect("config.json builds");
        serde_json::from_str(&rendered).expect("config.json should be valid JSON")
    }

    #[test]
    fn renders_defaults() {
        let config = config_json_for(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));

        // The bundled stackable service and its default polling values.
        assert_eq!(config["services"][0]["name"], "stackable");
        assert_eq!(
            config["bundles"]["stackable"]["polling"]["min_delay_seconds"],
            10
        );
        assert_eq!(
            config["bundles"]["stackable"]["polling"]["max_delay_seconds"],
            20
        );
        // Prometheus status metrics are enabled, decision logs are off by default.
        assert_eq!(config["status"]["prometheus"], true);
        assert!(config.get("decision_logs").is_none_or(Value::is_null));
    }

    #[test]
    fn applies_role_then_role_group_overrides() {
        let config = config_json_for(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "configOverrides": {
                    "config.json": { "jsonMergePatch": {
                        "bundles": { "stackable": { "polling": {
                            "min_delay_seconds": 3,
                            "max_delay_seconds": 7,
                        } } },
                        "default_decision": "test/hello",
                    } }
                },
                "roleGroups": { "default": {
                    "configOverrides": {
                        "config.json": { "jsonMergePatch": {
                            "bundles": { "stackable": { "polling": {
                                "max_delay_seconds": 5,
                            } } },
                            "labels": { "rolegroup": "default" },
                        } }
                    }
                } }
            },
        }));

        let polling = &config["bundles"]["stackable"]["polling"];
        // Role-only key survives.
        assert_eq!(polling["min_delay_seconds"], 3);
        // Role group wins over role for the shared key.
        assert_eq!(polling["max_delay_seconds"], 5);
        // Role-level addition is kept.
        assert_eq!(config["default_decision"], "test/hello");
        // Role-group-level addition is applied on top.
        assert_eq!(config["labels"]["rolegroup"], "default");
    }
}
