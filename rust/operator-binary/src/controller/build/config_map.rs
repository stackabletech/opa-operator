//! Assembles the rolegroup [`ConfigMap`] from the [`ValidatedCluster`], dispatching to the
//! per-file builders in [`super::properties`]. The owner object is only used for the owner
//! reference and object metadata, never for config content.

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    product_logging::framework::VECTOR_CONFIG_FILE,
    role_utils::RoleGroupRef,
    v2::builder::meta::ownerreference_from_resource,
};

use super::properties::{ConfigFileName, config_json, logging, user_info_fetcher};
use crate::{
    controller::{
        build_recommended_labels,
        validate::{OpaRoleGroupConfig, ValidatedCluster},
    },
    crd::v1alpha2,
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build config.json"))]
    BuildConfigJson { source: config_json::Error },

    #[snafu(display("failed to build user-info-fetcher.json"))]
    BuildUserInfoFetcher { source: user_info_fetcher::Error },

    #[snafu(display("failed to build ConfigMap for [{rolegroup}]"))]
    BuildConfigMap {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha2::OpaCluster>,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the
/// administrator.
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    rolegroup_config: &OpaRoleGroupConfig,
    rolegroup_ref: &RoleGroupRef<v1alpha2::OpaCluster>,
) -> Result<ConfigMap> {
    let mut cm_builder = ConfigMapBuilder::new();

    let metadata = ObjectMetaBuilder::new()
        .name(rolegroup_ref.object_name())
        .namespace(&cluster.namespace)
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_recommended_labels(&build_recommended_labels(
            cluster,
            &cluster.image.app_version_label_value,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .build();

    cm_builder.metadata(metadata).add_data(
        ConfigFileName::ConfigJson.to_string(),
        config_json::build(
            &rolegroup_config.merged_config,
            &rolegroup_config.config_overrides,
        )
        .context(BuildConfigJsonSnafu)?,
    );

    if let Some(user_info) = &cluster.cluster_config.user_info {
        cm_builder.add_data(
            ConfigFileName::UserInfoFetcher.to_string(),
            user_info_fetcher::build(user_info).context(BuildUserInfoFetcherSnafu)?,
        );
    }

    if let Some(vector_config) =
        logging::build_vector_config(rolegroup_ref, &rolegroup_config.merged_config.logging)
    {
        cm_builder.add_data(VECTOR_CONFIG_FILE, vector_config);
    }

    cm_builder.build().with_context(|_| BuildConfigMapSnafu {
        rolegroup: rolegroup_ref.clone(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use stackable_operator::kube::runtime::reflector::ObjectRef;

    use super::*;
    use crate::{
        controller::build::properties::test_support::validated_cluster_from_spec, crd::OpaRole,
    };

    /// Renders the ConfigMap of the `default` server role group of an `OpaCluster` built from `spec`.
    fn build_config_map(spec: Value) -> ConfigMap {
        let (opa, validated) = validated_cluster_from_spec(spec);

        let role = OpaRole::Server;
        let rg_config = &validated.role_group_configs[&role]["default"];
        let rolegroup_ref = RoleGroupRef {
            cluster: ObjectRef::from_obj(&opa),
            role: role.to_string(),
            role_group: "default".to_string(),
        };

        build_rolegroup_config_map(&validated, rg_config, &rolegroup_ref)
            .expect("the config map should build")
    }

    #[test]
    fn renders_config_json_without_user_info() {
        let cm = build_config_map(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));
        let data = cm.data.as_ref().expect("config map data");

        assert!(data.contains_key("config.json"));
        assert!(!data.contains_key("user-info-fetcher.json"));
    }

    #[test]
    fn renders_user_info_fetcher_json_when_configured() {
        let cm = build_config_map(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": {
                "userInfo": {
                    "backend": {
                        "experimentalXfscAas": {
                            "hostname": "aas.default.svc.cluster.local",
                            "port": 5000,
                        }
                    }
                }
            },
            "servers": { "roleGroups": { "default": {} } },
        }));
        let data = cm.data.as_ref().expect("config map data");

        assert!(data.contains_key("config.json"));
        assert!(data.contains_key("user-info-fetcher.json"));
    }
}
