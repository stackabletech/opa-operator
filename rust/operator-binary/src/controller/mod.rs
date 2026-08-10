//! Controller-level vocabulary: the [`ValidatedCluster`] type and the `build` / `validate`
//! sub-modules.

use std::{collections::BTreeMap, marker::PhantomData, str::FromStr};

use stackable_opa_operator::crd::{
    OpaConfig, OpaConfigOverrides, OpaRole, OpaStorageConfig, resource_info_fetcher,
    user_info_fetcher, v1alpha2,
};
// Re-exported so the rest of the controller refers to `crate::controller::RoleGroupName`.
pub use stackable_operator::v2::types::operator::RoleGroupName;
use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        product_image_selection::ResolvedProductImage,
        resources::{NoRuntimeLimits, Resources},
    },
    k8s_openapi::api::{
        apps::v1::{DaemonSet, Deployment},
        core::v1::{ConfigMap, Service, ServiceAccount},
        rbac::v1::RoleBinding,
    },
    kube::{Resource as KubeResource, api::ObjectMeta},
    shared::time::Duration,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        role_group_utils::ResourceNames,
        role_utils::{self, GenericCommonConfig, RoleGroupConfig},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::{ClusterName, ProductVersion, RoleName},
        },
    },
};

use crate::opa_controller::PRODUCT_NAME;

pub mod apply;
pub mod build;
pub mod update_status;
pub mod validate;

/// The validated [`v1alpha2::OpaCluster`].
///
/// The output of the validate step: config fragments and `configOverrides` merged and validated
/// for every role group.
pub struct ValidatedCluster {
    /// Object metadata (name, namespace, UID) of the owning `OpaCluster`, built from the validated
    /// fields below. Lets [`ValidatedCluster`] implement [`KubeResource`] so the build steps can
    /// derive owner references and object metadata without touching the raw `OpaCluster` spec.
    metadata: ObjectMeta,
    pub name: ClusterName,
    pub namespace: NamespaceName,
    pub uid: Uid,
    /// The product version as a valid label value, for the recommended `app.kubernetes.io/version`
    /// label. Derived from the resolved image's app-version label value.
    pub product_version: ProductVersion,
    pub image: ResolvedProductImage,
    pub cluster_config: ValidatedClusterConfig,
    /// The role-level configuration of every role, keyed the same way as `role_group_configs`.
    ///
    /// Role-level rather than role-group-level, because `workloadKind` decides the shape of the
    /// role Service, which selects across all of a role's role groups.
    pub role_configs: BTreeMap<OpaRole, v1alpha2::OpaRoleConfig>,
    pub role_group_configs: BTreeMap<OpaRole, BTreeMap<RoleGroupName, OpaRoleGroupConfig>>,
}

impl ValidatedCluster {
    pub(crate) fn new(
        name: ClusterName,
        namespace: NamespaceName,
        uid: Uid,
        image: ResolvedProductImage,
        cluster_config: ValidatedClusterConfig,
        role_configs: BTreeMap<OpaRole, v1alpha2::OpaRoleConfig>,
        role_group_configs: BTreeMap<OpaRole, BTreeMap<RoleGroupName, OpaRoleGroupConfig>>,
    ) -> Self {
        let product_version = ProductVersion::from_str(&image.app_version_label_value)
            .expect("the app version label value is a valid product version");

        let metadata = ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            uid: Some(uid.to_string()),
            ..ObjectMeta::default()
        };
        Self {
            metadata,
            name,
            namespace,
            uid,
            product_version,
            image,
            cluster_config,
            role_configs,
            role_group_configs,
        }
    }

    /// The role-level configuration of `role`.
    ///
    /// The validate step inserts an entry for every [`OpaRole`], falling back to the
    /// `OpaRoleConfig` default for roles the user did not configure.
    pub fn role_config(&self, role: &OpaRole) -> &v1alpha2::OpaRoleConfig {
        self.role_configs
            .get(role)
            .expect("the validate step inserts a role config for every role")
    }

    /// Whether the cluster serves HTTPS, derived from the validated cluster config.
    pub fn is_tls_enabled(&self) -> bool {
        self.cluster_config.tls.is_some()
    }

    /// The name of the role-level load-balanced Kubernetes `Service`, as used in the discovery URL.
    pub fn server_role_service_name(&self) -> String {
        format!(
            "{name}-{role}",
            name = self.name,
            role = OpaRole::Server.as_ref(),
        )
    }

    /// Type-safe names for the per-cluster RBAC resources: the ServiceAccount shared by all
    /// Pods, its (namespaced) RoleBinding, and the operator-deployed ClusterRole it binds.
    pub fn cluster_resource_names(&self) -> role_utils::ResourceNames {
        role_utils::ResourceNames {
            cluster_name: self.name.clone(),
            product_name: PRODUCT_NAME.clone(),
        }
    }

    /// Type-safe names for the resources of a given role group.
    pub(crate) fn role_group_resource_names(
        &self,
        role_group_name: &RoleGroupName,
    ) -> ResourceNames {
        ResourceNames {
            cluster_name: self.name.clone(),
            role_name: RoleName::clone(&OpaRole::Server),
            role_group_name: role_group_name.clone(),
        }
    }
}

impl HasName for ValidatedCluster {
    fn to_name(&self) -> String {
        self.name.to_string()
    }
}

impl HasUid for ValidatedCluster {
    fn to_uid(&self) -> Uid {
        self.uid.clone()
    }
}

impl NameIsValidLabelValue for ValidatedCluster {
    fn to_label_value(&self) -> String {
        self.name.to_label_value()
    }
}

impl KubeResource for ValidatedCluster {
    type DynamicType = <v1alpha2::OpaCluster as KubeResource>::DynamicType;
    type Scope = <v1alpha2::OpaCluster as KubeResource>::Scope;

    fn kind(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::kind(dt)
    }

    fn group(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::group(dt)
    }

    fn version(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::version(dt)
    }

    fn plural(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha2::OpaCluster::plural(dt)
    }

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }
}

/// Marker for prepared Kubernetes resources which are not applied yet.
pub struct Prepared;

/// Marker for applied Kubernetes resources.
pub struct Applied;

/// Every Kubernetes resource produced by the [`build`](build::build) step.
///
/// Each role group might run as either a `DaemonSet` or a `Deployment`, depending on its role's
/// `workloadKind`, so exactly one of `daemon_sets` and `deployments` holds an entry for it. There
/// are no `StatefulSet`s or `Listener`s. `services` holds the role-level `Service` and the
/// per-role-group headless and metrics `Service`s; `config_maps` holds the per-role-group
/// `ConfigMap`s and the cluster-level discovery `ConfigMap`.
///
/// `T` is a marker that indicates whether these resources are only [`Prepared`] or already
/// [`Applied`]. It lets the type system prove that e.g. the cluster status is derived from
/// applied resources rather than merely built ones.
pub struct KubernetesResources<T> {
    pub daemon_sets: Vec<DaemonSet>,
    pub deployments: Vec<Deployment>,
    pub services: Vec<Service>,
    pub config_maps: Vec<ConfigMap>,
    pub service_accounts: Vec<ServiceAccount>,
    pub role_bindings: Vec<RoleBinding>,
    pub status: PhantomData<T>,
}

/// Cluster-wide settings resolved once during validation, so the build steps no longer need the
/// raw `OpaCluster` to render config (except for owner references).
pub struct ValidatedClusterConfig {
    pub user_info: Option<user_info_fetcher::v1alpha2::Config>,
    pub resource_info: Option<resource_info_fetcher::v1alpha1::Config>,
    pub tls: Option<v1alpha2::OpaTls>,
    pub listener_class: v1alpha2::CurrentlySupportedListenerClasses,
}

/// The validated, merged configuration of a single OPA role group.
pub type OpaRoleGroupConfig =
    RoleGroupConfig<ValidatedOpaConfig, GenericCommonConfig, OpaConfigOverrides>;

/// A validated OPA config: the merged [`OpaConfig`].
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedOpaConfig {
    pub resources: Resources<OpaStorageConfig, NoRuntimeLimits>,
    pub logging: validate::ValidatedLogging,
    pub affinity: StackableAffinity,
    pub graceful_shutdown_timeout: Option<Duration>,
}

impl ValidatedOpaConfig {
    pub(crate) fn from_merged(merged: OpaConfig, logging: validate::ValidatedLogging) -> Self {
        Self {
            resources: merged.resources,
            logging,
            affinity: merged.affinity,
            graceful_shutdown_timeout: merged.graceful_shutdown_timeout,
        }
    }
}
