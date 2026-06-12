//! Controller-level vocabulary: the [`ValidatedCluster`] type and the `build` / `validate`
//! sub-modules.

use std::collections::BTreeMap;

use stackable_operator::{
    commons::product_image_selection::ResolvedProductImage,
    kube::{Resource as KubeResource, api::ObjectMeta},
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        role_utils::{GenericCommonConfig, RoleGroupConfig},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::{ClusterName, RoleGroupName},
        },
    },
};

use crate::crd::{OpaConfig, OpaConfigOverrides, OpaRole, user_info_fetcher, v1alpha2};

pub mod build;
pub mod validate;

/// The validated [`v1alpha2::OpaCluster`].
///
/// The output of the validate step: config fragments and `configOverrides` merged and validated
/// for every role group, ready to be turned into Kubernetes resources without touching the raw
/// `OpaCluster` spec again (except for owner references).
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

    /// The name of the role-level load-balanced Kubernetes `Service`, as used in the discovery URL.
    pub fn server_role_service_name(&self) -> String {
        format!("{name}-{role}", name = self.name, role = OpaRole::Server)
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

/// Cluster-wide settings resolved once during validation, so the build steps no longer need the
/// raw `OpaCluster` to render config (except for owner references).
pub struct ValidatedClusterConfig {
    pub user_info: Option<user_info_fetcher::v1alpha2::Config>,
    pub tls: Option<v1alpha2::OpaTls>,
}

/// The validated configuration of a single role group.
///
/// All override kinds (`config`, `configOverrides`, `envOverrides`, `cliOverrides`, `podOverrides`)
/// are merged once by [`with_validated_config`](stackable_operator::v2::role_utils::with_validated_config),
/// with the role group winning over the role, which wins over the operator defaults.
///
/// Note: `replicas` is carried by the framework type but unused here — OPA runs as a `DaemonSet`
/// (one Pod per node).
pub type OpaRoleGroupConfig = RoleGroupConfig<OpaConfig, GenericCommonConfig, OpaConfigOverrides>;
