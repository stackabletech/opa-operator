//! Ensures that `Pod`s are configured and running for each [`OpaCluster`]

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use indoc::formatdoc;
use product_config::{types::PropertyNameKind, ProductConfigManager};
use serde_json::json;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{
    user_info_fetcher, Container, OpaCluster, OpaClusterStatus, OpaConfig, OpaRole, APP_NAME,
    DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT, OPERATOR_NAME,
};
use stackable_operator::{
    builder::{
        resources::ResourceRequirementsBuilder, ConfigMapBuilder, ContainerBuilder,
        FieldPathEnvVar, ObjectMetaBuilder, PodBuilder, PodSecurityContextBuilder, VolumeBuilder,
    },
    cluster_resources::{ClusterResourceApplyStrategy, ClusterResources},
    commons::{product_image_selection::ResolvedProductImage, rbac::build_rbac_resources},
    k8s_openapi::{
        api::{
            apps::v1::{DaemonSet, DaemonSetSpec},
            core::v1::{
                ConfigMap, EmptyDirVolumeSource, EnvVar, HTTPGetAction, Probe, Service,
                ServicePort, ServiceSpec,
            },
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
        DeepMerge,
    },
    kube::{
        runtime::{controller::Action, reflector::ObjectRef},
        Resource as KubeResource, ResourceExt,
    },
    labels::{role_group_selector_labels, role_selector_labels, ObjectLabels},
    logging::controller::ReconcilerError,
    memory::{BinaryMultiple, MemoryQuantity},
    product_config_utils::transform_all_roles_to_config,
    product_logging::{
        self,
        framework::{create_vector_shutdown_file_command, remove_vector_shutdown_file_command},
        spec::{
            AppenderConfig, AutomaticContainerLogConfig, ContainerLogConfig,
            ContainerLogConfigChoice, LogLevel,
        },
    },
    role_utils::RoleGroupRef,
    status::condition::{
        compute_conditions, daemonset::DaemonSetConditionBuilder,
        operations::ClusterOperationsConditionBuilder,
    },
    time::Duration,
};
use stackable_operator::{
    k8s_openapi::api::core::v1::SecretVolumeSource,
    product_config_utils::validate_all_roles_and_groups_config, utils::COMMON_BASH_TRAP_FUNCTIONS,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::product_logging::{
    extend_role_group_config_map, resolve_vector_aggregator_address, BundleBuilderLogLevel,
    OpaLogLevel,
};
use crate::{
    discovery::{self, build_discovery_configmaps},
    operations::graceful_shutdown::add_graceful_shutdown_config,
};

pub const OPA_CONTROLLER_NAME: &str = "opacluster";

pub const CONFIG_FILE: &str = "config.yaml";
pub const APP_PORT: u16 = 8081;
pub const APP_PORT_NAME: &str = "http";
pub const METRICS_PORT_NAME: &str = "metrics";
pub const BUNDLES_ACTIVE_DIR: &str = "/bundles/active";
pub const BUNDLES_INCOMING_DIR: &str = "/bundles/incoming";
pub const BUNDLES_TMP_DIR: &str = "/bundles/tmp";
pub const BUNDLE_BUILDER_PORT: i32 = 3030;

const CONFIG_VOLUME_NAME: &str = "config";
const CONFIG_DIR: &str = "/stackable/config";
const LOG_VOLUME_NAME: &str = "log";
const STACKABLE_LOG_DIR: &str = "/stackable/log";
const BUNDLES_VOLUME_NAME: &str = "bundles";
const BUNDLES_DIR: &str = "/bundles";
const USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME: &str = "credentials";
const USER_INFO_FETCHER_CREDENTIALS_DIR: &str = "/stackable/credentials";

const DOCKER_IMAGE_BASE_NAME: &str = "opa";

// bundle builder: ~ 5 MB x 2
// these sizes are needed both for the single file (for multilog, in bytes) as well as the total (for the EmptyDir)
const OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_MB: u32 = 5;
const OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_BYTES: u32 =
    OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_MB * 1000000;
const OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES: u32 = 2;
const MAX_OPA_BUNDLE_BUILDER_LOG_FILE_SIZE: MemoryQuantity = MemoryQuantity {
    value: (OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_MB * OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES)
        as f32,
    unit: BinaryMultiple::Mebi,
};
// opa logs: ~ 5 MB x 2
// these sizes are needed both for the single file (for multilog, in bytes) as well as the total (for the EmptyDir)
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

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
    pub opa_bundle_builder_clusterrole: String,
    pub user_info_fetcher_image: String,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object does not define meta name"))]
    NoName,

    #[snafu(display("internal operator failure"))]
    InternalOperatorFailure { source: stackable_opa_crd::Error },

    #[snafu(display("failed to calculate role service name"))]
    RoleServiceNameNotFound,

    #[snafu(display("failed to apply role Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to apply Service for [{rolegroup}]"))]
    ApplyRoleGroupService {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },

    #[snafu(display("failed to build ConfigMap for [{rolegroup}]"))]
    BuildRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },

    #[snafu(display("failed to apply ConfigMap for [{rolegroup}]"))]
    ApplyRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },

    #[snafu(display("failed to apply DaemonSet for [{rolegroup}]"))]
    ApplyRoleGroupDaemonSet {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },

    #[snafu(display("failed to patch service account"))]
    ApplyServiceAccount {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to patch role binding"))]
    ApplyRoleBinding {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to build discovery ConfigMap"))]
    BuildDiscoveryConfig { source: discovery::Error },

    #[snafu(display("failed to apply discovery ConfigMap"))]
    ApplyDiscoveryConfig {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to transform configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::ConfigError,
    },

    #[snafu(display("failed to resolve and merge config for role and role group"))]
    FailedToResolveConfig { source: stackable_opa_crd::Error },

    #[snafu(display("illegal container name"))]
    IllegalContainerName {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to resolve the Vector aggregator address"))]
    ResolveVectorAggregatorAddress {
        source: crate::product_logging::Error,
    },

    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },

    #[snafu(display("failed to create cluster resources"))]
    FailedToCreateClusterResources {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphans {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to build RBAC resources"))]
    BuildRbacResources {
        source: stackable_operator::error::Error,
    },

    #[snafu(display("failed to configure graceful shutdown"))]
    GracefulShutdown {
        source: crate::operations::graceful_shutdown::Error,
    },

    #[snafu(display("failed to serialize user info fetcher configuration"))]
    SerializeUserInfoFetcherConfig { source: serde_json::Error },
}
type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_opa(opa: Arc<OpaCluster>, ctx: Arc<Ctx>) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let opa_ref = ObjectRef::from_obj(opa.as_ref());
    let client = &ctx.client;
    let resolved_product_image = opa
        .spec
        .image
        .resolve(DOCKER_IMAGE_BASE_NAME, crate::built_info::CARGO_PKG_VERSION);
    let opa_role = OpaRole::Server;

    let mut cluster_resources = ClusterResources::new(
        APP_NAME,
        OPERATOR_NAME,
        OPA_CONTROLLER_NAME,
        &opa.object_ref(&()),
        ClusterResourceApplyStrategy::from(&opa.spec.cluster_operation),
    )
    .context(FailedToCreateClusterResourcesSnafu)?;

    let validated_config = validate_all_roles_and_groups_config(
        &resolved_product_image.product_version,
        &transform_all_roles_to_config(
            opa.as_ref(),
            [(
                opa_role.to_string(),
                (
                    vec![
                        PropertyNameKind::File(CONFIG_FILE.to_string()),
                        PropertyNameKind::Cli,
                    ],
                    opa.spec.servers.clone(),
                ),
            )]
            .into(),
        )
        .context(ProductConfigTransformSnafu)?,
        &ctx.product_config,
        false,
        false,
    )
    .context(InvalidProductConfigSnafu)?;
    let role_server_config = validated_config
        .get(&opa_role.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let vector_aggregator_address = resolve_vector_aggregator_address(&opa, client)
        .await
        .context(ResolveVectorAggregatorAddressSnafu)?;

    let server_role_service = build_server_role_service(&opa, &resolved_product_image)?;
    // required for discovery config map later
    let server_role_service = cluster_resources
        .add(client, server_role_service)
        .await
        .context(ApplyRoleServiceSnafu)?;

    let (rbac_sa, rbac_rolebinding) = build_rbac_resources(
        opa.as_ref(),
        APP_NAME,
        cluster_resources.get_required_labels(),
    )
    .context(BuildRbacResourcesSnafu)?;

    let rbac_sa = cluster_resources
        .add(client, rbac_sa)
        .await
        .context(ApplyServiceAccountSnafu)?;
    cluster_resources
        .add(client, rbac_rolebinding)
        .await
        .context(ApplyRoleBindingSnafu)?;

    let mut ds_cond_builder = DaemonSetConditionBuilder::default();

    for (rolegroup_name, rolegroup_config) in role_server_config.iter() {
        let rolegroup = RoleGroupRef {
            cluster: opa_ref.clone(),
            role: opa_role.to_string(),
            role_group: rolegroup_name.to_string(),
        };

        let merged_config = opa
            .merged_config(&opa_role, &rolegroup)
            .context(FailedToResolveConfigSnafu)?;

        let rg_configmap = build_server_rolegroup_config_map(
            &opa,
            &resolved_product_image,
            &rolegroup,
            &merged_config,
            vector_aggregator_address.as_deref(),
        )?;
        let rg_service = build_rolegroup_service(&opa, &resolved_product_image, &rolegroup)?;
        let rg_daemonset = build_server_rolegroup_daemonset(
            &opa,
            &resolved_product_image,
            &opa_role,
            &rolegroup,
            rolegroup_config,
            &merged_config,
            &ctx.user_info_fetcher_image,
            &rbac_sa.name_any(),
        )?;

        cluster_resources
            .add(client, rg_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        cluster_resources
            .add(client, rg_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        ds_cond_builder.add(
            cluster_resources
                .add(client, rg_daemonset.clone())
                .await
                .with_context(|_| ApplyRoleGroupDaemonSetSnafu {
                    rolegroup: rolegroup.clone(),
                })?,
        );

        // Previous version of opa-operator used the field manager scope "opacluster" to write out a DaemonSet with the bundle-builder container called "opa-bundle-builder".
        // During https://github.com/stackabletech/opa-operator/pull/420 it was renamed to "bundle-builder".
        // As we are now using the field manager scope "opa.stackable.tech_opacluster", our old changes (with the old container) will stay valid.
        // We have to use the old field manager scope and post an empty path to get rid of it
        // https://github.com/stackabletech/issues/issues/390 will implement a proper fix, e.g. also fixing Services and ConfigMaps
        // For details see https://github.com/stackabletech/opa-operator/issues/444
        tracing::trace!(
            "Removing old field manager scope \"opacluster\" of DaemonSet {daemonset_name} to remove the \"opa-bundle-builder\" container. \
            See https://github.com/stackabletech/opa-operator/issues/444 and https://github.com/stackabletech/issues/issues/390 for details.",
            daemonset_name = rg_daemonset.name_any()
        );
        client
            .apply_patch(
                "opacluster",
                &rg_daemonset,
                // We can hardcode this here, as https://github.com/stackabletech/issues/issues/390 will solve the general problem and we always have created DaemonSets using the "apps/v1" version
                json!({"apiVersion": "apps/v1", "kind": "DaemonSet"}),
            )
            .await
            .context(ApplyRoleGroupDaemonSetSnafu { rolegroup })?;
    }

    for discovery_cm in build_discovery_configmaps(
        opa.as_ref(),
        opa.as_ref(),
        &resolved_product_image,
        &server_role_service,
    )
    .context(BuildDiscoveryConfigSnafu)?
    {
        cluster_resources
            .add(client, discovery_cm)
            .await
            .context(ApplyDiscoveryConfigSnafu)?;
    }

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&opa.spec.cluster_operation);

    let status = OpaClusterStatus {
        conditions: compute_conditions(
            opa.as_ref(),
            &[&ds_cond_builder, &cluster_operation_cond_builder],
        ),
    };

    client
        .apply_patch_status(OPERATOR_NAME, &*opa, &status)
        .await
        .context(ApplyStatusSnafu)?;

    cluster_resources
        .delete_orphaned_resources(client)
        .await
        .context(DeleteOrphansSnafu)?;

    Ok(Action::await_change())
}

/// The server-role service is the primary endpoint that should be used by clients that do not perform internal load balancing,
/// including targets outside of the cluster.
pub fn build_server_role_service(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service> {
    let role_name = OpaRole::Server.to_string();
    let role_svc_name = opa
        .server_role_service_name()
        .context(RoleServiceNameNotFoundSnafu)?;
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&role_svc_name)
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                opa,
                &resolved_product_image.app_version_label,
                &role_name,
                "global",
            ))
            .build(),
        spec: Some(ServiceSpec {
            type_: Some(opa.spec.cluster_config.listener_class.k8s_service_type()),
            ports: Some(vec![ServicePort {
                name: Some(APP_PORT_NAME.to_string()),
                port: APP_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(role_selector_labels(opa, APP_NAME, &role_name)),
            internal_traffic_policy: Some("Local".to_string()),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
fn build_rolegroup_service(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<OpaCluster>,
) -> Result<Service> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&rolegroup.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                opa,
                &resolved_product_image.app_version_label,
                &rolegroup.role,
                &rolegroup.role_group,
            ))
            .with_label("prometheus.io/scrape", "true")
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(service_ports()),
            selector: Some(role_group_selector_labels(
                opa,
                APP_NAME,
                &rolegroup.role,
                &rolegroup.role_group,
            )),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the administrator
fn build_server_rolegroup_config_map(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<OpaCluster>,
    merged_config: &OpaConfig,
    vector_aggregator_address: Option<&str>,
) -> Result<ConfigMap> {
    let mut cm_builder = ConfigMapBuilder::new();

    cm_builder
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(opa)
                .name(rolegroup.object_name())
                .ownerreference_from_resource(opa, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .with_recommended_labels(build_recommended_labels(
                    opa,
                    &resolved_product_image.app_version_label,
                    &rolegroup.role,
                    &rolegroup.role_group,
                ))
                .build(),
        )
        .add_data(CONFIG_FILE, build_config_file());

    if let Some(user_info) = &opa.spec.cluster_config.user_info {
        cm_builder.add_data(
            "user-info-fetcher.json",
            serde_json::to_string_pretty(user_info).context(SerializeUserInfoFetcherConfigSnafu)?,
        );
    }

    extend_role_group_config_map(
        rolegroup,
        vector_aggregator_address,
        &merged_config.logging,
        &mut cm_builder,
    )
    .context(InvalidLoggingConfigSnafu {
        cm_name: rolegroup.object_name(),
    })?;

    cm_builder
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}

/// The rolegroup [`DaemonSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`] (from [`build_server_role_service`]).
///
/// We run an OPA on each node, because we want to avoid requiring network roundtrips for services making
/// policy queries (which are often chained in serial, and block other tasks in the products).
#[allow(clippy::too_many_arguments)]
fn build_server_rolegroup_daemonset(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    opa_role: &OpaRole,
    rolegroup_ref: &RoleGroupRef<OpaCluster>,
    server_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &OpaConfig,
    user_info_fetcher_image: &str,
    sa_name: &str,
) -> Result<DaemonSet> {
    let role = opa.role(opa_role);
    let role_group = opa
        .rolegroup(rolegroup_ref)
        .context(InternalOperatorFailureSnafu)?;

    let env = server_config
        .get(&PropertyNameKind::Env)
        .iter()
        .flat_map(|env_vars| env_vars.iter())
        .map(|(k, v)| EnvVar {
            name: k.clone(),
            value: Some(v.clone()),
            ..EnvVar::default()
        })
        .collect::<Vec<_>>();

    let mut pb = PodBuilder::new();

    let prepare_container_name = Container::Prepare.to_string();
    let mut cb_prepare =
        ContainerBuilder::new(&prepare_container_name).context(IllegalContainerNameSnafu)?;

    let bundle_builder_container_name = Container::BundleBuilder.to_string();
    let mut cb_bundle_builder =
        ContainerBuilder::new(&bundle_builder_container_name).context(IllegalContainerNameSnafu)?;

    let opa_container_name = Container::Opa.to_string();
    let mut cb_opa =
        ContainerBuilder::new(&opa_container_name).context(IllegalContainerNameSnafu)?;

    let mut cb_user_info_fetcher =
        ContainerBuilder::new("user-info-fetcher").context(IllegalContainerNameSnafu)?;

    cb_prepare
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(vec![build_prepare_start_command(
            merged_config,
            &prepare_container_name,
        )
        .join(" && ")])
        .add_volume_mount(BUNDLES_VOLUME_NAME, BUNDLES_DIR)
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .resources(merged_config.resources.to_owned().into());

    cb_bundle_builder
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(vec![build_bundle_builder_start_command(
            merged_config,
            &bundle_builder_container_name,
        )])
        .add_env_var_from_field_path("WATCH_NAMESPACE", FieldPathEnvVar::Namespace)
        .add_env_var(
            "OPA_BUNDLE_BUILDER_LOG",
            bundle_builder_log_level(merged_config).to_string(),
        )
        .add_volume_mount(BUNDLES_VOLUME_NAME, BUNDLES_DIR)
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .resources(
            ResourceRequirementsBuilder::new()
                .with_cpu_request("100m")
                .with_cpu_limit("200m")
                .with_memory_request("128Mi")
                .with_memory_limit("128Mi")
                .build(),
        )
        .readiness_probe(Probe {
            initial_delay_seconds: Some(5),
            period_seconds: Some(10),
            failure_threshold: Some(5),
            http_get: Some(HTTPGetAction {
                port: IntOrString::Int(BUNDLE_BUILDER_PORT),
                path: Some("/status".to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        })
        .liveness_probe(Probe {
            initial_delay_seconds: Some(30),
            period_seconds: Some(10),
            http_get: Some(HTTPGetAction {
                port: IntOrString::Int(BUNDLE_BUILDER_PORT),
                path: Some("/status".to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        });

    cb_opa
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(vec![build_opa_start_command(
            merged_config,
            &opa_container_name,
        )])
        .add_env_vars(env)
        .add_container_port(APP_PORT_NAME, APP_PORT.into())
        .add_volume_mount(CONFIG_VOLUME_NAME, CONFIG_DIR)
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .resources(merged_config.resources.to_owned().into())
        .readiness_probe(Probe {
            initial_delay_seconds: Some(5),
            period_seconds: Some(10),
            failure_threshold: Some(5),
            http_get: Some(HTTPGetAction {
                port: IntOrString::String(APP_PORT_NAME.to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        })
        .liveness_probe(Probe {
            initial_delay_seconds: Some(30),
            period_seconds: Some(10),
            http_get: Some(HTTPGetAction {
                port: IntOrString::String(APP_PORT_NAME.to_string()),
                ..HTTPGetAction::default()
            }),
            ..Probe::default()
        });

    if let Some(user_info) = &opa.spec.cluster_config.user_info {
        cb_user_info_fetcher
            .image(user_info_fetcher_image)
            .command(vec!["stackable-opa-user-info-fetcher".to_string()])
            .add_env_var("CONFIG", format!("{CONFIG_DIR}/user-info-fetcher.json"))
            .add_env_var("CREDENTIALS_DIR", USER_INFO_FETCHER_CREDENTIALS_DIR)
            .add_volume_mount(CONFIG_VOLUME_NAME, CONFIG_DIR)
            .resources(
                ResourceRequirementsBuilder::new()
                    .with_cpu_request("100m")
                    .with_cpu_limit("200m")
                    .with_memory_request("128Mi")
                    .with_memory_limit("128Mi")
                    .build(),
            );

        match &user_info.backend {
            user_info_fetcher::Backend::None {} => {}
            user_info_fetcher::Backend::Keycloak(keycloak) => {
                cb_user_info_fetcher.add_volume_mount(
                    USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME,
                    USER_INFO_FETCHER_CREDENTIALS_DIR,
                );
                keycloak
                    .tls
                    .add_volumes_and_mounts(&mut pb, vec![&mut cb_user_info_fetcher]);
            }
        }
    }

    pb.metadata_builder(|m| {
        m.with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        ))
    })
    .add_init_container(cb_prepare.build())
    .add_container(cb_opa.build())
    .add_container(cb_bundle_builder.build())
    .image_pull_secrets_from_product_image(resolved_product_image)
    .node_selector_opt(opa.node_selector(&rolegroup_ref.role_group))
    .add_volume(
        VolumeBuilder::new(CONFIG_VOLUME_NAME)
            .with_config_map(rolegroup_ref.object_name())
            .build(),
    )
    .add_volume(
        VolumeBuilder::new(BUNDLES_VOLUME_NAME)
            .with_empty_dir(None::<String>, None)
            .build(),
    )
    .add_volume(
        VolumeBuilder::new(LOG_VOLUME_NAME)
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
    .service_account_name(sa_name)
    .security_context(
        PodSecurityContextBuilder::new()
            .run_as_user(1000)
            .run_as_group(0)
            .fs_group(1000)
            .build(),
    );

    if let Some(user_info) = &opa.spec.cluster_config.user_info {
        pb.add_container(cb_user_info_fetcher.build());
        match &user_info.backend {
            user_info_fetcher::Backend::None {} => {}
            user_info_fetcher::Backend::Keycloak(keycloak) => {
                pb.add_volume(
                    VolumeBuilder::new(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME)
                        .secret(SecretVolumeSource {
                            secret_name: Some(keycloak.credentials_secret_name.clone()),
                            ..Default::default()
                        })
                        .build(),
                );
            }
        }
    }

    if merged_config.logging.enable_vector_agent {
        pb.add_container(product_logging::framework::vector_container(
            resolved_product_image,
            CONFIG_VOLUME_NAME,
            LOG_VOLUME_NAME,
            merged_config.logging.containers.get(&Container::Vector),
            ResourceRequirementsBuilder::new()
                .with_cpu_request("250m")
                .with_cpu_limit("500m")
                .with_memory_request("128Mi")
                .with_memory_limit("128Mi")
                .build(),
        ));
    }

    add_graceful_shutdown_config(merged_config, &mut pb).context(GracefulShutdownSnafu)?;

    let mut pod_template = pb.build_template();
    pod_template.merge_from(role.config.pod_overrides.clone());
    pod_template.merge_from(role_group.config.pod_overrides.clone());

    Ok(DaemonSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                opa,
                &resolved_product_image.app_version_label,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            ))
            .build(),
        spec: Some(DaemonSetSpec {
            selector: LabelSelector {
                match_labels: Some(role_group_selector_labels(
                    opa,
                    APP_NAME,
                    &rolegroup_ref.role,
                    &rolegroup_ref.role_group,
                )),
                ..LabelSelector::default()
            },
            template: pod_template,
            ..DaemonSetSpec::default()
        }),
        status: None,
    })
}

pub fn error_policy(_obj: Arc<OpaCluster>, _error: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(*Duration::from_secs(5))
}

fn build_config_file() -> &'static str {
    // We currently do not activate decision logging like
    // decision_logs:
    //     console: true
    // This will log decisions to the console, but also sends an extra `decision_id` field in the
    // API JSON response. This currently leads to our Java authorizers (Druid, Trino) failing to
    // deserialize the JSON object since they only expect to have a `result` field returned.
    // see https://github.com/stackabletech/opa-operator/issues/422
    "
services:
  - name: stackable
    url: http://localhost:3030/opa/v1

bundles:
  stackable:
    service: stackable
    resource: opa/bundle.tar.gz
    persist: true
    polling:
      min_delay_seconds: 10
      max_delay_seconds: 20
"
}

fn build_opa_start_command(merged_config: &OpaConfig, container_name: &str) -> String {
    let mut opa_log_level = OpaLogLevel::Info;
    let mut console_logging_off = false;

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Opa)
    {
        // Retrieve the file log level for OPA and convert to OPA log levels
        if let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.file
        {
            opa_log_level = OpaLogLevel::from(log_level);
        }

        // We need to check if the console logging is deactivated (NONE)
        // This will result in not using `tee` later on in the start command
        if let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.console
        {
            console_logging_off = log_level == LogLevel::NONE
        }
    }

    // TODO: Think about adding --shutdown-wait-period, as suggested by https://github.com/open-policy-agent/opa/issues/2764
    formatdoc! {"
        {COMMON_BASH_TRAP_FUNCTIONS}
        {remove_vector_shutdown_file_command}
        prepare_signal_handlers
        /stackable/opa/opa run -s -a 0.0.0.0:{APP_PORT} -c {CONFIG_DIR}/config.yaml -l {opa_log_level} --shutdown-grace-period {shutdown_grace_period_s} --disable-telemetry{logging_redirects} &
        wait_for_termination $!
        {create_vector_shutdown_file_command}
        ",
        remove_vector_shutdown_file_command =
            remove_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        create_vector_shutdown_file_command =
            create_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        shutdown_grace_period_s = merged_config.graceful_shutdown_timeout.unwrap_or(DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT).as_secs(),
        // Redirects matter!
        // We need to watch out, that the following "$!" call returns the PID of the main (opa-bundle-builder) process,
        // and not some utility (e.g. multilog or tee) process.
        // See https://stackoverflow.com/a/8048493
        logging_redirects = if console_logging_off {
            format!(" &> >(/stackable/multilog s{OPA_ROLLING_LOG_FILE_SIZE_BYTES} n{OPA_ROLLING_LOG_FILES} {STACKABLE_LOG_DIR}/{container_name})")
        } else {
            format!(" &> >(tee >(/stackable/multilog s{OPA_ROLLING_LOG_FILE_SIZE_BYTES} n{OPA_ROLLING_LOG_FILES} {STACKABLE_LOG_DIR}/{container_name}))")
        },
    }
}

fn build_bundle_builder_start_command(merged_config: &OpaConfig, container_name: &str) -> String {
    let mut console_logging_off = false;

    // We need to check if the console logging is deactivated (NONE)
    // This will result in not using `tee` later on in the start command
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&Container::BundleBuilder)
    {
        if let Some(AppenderConfig {
            level: Some(log_level),
        }) = log_config.console
        {
            console_logging_off = log_level == LogLevel::NONE
        }
    };

    formatdoc! {"
        {COMMON_BASH_TRAP_FUNCTIONS}
        prepare_signal_handlers
        /stackable/opa-bundle-builder{logging_redirects} &
        wait_for_termination $!
        ",
        // Redirects matter!
        // We need to watch out, that the following "$!" call returns the PID of the main (opa-bundle-builder) process,
        // and not some utility (e.g. multilog or tee) process.
        // See https://stackoverflow.com/a/8048493
        logging_redirects = if console_logging_off {
            format!(" &> >(/stackable/multilog s{OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_BYTES} n{OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES} {STACKABLE_LOG_DIR}/{container_name})")
        } else {
            format!(" &> >(tee >(/stackable/multilog s{OPA_ROLLING_BUNDLE_BUILDER_LOG_FILE_SIZE_BYTES} n{OPA_ROLLING_BUNDLE_BUILDER_LOG_FILES} {STACKABLE_LOG_DIR}/{container_name}))")
        },
    }
}

fn bundle_builder_log_level(merged_config: &OpaConfig) -> BundleBuilderLogLevel {
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&Container::BundleBuilder)
    {
        if let Some(logger) = log_config
            .loggers
            .get(AutomaticContainerLogConfig::ROOT_LOGGER)
        {
            return BundleBuilderLogLevel::from(logger.level);
        }
    }

    BundleBuilderLogLevel::Info
}

fn build_prepare_start_command(merged_config: &OpaConfig, container_name: &str) -> Vec<String> {
    let mut prepare_container_args = vec![];
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Prepare)
    {
        prepare_container_args.push(product_logging::framework::capture_shell_output(
            STACKABLE_LOG_DIR,
            container_name,
            log_config,
        ));
    }

    prepare_container_args.push(format!("echo \"Create dir [{BUNDLES_ACTIVE_DIR}]\""));
    prepare_container_args.push(format!("mkdir -p {BUNDLES_ACTIVE_DIR}"));
    prepare_container_args.push(format!("echo \"Create dir [{BUNDLES_INCOMING_DIR}]\""));
    prepare_container_args.push(format!("mkdir -p {BUNDLES_INCOMING_DIR}"));
    prepare_container_args.push(format!("echo \"Create dir [{BUNDLES_TMP_DIR}]\""));
    prepare_container_args.push(format!("mkdir -p {BUNDLES_TMP_DIR}"));

    prepare_container_args
}

fn service_ports() -> Vec<ServicePort> {
    vec![
        ServicePort {
            name: Some(APP_PORT_NAME.to_string()),
            port: APP_PORT.into(),
            protocol: Some("TCP".to_string()),
            ..ServicePort::default()
        },
        ServicePort {
            name: Some(METRICS_PORT_NAME.to_string()),
            port: 9504, // Arbitrary port number, this is never actually used anywhere
            protocol: Some("TCP".to_string()),
            target_port: Some(IntOrString::String(APP_PORT_NAME.to_string())),
            ..ServicePort::default()
        },
    ]
}

/// Creates recommended `ObjectLabels` to be used in deployed resources
pub fn build_recommended_labels<'a, T>(
    owner: &'a T,
    app_version: &'a str,
    role: &'a str,
    role_group: &'a str,
) -> ObjectLabels<'a, T> {
    ObjectLabels {
        owner,
        app_name: APP_NAME,
        app_version,
        operator_name: OPERATOR_NAME,
        controller_name: OPA_CONTROLLER_NAME,
        role,
        role_group,
    }
}
