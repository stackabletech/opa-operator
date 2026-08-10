use std::{ops::Deref, str::FromStr};

use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        cluster_operation::ClusterOperation,
        product_image_selection::ProductImage,
        resources::{
            CpuLimitsFragment, MemoryLimitsFragment, NoRuntimeLimits, NoRuntimeLimitsFragment,
            Resources, ResourcesFragment,
        },
    },
    config::{fragment::Fragment, merge::Merge},
    constant,
    deep_merger::ObjectOverrides,
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::CustomResource,
    product_logging::{self, spec::Logging},
    schemars::{self, JsonSchema},
    shared::time::Duration,
    status::condition::{ClusterCondition, HasStatusCondition},
    v2::{
        config_overrides::JsonConfigOverrides,
        role_utils::{GenericCommonConfig, Role},
        types::{
            kubernetes::{ConfigMapName, SecretClassName},
            operator::RoleName,
        },
    },
    versioned::versioned,
};
use strum::{Display, EnumIter};

pub mod cache;
pub mod resource_info_fetcher;
pub mod user_info_fetcher;

pub const APP_NAME: &str = "opa";
pub const OPA_OPERATOR_NAME: &str = "opa.stackable.tech";
pub const FIELD_MANAGER: &str = "opa-operator";

pub const DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_minutes_unchecked(2);
/// Safety puffer to guarantee the graceful shutdown works every time.
pub const SERVER_GRACEFUL_SHUTDOWN_SAFETY_OVERHEAD: Duration = Duration::from_secs(5);

pub type OpaRoleType =
    Role<OpaConfigFragment, OpaConfigOverrides, v1alpha2::OpaRoleConfig, GenericCommonConfig>;

#[versioned(
    version(name = "v1alpha1"),
    version(name = "v1alpha2"),
    crates(
        kube_core = "stackable_operator::kube::core",
        kube_client = "stackable_operator::kube::client",
        k8s_openapi = "stackable_operator::k8s_openapi",
        schemars = "stackable_operator::schemars",
        versioned = "stackable_operator::versioned"
    )
)]
pub mod versioned {
    /// An OPA (Open Policy Agent) cluster stacklet. This resource is managed by the Stackable operator for OPA.
    /// Find more information on how to use it and the resources that the operator generates in the
    /// [operator documentation](DOCS_BASE_URL_PLACEHOLDER/opa/).
    #[versioned(crd(
        doc = "An Open Policy Agent (OPA) cluster stacklet. This resource is managed by the Stackable operator for OPA.",
        group = "opa.stackable.tech",
        status = "OpaClusterStatus",
        shortname = "opa",
        namespaced,
    ))]
    #[derive(Clone, Debug, Deserialize, CustomResource, JsonSchema, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaClusterSpec {
        /// Global OPA cluster configuration that applies to all roles and role groups.
        #[serde(default)]
        pub cluster_config: OpaClusterConfig,

        /// Cluster operations like pause reconciliation or cluster stop.
        #[serde(default)]
        pub cluster_operation: ClusterOperation,

        // Docs are on the ObjectOverrides struct
        #[serde(default)]
        pub object_overrides: ObjectOverrides,

        /// OPA server configuration.
        // #[versioned(hint(role))]
        pub servers: super::OpaRoleType,

        /// The OPA image to use
        pub image: ProductImage,
    }

    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaClusterConfig {
        /// Name of the Vector aggregator discovery ConfigMap.
        /// It must contain the key `ADDRESS` with the address of the Vector aggregator.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub vector_aggregator_config_map_name: Option<ConfigMapName>,

        /// This field controls which type of Service the operator creates for this OpaCluster:
        ///
        /// * cluster-internal: Use a ClusterIP service
        ///
        /// * external-unstable: Use a NodePort service
        ///
        /// * external-stable: Use a LoadBalancer service
        ///
        /// This is a temporary solution with the goal to keep yaml manifests forward compatible.
        /// In the future, this setting will control which ListenerClass <https://docs.stackable.tech/home/stable/listener-operator/listenerclass.html>
        /// will be used to expose the service, and ListenerClass names will stay the same, allowing for a non-breaking change.
        #[serde(default)]
        pub listener_class: CurrentlySupportedListenerClasses,

        /// Configures how to fetch additional metadata about users (such as group memberships)
        /// from an external directory service.
        #[versioned(
            changed(
                since = "v1alpha2",
                from_type = "Option<user_info_fetcher::v1alpha1::Config>"
            ),
            hint(option)
        )]
        #[serde(default)]
        pub user_info: Option<user_info_fetcher::v1alpha2::Config>,

        /// Configures how to fetch additional metadata about resource information from a data
        /// catalog.
        ///
        /// Data catalog could e.g. be DataHub and resources could be Trino catalogs, schemas,
        /// tables or Kafka topics etc.
        #[serde(default)]
        pub resource_info: Option<resource_info_fetcher::v1alpha1::Config>,

        /// TLS encryption settings for the OPA server.
        /// When configured, OPA will use HTTPS (port 8443) instead of HTTP (port 8081).
        /// Clients must connect using HTTPS and trust the certificates provided by the configured SecretClass.
        #[serde(default)]
        #[versioned(hint(option))]
        pub tls: Option<OpaTls>,
    }

    /// Role-level configuration for the OPA servers.
    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaRoleConfig {
        /// The Kubernetes workload the OPA servers run as.
        ///
        /// * `DaemonSet`: one Pod per node. `replicas` is ignored.
        ///
        /// * `Deployment`: fixed number of Pods, configured by `replicas`.
        #[serde(default)]
        pub workload_kind: WorkloadKind,

        // `internalTrafficPolicy` is deliberately not a field here: the operator derives it from
        // `workloadKind` in `OpaRoleConfig::internal_traffic_policy`. Exposing it as a user
        // override means adding an `Option<InternalTrafficPolicy>` field back and falling back to
        // that helper's `match`.
        //
        // We can not #[serde(flatten)] a `GenericRoleConfig` here, as we need a PodDisruptionBudget
        // default that depends on `workloadKind`.
        #[serde(default)]
        pub pod_disruption_budget: OpaPdbConfig,
    }

    /// The Kubernetes Kind currently supported.
    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "PascalCase")]
    pub enum WorkloadKind {
        #[default]
        DaemonSet,
        Deployment,
    }

    /// The `internalTrafficPolicy` of a Kubernetes Service.
    ///
    /// The variants are spelled as Kubernetes spells them, so the value can be passed through to
    /// `Service.spec.internalTrafficPolicy` unchanged.
    ///
    /// TODO: Not yet part of the CRD: the operator derives the policy from [`WorkloadKind`].
    #[derive(Clone, Debug, Deserialize, Display, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "PascalCase")]
    pub enum InternalTrafficPolicy {
        Local,
        Cluster,
    }

    // A copy of `PdbConfig` from stackable-operator, but with `enabled` as an `Option`. The
    // default depends on `workloadKind` and can therefore not be hard-coded.
    //
    /// This struct is used to configure:
    ///
    /// 1. If PodDisruptionBudgets are created by the operator
    /// 2. The allowed number of Pods to be unavailable (`maxUnavailable`)
    ///
    /// Documentation:
    /// [allowed Pod disruptions documentation](DOCS_BASE_URL_PLACEHOLDER/concepts/operations/pod_disruptions).
    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaPdbConfig {
        /// Whether a PodDisruptionBudget should be written out for this role.
        ///
        /// Defaults to `true` when `workloadKind` is `Deployment` and to `false` when it is
        /// `DaemonSet`, since a PodDisruptionBudget doesn't make sense for a DaemonSet.
        #[serde(default)]
        pub enabled: Option<bool>,

        /// The number of Pods that are allowed to be down simultaneous.
        #[serde(default)]
        pub max_unavailable: Option<u16>,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaTls {
        /// Name of the SecretClass which will provide TLS certificates for the OPA server.
        pub server_secret_class: SecretClassName,
    }

    // TODO: Temporary solution until listener-operator is finished
    #[derive(Clone, Debug, Default, Display, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "PascalCase")]
    pub enum CurrentlySupportedListenerClasses {
        #[default]
        #[serde(rename = "cluster-internal")]
        ClusterInternal,
        #[serde(rename = "external-unstable")]
        ExternalUnstable,
        #[serde(rename = "external-stable")]
        ExternalStable,
    }
}

/// Typed config override strategies for OPA config files.
///
/// OPA only has one config file (`config.json`), which is JSON-formatted.
/// Users can override it using key-value pairs, JSON merge patch (RFC 7396),
/// JSON patch (RFC 6902), or by providing the full file content.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, Merge, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaConfigOverrides {
    /// Overrides for the OPA `config.json` file.
    #[serde(default, rename = "config.json")]
    pub config_json: JsonConfigOverrides,
}

#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, Default, Fragment, JsonSchema, PartialEq)]
#[fragment_attrs(
    allow(clippy::derive_partial_eq_without_eq),
    derive(
        Clone,
        Debug,
        Default,
        Deserialize,
        Merge,
        JsonSchema,
        PartialEq,
        Serialize
    ),
    serde(rename_all = "camelCase")
)]
pub struct OpaStorageConfig {}

#[derive(
    Clone,
    Debug,
    Deserialize,
    Display,
    Eq,
    EnumIter,
    JsonSchema,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Container {
    Prepare,
    Vector,
    BundleBuilder,
    Opa,
    UserInfoFetcher,
    ResourceInfoFetcher,
}

// NOTE (@Techassi): This struct can currently NOT be versioned because it is used via Role which
// makes it incredible hard to implement the From trait for conversions.
#[derive(Clone, Debug, Default, Fragment, JsonSchema, PartialEq)]
#[fragment_attrs(
    derive(
        Clone,
        Debug,
        Default,
        Deserialize,
        Merge,
        JsonSchema,
        PartialEq,
        Serialize
    ),
    serde(rename_all = "camelCase")
)]
pub struct OpaConfig {
    #[fragment_attrs(serde(default))]
    pub resources: Resources<OpaStorageConfig, NoRuntimeLimits>,

    #[fragment_attrs(serde(default))]
    pub logging: Logging<Container>,

    #[fragment_attrs(serde(default))]
    pub affinity: StackableAffinity,

    /// Time period Pods have to gracefully shut down, e.g. `30m`, `1h` or `2d`. Consult the operator documentation for details.
    #[fragment_attrs(serde(default))]
    pub graceful_shutdown_timeout: Option<Duration>,
}

constant!(SERVER_ROLE_NAME: RoleName = "server");

#[derive(Clone, Debug, EnumIter, Eq, Ord, PartialOrd, PartialEq)]
pub enum OpaRole {
    Server,
}

impl Deref for OpaRole {
    type Target = RoleName;

    fn deref(&self) -> &Self::Target {
        match self {
            OpaRole::Server => &SERVER_ROLE_NAME,
        }
    }
}

// TODO (@Techassi): Support versioned status
#[derive(Clone, Default, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaClusterStatus {
    #[serde(default)]
    pub conditions: Vec<ClusterCondition>,
}

impl v1alpha2::CurrentlySupportedListenerClasses {
    pub fn k8s_service_type(&self) -> String {
        match self {
            v1alpha2::CurrentlySupportedListenerClasses::ClusterInternal => "ClusterIP".to_string(),
            v1alpha2::CurrentlySupportedListenerClasses::ExternalUnstable => "NodePort".to_string(),
            v1alpha2::CurrentlySupportedListenerClasses::ExternalStable => {
                "LoadBalancer".to_string()
            }
        }
    }
}

// TODO: Remove the `allow` once the PodDisruptionBudget builder calls
// `pod_disruption_budget_enabled`. Part of https://github.com/stackabletech/opa-operator/issues/525.
#[allow(dead_code)]
impl v1alpha2::OpaRoleConfig {
    /// The `internalTrafficPolicy` to write into the role Service.
    ///
    /// Derived from the [`v1alpha2::WorkloadKind`]: `Local` for a DaemonSet, which covers every
    /// node, and `Cluster` for a Deployment, whose Pods do not.
    ///
    /// This is the single place the policy is decided, so exposing a user override later means
    /// adding the CRD field back and wrapping this `match` in an `unwrap_or`; no call site changes.
    pub fn internal_traffic_policy(&self) -> v1alpha2::InternalTrafficPolicy {
        match self.workload_kind {
            v1alpha2::WorkloadKind::DaemonSet => v1alpha2::InternalTrafficPolicy::Local,
            v1alpha2::WorkloadKind::Deployment => v1alpha2::InternalTrafficPolicy::Cluster,
        }
    }

    /// Whether a PodDisruptionBudget should be written out for this role.
    ///
    /// Falls back to `true` for a Deployment only: `kubectl drain` requires `--ignore-daemonsets`
    /// and then leaves those Pods alone, so a PDB would protect nothing in DaemonSet mode.
    pub fn pod_disruption_budget_enabled(&self) -> bool {
        self.pod_disruption_budget
            .enabled
            .unwrap_or(self.workload_kind == v1alpha2::WorkloadKind::Deployment)
    }
}

impl OpaConfig {
    pub fn default_config() -> OpaConfigFragment {
        OpaConfigFragment {
            logging: product_logging::spec::default_logging(),
            resources: ResourcesFragment {
                cpu: CpuLimitsFragment {
                    min: Some(Quantity("250m".to_owned())),
                    max: Some(Quantity("500m".to_owned())),
                },
                memory: MemoryLimitsFragment {
                    limit: Some(Quantity("256Mi".to_owned())),
                    runtime_limits: NoRuntimeLimitsFragment {},
                },
                storage: OpaStorageConfigFragment {},
            },
            // There is no point in having a default affinity, as exactly one OPA Pods should run on every node.
            // We only have the affinity configurable to let users limit the nodes the OPA Pods run on.
            affinity: Default::default(),
            graceful_shutdown_timeout: Some(DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT),
        }
    }
}

impl v1alpha2::OpaCluster {
    /// Returns a reference to the role.
    pub fn role(&self, role_variant: &OpaRole) -> &OpaRoleType {
        match role_variant {
            OpaRole::Server => &self.spec.servers,
        }
    }
}

impl HasStatusCondition for v1alpha2::OpaCluster {
    fn conditions(&self) -> Vec<ClusterCondition> {
        match &self.status {
            Some(status) => status.conditions.clone(),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::formatdoc;
    use serde_json::json;
    use stackable_operator::versioned::test_utils::RoundtripTestData;

    use super::{SERVER_ROLE_NAME, v1alpha1, v1alpha2};

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *SERVER_ROLE_NAME;
    }

    /// The values the operator derives from `workloadKind`, which an OpenAPI schema default cannot
    /// express. `internalTrafficPolicy` is derived outright; `podDisruptionBudget.enabled` is a
    /// default the user can override.
    #[test]
    fn role_config_defaults_follow_workload_kind() {
        let role_config = |workload_kind| v1alpha2::OpaRoleConfig {
            workload_kind,
            ..v1alpha2::OpaRoleConfig::default()
        };

        let daemon_set = role_config(v1alpha2::WorkloadKind::DaemonSet);
        assert_eq!(
            daemon_set.internal_traffic_policy(),
            v1alpha2::InternalTrafficPolicy::Local
        );
        // `kubectl drain` skips DaemonSet Pods, so a PDB would protect nothing.
        assert!(!daemon_set.pod_disruption_budget_enabled());

        let deployment = role_config(v1alpha2::WorkloadKind::Deployment);
        assert_eq!(
            deployment.internal_traffic_policy(),
            v1alpha2::InternalTrafficPolicy::Cluster
        );
        assert!(deployment.pod_disruption_budget_enabled());
    }

    /// An explicitly configured `podDisruptionBudget.enabled` wins over the `workloadKind`-derived
    /// default, which is the point of exposing the field as an `Option` at all.
    #[test]
    fn explicit_role_config_overrides_the_derived_defaults() {
        let role_config = v1alpha2::OpaRoleConfig {
            workload_kind: v1alpha2::WorkloadKind::DaemonSet,
            pod_disruption_budget: v1alpha2::OpaPdbConfig {
                enabled: Some(true),
                max_unavailable: None,
            },
        };

        assert!(role_config.pod_disruption_budget_enabled());
        // `internalTrafficPolicy` is not yet user-configurable, so it stays at the DaemonSet default.
        assert_eq!(
            role_config.internal_traffic_policy(),
            v1alpha2::InternalTrafficPolicy::Local
        );
    }

    /// Leaving the PDB fields out and writing them as an explicit `null` must resolve to the same
    /// unset state, as the derived default is applied by the operator rather than by the schema.
    ///
    /// Only covers what serde does; substituting the `roleConfig` default for an entirely absent
    /// `roleConfig` is the apiserver's job and is not exercised here.
    #[test]
    fn unset_role_config_fields_deserialise_to_none() {
        let unset = v1alpha2::OpaRoleConfig::default();

        for value in [
            json!({}),
            json!({ "workloadKind": "DaemonSet" }),
            json!({
                "workloadKind": "DaemonSet",
                "podDisruptionBudget": { "enabled": null, "maxUnavailable": null },
            }),
        ] {
            let role_config: v1alpha2::OpaRoleConfig =
                serde_json::from_value(value.clone()).expect("a valid role config");
            assert_eq!(role_config, unset, "unexpected role config for {value}");
        }
    }

    /// The two enums must serialise the way Kubernetes spells them: `workloadKind` names the
    /// workload API kinds, and `internalTrafficPolicy` is passed through to `Service.spec`.
    #[test]
    fn enums_use_the_kubernetes_spelling() {
        assert_eq!(
            serde_json::to_value(v1alpha2::WorkloadKind::DaemonSet).unwrap(),
            json!("DaemonSet")
        );
        assert_eq!(
            serde_json::to_value(v1alpha2::WorkloadKind::Deployment).unwrap(),
            json!("Deployment")
        );
        assert_eq!(
            serde_json::to_value(v1alpha2::InternalTrafficPolicy::Local).unwrap(),
            json!("Local")
        );
        assert_eq!(
            serde_json::to_value(v1alpha2::InternalTrafficPolicy::Cluster).unwrap(),
            json!("Cluster")
        );

        // The Service builder writes the policy via `Display`, which is derived by strum and does
        // not honour `#[serde(rename_all)]`. Asserted separately, so renaming a variant cannot
        // leave serde green while the Service gets a value Kubernetes rejects.
        assert_eq!(v1alpha2::InternalTrafficPolicy::Local.to_string(), "Local");
        assert_eq!(
            v1alpha2::InternalTrafficPolicy::Cluster.to_string(),
            "Cluster"
        );
    }

    impl RoundtripTestData for v1alpha1::OpaClusterSpec {
        fn roundtrip_test_data() -> Vec<Self> {
            let user_info_fetcher_sections = vec![
                r#"
  userInfo:
    backend:
      experimentalXfscAas:
        hostname: aas.default.svc.cluster.local
        port: 5000
    "#,
                r#"
  userInfo:
    backend:
      experimentalActiveDirectory:
        ldapServer: sble-addc.sble.test
        baseDistinguishedName: DC=sble,DC=test
        customAttributeMappings:
          country: c
        kerberosSecretClassName: kerberos-ad
        tls:
          verification:
            server:
              caCert:
                secretClass: tls-ad
    cache:
      entryTimeToLive: 60s
    "#,
                r#"
  userInfo:
    backend:
      keycloak:
        hostname: keycloak.default.svc.cluster.local
        port: 8443
        tls:
          verification:
            server:
              caCert:
                secretClass: keycloak-tls
        clientCredentialsSecret: user-info-fetcher-client-credentials
        adminRealm: my-dataspace
        userRealm: my-dataspace
    "#,
                r#"
  userInfo:
    backend:
      experimentalOpenLdap:
        hostname: test-openldap.default.svc.cluster.local
        port: 1636
        searchBase: ou=users,dc=example,dc=org
        bindCredentials:
          secretClass: ldap-bind-test
        groupsSearchBase: ou=groups,dc=example,dc=org
        customAttributeMappings:
          hdir: homeDirectory
          displayName: cn
          surname: sn
        tls:
          verification:
            server:
              caCert:
                secretClass: ldap-tls-test
    cache:
      entryTimeToLive: 60s
    "#,
                r#"
  userInfo:
    backend:
      # Note the experimentalEntra vs entra here!
      experimentalEntra:
        tenantId: 00000000-0000-0000-0000-000000000000
        clientCredentialsSecret: user-info-fetcher-client-credentials
    "#,
            ];
            user_info_fetcher_sections
                .into_iter()
                .map(test_opa_cluster_yaml)
                .map(|yaml| {
                    println!("{}", yaml);
                    stackable_operator::utils::yaml_from_str_singleton_map(&yaml)
                        .expect("Failed to parse OpaClusterSpec YAML")
                })
                .collect()
        }
    }

    impl RoundtripTestData for v1alpha2::OpaClusterSpec {
        fn roundtrip_test_data() -> Vec<Self> {
            let user_info_fetcher_sections = vec![
                r#"
  userInfo:
    backend:
      experimentalXfscAas:
        hostname: aas.default.svc.cluster.local
        port: 5000
    "#,
                r#"
  userInfo:
    backend:
      experimentalActiveDirectory:
        ldapServer: sble-addc.sble.test
        baseDistinguishedName: DC=sble,DC=test
        customAttributeMappings:
          country: c
        kerberosSecretClassName: kerberos-ad
        tls:
          verification:
            server:
              caCert:
                secretClass: tls-ad
    cache:
      entryTimeToLive: 60s
    "#,
                r#"
  userInfo:
    backend:
      keycloak:
        hostname: keycloak.default.svc.cluster.local
        port: 8443
        tls:
          verification:
            server:
              caCert:
                secretClass: keycloak-tls
        clientCredentialsSecret: user-info-fetcher-client-credentials
        adminRealm: my-dataspace
        userRealm: my-dataspace
    "#,
                r#"
  userInfo:
    backend:
      experimentalOpenLdap:
        hostname: test-openldap.default.svc.cluster.local
        port: 1636
        searchBase: ou=users,dc=example,dc=org
        bindCredentials:
          secretClass: ldap-bind-test
        groupsSearchBase: ou=groups,dc=example,dc=org
        customAttributeMappings:
          hdir: homeDirectory
          displayName: cn
          surname: sn
        tls:
          verification:
            server:
              caCert:
                secretClass: ldap-tls-test
    cache:
      entryTimeToLive: 60s
    "#,
                r#"
  userInfo:
    backend:
      # Note the experimentalEntra vs entra here!
      entra:
        tenantId: 00000000-0000-0000-0000-000000000000
        clientCredentialsSecret: user-info-fetcher-client-credentials
    "#,
            ];
            user_info_fetcher_sections
                .into_iter()
                .map(test_opa_cluster_yaml)
                .map(|yaml| {
                    println!("{}", yaml);
                    stackable_operator::utils::yaml_from_str_singleton_map(&yaml)
                        .expect("Failed to parse OpaClusterSpec YAML")
                })
                .collect()
        }
    }

    fn test_opa_cluster_yaml(user_info_fetcher_section: &str) -> String {
        formatdoc! {
          r#"
            image:
              productVersion: 1.2.3
              pullPolicy: IfNotPresent
            clusterOperation:
              stopped: false
              reconciliationPaused: false
            clusterConfig:
              tls:
                serverSecretClass: my-tls
              vectorAggregatorConfigMapName: vector-aggregator-discovery
              {user_info_fetcher_section}
            servers:
              config:
                logging:
                  enableVectorAgent: true
              configOverrides:
                config.json:
                  jsonMergePatch:
                    bundles:
                      stackable:
                        polling:
                          min_delay_seconds: 3
                          max_delay_seconds: 7
                    default_decision: test/hello
              envOverrides:
                SERVER_ROLE_LEVEL_ENV_VAR: SERVER_ROLE_LEVEL_ENV_VAR
              roleGroups:
                default:
                  configOverrides:
                    config.json:
                      jsonMergePatch:
                        bundles:
                          stackable:
                            polling:
                              max_delay_seconds: 5
                        labels:
                          rolegroup: default
                  envOverrides:
                    SERVER_ROLE_GROUP_LEVEL_ENV_VAR: SERVER_ROLE_GROUP_LEVEL_ENV_VAR
                  replicas: 1
              "#
        }
    }
}
