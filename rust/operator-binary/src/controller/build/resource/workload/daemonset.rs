//! Builds the rolegroup [`DaemonSet`] that runs OPA on every node.

use stackable_operator::k8s_openapi::{
    api::apps::v1::{DaemonSet, DaemonSetSpec, DaemonSetUpdateStrategy, RollingUpdateDaemonSet},
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};

// The shared building blocks (constants, `Error`, the Pod template, the start-command helpers) all
// live in the parent module; a glob keeps this wrapper from restating the whole list.
use super::*;
use crate::controller::build::role_group_selector;

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
        recommended_labels_for_role_group_resources(cluster, &OpaRole::Server, role_group_name),
    )
    .build();

    let daemonset_spec = DaemonSetSpec {
        selector: LabelSelector {
            match_labels: Some(
                role_group_selector(cluster, &OpaRole::Server, role_group_name).into(),
            ),
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
    use stackable_opa_operator::crd::OpaRole;
    use stackable_operator::{
        commons::networking::DomainName, k8s_openapi::api::core::v1::Container,
    };

    use super::*;
    use crate::controller::build::properties::test_support::validated_cluster_from_spec;

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::try_from("cluster.local").unwrap(),
        }
    }

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *CONFIG_VOLUME_NAME;
        let _ = *LOG_VOLUME_NAME;
        let _ = *BUNDLES_VOLUME_NAME;
        let _ = *USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME;
        let _ = *USER_INFO_FETCHER_KERBEROS_VOLUME_NAME;
        let _ = *TLS_VOLUME_NAME;
        let _ = *CONTAINERDEBUG_LOG_DIRECTORY;
        let _ = *WATCH_NAMESPACE;
        let _ = *CONSOLE_LOG_LEVEL;
        let _ = *FILE_LOG_LEVEL;
        let _ = *FILE_LOG_DIRECTORY;
        let _ = *FILE_LOG_ROTATION_PERIOD;
        let _ = *FILE_LOG_MAX_FILES;
        let _ = *KUBERNETES_NODE_NAME;
        let _ = *KUBERNETES_CLUSTER_DOMAIN;
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

    fn container_by_name(ds: &DaemonSet, name: &str) -> Container {
        ds.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("the {name} container should exist"))
            .clone()
    }

    fn uif_container(ds: &DaemonSet) -> Container {
        container_by_name(ds, "user-info-fetcher")
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
        // The credential cache lives in memory (`KRB5CCNAME`), so nothing is ever written into the
        // kerberos directory. Only the krb5.conf and keytab above are read from it.
        assert_eq!(read_only(&uif, "kerberos"), Some(true));
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
        // The sidecar only reads the secret.
        assert_eq!(
            read_only(&uif_container(&ds), "user-info-fetcher-credentials"),
            Some(true)
        );
    }

    /// A cluster running both info-fetcher sidecars, so their shared wiring can be asserted in one go.
    fn cluster_with_both_info_fetchers() -> ValidatedCluster {
        validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "clusterConfig": {
                "userInfo": {
                    "backend": {
                        "experimentalXfscAas": {
                            "hostname": "aas.default.svc.cluster.local",
                            "port": 5000,
                        }
                    }
                },
                "resourceInfo": {
                    "backend": {
                        "dataHub": {
                            "hostname": "datahub-gms.default.svc.cluster.local",
                            "credentialsSecretName": "datahub-credentials",
                        }
                    }
                },
            },
            "servers": { "roleGroups": { "default": {} } },
        }))
    }

    fn log_volume_size_limit(ds: &DaemonSet) -> Quantity {
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
            .find(|volume| volume.name == LOG_VOLUME_NAME.as_ref())
            .expect("the log volume should exist")
            .empty_dir
            .as_ref()
            .expect("the log volume should be an emptyDir")
            .size_limit
            .clone()
            .expect("the log volume should have a size limit")
    }

    /// Whether `container` mounts `volume` read-only.
    fn read_only(container: &Container, volume: &str) -> Option<bool> {
        container
            .volume_mounts
            .as_ref()
            .expect("the container should have volume mounts")
            .iter()
            .find(|mount| mount.name == volume)
            .unwrap_or_else(|| panic!("volume mount {volume} should exist"))
            .read_only
    }

    /// Both sidecars write their file logs below `STACKABLE_LOG_DIR`, so they have to mount the `log`
    /// volume. Otherwise the logs land in the container's own filesystem, where the Vector agent
    /// (which only reads the shared volume) cannot see them.
    #[test]
    fn info_fetcher_sidecars_mount_the_log_volume() {
        let ds = build(&cluster_with_both_info_fetchers());

        for container_name in ["user-info-fetcher", "resource-info-fetcher"] {
            let container = container_by_name(&ds, container_name);
            assert_eq!(
                mount_path(&container, "log"),
                "/stackable/log",
                "{container_name} should mount the log volume"
            );
            // The directory the sidecar logs into must be inside the mounted volume.
            assert_eq!(
                env_var(&container, "FILE_LOG_DIRECTORY"),
                format!("/stackable/log/{container_name}")
            );
        }
    }

    /// The sidecars only read their config and credentials, so those mounts must not be writable.
    /// The log volume must stay writable, because that is where they write their file logs.
    #[test]
    fn info_fetcher_sidecars_mount_config_and_credentials_read_only() {
        let ds = build(&cluster_with_both_info_fetchers());

        for container_name in ["user-info-fetcher", "resource-info-fetcher"] {
            let container = container_by_name(&ds, container_name);

            assert_eq!(
                read_only(&container, "config"),
                Some(true),
                "{container_name} should mount its config read-only"
            );
            assert_ne!(
                read_only(&container, "log"),
                Some(true),
                "{container_name} has to be able to write its file logs"
            );
        }

        // Only the resource-info-fetcher has a credentials volume in this fixture; the
        // user-info-fetcher's is covered by the Keycloak backend test above, as the xfsc-aas backend
        // used here needs no credentials.
        assert_eq!(
            read_only(
                &container_by_name(&ds, "resource-info-fetcher"),
                "resource-info-fetcher-credentials"
            ),
            Some(true)
        );
    }

    /// Nothing else bounds these files. Vector only reads them, and the products' log frameworks
    /// (which operator-rs configures with a size-based rotating appender) are not involved here, so
    /// the writer has to be told to roll them over.
    ///
    /// The period is asserted because it is what caps the damage during a sustained backend outage,
    /// when the info-fetchers log one line per failed request.
    #[test]
    fn the_rust_containers_rotate_their_file_logs() {
        let ds = build(&cluster_with_both_info_fetchers());

        for container in [
            "bundle-builder",
            "user-info-fetcher",
            "resource-info-fetcher",
        ] {
            let container = container_by_name(&ds, container);
            assert_eq!(env_var(&container, "FILE_LOG_ROTATION_PERIOD"), "minutely");
            assert_eq!(env_var(&container, "FILE_LOG_MAX_FILES"), "5");
        }
    }

    /// The sidecars share the `log` volume with the other containers, so their log files have to be
    /// budgeted for in its size limit as well.
    #[test]
    fn log_volume_size_limit_accounts_for_the_info_fetcher_sidecars() {
        let without_sidecars = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        })));
        let with_sidecars = build(&cluster_with_both_info_fetchers());

        // prepare + opa + bundle-builder
        assert_eq!(
            log_volume_size_limit(&without_sidecars),
            Quantity("108Mi".to_owned())
        );
        // ... plus the two info-fetcher sidecars
        assert_eq!(
            log_volume_size_limit(&with_sidecars),
            Quantity("138Mi".to_owned())
        );
    }

    /// The bundle-builder sidecar must carry the Stackable Rust CLI environment variables and
    /// `WATCH_NAMESPACE`, each exactly once.
    #[test]
    fn bundle_builder_has_cli_env_vars_exactly_once() {
        let ds = build(&validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        })));

        let env = ds
            .spec
            .expect("the DaemonSet has a spec")
            .template
            .spec
            .expect("the pod template has a spec")
            .containers
            .into_iter()
            .find(|container| container.name == "bundle-builder")
            .expect("the bundle-builder container exists")
            .env
            .expect("the bundle-builder container has env vars");

        for name in [
            "WATCH_NAMESPACE",
            "CONSOLE_LOG_LEVEL",
            "FILE_LOG_LEVEL",
            "FILE_LOG_DIRECTORY",
            "KUBERNETES_NODE_NAME",
            "KUBERNETES_CLUSTER_DOMAIN",
        ] {
            assert_eq!(
                env.iter().filter(|env_var| env_var.name == name).count(),
                1,
                "the env var {name} should be set exactly once"
            );
        }

        let cluster_domain = env
            .iter()
            .find(|env_var| env_var.name == "KUBERNETES_CLUSTER_DOMAIN")
            .expect("KUBERNETES_CLUSTER_DOMAIN is set");
        assert_eq!(cluster_domain.value.as_deref(), Some("cluster.local"));
    }

    /// The user-supplied `envOverrides` must be merged in after all operator-set environment
    /// variables, so that they can override any of them. `CONTAINERDEBUG_LOG_DIRECTORY` is used
    /// as the example here because it is set unconditionally by the operator.
    #[test]
    fn env_overrides_override_operator_set_env_vars() {
        use std::str::FromStr;

        use stackable_operator::v2::builder::pod::container::EnvVarName;

        let cluster = validated_cluster_from_spec(json!({
            "image": { "productVersion": "1.2.3" },
            "servers": { "roleGroups": { "default": {} } },
        }));
        let (role_group_name, role_group) = cluster.role_group_configs[&OpaRole::Server]
            .iter()
            .next()
            .expect("the default role group should exist");
        let mut role_group = role_group.clone();
        role_group.env_overrides = EnvVarSet::new().with_value(
            &EnvVarName::from_str("CONTAINERDEBUG_LOG_DIRECTORY").expect("valid env var name"),
            "/custom/log/dir",
        );

        let ds = build_server_rolegroup_daemonset(
            &cluster,
            role_group_name,
            &role_group,
            "bundle-builder-image",
            "user-info-fetcher-image",
            "resource-info-fetcher-image",
            &cluster_info(),
        )
        .expect("the daemonset should build");

        let env = ds
            .spec
            .expect("the DaemonSet has a spec")
            .template
            .spec
            .expect("the pod template has a spec")
            .containers
            .into_iter()
            .find(|container| container.name == "opa")
            .expect("the opa container exists")
            .env
            .expect("the opa container has env vars");

        let containerdebug: Vec<_> = env
            .iter()
            .filter(|env_var| env_var.name == "CONTAINERDEBUG_LOG_DIRECTORY")
            .collect();
        assert_eq!(
            containerdebug.len(),
            1,
            "the override must replace the operator-set value, not duplicate it"
        );
        assert_eq!(containerdebug[0].value.as_deref(), Some("/custom/log/dir"));
    }
}
