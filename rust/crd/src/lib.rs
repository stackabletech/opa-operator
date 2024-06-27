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
    config::{fragment, fragment::Fragment, fragment::ValidationError, merge::Merge},
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::CustomResource,
    product_config_utils::Configuration,
    product_logging::{self, spec::Logging},
    role_utils::Role,
    role_utils::{EmptyRoleConfig, RoleGroup, RoleGroupRef},
    schemars::{self, JsonSchema},
    status::condition::{ClusterCondition, HasStatusCondition},
    time::Duration,
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

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "opa.stackable.tech",
    version = "v1alpha1",
    kind = "OpaCluster",
    shortname = "opa",
    status = "OpaClusterStatus",
    namespaced,
    crates(
        kube_core = "stackable_operator::kube::core",
        k8s_openapi = "stackable_operator::k8s_openapi",
        schemars = "stackable_operator::schemars"
    )
)]
#[serde(rename_all = "camelCase")]
pub struct OpaSpec {
    /// Global OPA cluster configuration that applies to all roles and role groups.
    #[serde(default)]
    pub cluster_config: OpaClusterConfig,
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
    pub listener_class: CurrentlySupportedListenerClasses,
    /// Configures how to fetch additional metadata about users (such as group memberships)
    /// from an external directory service.
    #[serde(default)]
    pub user_info: Option<user_info_fetcher::Config>,
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

impl CurrentlySupportedListenerClasses {
    pub fn k8s_service_type(&self) -> String {
        match self {
            CurrentlySupportedListenerClasses::ClusterInternal => "ClusterIP".to_string(),
            CurrentlySupportedListenerClasses::ExternalUnstable => "NodePort".to_string(),
            CurrentlySupportedListenerClasses::ExternalStable => "LoadBalancer".to_string(),
        }
    }
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
    pub resources: Resources<OpaStorageConfig, NoRuntimeLimits>,

    #[fragment_attrs(serde(default))]
    pub logging: Logging<Container>,

    #[fragment_attrs(serde(default))]
    pub affinity: StackableAffinity,

    /// Time period Pods have to gracefully shut down, e.g. `30m`, `1h` or `2d`. Consult the operator documentation for details.
    #[fragment_attrs(serde(default))]
    pub graceful_shutdown_timeout: Option<Duration>,
}

impl OpaConfig {
    fn default_config() -> OpaConfigFragment {
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

impl Configuration for OpaConfigFragment {
    type Configurable = OpaCluster;

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

impl OpaCluster {
    /// Returns a reference to the role.
    pub fn role(&self, role_variant: &OpaRole) -> &Role<OpaConfigFragment, EmptyRoleConfig> {
        match role_variant {
            OpaRole::Server => &self.spec.servers,
        }
    }

    /// Returns a reference to the role group. Raises an error if the role or role group are not defined.
    pub fn rolegroup(
        &self,
        rolegroup_ref: &RoleGroupRef<OpaCluster>,
    ) -> Result<&RoleGroup<OpaConfigFragment>, Error> {
        let role_variant =
            OpaRole::from_str(&rolegroup_ref.role).with_context(|_| UnknownOpaRoleSnafu {
                role: rolegroup_ref.role.to_owned(),
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
        self.metadata.name.clone()
    }

    /// The fully-qualified domain name of the role-level load-balanced Kubernetes `Service`
    pub fn server_role_service_fqdn(&self) -> Option<String> {
        Some(format!(
            "{}.{}.svc.cluster.local",
            self.server_role_service_name()?,
            self.metadata.namespace.as_ref()?
        ))
    }

    /// Retrieve and merge resource configs for role and role groups
    pub fn merged_config(
        &self,
        role: &OpaRole,
        rolegroup_ref: &RoleGroupRef<OpaCluster>,
    ) -> Result<OpaConfig, Error> {
        // Initialize the result with all default values as baseline
        let conf_defaults = OpaConfig::default_config();

        let opa_role = match role {
            OpaRole::Server => &self.spec.servers,
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

#[derive(Clone, Default, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpaClusterStatus {
    #[serde(default)]
    pub conditions: Vec<ClusterCondition>,
}

impl HasStatusCondition for OpaCluster {
    fn conditions(&self) -> Vec<ClusterCondition> {
        match &self.status {
            Some(status) => status.conditions.clone(),
            None => vec![],
        }
    }
}
