//! Assembles the rolegroup [`ConfigMap`] from the [`ValidatedCluster`], dispatching to the
//! per-file builders in [`crate::controller::build::properties`].

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    product_logging::framework::VECTOR_CONFIG_FILE,
    v2::builder::meta::ownerreference_from_resource,
};

use crate::controller::{
    OpaRoleGroupConfig, RoleGroupName, ValidatedCluster,
    build::properties::{ConfigFileName, config_json, user_info_fetcher},
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build config.json"))]
    BuildConfigJson { source: config_json::Error },

    #[snafu(display("failed to build user-info-fetcher.json"))]
    BuildUserInfoFetcher { source: user_info_fetcher::Error },

    #[snafu(display("failed to assemble ConfigMap for role group {role_group}"))]
    Assemble {
        source: stackable_operator::builder::configmap::Error,
        role_group: RoleGroupName,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the
/// administrator.
///
/// `vector_config` is the Vector agent config (`vector.yaml`) built by the caller; it is `None`
/// when the Vector agent is disabled.
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    rolegroup_config: &OpaRoleGroupConfig,
    vector_config: Option<String>,
) -> Result<ConfigMap> {
    let mut cm_builder = ConfigMapBuilder::new();

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .name(
            cluster
                .resource_names(role_group_name)
                .role_group_config_map()
                .to_string(),
        )
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(cluster.recommended_labels(role_group_name))
        .build();

    cm_builder.metadata(metadata).add_data(
        ConfigFileName::ConfigJson.to_string(),
        config_json::build(&rolegroup_config.config, &rolegroup_config.config_overrides)
            .context(BuildConfigJsonSnafu)?,
    );

    if let Some(user_info) = &cluster.cluster_config.user_info {
        cm_builder.add_data(
            ConfigFileName::UserInfoFetcher.to_string(),
            user_info_fetcher::build(user_info).context(BuildUserInfoFetcherSnafu)?,
        );
    }

    if let Some(vector_config) = vector_config {
        cm_builder.add_data(VECTOR_CONFIG_FILE, vector_config);
    }

    cm_builder.build().with_context(|_| AssembleSnafu {
        role_group: role_group_name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        controller::build::properties::test_support::validated_cluster_from_spec, crd::OpaRole,
    };

    /// Renders the ConfigMap of the `default` server role group of an `OpaCluster` built from `spec`.
    fn build_config_map(spec: Value) -> ConfigMap {
        let validated = validated_cluster_from_spec(spec);

        let role = OpaRole::Server;
        let (role_group_name, rg) = validated.role_group_configs[&role]
            .iter()
            .next()
            .expect("the default role group should exist");

        build_rolegroup_config_map(&validated, role_group_name, rg, None)
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
