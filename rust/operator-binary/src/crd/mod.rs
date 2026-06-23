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
    deep_merger::ObjectOverrides,
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::CustomResource,
    product_logging::{self, spec::Logging},
    role_utils::{EmptyRoleConfig, Role},
    schemars::{self, JsonSchema},
    shared::time::Duration,
    status::condition::{ClusterCondition, HasStatusCondition},
    v2::{
        config_overrides::JsonOrKeyValueConfigOverrides,
        role_utils::GenericCommonConfig,
        types::kubernetes::{ConfigMapName, SecretClassName},
    },
    versioned::versioned,
};
use strum::{Display, EnumIter, EnumString};

pub mod user_info_fetcher;

pub const APP_NAME: &str = "opa";
pub const OPERATOR_NAME: &str = "opa.stackable.tech";
pub const FIELD_MANAGER: &str = "opa-operator";

pub const DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_minutes_unchecked(2);
/// Safety puffer to guarantee the graceful shutdown works every time.
pub const SERVER_GRACEFUL_SHUTDOWN_SAFETY_OVERHEAD: Duration = Duration::from_secs(5);

pub type OpaRoleType =
    Role<OpaConfigFragment, OpaConfigOverrides, EmptyRoleConfig, GenericCommonConfig>;

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

        /// TLS encryption settings for the OPA server.
        /// When configured, OPA will use HTTPS (port 8443) instead of HTTP (port 8081).
        /// Clients must connect using HTTPS and trust the certificates provided by the configured SecretClass.
        #[serde(default)]
        #[versioned(hint(option))]
        pub tls: Option<OpaTls>,
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
    pub config_json: JsonOrKeyValueConfigOverrides,
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

#[derive(
    EnumIter,
    Clone,
    Debug,
    Hash,
    Deserialize,
    Eq,
    JsonSchema,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Display,
    EnumString,
)]
pub enum OpaRole {
    #[serde(rename = "server")]
    #[strum(serialize = "server")]
    Server,
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
    use stackable_operator::versioned::test_utils::RoundtripTestData;

    use super::{v1alpha1, v1alpha2};

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
                    println!("{}", &yaml);
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
                    println!("{}", &yaml);
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
