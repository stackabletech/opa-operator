//! Builds the rolegroup [`DaemonSet`] that runs OPA on every node.

use stackable_operator::k8s_openapi::{
    api::apps::v1::{DaemonSet, DaemonSetSpec, DaemonSetUpdateStrategy, RollingUpdateDaemonSet},
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};

use super::*;

/// The rolegroup [`DaemonSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`](`stackable_operator::k8s_openapi::api::core::v1::Service`) (from
/// [`build_server_role_service`](super::super::service::build_server_role_service)).
///
/// We run an OPA on each node, because we want to avoid requiring network roundtrips for services making
/// policy queries (which are often chained in serial, and block other tasks in the products).
#[allow(clippy::too_many_arguments)]
pub fn build_server_rolegroup_daemonset(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    role_group: &OpaRoleGroupConfig,
    opa_bundle_builder_image: &str,
    user_info_fetcher_image: &str,
    resource_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<DaemonSet> {
    let pod_template = build_server_rolegroup_pod_template(
        cluster,
        role_group_name,
        role_group,
        opa_bundle_builder_image,
        user_info_fetcher_image,
        resource_info_fetcher_image,
        cluster_info,
    )?;

    let metadata = build::object_meta(
        cluster,
        cluster
            .role_group_resource_names(role_group_name)
            .daemon_set_name()
            .to_string(),
        role_group_name,
    )
    .build();

    let daemonset_spec = DaemonSetSpec {
        selector: LabelSelector {
            match_labels: Some(cluster.role_group_selector(role_group_name).into()),
            ..LabelSelector::default()
        },
        template: pod_template,
        update_strategy: Some(DaemonSetUpdateStrategy {
            type_: Some("RollingUpdate".to_string()),
            rolling_update: Some(RollingUpdateDaemonSet {
                max_surge: Some(IntOrString::Int(1)),
                max_unavailable: Some(IntOrString::Int(0)),
            }),
        }),
        ..DaemonSetSpec::default()
    };

    Ok(DaemonSet {
        metadata,
        spec: Some(daemonset_spec),
        status: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stackable_operator::{
        commons::networking::DomainName, k8s_openapi::api::core::v1::Container,
    };

    use stackable_opa_operator::crd::OpaRole;

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    fn build(cluster: &ValidatedCluster) -> DaemonSet {
        let (role_group_name, role_group) = cluster.role_group_configs[&OpaRole::Server]
            .iter()
            .next()
            .expect("the default role group should exist");
        build_server_rolegroup_daemonset(
            cluster,
            role_group_name,
            role_group,
            "bundle-builder-image",
            "user-info-fetcher-image",
            "resource-info-fetcher-image",
            &cluster_info(),
        )
        .expect("the daemonset should build")
    }

    fn container_names(ds: &DaemonSet) -> Vec<String> {
        ds.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    fn volume_names(ds: &DaemonSet) -> Vec<String> {
        ds.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.name.clone())
            .collect()
    }

    #[test]
    fn daemonset_has_expected_name_and_rolling_update_strategy() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        })));

        assert_eq!(ds.metadata.name.as_deref(), Some("test-opa-server-default"));
        let strategy = ds.spec.as_ref().unwrap().update_strategy.as_ref().unwrap();
        assert_eq!(strategy.type_.as_deref(), Some("RollingUpdate"));
        let rolling_update = strategy.rolling_update.as_ref().unwrap();
        // A DaemonSet must never take an OPA pod down before the replacement is ready.
        assert_eq!(rolling_update.max_unavailable, Some(IntOrString::Int(0)));
    }

    #[test]
    fn daemonset_runs_opa_and_bundle_builder_with_prepare_init_container() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        })));

        let containers = container_names(&ds);
        assert!(containers.contains(&"opa".to_owned()));
        assert!(containers.contains(&"bundle-builder".to_owned()));
        // No sidecars without the corresponding cluster config.
        assert!(!containers.contains(&"user-info-fetcher".to_owned()));
        assert!(!containers.contains(&"vector".to_owned()));

        let pod_spec = ds.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let init_containers: Vec<_> = pod_spec
            .init_containers
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(init_containers, vec!["prepare".to_owned()]);

        // The standard volumes are always present; the TLS volume is not (no TLS configured).
        let volumes = volume_names(&ds);
        for expected in ["config", "bundles", "log"] {
            assert!(
                volumes.contains(&expected.to_owned()),
                "missing volume {expected}"
            );
        }
        assert!(!volumes.contains(&"tls".to_owned()));
    }

    #[test]
    fn daemonset_adds_vector_container_when_agent_enabled() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": { "vectorAggregatorConfigMapName": "vector-aggregator-discovery" },
            "servers": {
                "config": { "logging": { "enableVectorAgent": true } },
                "roleGroups": { "default": {} },
            },
        })));

        assert!(container_names(&ds).contains(&"vector".to_owned()));
    }

    #[test]
    fn daemonset_adds_user_info_fetcher_container_when_configured() {
        let ds = build(&validated_cluster_from_spec(json!({
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
        })));

        assert!(container_names(&ds).contains(&"user-info-fetcher".to_owned()));
    }

    #[test]
    fn opa_probes_root_and_bundle_builder_probes_status() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        })));
        let pod_spec = ds.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let liveness_path = |container: &str| -> String {
            pod_spec
                .containers
                .iter()
                .find(|c| c.name == container)
                .unwrap_or_else(|| panic!("container {container} should exist"))
                .liveness_probe
                .as_ref()
                .unwrap()
                .http_get
                .as_ref()
                .unwrap()
                .path
                .clone()
                .unwrap()
        };
        // OPA's HTTP server answers `/`; only the bundle-builder exposes `/status`. A wrong path
        // here makes the liveness probe fail and the OPA container CrashLoop.
        assert_eq!(liveness_path("opa"), "/");
        assert_eq!(liveness_path("bundle-builder"), "/status");
    }

    #[test]
    fn daemonset_adds_tls_volume_when_tls_enabled() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": { "tls": { "serverSecretClass": "tls" } },
            "servers": { "roleGroups": { "default": {} } },
        })));

        assert!(volume_names(&ds).contains(&"tls".to_owned()));
    }

    #[test]
    fn opa_container_serves_https_when_tls_enabled() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": { "tls": { "serverSecretClass": "tls" } },
            "servers": { "roleGroups": { "default": {} } },
        })));
        let pod_spec = ds.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let opa = pod_spec
            .containers
            .iter()
            .find(|c| c.name == "opa")
            .expect("opa container should exist");

        // The single container port is the HTTPS data port.
        let ports = opa.ports.as_ref().unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].name.as_deref(), Some("https"));
        assert_eq!(ports[0].container_port, 8443);

        // The probe must speak HTTPS, otherwise it would fail against the TLS-only server.
        let scheme = opa
            .liveness_probe
            .as_ref()
            .unwrap()
            .http_get
            .as_ref()
            .unwrap()
            .scheme
            .clone();
        assert_eq!(scheme.as_deref(), Some("HTTPS"));

        // The start command binds the HTTPS port and passes the TLS cert/key flags.
        let args = opa.args.as_ref().unwrap();
        assert!(args[0].contains("-a 0.0.0.0:8443"));
        assert!(args[0].contains("--tls-cert-file"));
    }

    #[test]
    fn bundle_builder_start_command_silences_console_only_when_none() {
        let role_group_config = |spec: serde_json::Value| {
            let cluster = validated_cluster_from_spec(spec);
            cluster.role_group_configs[&OpaRole::Server]
                .values()
                .next()
                .expect("the default role group should exist")
                .config
                .clone()
        };

        // Console level NONE redirects bundle-builder output to /dev/null (no `tee`).
        let silenced = role_group_config(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "config": { "logging": { "containers": {
                    "bundle-builder": { "console": { "level": "NONE" } }
                } } },
                "roleGroups": { "default": {} },
            },
        }));
        // The redirect is appended directly after the bundle-builder invocation. (`/dev/null` also
        // appears in the shared bash trap helpers, so match the specific redirect.)
        assert!(
            build_bundle_builder_start_command(&silenced, "bundle-builder")
                .contains("stackable-opa-bundle-builder > /dev/null")
        );

        // With a console level above NONE, output is not discarded.
        let logging = role_group_config(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": {
                "config": { "logging": { "containers": {
                    "bundle-builder": { "console": { "level": "INFO" } }
                } } },
                "roleGroups": { "default": {} },
            },
        }));
        assert!(
            build_bundle_builder_start_command(&logging, "bundle-builder")
                .contains("stackable-opa-bundle-builder &")
        );
    }

    fn uif_container(ds: &DaemonSet) -> Container {
        ds.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers
            .iter()
            .find(|c| c.name == "user-info-fetcher")
            .expect("the user-info-fetcher container should exist")
            .clone()
    }

    fn env_var(container: &Container, name: &str) -> String {
        container
            .env
            .as_ref()
            .expect("the container should have env vars")
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("env var {name} should be set"))
            .value
            .clone()
            .unwrap_or_else(|| panic!("env var {name} should have a literal value"))
    }

    fn mount_path(container: &Container, volume_name: &str) -> String {
        container
            .volume_mounts
            .as_ref()
            .expect("the container should have volume mounts")
            .iter()
            .find(|m| m.name == volume_name)
            .unwrap_or_else(|| panic!("volume mount {volume_name} should exist"))
            .mount_path
            .clone()
    }

    #[test]
    fn user_info_fetcher_container_has_expected_command_and_config_wiring() {
        let ds = build(&validated_cluster_from_spec(json!({
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
        })));

        let uif = uif_container(&ds);
        assert_eq!(
            uif.command,
            Some(vec!["stackable-opa-user-info-fetcher".to_owned()])
        );
        // The sidecar reads its config from the shared config volume, and looks for backend
        // credentials in a fixed directory (populated by the backend-specific arms below).
        assert_eq!(
            env_var(&uif, "CONFIG"),
            "/stackable/config/user-info-fetcher.json"
        );
        assert_eq!(env_var(&uif, "CREDENTIALS_DIR"), "/stackable/credentials");
        assert_eq!(mount_path(&uif, "config"), "/stackable/config");
    }

    #[test]
    fn user_info_fetcher_active_directory_backend_mounts_kerberos_and_sets_krb5_env() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": {
                "userInfo": {
                    "backend": {
                        "experimentalActiveDirectory": {
                            "ldapServer": "ad.example.com",
                            "baseDistinguishedName": "dc=example,dc=com",
                            "kerberosSecretClassName": "kerberos",
                        }
                    }
                }
            },
            "servers": { "roleGroups": { "default": {} } },
        })));

        // A Kerberos secret volume is provisioned and mounted for the sidecar.
        assert!(volume_names(&ds).contains(&"kerberos".to_owned()));
        let uif = uif_container(&ds);
        assert_eq!(mount_path(&uif, "kerberos"), "/stackable/kerberos");
        // The krb5 client must find the config and keytab, and keep tickets in memory only.
        assert_eq!(
            env_var(&uif, "KRB5_CONFIG"),
            "/stackable/kerberos/krb5.conf"
        );
        assert_eq!(
            env_var(&uif, "KRB5_CLIENT_KTNAME"),
            "/stackable/kerberos/keytab"
        );
        assert_eq!(env_var(&uif, "KRB5CCNAME"), "MEMORY:");
    }

    #[test]
    fn user_info_fetcher_keycloak_backend_mounts_client_credentials_secret() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": {
                "userInfo": {
                    "backend": {
                        "keycloak": {
                            "hostname": "keycloak.example.com",
                            "clientCredentialsSecret": "keycloak-credentials",
                            "adminRealm": "master",
                            "userRealm": "my-realm",
                        }
                    }
                }
            },
            "servers": { "roleGroups": { "default": {} } },
        })));

        // The client credentials secret is projected into the sidecar's credentials dir.
        assert!(volume_names(&ds).contains(&"user-info-fetcher-credentials".to_owned()));
        assert_eq!(
            mount_path(&uif_container(&ds), "user-info-fetcher-credentials"),
            "/stackable/credentials"
        );
    }
}
