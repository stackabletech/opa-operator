//! Building blocks shared by the rolegroup workload objects that run OPA (plus its
//! bundle-builder, optional user-info-fetcher, and Vector sidecars).
//!
//! The Pod template is identical regardless of how the rolegroup is deployed, so it is built here
//! and wrapped by the workload-specific submodules ([`daemonset`], [`deployment`]).

use std::{collections::BTreeMap, str::FromStr};

use indoc::formatdoc;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{Container, DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT};
use stackable_operator::{
    builder::{
        self,
        meta::ObjectMetaBuilder,
        pod::{
            PodBuilder,
            container::{ContainerBuilder, FieldPathEnvVar},
            resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder,
            volume::{SecretOperatorVolumeSourceBuilder, VolumeBuilder},
        },
    },
    commons::secret_class::SecretClassVolumeProvisionParts,
    k8s_openapi::{
        DeepMerge,
        api::core::v1::{
            EmptyDirVolumeSource, EnvVarSource, HTTPGetAction, ObjectFieldSelector,
            PodTemplateSpec, Probe, ResourceRequirements,
        },
        apimachinery::pkg::util::intstr::IntOrString,
    },
    memory::{BinaryMultiple, MemoryQuantity},
    product_logging::{
        self,
        framework::{create_vector_shutdown_file_command, remove_vector_shutdown_file_command},
        spec::{AppenderConfig, AutomaticContainerLogConfig, LogLevel},
    },
    utils::{COMMON_BASH_TRAP_FUNCTIONS, cluster_info::KubernetesClusterInfo},
    v2::{
        builder::pod::container::{EnvVarSet, new_container_builder},
        product_logging::framework::{
            STACKABLE_LOG_DIR, ValidatedContainerLogConfigChoice, vector_container,
        },
        types::kubernetes::{ContainerName, VolumeName},
    },
};

use super::service::{self, APP_PORT, APP_PORT_NAME};
use crate::{
    controller::{
        OpaRoleGroupConfig, RoleGroupName, ValidatedCluster, ValidatedOpaConfig,
        build::{
            self,
            resource::workload::{
                resource_info_fetcher::add_resource_info_fetcher_sidecar,
                user_info_fetcher::add_user_info_fetcher_sidecar,
            },
        },
    },
    operations::graceful_shutdown::add_graceful_shutdown_config,
};

pub mod daemonset;
pub mod deployment;
mod resource_info_fetcher;
mod user_info_fetcher;

pub const BUNDLES_ACTIVE_DIR: &str = "/bundles/active";
pub const BUNDLES_INCOMING_DIR: &str = "/bundles/incoming";
pub const BUNDLES_TMP_DIR: &str = "/bundles/tmp";

stackable_operator::constant!(CONFIG_VOLUME_NAME: VolumeName = "config");
const CONFIG_DIR: &str = "/stackable/config";
stackable_operator::constant!(LOG_VOLUME_NAME: VolumeName = "log");
stackable_operator::constant!(BUNDLES_VOLUME_NAME: VolumeName = "bundles");
const BUNDLES_DIR: &str = "/bundles";
stackable_operator::constant!(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME: VolumeName = "user-info-fetcher-credentials");
// As UIF and RIF run in two different containers, the directories won't clash
const USER_INFO_FETCHER_CREDENTIALS_DIR: &str = "/stackable/credentials";
stackable_operator::constant!(USER_INFO_FETCHER_KERBEROS_VOLUME_NAME: VolumeName = "kerberos");
const USER_INFO_FETCHER_KERBEROS_DIR: &str = "/stackable/kerberos";
stackable_operator::constant!(RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME: VolumeName = "resource-info-fetcher-credentials");
const RESOURCE_INFO_FETCHER_CREDENTIALS_DIR: &str = "/stackable/credentials";
stackable_operator::constant!(TLS_VOLUME_NAME: VolumeName = "tls");
const TLS_STORE_DIR: &str = "/stackable/tls";

// HTTP probe configuration shared by the bundle-builder and OPA containers. They differ in the
// probed path (the bundle-builder exposes `/status`, OPA's HTTP server answers `/`), the port and,
// for OPA, the URI scheme.
const BUNDLE_BUILDER_PROBE_PATH: &str = "/status";
const OPA_PROBE_PATH: &str = "/";
const PROBE_PERIOD_SECONDS: i32 = 10;
const READINESS_PROBE_INITIAL_DELAY_SECONDS: i32 = 5;
const READINESS_PROBE_FAILURE_THRESHOLD: i32 = 5;
const LIVENESS_PROBE_INITIAL_DELAY_SECONDS: i32 = 30;

const CONSOLE_LOG_LEVEL_ENV: &str = "CONSOLE_LOG_LEVEL";
const FILE_LOG_LEVEL_ENV: &str = "FILE_LOG_LEVEL";
const FILE_LOG_DIRECTORY_ENV: &str = "FILE_LOG_DIRECTORY";
const KUBERNETES_NODE_NAME_ENV: &str = "KUBERNETES_NODE_NAME";
const KUBERNETES_CLUSTER_DOMAIN_ENV: &str = "KUBERNETES_CLUSTER_DOMAIN";

// logging defaults
const DEFAULT_FILE_LOG_LEVEL: LogLevel = LogLevel::INFO;
const DEFAULT_CONSOLE_LOG_LEVEL: LogLevel = LogLevel::INFO;
const DEFAULT_SERVER_LOG_LEVEL: LogLevel = LogLevel::INFO;
const DEFAULT_DECISION_LOG_LEVEL: LogLevel = LogLevel::NONE;

// Bundle builder: ~ 5 MB x 5
// These sizes are needed both for the single file (for rotation, in bytes) as well as the total (for the EmptyDir).
//
// Ideally, we would rotate the logs by size, but this is currently not supported due to upstream issues.
// Please see https://github.com/stackabletech/opa-operator/issues/606 for more details.
const OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_MB: u32 = 5;
const OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES: u32 = 5;
const MAX_OPA_BUNDLE_BUILDER_LOG_FILE_SIZE: MemoryQuantity = MemoryQuantity {
    value: (OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_MB * OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES)
        as f32,
    unit: BinaryMultiple::Mebi,
};
// OPA logs: ~ 5 MB x 2
// These sizes are needed both for the single file (for multilog, in bytes) as well as the total (for the EmptyDir).
const OPA_ROLLING_LOG_FILE_SIZE_MB: u32 = 5;
const OPA_ROLLING_LOG_FILE_SIZE_BYTES: u32 = OPA_ROLLING_LOG_FILE_SIZE_MB * 1000000;
const OPA_ROLLING_LOG_FILES: u32 = 2;
const MAX_OPA_LOG_FILE_SIZE: MemoryQuantity = MemoryQuantity {
    value: (OPA_ROLLING_LOG_FILE_SIZE_MB * OPA_ROLLING_LOG_FILES) as f32,
    unit: BinaryMultiple::Mebi,
};

// ~ 1 MB
const MAX_PREPARE_LOG_FILE_SIZE: MemoryQuantity = MemoryQuantity {
    value: 1.0,
    unit: BinaryMultiple::Mebi,
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to configure graceful shutdown"))]
    GracefulShutdown {
        source: crate::operations::graceful_shutdown::Error,
    },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },

    #[snafu(display("failed to build TLS volume"))]
    TlsVolumeBuild {
        source: builder::pod::volume::SecretOperatorVolumeSourceBuilderError,
    },

    #[snafu(display("failed to build User Info Fetcher sidecar"))]
    BuildUserInfoFetcherSidecar { source: user_info_fetcher::Error },

    #[snafu(display("failed to build Resource Info Fetcher sidecar"))]
    BuildResourceInfoFetcherSidecar {
        source: resource_info_fetcher::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// The typed [`ContainerName`] for a [`Container`]. The enum's `Display` values are all valid
/// container names, so this conversion is infallible.
fn container_name(container: &Container) -> ContainerName {
    ContainerName::from_str(&container.to_string())
        .expect("Container enum variants are valid container names")
}

/// The CPU and memory requests/limits shared by the bundle-builder and user-info-fetcher sidecars.
fn sidecar_resource_requirements() -> ResourceRequirements {
    ResourceRequirementsBuilder::new()
        .with_cpu_request("100m")
        .with_cpu_limit("200m")
        .with_memory_request("128Mi")
        .with_memory_limit("128Mi")
        .build()
}

/// The strict-mode `bash` entrypoint shared by the prepare, bundle-builder, and OPA containers.
/// The actual script is passed via `.args(...)`.
fn bash_entrypoint_command() -> Vec<String> {
    ["/bin/bash", "-x", "-euo", "pipefail", "-c"]
        .iter()
        .map(|arg| arg.to_string())
        .collect()
}

/// An HTTP readiness [`Probe`] against `path` on the given `port`/`scheme`.
fn http_readiness_probe(path: &str, port: IntOrString, scheme: Option<String>) -> Probe {
    Probe {
        initial_delay_seconds: Some(READINESS_PROBE_INITIAL_DELAY_SECONDS),
        period_seconds: Some(PROBE_PERIOD_SECONDS),
        failure_threshold: Some(READINESS_PROBE_FAILURE_THRESHOLD),
        http_get: Some(HTTPGetAction {
            port,
            path: Some(path.to_string()),
            scheme,
            ..HTTPGetAction::default()
        }),
        ..Probe::default()
    }
}

/// An HTTP liveness [`Probe`] against `path` on the given `port`/`scheme`.
fn http_liveness_probe(path: &str, port: IntOrString, scheme: Option<String>) -> Probe {
    Probe {
        initial_delay_seconds: Some(LIVENESS_PROBE_INITIAL_DELAY_SECONDS),
        period_seconds: Some(PROBE_PERIOD_SECONDS),
        http_get: Some(HTTPGetAction {
            port,
            path: Some(path.to_string()),
            scheme,
            ..HTTPGetAction::default()
        }),
        ..Probe::default()
    }
}

/// Builds the [`PodTemplateSpec`] for a rolegroup, shared by every deployment mode.
///
/// The template carries the `prepare` init container, the OPA and bundle-builder containers, the
/// optional user-info-fetcher and Vector sidecars, and all volumes they mount. Callers wrap it in
/// the workload object of their choice; see [`daemonset::build_server_rolegroup_daemonset`] and
/// [`deployment::build_server_rolegroup_deployment`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_server_rolegroup_pod_template(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    role_group: &OpaRoleGroupConfig,
    opa_bundle_builder_image: &str,
    user_info_fetcher_image: &str,
    resource_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<PodTemplateSpec> {
    let resolved_product_image = &cluster.image;
    let rolegroup_config = role_group;
    // All overrides were already merged (role group over role over defaults) in the validate step.
    let merged_config = &rolegroup_config.config;

    let mut pb = PodBuilder::new();

    let prepare_container_name = container_name(&Container::Prepare);
    let mut cb_prepare = new_container_builder(&prepare_container_name);

    let bundle_builder_container_name = container_name(&Container::BundleBuilder);
    let mut cb_bundle_builder = new_container_builder(&bundle_builder_container_name);

    let opa_container_name = container_name(&Container::Opa);
    let mut cb_opa = new_container_builder(&opa_container_name);

    cb_prepare
        .image_from_product_image(resolved_product_image)
        .command(bash_entrypoint_command())
        .args(vec![
            build_prepare_start_command(merged_config, prepare_container_name.as_ref())
                .join(" && "),
        ])
        .add_volume_mount(BUNDLES_VOLUME_NAME.as_ref(), BUNDLES_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME.as_ref(), STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .resources(merged_config.resources.to_owned().into());

    cb_bundle_builder
        .image_from_product_image(resolved_product_image) // inherit the pull policy and pull secrets, and then...
        .image(opa_bundle_builder_image) // ...override the image
        .command(bash_entrypoint_command())
        .args(vec![build_bundle_builder_start_command(
            merged_config,
            bundle_builder_container_name.as_ref(),
        )])
        .add_env_var_from_field_path("WATCH_NAMESPACE", &FieldPathEnvVar::Namespace)
        .add_volume_mount(BUNDLES_VOLUME_NAME.as_ref(), BUNDLES_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME.as_ref(), STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .resources(sidecar_resource_requirements())
        .readiness_probe(http_readiness_probe(
            BUNDLE_BUILDER_PROBE_PATH,
            IntOrString::Int(build::BUNDLE_BUILDER_PORT.into()),
            None,
        ))
        .liveness_probe(http_liveness_probe(
            BUNDLE_BUILDER_PROBE_PATH,
            IntOrString::Int(build::BUNDLE_BUILDER_PORT.into()),
            None,
        ));
    add_stackable_rust_cli_env_vars(
        &mut cb_bundle_builder,
        cluster_info,
        sidecar_container_log_level(merged_config, &Container::BundleBuilder).to_string(),
        &Container::BundleBuilder,
    );

    cb_opa
        .image_from_product_image(resolved_product_image)
        .command(bash_entrypoint_command())
        .args(vec![build_opa_start_command(
            merged_config,
            opa_container_name.as_ref(),
            cluster.is_tls_enabled(),
            &rolegroup_config.cli_overrides,
        )])
        .add_env_vars(rolegroup_config.env_overrides.clone())
        .add_env_var(
            "CONTAINERDEBUG_LOG_DIRECTORY",
            format!("{STACKABLE_LOG_DIR}/containerdebug"),
        );

    // Add appropriate container port based on TLS configuration
    // If we also add a container port "metrics" pointing to the same port number, we get a
    //
    // .spec.template.spec.containers[name="opa"].ports: duplicate entries for key [containerPort=8081,protocol="TCP"]
    //
    // So we don't do that
    if cluster.is_tls_enabled() {
        cb_opa.add_container_port(service::APP_TLS_PORT_NAME, service::APP_TLS_PORT.into());
        cb_opa
            .add_volume_mount(TLS_VOLUME_NAME.as_ref(), TLS_STORE_DIR)
            .context(AddVolumeMountSnafu)?;
    } else {
        cb_opa.add_container_port(APP_PORT_NAME, APP_PORT.into());
    }

    cb_opa
        .add_volume_mount(CONFIG_VOLUME_NAME.as_ref(), CONFIG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME.as_ref(), STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .resources(merged_config.resources.to_owned().into());

    let (probe_port_name, probe_scheme) = if cluster.is_tls_enabled() {
        (service::APP_TLS_PORT_NAME, Some("HTTPS".to_string()))
    } else {
        (APP_PORT_NAME, Some("HTTP".to_string()))
    };

    cb_opa
        .readiness_probe(http_readiness_probe(
            OPA_PROBE_PATH,
            IntOrString::String(probe_port_name.to_string()),
            probe_scheme.clone(),
        ))
        .liveness_probe(http_liveness_probe(
            OPA_PROBE_PATH,
            IntOrString::String(probe_port_name.to_string()),
            probe_scheme,
        ));

    let pb_metadata = ObjectMetaBuilder::new()
        .with_labels(cluster.recommended_labels(role_group_name))
        .build();

    pb.metadata(pb_metadata)
        .add_init_container(cb_prepare.build())
        .add_container(cb_opa.build())
        .add_container(cb_bundle_builder.build())
        .image_pull_secrets_from_product_image(resolved_product_image)
        .affinity(&merged_config.affinity)
        .add_volume(
            VolumeBuilder::new(CONFIG_VOLUME_NAME.as_ref())
                .with_config_map(
                    cluster
                        .role_group_resource_names(role_group_name)
                        .role_group_config_map()
                        .to_string(),
                )
                .build(),
        )
        .context(AddVolumeSnafu)?
        .add_volume(
            VolumeBuilder::new(BUNDLES_VOLUME_NAME.as_ref())
                .with_empty_dir(None::<String>, None)
                .build(),
        )
        .context(AddVolumeSnafu)?
        .add_volume(
            VolumeBuilder::new(LOG_VOLUME_NAME.as_ref())
                .empty_dir(EmptyDirVolumeSource {
                    medium: None,
                    size_limit: Some(product_logging::framework::calculate_log_volume_size_limit(
                        &[
                            MAX_OPA_BUNDLE_BUILDER_LOG_FILE_SIZE,
                            MAX_OPA_LOG_FILE_SIZE,
                            MAX_PREPARE_LOG_FILE_SIZE,
                        ],
                    )),
                })
                .build(),
        )
        .context(AddVolumeSnafu)?
        .service_account_name(
            cluster
                .cluster_resource_names()
                .service_account_name()
                .to_string(),
        )
        .security_context(
            PodSecurityContextBuilder::with_stackable_defaults()
                .fs_group(1000)
                .build(),
        );

    if let Some(tls) = &cluster.cluster_config.tls {
        pb.add_volume(
            VolumeBuilder::new(TLS_VOLUME_NAME.as_ref())
                .ephemeral(
                    SecretOperatorVolumeSourceBuilder::new(
                        tls.server_secret_class.to_string(),
                        // OPA needs the full TLS keypair (public cert + private key) to serve HTTPS.
                        SecretClassVolumeProvisionParts::PublicPrivate,
                    )
                    .with_service_scope(cluster.server_role_service_name())
                    .with_service_scope(
                        cluster
                            .role_group_resource_names(role_group_name)
                            .headless_service_name()
                            .to_string(),
                    )
                    .with_service_scope(
                        cluster
                            .role_group_resource_names(role_group_name)
                            .metrics_service_name()
                            .to_string(),
                    )
                    .build()
                    .context(TlsVolumeBuildSnafu)?,
                )
                .build(),
        )
        .context(AddVolumeSnafu)?;
    }

    add_user_info_fetcher_sidecar(
        &mut pb,
        cluster,
        merged_config,
        user_info_fetcher_image,
        cluster_info,
    )
    .context(BuildUserInfoFetcherSidecarSnafu)?;
    add_resource_info_fetcher_sidecar(
        &mut pb,
        cluster,
        merged_config,
        resource_info_fetcher_image,
        cluster_info,
    )
    .context(BuildResourceInfoFetcherSidecarSnafu)?;

    // The Vector logging config was validated up-front (see `ValidatedLogging`); a `Some` here means
    // the Vector agent is enabled and the aggregator discovery ConfigMap name is valid.
    if let Some(vector_log_config) = &merged_config.logging.vector_container {
        pb.add_container(vector_container(
            &container_name(&Container::Vector),
            resolved_product_image,
            vector_log_config,
            &cluster.role_group_resource_names(role_group_name),
            &CONFIG_VOLUME_NAME,
            &LOG_VOLUME_NAME,
            EnvVarSet::new(),
        ));
    }

    add_graceful_shutdown_config(merged_config, &mut pb).context(GracefulShutdownSnafu)?;

    let mut pod_template = pb.build_template();
    pod_template.merge_from(rolegroup_config.pod_overrides.clone());

    Ok(pod_template)
}

/// Env variables that are need to run stackable Rust binaries, such as
/// * opa-bundle-builder
/// * user-info-fetcher
fn add_stackable_rust_cli_env_vars(
    container_builder: &mut ContainerBuilder,
    cluster_info: &KubernetesClusterInfo,
    log_level: impl Into<String>,
    container: &Container,
) {
    let log_level = log_level.into();
    container_builder
        .add_env_var(CONSOLE_LOG_LEVEL_ENV, log_level.clone())
        .add_env_var(FILE_LOG_LEVEL_ENV, log_level)
        .add_env_var(
            FILE_LOG_DIRECTORY_ENV,
            format!("{STACKABLE_LOG_DIR}/{container}",),
        )
        .add_env_var_from_source(
            KUBERNETES_NODE_NAME_ENV,
            EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "spec.nodeName".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        // We set the cluster domain always explicitly, because the product Pods does not have the
        // RBAC permission to get the `nodes/proxy` resource at cluster scope. This is likely
        // because it only has a RoleBinding and no ClusterRoleBinding.
        // By setting the cluster domain explicitly we avoid that the sidecars try to look it up
        // based on some information coming from the node.
        .add_env_var(
            KUBERNETES_CLUSTER_DOMAIN_ENV,
            cluster_info.cluster_domain.to_string(),
        );
}

fn build_opa_start_command(
    merged_config: &ValidatedOpaConfig,
    container_name: &str,
    tls_enabled: bool,
    cli_overrides: &BTreeMap<String, String>,
) -> String {
    let mut file_log_level = DEFAULT_FILE_LOG_LEVEL;
    let mut console_log_level = DEFAULT_CONSOLE_LOG_LEVEL;
    let mut server_log_level = DEFAULT_SERVER_LOG_LEVEL;
    let mut decision_log_level = DEFAULT_DECISION_LOG_LEVEL;

    if let Some(ValidatedContainerLogConfigChoice::Automatic(log_config)) =
        merged_config.logging.containers.get(&Container::Opa)
    {
        if let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.file
        {
            file_log_level = log_level;
        }

        if let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.console
        {
            console_log_level = log_level;
        }

        // Retrieve the decision log level for OPA. If not set, keep the defined default of LogLevel::NONE.
        // This is because, if decision logs are not explicitly set to something different than LogLevel::NONE,
        // the decision logs should remain disabled and not set to ROOT log level automatically.
        if let Some(config) = log_config.loggers.get("decision") {
            decision_log_level = config.level
        }

        // Retrieve the server log level for OPA. If not set, set it to the ROOT log level.
        match log_config.loggers.get("server") {
            Some(config) => server_log_level = config.level,
            None => server_log_level = log_config.root_log_level(),
        }
    }

    let (bind_port, tls_flags) = if tls_enabled {
        (
            service::APP_TLS_PORT,
            format!(
                "--tls-cert-file {TLS_STORE_DIR}/tls.crt --tls-private-key-file {TLS_STORE_DIR}/tls.key"
            ),
        )
    } else {
        (APP_PORT, String::new())
    };

    // Redirects matter!
    // We need to watch out, that the following "$!" call returns the PID of the main (opa-bundle-builder) process,
    // and not some utility (e.g. multilog or tee) process.
    // See https://stackoverflow.com/a/8048493

    let logging_redirects = format!(
        "&> >(CONSOLE_LEVEL={console_log_level} FILE_LEVEL={file_log_level} DECISION_LEVEL={decision_log_level} SERVER_LEVEL={server_log_level} OPA_ROLLING_LOG_FILE_SIZE_BYTES={OPA_ROLLING_LOG_FILE_SIZE_BYTES} OPA_ROLLING_LOG_FILES={OPA_ROLLING_LOG_FILES} STACKABLE_LOG_DIR={STACKABLE_LOG_DIR} CONTAINER_NAME={container_name} process-logs)"
    );

    let extra_cli_args = cli_overrides
        .iter()
        .map(|(key, value)| format!("{key} {value}"))
        .collect::<Vec<_>>()
        .join(" ");

    let config_file = build::properties::ConfigFileName::ConfigJson;

    // TODO: Think about adding --shutdown-wait-period, as suggested by https://github.com/open-policy-agent/opa/issues/2764
    formatdoc! {"
        {COMMON_BASH_TRAP_FUNCTIONS}
        {remove_vector_shutdown_file_command}
        prepare_signal_handlers
        containerdebug --output={STACKABLE_LOG_DIR}/containerdebug-state.json --loop &
        opa run -s -a 0.0.0.0:{bind_port} -c {CONFIG_DIR}/{config_file} -l {opa_log_level} --shutdown-grace-period {shutdown_grace_period_s} --disable-telemetry {tls_flags} {extra_cli_args} {logging_redirects} &
        wait_for_termination $!
        {create_vector_shutdown_file_command}
        ",
        remove_vector_shutdown_file_command =
            remove_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        create_vector_shutdown_file_command =
            create_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        shutdown_grace_period_s = merged_config.graceful_shutdown_timeout.unwrap_or(DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT).as_secs(),
        opa_log_level = [console_log_level, file_log_level].iter().min().unwrap_or(&LogLevel::INFO).to_opa_literal(),
        extra_cli_args = extra_cli_args
    }
}

fn build_bundle_builder_start_command(
    merged_config: &ValidatedOpaConfig,
    container_name: &str,
) -> String {
    let mut console_logging_off = false;

    // We need to check if the console logging is deactivated (NONE)
    // This will result in not using `tee` later on in the start command
    if let Some(ValidatedContainerLogConfigChoice::Automatic(log_config)) = merged_config
        .logging
        .containers
        .get(&Container::BundleBuilder)
        && let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.console
    {
        console_logging_off = log_level == LogLevel::NONE
    };

    formatdoc! {"
        {COMMON_BASH_TRAP_FUNCTIONS}
        prepare_signal_handlers
        mkdir -p {STACKABLE_LOG_DIR}/{container_name}
        stackable-opa-bundle-builder{logging_redirects} &
        wait_for_termination $!
        ",
        logging_redirects = if console_logging_off {
            " > /dev/null"
        } else {
            ""
        }
    }
}

/// TODO: *Technically* this function would need to be way more complex.
/// For now it's a good-enough approximation, this is fine :D
///
/// The following config
///
/// ```
/// containers:
///   opa-bundle-builder:
///     console:
///       level: DEBUG
///     file:
///       level: INFO
///     loggers:
///       ROOT:
///         level: INFO
///     my.module:
///       level: DEBUG
///     some.chatty.module:
///       level: NONE
/// ```
///
/// should result in
/// `CONSOLE_LOG_LEVEL=info,my.module=debug,some.chatty.module=none`
///  and
/// `FILE_LOG_LEVEL=info,my.module=info,some.chatty.module=none`.
/// Note that `my.module` is `info` instead of `debug`, because it's clamped by the global file log
/// level.
///
/// Context: https://docs.stackable.tech/home/stable/concepts/logging/
fn sidecar_container_log_level(
    merged_config: &ValidatedOpaConfig,
    sidecar_container: &Container,
) -> build::properties::product_logging::BundleBuilderLogLevel {
    if let Some(ValidatedContainerLogConfigChoice::Automatic(log_config)) =
        merged_config.logging.containers.get(sidecar_container)
        && let Some(logger) = log_config
            .loggers
            .get(AutomaticContainerLogConfig::ROOT_LOGGER)
    {
        return build::properties::product_logging::BundleBuilderLogLevel::from(logger.level);
    }

    build::properties::product_logging::BundleBuilderLogLevel::Info
}

fn build_prepare_start_command(
    merged_config: &ValidatedOpaConfig,
    container_name: &str,
) -> Vec<String> {
    let mut prepare_container_args = vec![];
    if let Some(ValidatedContainerLogConfigChoice::Automatic(log_config)) =
        merged_config.logging.containers.get(&Container::Prepare)
    {
        prepare_container_args.push(product_logging::framework::capture_shell_output(
            STACKABLE_LOG_DIR,
            container_name,
            log_config,
        ));
    }

    for dir in [BUNDLES_ACTIVE_DIR, BUNDLES_INCOMING_DIR, BUNDLES_TMP_DIR] {
        prepare_container_args.push(format!("echo \"Create dir [{dir}]\""));
        prepare_container_args.push(format!("mkdir -p {dir}"));
    }

    prepare_container_args
}
