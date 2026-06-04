//! Builds the rolegroup [`ConfigMap`] (the OPA `config.json` and sidecar config) from the
//! [`ValidatedCluster`], without reaching into the raw `OpaCluster` spec (the owner object is
//! only used for the owner reference and object metadata).

use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    kvp::ObjectLabels,
    product_logging::spec::{ContainerLogConfig, ContainerLogConfigChoice, LogLevel},
    role_utils::RoleGroupRef,
    v2::config_overrides::JsonConfigOverrides,
};

use crate::{
    controller::{
        CONFIG_FILE, OPA_STACKABLE_SERVICE_NAME,
        validate::{OpaRoleGroupConfig, ValidatedCluster},
    },
    crd::{Container, OpaConfig, OpaConfigOverrides, v1alpha2},
    product_logging::extend_role_group_config_map,
};

/// Decision logging is disabled by default. It is enabled when the `decision` logger is set to a
/// level other than `NONE`.
const DEFAULT_DECISION_LOGGING_ENABLED: bool = false;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to serialize user info fetcher configuration"))]
    SerializeUserInfoFetcherConfig { source: serde_json::Error },

    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },

    #[snafu(display("failed to build ConfigMap for [{rolegroup}]"))]
    BuildConfigMap {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha2::OpaCluster>,
    },

    #[snafu(display("failed to serialize config file {file:?}"))]
    SerializeConfigFile {
        source: serde_json::Error,
        file: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the
/// administrator.
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    rolegroup_config: &OpaRoleGroupConfig,
    rolegroup_ref: &RoleGroupRef<v1alpha2::OpaCluster>,
    recommended_labels: &ObjectLabels<'_, v1alpha2::OpaCluster>,
    owner: &v1alpha2::OpaCluster,
) -> Result<ConfigMap> {
    let mut cm_builder = ConfigMapBuilder::new();

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(owner)
        .name(rolegroup_ref.object_name())
        .ownerreference_from_resource(owner, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(recommended_labels)
        .context(ObjectMetaSnafu)?
        .build();

    cm_builder.metadata(metadata).add_data(
        CONFIG_FILE,
        build_config_file(
            &rolegroup_config.merged_config,
            &rolegroup_config.config_overrides,
        )?,
    );

    if let Some(user_info) = &cluster.cluster_config.user_info {
        cm_builder.add_data(
            "user-info-fetcher.json",
            serde_json::to_string_pretty(user_info).context(SerializeUserInfoFetcherConfigSnafu)?,
        );
    }

    extend_role_group_config_map(
        rolegroup_ref,
        &rolegroup_config.merged_config.logging,
        &mut cm_builder,
    )
    .context(InvalidLoggingConfigSnafu {
        cm_name: rolegroup_ref.object_name(),
    })?;

    cm_builder
        .build()
        .with_context(|_| BuildConfigMapSnafu {
            rolegroup: rolegroup_ref.clone(),
        })
}

/// Builds the OPA `config.json` from the operator defaults and the merged user `configOverrides`.
fn build_config_file(
    merged_config: &OpaConfig,
    config_overrides: &OpaConfigOverrides,
) -> Result<String> {
    let mut decision_logging_enabled = DEFAULT_DECISION_LOGGING_ENABLED;

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Opa)
    {
        if let Some(config) = log_config.loggers.get("decision") {
            decision_logging_enabled = config.level != LogLevel::NONE;
        }
    }

    let decision_logging = if decision_logging_enabled {
        Some(OpaClusterConfigDecisionLog { console: true })
    } else {
        None
    };

    let config = OpaClusterConfigFile::new(decision_logging);

    let config_value =
        serde_json::to_value(&config).with_context(|_| SerializeConfigFileSnafu {
            file: CONFIG_FILE.to_string(),
        })?;

    // Apply the merged user `configOverrides`. The merge built a sequence that applies the
    // role-level patch first, then the role-group-level patch on top. `apply` is infallible; an
    // invalid patch is logged and skipped.
    let config_json = JsonConfigOverrides::from(config_overrides.config_json.clone());
    let config_value = config_json.apply(&config_value);

    serde_json::to_string_pretty(&config_value).with_context(|_| SerializeConfigFileSnafu {
        file: CONFIG_FILE.to_string(),
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
    use stackable_operator::{
        cli::OperatorEnvironmentOptions, kube::runtime::reflector::ObjectRef,
    };

    use super::*;
    use crate::{
        controller::{build_recommended_labels, validate::validate},
        crd::OpaRole,
    };

    /// Validates an `OpaCluster` built from the given `spec` and renders the ConfigMap of its first
    /// (and, in these tests, only) server role group.
    fn build_config_map(spec: Value) -> ConfigMap {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": { "name": "test-opa", "namespace": "default", "uid": "42" },
            "spec": spec,
        }))
        .expect("invalid test input");

        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_string(),
            operator_service_name: "opa-operator".to_string(),
            image_repository: "oci.stackable.tech/sdp".to_string(),
        };

        let validated = validate(&opa, &operator_environment).expect("validation should succeed");

        let role = OpaRole::Server;
        let role_group_configs = validated
            .role_group_configs
            .get(&role)
            .expect("the server role should be present");
        let (rg_name, rg_config) = role_group_configs
            .iter()
            .next()
            .expect("at least one role group");
        let rolegroup_ref = RoleGroupRef {
            cluster: ObjectRef::from_obj(&opa),
            role: role.to_string(),
            role_group: rg_name.clone(),
        };
        let recommended_labels = build_recommended_labels(
            &opa,
            &validated.image.app_version_label_value,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        );

        build_rolegroup_config_map(
            &validated,
            rg_config,
            &rolegroup_ref,
            &recommended_labels,
            &opa,
        )
        .expect("the config map should build")
    }

    /// Extracts and parses the rendered `config.json` from a ConfigMap.
    fn config_json(config_map: &ConfigMap) -> Value {
        let raw = config_map
            .data
            .as_ref()
            .expect("the config map should have data")
            .get(CONFIG_FILE)
            .expect("config.json should be present");
        serde_json::from_str(raw).expect("config.json should be valid JSON")
    }

    #[test]
    fn renders_default_config_json() {
        let cm = build_config_map(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));
        let config = config_json(&cm);

        // The bundled stackable service and its default polling values.
        assert_eq!(config["services"][0]["name"], "stackable");
        assert_eq!(config["bundles"]["stackable"]["polling"]["min_delay_seconds"], 10);
        assert_eq!(config["bundles"]["stackable"]["polling"]["max_delay_seconds"], 20);
        // Prometheus status metrics are enabled, decision logs are off by default.
        assert_eq!(config["status"]["prometheus"], true);
        assert!(config.get("decision_logs").is_none_or(Value::is_null));
        // No user info configured -> no user-info-fetcher.json.
        assert!(!cm.data.as_ref().unwrap().contains_key("user-info-fetcher.json"));
    }

    #[test]
    fn applies_role_then_role_group_config_json_overrides() {
        let cm = build_config_map(json!({
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
        let config = config_json(&cm);

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
