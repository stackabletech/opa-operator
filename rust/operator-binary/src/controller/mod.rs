//! Controller-level vocabulary: the [`ValidatedCluster`] type and the `build` / `validate`
//! sub-modules.

use std::{collections::BTreeMap, str::FromStr};

use stackable_opa_operator::crd::{
    APP_NAME, OPERATOR_NAME, OpaConfig, OpaConfigOverrides, OpaRole, OpaStorageConfig,
    resource_info_fetcher, user_info_fetcher, v1alpha2,
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
        apps::v1::DaemonSet,
        core::v1::{ConfigMap, Service, ServiceAccount},
        rbac::v1::RoleBinding,
    },
    kube::{Resource as KubeResource, api::ObjectMeta},
    kvp::Labels,
    shared::time::Duration,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        kvp::label::{recommended_labels, role_group_selector, role_selector},
        role_group_utils::ResourceNames,
        role_utils::{self, GenericCommonConfig, RoleGroupConfig},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::{
                ClusterName, ControllerName, OperatorName, ProductName, ProductVersion, RoleName,
            },
        },
    },
};

use crate::opa_controller::OPA_CONTROLLER_NAME;

pub mod build;
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
    pub role_group_configs: BTreeMap<OpaRole, BTreeMap<RoleGroupName, OpaRoleGroupConfig>>,
}

impl ValidatedCluster {
    pub(crate) fn new(
        name: ClusterName,
        namespace: NamespaceName,
        uid: Uid,
        image: ResolvedProductImage,
        cluster_config: ValidatedClusterConfig,
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
            role_group_configs,
        }
    }

    /// Whether the cluster serves HTTPS, derived from the validated cluster config.
    pub fn is_tls_enabled(&self) -> bool {
        self.cluster_config.tls.is_some()
    }

    /// The name of the role-level load-balanced Kubernetes `Service`, as used in the discovery URL.
    pub fn server_role_service_name(&self) -> String {
        format!("{name}-{role}", name = self.name, role = OpaRole::Server)
    }

    /// Type-safe names for the per-cluster RBAC resources: the ServiceAccount shared by all
    /// Pods, its (namespaced) RoleBinding, and the operator-deployed ClusterRole it binds.
    pub fn cluster_resource_names(&self) -> role_utils::ResourceNames {
        role_utils::ResourceNames {
            cluster_name: self.name.clone(),
            product_name: product_name(),
        }
    }

    /// Type-safe names for the resources of a given role group.
    pub(crate) fn role_group_resource_names(
        &self,
        role_group_name: &RoleGroupName,
    ) -> ResourceNames {
        ResourceNames {
            cluster_name: self.name.clone(),
            role_name: OpaRole::Server.into(),
            role_group_name: role_group_name.clone(),
        }
    }

    pub fn recommended_labels(&self, role_group_name: &RoleGroupName) -> Labels {
        self.recommended_labels_for(&OpaRole::Server.into(), role_group_name)
    }

    /// Recommended labels for a resource that is not tied to a concrete role,
    /// using a free-form role/role-group label value.
    pub fn recommended_labels_for(
        &self,
        role_name: &RoleName,
        role_group_name: &RoleGroupName,
    ) -> Labels {
        self.recommended_labels_with(&self.product_version, role_name, role_group_name)
    }

    fn recommended_labels_with(
        &self,
        product_version: &ProductVersion,
        role_name: &RoleName,
        role_group_name: &RoleGroupName,
    ) -> Labels {
        recommended_labels(
            self,
            &product_name(),
            product_version,
            &operator_name(),
            &controller_name(),
            role_name,
            role_group_name,
        )
    }

    /// Selector labels matching the pods of a role group.
    pub fn role_group_selector(&self, role_group_name: &RoleGroupName) -> Labels {
        role_group_selector(
            self,
            &product_name(),
            &OpaRole::Server.into(),
            role_group_name,
        )
    }

    /// Selector labels matching all pods of the (single) OPA role.
    pub fn role_selector(&self) -> Labels {
        role_selector(self, &product_name(), &OpaRole::Server.into())
    }
}

/// The product name (`opa`) as a type-safe label value.
pub(crate) fn product_name() -> ProductName {
    ProductName::from_str(APP_NAME).expect("'opa' is a valid product name")
}

/// The operator name as a type-safe label value.
pub(crate) fn operator_name() -> OperatorName {
    OperatorName::from_str(OPERATOR_NAME).expect("the operator name is a valid label value")
}

/// The controller name as a type-safe label value.
pub(crate) fn controller_name() -> ControllerName {
    ControllerName::from_str(OPA_CONTROLLER_NAME)
        .expect("the controller name is a valid label value")
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

/// Every Kubernetes resource produced by the [`build`](build::build) step.
///
/// OPA runs as a `DaemonSet` (one Pod per node), so there are no `StatefulSet`s, PDBs or
/// `Listener`s. `services` holds the role-level `Service` and the per-role-group headless and
/// metrics `Service`s; `config_maps` holds the per-role-group `ConfigMap`s and the cluster-level
/// discovery `ConfigMap`.
pub struct KubernetesResources {
    pub daemon_sets: Vec<DaemonSet>,
    pub services: Vec<Service>,
    pub config_maps: Vec<ConfigMap>,
    pub service_accounts: Vec<ServiceAccount>,
    pub role_bindings: Vec<RoleBinding>,
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

#[cfg(test)]
mod tests {
    use stackable_opa_operator::crd::OpaRole;
    use stackable_operator::v2::types::operator::RoleName;
    use strum::IntoEnumIterator;

    /// Locks the invariant behind the `expect` in the `From<OpaRole> for RoleName` impls:
    /// every `OpaRole` variant (present and future) must serialise to a valid `RoleName`.
    #[test]
    fn every_opa_role_serialises_to_a_valid_role_name() {
        for role in OpaRole::iter() {
            let _: RoleName = (&role).into();
            let _: RoleName = role.into();
        }
    }
}
