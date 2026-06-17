//! Controller-level vocabulary: the [`ValidatedCluster`] type and the `build` / `validate`
//! sub-modules.

use std::{collections::BTreeMap, str::FromStr};

// Re-exported so the rest of the controller refers to `crate::controller::RoleGroupName`.
pub use stackable_operator::v2::types::operator::RoleGroupName;
use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        product_image_selection::ResolvedProductImage,
        resources::{NoRuntimeLimits, Resources},
    },
    kube::{Resource as KubeResource, api::ObjectMeta},
    kvp::Labels,
    shared::time::Duration,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        kvp::label::{recommended_labels, role_group_selector, role_selector},
        role_group_utils::ResourceNames,
        role_utils::{GenericCommonConfig, RoleGroupConfig},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::{
                ClusterName, ControllerName, OperatorName, ProductName, ProductVersion, RoleName,
            },
        },
    },
};

use crate::{
    crd::{
        APP_NAME, OPERATOR_NAME, OpaConfig, OpaConfigOverrides, OpaRole, OpaStorageConfig,
        user_info_fetcher, v1alpha2,
    },
    opa_controller::OPA_CONTROLLER_NAME,
};

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

    /// The single OPA role name (`server`).
    pub fn role_name() -> RoleName {
        RoleName::from_str(&OpaRole::Server.to_string())
            .expect("the server role name is a valid role name")
    }

    /// Type-safe names for the resources of a given role group.
    pub(crate) fn resource_names(&self, role_group_name: &RoleGroupName) -> ResourceNames {
        ResourceNames {
            cluster_name: self.name.clone(),
            role_name: Self::role_name(),
            role_group_name: role_group_name.clone(),
        }
    }

    /// The product version as a type-safe label value.
    ///
    /// `app_version_label_value` is constructed to be a valid label value, so it is also a valid
    /// [`ProductVersion`].
    fn product_version(&self) -> ProductVersion {
        ProductVersion::from_str(&self.image.app_version_label_value)
            .expect("the app version label value is a valid product version")
    }

    /// Recommended labels for a role-group resource.
    ///
    /// For role-level or cluster-level resources (e.g. the role `Service` or the discovery
    /// `ConfigMap`) pass a placeholder role-group name such as `global` or `discovery`.
    pub fn recommended_labels(&self, role_group_name: &RoleGroupName) -> Labels {
        recommended_labels(
            self,
            &product_name(),
            &self.product_version(),
            &operator_name(),
            &controller_name(),
            &Self::role_name(),
            role_group_name,
        )
    }

    /// Selector labels matching the pods of a role group.
    pub fn role_group_selector(&self, role_group_name: &RoleGroupName) -> Labels {
        role_group_selector(self, &product_name(), &Self::role_name(), role_group_name)
    }

    /// Selector labels matching all pods of the (single) OPA role.
    pub fn role_selector(&self) -> Labels {
        role_selector(self, &product_name(), &Self::role_name())
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

/// Cluster-wide settings resolved once during validation, so the build steps no longer need the
/// raw `OpaCluster` to render config (except for owner references).
pub struct ValidatedClusterConfig {
    pub user_info: Option<user_info_fetcher::v1alpha2::Config>,
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
