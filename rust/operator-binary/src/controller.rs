use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use const_format::concatcp;
use indoc::formatdoc;
use product_config::{ProductConfigManager, types::PropertyNameKind};
use serde::{Deserialize, Serialize};
use serde_json::json;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::{
    APP_NAME, DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT, OPERATOR_NAME, user_info_fetcher, v1alpha1,
};
use stackable_operator::{
    builder::{
        self,
        configmap::ConfigMapBuilder,
        meta::ObjectMetaBuilder,
        pod::{
            PodBuilder,
            container::{ContainerBuilder, FieldPathEnvVar},
            resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder,
            volume::VolumeBuilder,
        },
    },
    cluster_resources::{ClusterResourceApplyStrategy, ClusterResources},
    commons::{
        product_image_selection::ResolvedProductImage,
        rbac::build_rbac_resources,
        secret_class::{SecretClassVolume, SecretClassVolumeScope},
        tls_verification::{TlsClientDetails, TlsClientDetailsError},
    },
    k8s_openapi::{
        DeepMerge,
        api::{
            apps::v1::{DaemonSet, DaemonSetSpec},
            core::v1::{
                ConfigMap, EmptyDirVolumeSource, EnvVar, EnvVarSource, HTTPGetAction,
                ObjectFieldSelector, Probe, SecretVolumeSource, Service, ServiceAccount,
                ServicePort, ServiceSpec,
            },
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
    },
    kube::{
        Resource as KubeResource, ResourceExt,
        core::{DeserializeGuard, error_boundary},
        runtime::{controller::Action, reflector::ObjectRef},
    },
    kvp::{LabelError, Labels, ObjectLabels},
    logging::controller::ReconcilerError,
    memory::{BinaryMultiple, MemoryQuantity},
    product_config_utils::{transform_all_roles_to_config, validate_all_roles_and_groups_config},
    product_logging::{
        self,
        framework::{
            LoggingError, create_vector_shutdown_file_command, remove_vector_shutdown_file_command,
        },
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
    utils::{COMMON_BASH_TRAP_FUNCTIONS, cluster_info::KubernetesClusterInfo},
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    discovery::{self, build_discovery_configmaps},
    operations::graceful_shutdown::add_graceful_shutdown_config,
    product_logging::{BundleBuilderLogLevel, extend_role_group_config_map},
};

pub const OPA_CONTROLLER_NAME: &str = "opacluster";
pub const OPA_FULL_CONTROLLER_NAME: &str = concatcp!(OPA_CONTROLLER_NAME, '.', OPERATOR_NAME);

pub const CONFIG_FILE: &str = "config.json";
pub const APP_PORT: u16 = 8081;
pub const APP_PORT_NAME: &str = "http";
pub const METRICS_PORT_NAME: &str = "metrics";
pub const BUNDLES_ACTIVE_DIR: &str = "/bundles/active";
pub const BUNDLES_INCOMING_DIR: &str = "/bundles/incoming";
pub const BUNDLES_TMP_DIR: &str = "/bundles/tmp";
pub const BUNDLE_BUILDER_PORT: i32 = 3030;
pub const OPA_STACKABLE_SERVICE_NAME: &str = "stackable";

const CONFIG_VOLUME_NAME: &str = "config";
const CONFIG_DIR: &str = "/stackable/config";
const LOG_VOLUME_NAME: &str = "log";
const STACKABLE_LOG_DIR: &str = "/stackable/log";
const BUNDLES_VOLUME_NAME: &str = "bundles";
const BUNDLES_DIR: &str = "/bundles";
const USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME: &str = "credentials";
const USER_INFO_FETCHER_CREDENTIALS_DIR: &str = "/stackable/credentials";
const USER_INFO_FETCHER_KERBEROS_VOLUME_NAME: &str = "kerberos";
const USER_INFO_FETCHER_KERBEROS_DIR: &str = "/stackable/kerberos";

const DOCKER_IMAGE_BASE_NAME: &str = "opa";

const CONSOLE_LOG_LEVEL_ENV: &str = "CONSOLE_LOG_LEVEL";
const FILE_LOG_LEVEL_ENV: &str = "FILE_LOG_LEVEL";
const FILE_LOG_DIRECTORY_ENV: &str = "FILE_LOG_DIRECTORY";
const KUBERNETES_NODE_NAME_ENV: &str = "KUBERNETES_NODE_NAME";
const KUBERNETES_CLUSTER_DOMAIN_ENV: &str = "KUBERNETES_CLUSTER_DOMAIN";

// logging defaults
const DEFAULT_DECISION_LOGGING_ENABLED: bool = false;
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

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
    pub opa_bundle_builder_image: String,
    pub user_info_fetcher_image: String,
    pub cluster_info: KubernetesClusterInfo,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("OpaCluster object is invalid"))]
    InvalidOpaCluster {
        source: error_boundary::InvalidObject,
    },

    #[snafu(display("object does not define meta name"))]
    NoName,

    #[snafu(display("internal operator failure"))]
    InternalOperatorFailure {
        source: stackable_opa_operator::crd::Error,
    },

    #[snafu(display("failed to calculate role service name"))]
    RoleServiceNameNotFound,

    #[snafu(display("failed to apply role Service"))]
    ApplyRoleService {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply Service for [{rolegroup}]"))]
    ApplyRoleGroupService {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to apply metrics Service for [{rolegroup}]"))]
    ApplyRoleGroupMetricsService {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to build ConfigMap for [{rolegroup}]"))]
    BuildRoleGroupConfig {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to apply ConfigMap for [{rolegroup}]"))]
    ApplyRoleGroupConfig {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to apply DaemonSet for [{rolegroup}]"))]
    ApplyRoleGroupDaemonSet {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to apply patch for DaemonSet for [{rolegroup}]"))]
    ApplyPatchRoleGroupDaemonSet {
        source: stackable_operator::client::Error,
        rolegroup: RoleGroupRef<v1alpha1::OpaCluster>,
    },

    #[snafu(display("failed to patch service account"))]
    ApplyServiceAccount {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to patch role binding"))]
    ApplyRoleBinding {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build discovery ConfigMap"))]
    BuildDiscoveryConfig { source: discovery::Error },

    #[snafu(display("failed to apply discovery ConfigMap"))]
    ApplyDiscoveryConfig {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to transform configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("failed to resolve and merge config for role and role group"))]
    FailedToResolveConfig {
        source: stackable_opa_operator::crd::Error,
    },

    #[snafu(display("illegal container name"))]
    IllegalContainerName {
        source: stackable_operator::builder::pod::container::Error,
    },

    #[snafu(display("vector agent is enabled but vector aggregator ConfigMap is missing"))]
    VectorAggregatorConfigMapMissing,

    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },

    #[snafu(display("failed to create cluster resources"))]
    FailedToCreateClusterResources {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphans {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to build RBAC resources"))]
    BuildRbacResources {
        source: stackable_operator::commons::rbac::Error,
    },

    #[snafu(display("failed to configure graceful shutdown"))]
    GracefulShutdown {
        source: crate::operations::graceful_shutdown::Error,
    },

    #[snafu(display("failed to serialize user info fetcher configuration"))]
    SerializeUserInfoFetcherConfig { source: serde_json::Error },

    #[snafu(display("failed to build label"))]
    BuildLabel { source: LabelError },

    #[snafu(display("failed to build object meta data"))]
    ObjectMeta {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build volume spec for the User Info Fetcher TLS config"))]
    UserInfoFetcherKerberosVolume {
        source: stackable_operator::builder::pod::Error,
    },

    #[snafu(display("failed to build volume mount spec for the User Info Fetcher TLS config"))]
    UserInfoFetcherKerberosVolumeMount {
        source: stackable_operator::builder::pod::container::Error,
    },

    #[snafu(display(
        "failed to build volume or volume mount spec for the User Info Fetcher TLS config"
    ))]
    UserInfoFetcherTlsVolumeAndMounts { source: TlsClientDetailsError },

    #[snafu(display("failed to configure logging"))]
    ConfigureLogging { source: LoggingError },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },
}
type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

#[derive(Serialize, Deserialize)]
pub struct OpaClusterConfigFile {
    services: Vec<OpaClusterConfigService>,
    bundles: OpaClusterBundle,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_logs: Option<OpaClusterConfigDecisionLog>,
    status: Option<OpaClusterConfigStatus>,
}

impl OpaClusterConfigFile {
    pub fn new(decision_logging: Option<OpaClusterConfigDecisionLog>) -> Self {
        Self {
            services: vec![OpaClusterConfigService {
                name: OPA_STACKABLE_SERVICE_NAME.to_owned(),
                url: "http://localhost:3030/opa/v1".to_owned(),
            }],
            bundles: OpaClusterBundle {
                stackable: OpaClusterBundleConfig {
                    service: OPA_STACKABLE_SERVICE_NAME.to_owned(),
                    resource: "opa/bundle.tar.gz".to_owned(),
                    persist: true,
                    polling: OpaClusterBundleConfigPolling {
                        min_delay_seconds: 10,
                        max_delay_seconds: 20,
                    },
                },
            },
            decision_logs: decision_logging,
            // Enable more Prometheus metrics, such as bundle loads
            // See https://www.openpolicyagent.org/docs/monitoring#status-metrics
            status: Some(OpaClusterConfigStatus {
                service: OPA_STACKABLE_SERVICE_NAME.to_owned(),
                prometheus: true,
            }),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct OpaClusterConfigService {
    name: String,
    url: String,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundle {
    stackable: OpaClusterBundleConfig,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundleConfig {
    service: String,
    resource: String,
    persist: bool,
    polling: OpaClusterBundleConfigPolling,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterBundleConfigPolling {
    min_delay_seconds: i32,
    max_delay_seconds: i32,
}

#[derive(Serialize, Deserialize)]
pub struct OpaClusterConfigDecisionLog {
    console: bool,
}

#[derive(Serialize, Deserialize)]
struct OpaClusterConfigStatus {
    service: String,
    prometheus: bool,
}

pub async fn reconcile_opa(
    opa: Arc<DeserializeGuard<v1alpha1::OpaCluster>>,
    ctx: Arc<Ctx>,
) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let opa = opa
        .0
        .as_ref()
        .map_err(error_boundary::InvalidObject::clone)
        .context(InvalidOpaClusterSnafu)?;
    let opa_ref = ObjectRef::from_obj(opa);

    let client = &ctx.client;
    let resolved_product_image = opa
        .spec
        .image
        .resolve(DOCKER_IMAGE_BASE_NAME, crate::built_info::PKG_VERSION);
    let opa_role = v1alpha1::OpaRole::Server;

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
            opa,
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

    let server_role_service = build_server_role_service(opa, &resolved_product_image)?;
    // required for discovery config map later
    let server_role_service = cluster_resources
        .add(client, server_role_service)
        .await
        .context(ApplyRoleServiceSnafu)?;

    let required_labels = cluster_resources
        .get_required_labels()
        .context(BuildLabelSnafu)?;

    let (rbac_sa, rbac_rolebinding) =
        build_rbac_resources(opa, APP_NAME, required_labels).context(BuildRbacResourcesSnafu)?;

    let rbac_sa = cluster_resources
        .add(client, rbac_sa.clone())
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
            opa,
            &resolved_product_image,
            &rolegroup,
            &merged_config,
        )?;
        let rg_service =
            build_rolegroup_headless_service(opa, &resolved_product_image, &rolegroup)?;
        let rg_metrics_service =
            build_rolegroup_metrics_service(opa, &resolved_product_image, &rolegroup)?;
        let rg_daemonset = build_server_rolegroup_daemonset(
            opa,
            &resolved_product_image,
            &opa_role,
            &rolegroup,
            rolegroup_config,
            &merged_config,
            &ctx.opa_bundle_builder_image,
            &ctx.user_info_fetcher_image,
            &rbac_sa,
            &ctx.cluster_info,
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
        cluster_resources
            .add(client, rg_metrics_service)
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
            .context(ApplyPatchRoleGroupDaemonSetSnafu { rolegroup })?;
    }

    for discovery_cm in build_discovery_configmaps(
        opa,
        opa,
        &resolved_product_image,
        &server_role_service,
        &client.kubernetes_cluster_info,
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

    let status = v1alpha1::OpaClusterStatus {
        conditions: compute_conditions(opa, &[&ds_cond_builder, &cluster_operation_cond_builder]),
    };

    client
        .apply_patch_status(OPERATOR_NAME, opa, &status)
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
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service> {
    let role_name = v1alpha1::OpaRole::Server.to_string();
    let role_svc_name = opa
        .server_role_service_name()
        .context(RoleServiceNameNotFoundSnafu)?;

    let metadata = ObjectMetaBuilder::new()
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
        .context(ObjectMetaSnafu)?
        .build();

    let service_selector_labels =
        Labels::role_selector(opa, APP_NAME, &role_name).context(BuildLabelSnafu)?;

    let service_spec = ServiceSpec {
        type_: Some(opa.spec.cluster_config.listener_class.k8s_service_type()),
        ports: Some(data_service_ports()),
        selector: Some(service_selector_labels.into()),
        internal_traffic_policy: Some("Local".to_string()),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
fn build_rolegroup_headless_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Service> {
    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup.rolegroup_headless_service_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup.role,
            &rolegroup.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .build();

    let service_spec = ServiceSpec {
        // Currently we don't offer listener-exposition of OPA mostly due to security concerns.
        // OPA is currently public within the Kubernetes (without authentication).
        // Opening it up to outside of Kubernetes might worsen things.
        // We are open to implement listener-integration, but this needs to be thought through before
        // implementing it.
        // Note: We have kind of similar situations for HMS and Zookeeper, as the authentication
        // options there are non-existent (mTLS still opens plain port) or suck (Kerberos).
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(data_service_ports()),
        selector: Some(role_group_selector_labels(opa, rolegroup)?.into()),
        publish_not_ready_addresses: Some(true),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and has the
/// prometheus.io/scrape label.
fn build_rolegroup_metrics_service(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Service> {
    let labels = Labels::try_from([("prometheus.io/scrape", "true")])
        .expect("static Prometheus labels must be valid");

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup.rolegroup_metrics_service_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup.role,
            &rolegroup.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .with_labels(labels)
        .build();

    let service_spec = ServiceSpec {
        type_: Some("ClusterIP".to_string()),
        cluster_ip: Some("None".to_string()),
        ports: Some(vec![metrics_service_port()]),
        selector: Some(role_group_selector_labels(opa, rolegroup)?.into()),
        ..ServiceSpec::default()
    };

    Ok(Service {
        metadata,
        spec: Some(service_spec),
        status: None,
    })
}

/// Returns the [`Labels`] that can be used to select all Pods that are part of the roleGroup.
fn role_group_selector_labels(
    opa: &v1alpha1::OpaCluster,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
) -> Result<Labels> {
    Labels::role_group_selector(opa, APP_NAME, &rolegroup.role, &rolegroup.role_group)
        .context(BuildLabelSnafu)
}

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the administrator
fn build_server_rolegroup_config_map(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::OpaCluster>,
    merged_config: &v1alpha1::OpaConfig,
) -> Result<ConfigMap> {
    let mut cm_builder = ConfigMapBuilder::new();

    let metadata = ObjectMetaBuilder::new()
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
        .context(ObjectMetaSnafu)?
        .build();

    cm_builder
        .metadata(metadata)
        .add_data(CONFIG_FILE, build_config_file(merged_config));

    if let Some(user_info) = &opa.spec.cluster_config.user_info {
        cm_builder.add_data(
            "user-info-fetcher.json",
            serde_json::to_string_pretty(user_info).context(SerializeUserInfoFetcherConfigSnafu)?,
        );
    }

    extend_role_group_config_map(rolegroup, &merged_config.logging, &mut cm_builder).context(
        InvalidLoggingConfigSnafu {
            cm_name: rolegroup.object_name(),
        },
    )?;

    cm_builder
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}

/// Env variables that are need to run stackable Rust binaries, such as
/// * opa-bundle-builder
/// * user-info-fetcher
fn add_stackable_rust_cli_env_vars(
    container_builder: &mut ContainerBuilder,
    cluster_info: &KubernetesClusterInfo,
    log_level: impl Into<String>,
    container: &v1alpha1::Container,
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

/// The rolegroup [`DaemonSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`] (from [`build_server_role_service`]).
///
/// We run an OPA on each node, because we want to avoid requiring network roundtrips for services making
/// policy queries (which are often chained in serial, and block other tasks in the products).
#[allow(clippy::too_many_arguments)]
fn build_server_rolegroup_daemonset(
    opa: &v1alpha1::OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    opa_role: &v1alpha1::OpaRole,
    rolegroup_ref: &RoleGroupRef<v1alpha1::OpaCluster>,
    server_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &v1alpha1::OpaConfig,
    opa_bundle_builder_image: &str,
    user_info_fetcher_image: &str,
    service_account: &ServiceAccount,
    cluster_info: &KubernetesClusterInfo,
) -> Result<DaemonSet> {
    let opa_name = opa.metadata.name.as_deref().context(NoNameSnafu)?;
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

    let prepare_container_name = v1alpha1::Container::Prepare.to_string();
    let mut cb_prepare =
        ContainerBuilder::new(&prepare_container_name).context(IllegalContainerNameSnafu)?;

    let bundle_builder_container_name = v1alpha1::Container::BundleBuilder.to_string();
    let mut cb_bundle_builder =
        ContainerBuilder::new(&bundle_builder_container_name).context(IllegalContainerNameSnafu)?;

    let opa_container_name = v1alpha1::Container::Opa.to_string();
    let mut cb_opa =
        ContainerBuilder::new(&opa_container_name).context(IllegalContainerNameSnafu)?;

    cb_prepare
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(vec![
            build_prepare_start_command(merged_config, &prepare_container_name).join(" && "),
        ])
        .add_volume_mount(BUNDLES_VOLUME_NAME, BUNDLES_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .resources(merged_config.resources.to_owned().into());

    cb_bundle_builder
        .image_from_product_image(resolved_product_image) // inherit the pull policy and pull secrets, and then...
        .image(opa_bundle_builder_image) // ...override the image
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
        .add_volume_mount(BUNDLES_VOLUME_NAME, BUNDLES_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
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
    add_stackable_rust_cli_env_vars(
        &mut cb_bundle_builder,
        cluster_info,
        sidecar_container_log_level(merged_config, &v1alpha1::Container::BundleBuilder).to_string(),
        &v1alpha1::Container::BundleBuilder,
    );

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
        .add_env_var(
            "CONTAINERDEBUG_LOG_DIRECTORY",
            format!("{STACKABLE_LOG_DIR}/containerdebug"),
        )
        .add_container_port(APP_PORT_NAME, APP_PORT.into())
        // If we also add a container port "metrics" pointing to the same port number, we get a
        //
        // .spec.template.spec.containers[name="opa"].ports: duplicate entries for key [containerPort=8081,protocol="TCP"]
        //
        // So we don't do that
        .add_volume_mount(CONFIG_VOLUME_NAME, CONFIG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
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

    let pb_metadata = ObjectMetaBuilder::new()
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .build();

    pb.metadata(pb_metadata)
        .add_init_container(cb_prepare.build())
        .add_container(cb_opa.build())
        .add_container(cb_bundle_builder.build())
        .image_pull_secrets_from_product_image(resolved_product_image)
        .affinity(&merged_config.affinity)
        .add_volume(
            VolumeBuilder::new(CONFIG_VOLUME_NAME)
                .with_config_map(rolegroup_ref.object_name())
                .build(),
        )
        .context(AddVolumeSnafu)?
        .add_volume(
            VolumeBuilder::new(BUNDLES_VOLUME_NAME)
                .with_empty_dir(None::<String>, None)
                .build(),
        )
        .context(AddVolumeSnafu)?
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
        .context(AddVolumeSnafu)?
        .service_account_name(service_account.name_any())
        .security_context(PodSecurityContextBuilder::new().fs_group(1000).build());

    if let Some(user_info) = &opa.spec.cluster_config.user_info {
        let mut cb_user_info_fetcher =
            ContainerBuilder::new("user-info-fetcher").context(IllegalContainerNameSnafu)?;

        cb_user_info_fetcher
            .image_from_product_image(resolved_product_image) // inherit the pull policy and pull secrets, and then...
            .image(user_info_fetcher_image) // ...override the image
            .command(vec!["stackable-opa-user-info-fetcher".to_string()])
            .add_env_var("CONFIG", format!("{CONFIG_DIR}/user-info-fetcher.json"))
            .add_env_var("CREDENTIALS_DIR", USER_INFO_FETCHER_CREDENTIALS_DIR)
            .add_volume_mount(CONFIG_VOLUME_NAME, CONFIG_DIR)
            .context(AddVolumeMountSnafu)?
            .resources(
                ResourceRequirementsBuilder::new()
                    .with_cpu_request("100m")
                    .with_cpu_limit("200m")
                    .with_memory_request("128Mi")
                    .with_memory_limit("128Mi")
                    .build(),
            );
        add_stackable_rust_cli_env_vars(
            &mut cb_user_info_fetcher,
            cluster_info,
            sidecar_container_log_level(merged_config, &v1alpha1::Container::UserInfoFetcher)
                .to_string(),
            &v1alpha1::Container::UserInfoFetcher,
        );

        match &user_info.backend {
            user_info_fetcher::v1alpha1::Backend::None {} => {}
            user_info_fetcher::v1alpha1::Backend::ExperimentalXfscAas(_) => {}
            user_info_fetcher::v1alpha1::Backend::ActiveDirectory(ad) => {
                pb.add_volume(
                    SecretClassVolume::new(
                        ad.kerberos_secret_class_name.clone(),
                        Some(SecretClassVolumeScope {
                            pod: false,
                            node: false,
                            services: vec![opa_name.to_string()],
                            listener_volumes: Vec::new(),
                        }),
                    )
                    .to_volume(USER_INFO_FETCHER_KERBEROS_VOLUME_NAME)
                    .unwrap(),
                )
                .context(UserInfoFetcherKerberosVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_KERBEROS_VOLUME_NAME,
                        USER_INFO_FETCHER_KERBEROS_DIR,
                    )
                    .context(UserInfoFetcherKerberosVolumeMountSnafu)?;
                cb_user_info_fetcher.add_env_var(
                    "KRB5_CONFIG",
                    format!("{USER_INFO_FETCHER_KERBEROS_DIR}/krb5.conf"),
                );
                cb_user_info_fetcher.add_env_var(
                    "KRB5_CLIENT_KTNAME",
                    format!("{USER_INFO_FETCHER_KERBEROS_DIR}/keytab"),
                );
                cb_user_info_fetcher.add_env_var("KRB5CCNAME", "MEMORY:".to_string());
                ad.tls
                    .add_volumes_and_mounts(&mut pb, vec![&mut cb_user_info_fetcher])
                    .context(UserInfoFetcherTlsVolumeAndMountsSnafu)?;
            }
            user_info_fetcher::v1alpha1::Backend::Keycloak(keycloak) => {
                pb.add_volume(
                    VolumeBuilder::new(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME)
                        .secret(SecretVolumeSource {
                            secret_name: Some(keycloak.client_credentials_secret.clone()),
                            ..Default::default()
                        })
                        .build(),
                )
                .context(AddVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME,
                        USER_INFO_FETCHER_CREDENTIALS_DIR,
                    )
                    .context(AddVolumeMountSnafu)?;
                keycloak
                    .tls
                    .add_volumes_and_mounts(&mut pb, vec![&mut cb_user_info_fetcher])
                    .context(UserInfoFetcherTlsVolumeAndMountsSnafu)?;
            }
            user_info_fetcher::v1alpha1::Backend::Entra(entra) => {
                pb.add_volume(
                    VolumeBuilder::new(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME)
                        .secret(SecretVolumeSource {
                            secret_name: Some(entra.client_credentials_secret.clone()),
                            ..Default::default()
                        })
                        .build(),
                )
                .context(AddVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME,
                        USER_INFO_FETCHER_CREDENTIALS_DIR,
                    )
                    .context(AddVolumeMountSnafu)?;

                TlsClientDetails {
                    tls: entra.tls.clone(),
                }
                .add_volumes_and_mounts(&mut pb, vec![&mut cb_user_info_fetcher])
                .context(UserInfoFetcherTlsVolumeAndMountsSnafu)?;
            }
        }

        pb.add_container(cb_user_info_fetcher.build());
    }

    if merged_config.logging.enable_vector_agent {
        match &opa.spec.cluster_config.vector_aggregator_config_map_name {
            Some(vector_aggregator_config_map_name) => {
                pb.add_container(
                    product_logging::framework::vector_container(
                        resolved_product_image,
                        CONFIG_VOLUME_NAME,
                        LOG_VOLUME_NAME,
                        merged_config
                            .logging
                            .containers
                            .get(&v1alpha1::Container::Vector),
                        ResourceRequirementsBuilder::new()
                            .with_cpu_request("250m")
                            .with_cpu_limit("500m")
                            .with_memory_request("128Mi")
                            .with_memory_limit("128Mi")
                            .build(),
                        vector_aggregator_config_map_name,
                    )
                    .context(ConfigureLoggingSnafu)?,
                );
            }
            None => {
                VectorAggregatorConfigMapMissingSnafu.fail()?;
            }
        }
    }

    add_graceful_shutdown_config(merged_config, &mut pb).context(GracefulShutdownSnafu)?;

    let mut pod_template = pb.build_template();
    pod_template.merge_from(role.config.pod_overrides.clone());
    pod_template.merge_from(role_group.config.pod_overrides.clone());

    let metadata = ObjectMetaBuilder::new()
        .name_and_namespace(opa)
        .name(rolegroup_ref.object_name())
        .ownerreference_from_resource(opa, None, Some(true))
        .context(ObjectMissingMetadataForOwnerRefSnafu)?
        .with_recommended_labels(build_recommended_labels(
            opa,
            &resolved_product_image.app_version_label,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        ))
        .context(ObjectMetaSnafu)?
        .build();

    let daemonset_match_labels = Labels::role_group_selector(
        opa,
        APP_NAME,
        &rolegroup_ref.role,
        &rolegroup_ref.role_group,
    )
    .context(BuildLabelSnafu)?;

    let daemonset_spec = DaemonSetSpec {
        selector: LabelSelector {
            match_labels: Some(daemonset_match_labels.into()),
            ..LabelSelector::default()
        },
        template: pod_template,
        ..DaemonSetSpec::default()
    };

    Ok(DaemonSet {
        metadata,
        spec: Some(daemonset_spec),
        status: None,
    })
}

pub fn error_policy(
    _obj: Arc<DeserializeGuard<v1alpha1::OpaCluster>>,
    error: &Error,
    _ctx: Arc<Ctx>,
) -> Action {
    match error {
        // root object is invalid, will be requeued when modified anyway
        Error::InvalidOpaCluster { .. } => Action::await_change(),

        _ => Action::requeue(*Duration::from_secs(10)),
    }
}

fn build_config_file(merged_config: &v1alpha1::OpaConfig) -> String {
    let mut decision_logging_enabled = DEFAULT_DECISION_LOGGING_ENABLED;

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&v1alpha1::Container::Opa)
    {
        if let Some(config) = log_config.loggers.get("decision") {
            decision_logging_enabled = config.level != LogLevel::NONE;
        }
    }

    let decision_logging = if decision_logging_enabled {
        Some(OpaClusterConfigDecisionLog { console: true })
    } else {
        None
    };

    let config = OpaClusterConfigFile::new(decision_logging);

    // The unwrap() shouldn't panic under any circumstances because Rusts type checker takes care of the OpaClusterConfigFile
    // and serde + serde_json therefore serialize/deserialize a valid struct
    serde_json::to_string_pretty(&json!(config)).unwrap()
}

fn build_opa_start_command(merged_config: &v1alpha1::OpaConfig, container_name: &str) -> String {
    let mut file_log_level = DEFAULT_FILE_LOG_LEVEL;
    let mut console_log_level = DEFAULT_CONSOLE_LOG_LEVEL;
    let mut server_log_level = DEFAULT_SERVER_LOG_LEVEL;
    let mut decision_log_level = DEFAULT_DECISION_LOG_LEVEL;

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&v1alpha1::Container::Opa)
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

    // Redirects matter!
    // We need to watch out, that the following "$!" call returns the PID of the main (opa-bundle-builder) process,
    // and not some utility (e.g. multilog or tee) process.
    // See https://stackoverflow.com/a/8048493

    let logging_redirects = format!(
        "&> >(CONSOLE_LEVEL={console_log_level} FILE_LEVEL={file_log_level} DECISION_LEVEL={decision_log_level} SERVER_LEVEL={server_log_level} OPA_ROLLING_LOG_FILE_SIZE_BYTES={OPA_ROLLING_LOG_FILE_SIZE_BYTES} OPA_ROLLING_LOG_FILES={OPA_ROLLING_LOG_FILES} STACKABLE_LOG_DIR={STACKABLE_LOG_DIR} CONTAINER_NAME={container_name} process-logs)"
    );

    // TODO: Think about adding --shutdown-wait-period, as suggested by https://github.com/open-policy-agent/opa/issues/2764
    formatdoc! {"
        {COMMON_BASH_TRAP_FUNCTIONS}
        {remove_vector_shutdown_file_command}
        prepare_signal_handlers
        containerdebug --output={STACKABLE_LOG_DIR}/containerdebug-state.json --loop &
        opa run -s -a 0.0.0.0:{APP_PORT} -c {CONFIG_DIR}/{CONFIG_FILE} -l {opa_log_level} --shutdown-grace-period {shutdown_grace_period_s} --disable-telemetry {logging_redirects} &
        wait_for_termination $!
        {create_vector_shutdown_file_command}
        ",
        remove_vector_shutdown_file_command =
            remove_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        create_vector_shutdown_file_command =
            create_vector_shutdown_file_command(STACKABLE_LOG_DIR),
        shutdown_grace_period_s = merged_config.graceful_shutdown_timeout.unwrap_or(DEFAULT_SERVER_GRACEFUL_SHUTDOWN_TIMEOUT).as_secs(),
        opa_log_level = [console_log_level, file_log_level].iter().min().unwrap_or(&LogLevel::INFO).to_opa_literal()
    }
}

fn build_bundle_builder_start_command(
    merged_config: &v1alpha1::OpaConfig,
    container_name: &str,
) -> String {
    let mut console_logging_off = false;

    // We need to check if the console logging is deactivated (NONE)
    // This will result in not using `tee` later on in the start command
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&v1alpha1::Container::BundleBuilder)
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
    merged_config: &v1alpha1::OpaConfig,
    sidecar_container: &v1alpha1::Container,
) -> BundleBuilderLogLevel {
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(sidecar_container)
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

fn build_prepare_start_command(
    merged_config: &v1alpha1::OpaConfig,
    container_name: &str,
) -> Vec<String> {
    let mut prepare_container_args = vec![];
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config
        .logging
        .containers
        .get(&v1alpha1::Container::Prepare)
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

fn data_service_ports() -> Vec<ServicePort> {
    // Currently only HTTP is exposed
    vec![ServicePort {
        name: Some(APP_PORT_NAME.to_string()),
        port: APP_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}

fn metrics_service_port() -> ServicePort {
    ServicePort {
        name: Some(METRICS_PORT_NAME.to_string()),
        // The metrics are served on the same port as the HTTP traffic
        port: APP_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }
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
