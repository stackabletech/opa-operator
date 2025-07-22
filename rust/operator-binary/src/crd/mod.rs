use std::{collections::BTreeMap, str::FromStr};

use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
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
    config::{
        fragment::{self, Fragment, ValidationError},
        merge::Merge,
    },
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::{CustomResource, ResourceExt},
    product_config_utils::Configuration,
    product_logging::{self, spec::Logging},
    role_utils::{
        EmptyRoleConfig, GenericProductSpecificCommonConfig, Role, RoleGroup, RoleGroupRef,
    },
    schemars::{self, JsonSchema},
    status::condition::{ClusterCondition, HasStatusCondition},
    time::Duration,
    utils::cluster_info::KubernetesClusterInfo,
    versioned::versioned,
};
use strum::{Display, EnumIter, EnumString};

pub mod user_info_fetcher;

pub const APP_NAME: &str = "opa";
pub const OPERATOR_NAME: &str = "opa.stackable.tech";

pub const DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_minutes_unchecked(2);
/// Safety puffer to guarantee the graceful shutdown works every time.
pub const SERVER_GRACEFUL_SHUTDOWN_SAFETY_OVERHEAD: Duration = Duration::from_secs(5);

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("the role group {role_group} is not defined"))]
    CannotRetrieveOpaRoleGroup { role_group: String },

    #[snafu(display("unknown role {role}"))]
    UnknownOpaRole {
        source: strum::ParseError,
        role: String,
    },

    #[snafu(display("the role group [{role_group}] is missing"))]
    MissingRoleGroup { role_group: String },

    #[snafu(display("fragment validation failure"))]
    FragmentValidationFailure { source: ValidationError },
}

#[versioned(
    version(name = "v1alpha1"),
    crates(
        kube_core = "stackable_operator::kube::core",
        kube_client = "stackable_operator::kube::client",
        k8s_openapi = "stackable_operator::k8s_openapi",
        schemars = "stackable_operator::schemars",
        versioned = "stackable_operator::versioned"
    ),
    skip(from)
)]
pub mod versioned {
    #[versioned(crd(
        group = "opa.stackable.tech",
        status = "OpaClusterStatus",
        namespaced,
        shortname = "opa",
    ))]
    #[derive(Clone, Debug, Deserialize, CustomResource, JsonSchema, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaClusterSpec {
        /// Global OPA cluster configuration that applies to all roles and role groups.
        #[serde(default)]
        pub cluster_config: v1alpha1::OpaClusterConfig,
        /// Cluster operations like pause reconciliation or cluster stop.
        #[serde(default)]
        pub cluster_operation: ClusterOperation,
        /// OPA server configuration.
        pub servers: Role<OpaConfigFragment, EmptyRoleConfig>,
        /// The OPA image to use
        pub image: ProductImage,
    }

    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaClusterConfig {
        /// Name of the Vector aggregator discovery ConfigMap.
        /// It must contain the key `ADDRESS` with the address of the Vector aggregator.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub vector_aggregator_config_map_name: Option<String>,
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
        pub listener_class: v1alpha1::CurrentlySupportedListenerClasses,
        /// Configures how to fetch additional metadata about users (such as group memberships)
        /// from an external directory service.
        #[serde(default)]
        pub user_info: Option<user_info_fetcher::v1alpha1::Config>,
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
        pub resources: Resources<v1alpha1::OpaStorageConfig, NoRuntimeLimits>,

        #[fragment_attrs(serde(default))]
        pub logging: Logging<v1alpha1::Container>,

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
        PartialEq,
        Serialize,
        Display,
        EnumString,
    )]
    pub enum OpaRole {
        #[serde(rename = "server")]
        #[strum(serialize = "server")]
        Server,
    }

    #[derive(Clone, Default, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OpaClusterStatus {
        #[serde(default)]
        pub conditions: Vec<ClusterCondition>,
    }
}

impl v1alpha1::CurrentlySupportedListenerClasses {
    pub fn k8s_service_type(&self) -> String {
        match self {
            v1alpha1::CurrentlySupportedListenerClasses::ClusterInternal => "ClusterIP".to_string(),
            v1alpha1::CurrentlySupportedListenerClasses::ExternalUnstable => "NodePort".to_string(),
            v1alpha1::CurrentlySupportedListenerClasses::ExternalStable => {
                "LoadBalancer".to_string()
            }
        }
    }
}

impl v1alpha1::OpaConfig {
    fn default_config() -> v1alpha1::OpaConfigFragment {
        v1alpha1::OpaConfigFragment {
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
                storage: v1alpha1::OpaStorageConfigFragment {},
            },
            // There is no point in having a default affinity, as exactly one OPA Pods should run on every node.
            // We only have the affinity configurable to let users limit the nodes the OPA Pods run on.
            affinity: Default::default(),
            graceful_shutdown_timeout: Some(DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT),
        }
    }
}

impl Configuration for v1alpha1::OpaConfigFragment {
    type Configurable = v1alpha1::OpaCluster;

    fn compute_env(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, stackable_operator::product_config_utils::Error>
    {
        Ok(BTreeMap::new())
    }

    fn compute_cli(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, stackable_operator::product_config_utils::Error>
    {
        Ok(BTreeMap::new())
    }

    fn compute_files(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
        _file: &str,
    ) -> Result<BTreeMap<String, Option<String>>, stackable_operator::product_config_utils::Error>
    {
        Ok(BTreeMap::new())
    }
}

impl v1alpha1::OpaCluster {
    /// Returns a reference to the role.
    pub fn role(
        &self,
        role_variant: &v1alpha1::OpaRole,
    ) -> &Role<v1alpha1::OpaConfigFragment, EmptyRoleConfig> {
        match role_variant {
            v1alpha1::OpaRole::Server => &self.spec.servers,
        }
    }

    /// Returns a reference to the role group. Raises an error if the role or role group are not defined.
    pub fn rolegroup(
        &self,
        rolegroup_ref: &RoleGroupRef<v1alpha1::OpaCluster>,
    ) -> Result<&RoleGroup<v1alpha1::OpaConfigFragment, GenericProductSpecificCommonConfig>, Error>
    {
        let role_variant = v1alpha1::OpaRole::from_str(&rolegroup_ref.role).with_context(|_| {
            UnknownOpaRoleSnafu {
                role: rolegroup_ref.role.to_owned(),
            }
        })?;
        let role = self.role(&role_variant);
        role.role_groups
            .get(&rolegroup_ref.role_group)
            .with_context(|| CannotRetrieveOpaRoleGroupSnafu {
                role_group: rolegroup_ref.role_group.to_owned(),
            })
    }

    /// The name of the role-level load-balanced Kubernetes `Service`
    pub fn server_role_service_name(&self) -> Option<String> {
        Some(format!(
            "{cluster_name}-{role}",
            cluster_name = self.name_any(),
            role = v1alpha1::OpaRole::Server
        ))
    }

    /// The fully-qualified domain name of the role-level load-balanced Kubernetes `Service`
    pub fn server_role_service_fqdn(&self, cluster_info: &KubernetesClusterInfo) -> Option<String> {
        Some(format!(
            "{role_service_name}.{namespace}.svc.{cluster_domain}",
            role_service_name = self.server_role_service_name()?,
            namespace = self.metadata.namespace.as_ref()?,
            cluster_domain = cluster_info.cluster_domain
        ))
    }

    /// Retrieve and merge resource configs for role and role groups
    pub fn merged_config(
        &self,
        role: &v1alpha1::OpaRole,
        rolegroup_ref: &RoleGroupRef<v1alpha1::OpaCluster>,
    ) -> Result<v1alpha1::OpaConfig, Error> {
        // Initialize the result with all default values as baseline
        let conf_defaults = v1alpha1::OpaConfig::default_config();

        let opa_role = match role {
            v1alpha1::OpaRole::Server => &self.spec.servers,
        };

        let mut conf_role = opa_role.config.config.to_owned();

        // Retrieve rolegroup specific resource config
        let mut conf_rolegroup = opa_role
            .role_groups
            .get(&rolegroup_ref.role_group)
            .context(MissingRoleGroupSnafu {
                role_group: rolegroup_ref.role_group.clone(),
            })?
            .to_owned()
            .config
            .config;

        // Merge more specific configs into default config
        // Hierarchy is:
        // 1. RoleGroup
        // 2. Role
        // 3. Default
        conf_role.merge(&conf_defaults);
        conf_rolegroup.merge(&conf_role);

        tracing::debug!("Merged config: {:?}", conf_rolegroup);
        fragment::validate(conf_rolegroup).context(FragmentValidationFailureSnafu)
    }
}

impl HasStatusCondition for v1alpha1::OpaCluster {
    fn conditions(&self) -> Vec<ClusterCondition> {
        match &self.status {
            Some(status) => status.conditions.clone(),
            None => vec![],
        }
    }
}
