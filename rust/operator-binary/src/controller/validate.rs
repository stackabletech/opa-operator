//! The validate step in the OpaCluster controller
//!
//! Synchronously merges and validates the cluster spec into the typed
//! [`ValidatedCluster`] consumed by the rest of `reconcile_opa`. No Kubernetes
//! client is required.

use std::{collections::BTreeMap, str::FromStr};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::{Container, OpaConfig, OpaRole, OpaRoleType, v1alpha2};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    product_logging::spec::Logging,
    v2::{
        controller_utils::{get_cluster_name, get_namespace, get_uid},
        product_logging::framework::{
            ValidatedContainerLogConfigChoice, VectorContainerLogConfig,
            validate_logging_configuration_for_container,
        },
        role_utils::{RoleGroup, with_validated_config},
        types::{kubernetes::ConfigMapName, operator::RoleGroupName},
    },
};
use strum::IntoEnumIterator;

use super::{OpaRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig, ValidatedOpaConfig};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("failed to get the cluster name"))]
    GetClusterName {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to get the cluster namespace"))]
    GetNamespace {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to get the cluster UID"))]
    GetUid {
        source: stackable_operator::v2::controller_utils::Error,
    },

    #[snafu(display("failed to merge and validate config for role group {role_group:?}"))]
    ValidateRoleGroupConfig {
        source: stackable_operator::config::fragment::ValidationError,
        role_group: String,
    },

    #[snafu(display("the role group name {role_group:?} is invalid"))]
    ParseRoleGroupName {
        source: <RoleGroupName as FromStr>::Err,
        role_group: String,
    },

    #[snafu(display("failed to validate the logging configuration"))]
    ValidateLoggingConfig {
        source: stackable_operator::v2::product_logging::framework::Error,
    },

    #[snafu(display(
        "the Vector agent is enabled but no Vector aggregator discovery ConfigMap name is set"
    ))]
    MissingVectorAggregatorConfigMapName,

    #[snafu(display(
        "role \"{}\" enables a PodDisruptionBudget, but its workloadKind is DaemonSet; DaemonSets do not implement the scale subresource, so the budget could never be evaluated",
        **role
    ))]
    PodDisruptionBudgetOnDaemonSet { role: OpaRole },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Validated logging configuration for a role group.
///
/// Produced up-front by [`validate_logging`] so that a missing or invalid Vector aggregator
/// discovery ConfigMap name fails reconciliation during validation rather than at
/// resource-build time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedLogging {
    /// The validated log config choice of every OPA container except [`Container::Vector`], whose
    /// validated config lives in `vector_container` (it also carries the aggregator discovery
    /// ConfigMap name).
    pub containers: BTreeMap<Container, ValidatedContainerLogConfigChoice>,
    /// The validated Vector container config, or `None` when the Vector agent is disabled.
    pub vector_container: Option<VectorContainerLogConfig>,
}

/// Validates the logging configuration of every OPA container up-front.
///
/// Each non-Vector container's log config choice is validated into the `containers` map (so a
/// custom log ConfigMap name is parsed and checked during validation). The Vector container is
/// validated separately into `vector_container`, which is only present — and whose
/// `vector_aggregator_config_map_name` is only required (and validated) — when the Vector agent is
/// enabled.
fn validate_logging(
    logging: &Logging<Container>,
    vector_aggregator_config_map_name: &Option<ConfigMapName>,
) -> Result<ValidatedLogging> {
    let mut containers = BTreeMap::new();
    for container in Container::iter() {
        // The Vector container is handled separately (see `vector_container` below).
        if container == Container::Vector {
            continue;
        }
        let validated = validate_logging_configuration_for_container(logging, &container)
            .context(ValidateLoggingConfigSnafu)?;
        containers.insert(container, validated);
    }

    let vector_container = if logging.enable_vector_agent {
        let vector_aggregator_config_map_name = vector_aggregator_config_map_name
            .clone()
            .context(MissingVectorAggregatorConfigMapNameSnafu)?;
        Some(VectorContainerLogConfig {
            log_config: validate_logging_configuration_for_container(logging, &Container::Vector)
                .context(ValidateLoggingConfigSnafu)?,
            vector_aggregator_config_map_name,
        })
    } else {
        None
    };

    Ok(ValidatedLogging {
        containers,
        vector_container,
    })
}

/// The settings a DaemonSet has no use for, one message each.
///
/// Ignored rather than rejected, as they do no harm, but returned so that `validate` can log them
/// and the user is not left wondering why they have no effect. A DaemonSet runs one Pod per node,
/// so `replicas` is meaningless, and it gets no PodDisruptionBudget, so `maxUnavailable` is too.
fn settings_ignored_by_daemon_set(opa_role: &OpaRole, role: &OpaRoleType) -> Vec<String> {
    if role.role_config.workload_kind != v1alpha2::WorkloadKind::DaemonSet {
        return vec![];
    }

    let mut ignored = vec![];
    if role
        .role_config
        .pod_disruption_budget
        .max_unavailable
        .is_some()
    {
        ignored.push(format!(
            "role \"{}\": podDisruptionBudget.maxUnavailable is ignored, as a DaemonSet gets no PodDisruptionBudget",
            **opa_role
        ));
    }
    // Sorted, as `role_groups` is a `HashMap`: the same CR logs the same messages in the same order
    // on every reconcile.
    let role_groups: BTreeMap<_, _> = role.role_groups.iter().collect();
    for (role_group_name, role_group) in role_groups {
        if role_group.replicas.is_some() {
            ignored.push(format!(
                "role group {role_group_name:?} of role \"{}\": replicas is ignored, as a DaemonSet runs one Pod per node",
                **opa_role
            ));
        }
    }
    ignored
}

/// Validates the cluster spec and produces a [`ValidatedCluster`].
pub fn validate(
    opa: &v1alpha2::OpaCluster,
    operator_environment: &OperatorEnvironmentOptions,
) -> Result<ValidatedCluster> {
    let name = get_cluster_name(opa).context(GetClusterNameSnafu)?;
    let namespace = get_namespace(opa).context(GetNamespaceSnafu)?;
    let uid = get_uid(opa).context(GetUidSnafu)?;

    let image = opa
        .spec
        .image
        .resolve(
            crate::opa_controller::CONTAINER_IMAGE_BASE_NAME,
            &operator_environment.image_repository,
            &crate::built_info::PKG_VERSION_SEMVER,
        )
        .context(ResolveProductImageSnafu)?;

    // The Vector aggregator discovery ConfigMap name. Validated at deserialization by the
    // `ConfigMapName` newtype on the CRD field. It is only required when the Vector agent is
    // enabled for a role group.
    let vector_aggregator_config_map_name = opa
        .spec
        .cluster_config
        .vector_aggregator_config_map_name
        .clone();

    let mut role_configs = BTreeMap::new();
    let mut role_group_configs = BTreeMap::new();
    for opa_role in OpaRole::iter() {
        let role = opa.role(&opa_role);

        // Carried per role rather than per cluster, so a second role could pick its own
        // `workloadKind`. `serde(default)` on `Role::role_config` means this is the
        // `OpaRoleConfig` default.
        let role_config = &role.role_config;
        // Rejected rather than ignored: the build step never writes a budget for a DaemonSet, so
        // silently dropping an explicit `enabled: true` would leave the user wondering where it went.
        if role_config.workload_kind == v1alpha2::WorkloadKind::DaemonSet
            && role_config.pod_disruption_budget.enabled == Some(true)
        {
            return PodDisruptionBudgetOnDaemonSetSnafu { role: opa_role }.fail();
        }
        for ignored in settings_ignored_by_daemon_set(&opa_role, role) {
            tracing::warn!("{ignored}");
        }
        role_configs.insert(opa_role.clone(), role_config.clone());

        let mut group_configs = BTreeMap::new();
        for (role_group_name, role_group) in &role.role_groups {
            // Merge default <- role <- role group and validate the config fragment, plus merge all
            // four override kinds (config/env/cli/pod) in one shot. Role group wins over role wins
            // over defaults.
            let merged: RoleGroup<OpaConfig, _, _> = with_validated_config(
                role_group,
                role,
                &OpaConfig::default_config(name.as_ref(), &opa_role),
            )
            .context(ValidateRoleGroupConfigSnafu {
                role_group: role_group_name.clone(),
            })?;

            // Validate the logging configuration up-front (borrows the merged config before it is
            // moved into the `OpaRoleGroupConfig` below).
            let logging = validate_logging(
                &merged.config.config.logging,
                &vector_aggregator_config_map_name,
            )?;

            // Validate the role group name against the `RoleGroupName` newtype (RFC 1123 label,
            // length-bounded) so the typed key is guaranteed to produce valid resource names.
            let role_group_name =
                RoleGroupName::from_str(role_group_name).context(ParseRoleGroupNameSnafu {
                    role_group: role_group_name.clone(),
                })?;

            group_configs.insert(
                role_group_name,
                OpaRoleGroupConfig {
                    // Only used in `Deployment` mode; a DaemonSet derives its Pod count from the
                    // number of nodes.
                    replicas: merged.replicas,
                    config: ValidatedOpaConfig::from_merged(merged.config.config, logging),
                    config_overrides: merged.config.config_overrides,
                    env_overrides: merged.config.env_overrides.into(),
                    cli_overrides: merged.config.cli_overrides,
                    pod_overrides: merged.config.pod_overrides,
                    product_specific_common_config: merged.config.product_specific_common_config,
                },
            );
        }

        role_group_configs.insert(opa_role, group_configs);
    }

    Ok(ValidatedCluster::new(
        name,
        namespace,
        uid,
        image,
        ValidatedClusterConfig {
            user_info: opa.spec.cluster_config.user_info.clone(),
            resource_info: opa.spec.cluster_config.resource_info.clone(),
            tls: opa.spec.cluster_config.tls.clone(),
            listener_class: opa.spec.cluster_config.listener_class.clone(),
        },
        role_configs,
        role_group_configs,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stackable_operator::product_logging::spec::{
        AutomaticContainerLogConfig, ContainerLogConfig, ContainerLogConfigChoice,
    };

    use super::*;
    use crate::controller::build::properties::test_support::app_version_label;

    /// Locks every value the validate step itself derives from the minimal fixture — so a
    /// validation regression fails here, with a validate-shaped message, instead of surfacing as
    /// a confusing build-test failure downstream.
    ///
    /// The merged per-role-group config (resources, affinity, logging defaults, …) is produced by
    /// the config merge machinery, whose contracts are tested in operator-rs and the properties
    /// tests; only the values this module derives on top are re-asserted here.
    #[test]
    fn validate_ok_derives_expected_values() {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": {
                "name": "test-opa",
                "namespace": "default",
                "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd",
            },
            "spec": {
                "image": { "productVersion": "1.2.3" },
                "servers": { "roleGroups": { "default": {} } },
            },
        }))
        .expect("valid test input");
        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_string(),
            operator_service_name: "opa-operator".to_string(),
            image_repository: "oci.example.org".to_string(),
        };

        let cluster = validate(&opa, &operator_environment).expect("the minimal fixture validates");

        assert_eq!(cluster.name.to_string(), "test-opa");
        assert_eq!(cluster.namespace.to_string(), "default");
        assert_eq!(
            cluster.uid.to_string(),
            "c27b3971-ca72-42c1-80a4-abdfc1db0ddd"
        );
        assert_eq!(
            cluster.image.image,
            format!("oci.example.org/opa:{}", app_version_label("1.2.3"))
        );
        assert_eq!(cluster.image.product_version, "1.2.3");
        assert_eq!(
            cluster.product_version.to_string(),
            app_version_label("1.2.3")
        );

        // The minimal fixture configures no user-info fetcher and no TLS; the listener class
        // falls back to its default.
        assert_eq!(cluster.cluster_config.user_info, None);
        assert_eq!(cluster.cluster_config.tls, None);
        assert_eq!(
            cluster.cluster_config.listener_class,
            v1alpha2::CurrentlySupportedListenerClasses::ClusterInternal
        );

        // The fixture sets no `roleConfig`, so the role falls back to the `OpaRoleConfig` default.
        assert_eq!(
            cluster.role_config(&OpaRole::Server),
            &v1alpha2::OpaRoleConfig::default()
        );

        // A single `server` role with the single `default` role group; the Vector agent is off.
        assert_eq!(cluster.role_group_configs.len(), 1);
        let role_groups = &cluster.role_group_configs[&OpaRole::Server];
        let role_group_names: Vec<String> = role_groups.keys().map(ToString::to_string).collect();
        assert_eq!(role_group_names, ["default"]);
        let role_group = role_groups
            .values()
            .next()
            .expect("the default role group exists");
        assert_eq!(role_group.config.logging.vector_container, None);
    }

    /// A configured `roleConfig` reaches the build step, and every role gets an entry so
    /// `ValidatedCluster::role_config` cannot panic.
    #[test]
    fn validate_carries_the_role_config_of_every_role() {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": {
                "name": "test-opa",
                "namespace": "default",
                "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd",
            },
            "spec": {
                "image": { "productVersion": "1.2.3" },
                "servers": {
                    "roleConfig": { "workloadKind": "Deployment" },
                    "roleGroups": { "default": {} },
                },
            },
        }))
        .expect("valid test input");
        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_string(),
            operator_service_name: "opa-operator".to_string(),
            image_repository: "oci.example.org".to_string(),
        };

        let cluster = validate(&opa, &operator_environment).expect("the fixture validates");

        assert_eq!(
            cluster.role_config(&OpaRole::Server).workload_kind,
            v1alpha2::WorkloadKind::Deployment
        );
        // Every role is present, whether or not the user configured it.
        assert_eq!(cluster.role_configs.len(), OpaRole::iter().count());
        for opa_role in OpaRole::iter() {
            cluster.role_config(&opa_role);
        }
    }

    /// A PodDisruptionBudget over a DaemonSet can never be evaluated, so asking for one is a
    /// misconfiguration that fails validation instead of being silently dropped.
    #[test]
    fn validate_rejects_a_pod_disruption_budget_on_a_daemon_set() {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": {
                "name": "test-opa",
                "namespace": "default",
                "uid": "c27b3971-ca72-42c1-80a4-abdfc1db0ddd",
            },
            "spec": {
                "image": { "productVersion": "1.2.3" },
                "servers": {
                    "roleConfig": {
                        "workloadKind": "DaemonSet",
                        "podDisruptionBudget": { "enabled": true },
                    },
                    "roleGroups": { "default": {} },
                },
            },
        }))
        .expect("valid test input");
        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_string(),
            operator_service_name: "opa-operator".to_string(),
            image_repository: "oci.example.org".to_string(),
        };

        let Err(error) = validate(&opa, &operator_environment) else {
            panic!("the fixture must be rejected");
        };
        assert!(matches!(
            error,
            Error::PodDisruptionBudgetOnDaemonSet {
                role: OpaRole::Server
            }
        ));
        // Named as the user knows the role, not by its Rust variant.
        assert!(error.to_string().starts_with("role \"server\" "), "{error}");
    }

    fn server_role(servers: serde_json::Value) -> OpaRoleType {
        let opa: v1alpha2::OpaCluster = serde_json::from_value(json!({
            "apiVersion": "opa.stackable.tech/v1alpha2",
            "kind": "OpaCluster",
            "metadata": { "name": "test-opa" },
            "spec": { "image": { "productVersion": "1.2.3" }, "servers": servers },
        }))
        .expect("valid test input");
        opa.spec.servers
    }

    /// `maxUnavailable` and `replicas` do nothing for a DaemonSet, so each one set is reported
    /// for `validate` to log, rather than silently dropped.
    #[test]
    fn daemon_set_reports_every_ignored_setting() {
        let role = server_role(json!({
            "roleConfig": { "podDisruptionBudget": { "maxUnavailable": 2 } },
            "roleGroups": { "default": { "replicas": 2 }, "other": { "replicas": 3 }, "unset": {} },
        }));

        let ignored = settings_ignored_by_daemon_set(&OpaRole::Server, &role);

        assert_eq!(ignored.len(), 3, "unexpected messages: {ignored:?}");
        assert!(
            ignored
                .iter()
                .all(|message| message.contains("role \"server\""))
        );
        assert!(ignored[0].contains("maxUnavailable"));
        assert!(ignored[1].contains("\"default\"") && ignored[1].contains("replicas"));
        assert!(ignored[2].contains("\"other\"") && ignored[2].contains("replicas"));
    }

    #[test]
    fn daemon_set_without_those_settings_reports_nothing() {
        let role = server_role(json!({ "roleGroups": { "default": {} } }));

        assert!(settings_ignored_by_daemon_set(&OpaRole::Server, &role).is_empty());
    }

    /// Both settings are honoured by a Deployment, so nothing is reported.
    #[test]
    fn deployment_reports_nothing() {
        let role = server_role(json!({
            "roleConfig": {
                "workloadKind": "Deployment",
                "podDisruptionBudget": { "maxUnavailable": 2 },
            },
            "roleGroups": { "default": { "replicas": 2 } },
        }));

        assert!(settings_ignored_by_daemon_set(&OpaRole::Server, &role).is_empty());
    }

    /// A [`Logging`] with an automatic log config for every container, as the (defaulted) merged
    /// config provides at runtime. `validate_logging` validates all containers, so all must be
    /// present.
    fn logging(enable_vector_agent: bool) -> Logging<Container> {
        Logging {
            enable_vector_agent,
            containers: Container::iter()
                .map(|container| {
                    (
                        container,
                        ContainerLogConfig {
                            choice: Some(ContainerLogConfigChoice::Automatic(
                                AutomaticContainerLogConfig::default(),
                            )),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn validate_logging_disabled_has_no_vector_container() {
        let validated = validate_logging(&logging(false), &None).expect("should validate");
        assert!(validated.vector_container.is_none());
    }

    #[test]
    fn validate_logging_enabled_requires_aggregator_config_map() {
        assert!(matches!(
            validate_logging(&logging(true), &None),
            Err(Error::MissingVectorAggregatorConfigMapName)
        ));
    }

    #[test]
    fn validate_logging_enabled_with_aggregator_yields_vector_container() {
        let aggregator =
            Some(ConfigMapName::from_str("vector-aggregator-discovery").expect("valid name"));
        let validated = validate_logging(&logging(true), &aggregator).expect("should validate");
        let vector = validated.vector_container.expect("vector container config");
        assert_eq!(
            vector.vector_aggregator_config_map_name.as_ref(),
            "vector-aggregator-discovery"
        );
    }
}
