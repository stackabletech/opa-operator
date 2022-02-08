//! Ensures that `Pod`s are configured and running for each [`OpenPolicyAgent`]

use crate::discovery::{self, build_discovery_configmaps};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{OpaRole, OpenPolicyAgent, APP_NAME, REGO_RULE_REFERENCE};
use stackable_operator::{
    builder::{ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder},
    k8s_openapi::{
        api::{
            apps::v1::{DaemonSet, DaemonSetSpec},
            core::v1::{
                ConfigMap, ConfigMapVolumeSource, EnvVar, Service, ServicePort, ServiceSpec, Volume,
            },
        },
        apimachinery::pkg::apis::meta::v1::LabelSelector,
    },
    kube::runtime::{
        controller::{Context, ReconcilerAction},
        reflector::ObjectRef,
    },
    labels::{role_group_selector_labels, role_selector_labels},
    product_config::{types::PropertyNameKind, ProductConfigManager},
    product_config_utils::{transform_all_roles_to_config, validate_all_roles_and_groups_config},
    role_utils::RoleGroupRef,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

const FIELD_MANAGER_SCOPE: &str = "openpolicyagent";

pub const CONFIG_FILE: &str = "config.yaml";
pub const APP_PORT: u16 = 8081;
pub const APP_PORT_NAME: &str = "http";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object defines no version"))]
    ObjectHasNoVersion,
    #[snafu(display("failed to calculate role service name"))]
    RoleServiceNameNotFound,
    #[snafu(display("failed to apply role Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to apply Service for {}", rolegroup))]
    ApplyRoleGroupService {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to build ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to apply DaemonSet for {}", rolegroup))]
    ApplyRoleGroupDaemonSet {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
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
}
type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile_opa(
    opa: Arc<OpenPolicyAgent>,
    ctx: Context<Ctx>,
) -> Result<ReconcilerAction> {
    tracing::info!("Starting reconcile");
    let opa_ref = ObjectRef::from_obj(&*opa);
    let client = ctx.get_ref().client.clone();
    let opa_version = opa_version(&opa)?;

    let validated_config = validate_all_roles_and_groups_config(
        opa_version,
        &transform_all_roles_to_config(
            &*opa,
            [(
                OpaRole::Server.to_string(),
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
        &ctx.get_ref().product_config,
        false,
        false,
    )
    .context(InvalidProductConfigSnafu)?;
    let role_server_config = validated_config
        .get(&OpaRole::Server.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let server_role_service = build_server_role_service(&opa)?;
    let server_role_service = client
        .apply_patch(
            FIELD_MANAGER_SCOPE,
            &server_role_service,
            &server_role_service,
        )
        .await
        .context(ApplyRoleServiceSnafu)?;
    for (rolegroup_name, rolegroup_config) in role_server_config.iter() {
        let rolegroup = RoleGroupRef {
            cluster: opa_ref.clone(),
            role: OpaRole::Server.to_string(),
            role_group: rolegroup_name.to_string(),
        };

        let rg_configmap = build_server_rolegroup_config_map(&rolegroup, &opa, rolegroup_config)?;
        let rg_daemonset = build_server_rolegroup_daemonset(&rolegroup, &opa, rolegroup_config)?;
        let rg_service = build_rolegroup_service(&opa, &rolegroup)?;

        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_configmap, &rg_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_daemonset, &rg_daemonset)
            .await
            .with_context(|_| ApplyRoleGroupDaemonSetSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_service, &rg_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup.clone(),
            })?;
    }

    for discovery_cm in build_discovery_configmaps(&*opa, &*opa, &server_role_service)
        .context(BuildDiscoveryConfigSnafu)?
    {
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &discovery_cm, &discovery_cm)
            .await
            .context(ApplyDiscoveryConfigSnafu)?;
    }

    Ok(ReconcilerAction {
        requeue_after: None,
    })
}

/// The server-role service is the primary endpoint that should be used by clients that do not perform internal load balancing,
/// including targets outside of the cluster.
pub fn build_server_role_service(opa: &OpenPolicyAgent) -> Result<Service> {
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
            .with_recommended_labels(opa, APP_NAME, opa_version(opa)?, &role_name, "global")
            .build(),
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                name: Some(APP_PORT_NAME.to_string()),
                port: APP_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(role_selector_labels(opa, APP_NAME, &role_name)),
            type_: Some("NodePort".to_string()),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
fn build_rolegroup_service(
    opa: &OpenPolicyAgent,
    rolegroup: &RoleGroupRef<OpenPolicyAgent>,
) -> Result<Service> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&rolegroup.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(
                opa,
                APP_NAME,
                opa_version(opa)?,
                &rolegroup.role,
                &rolegroup.role_group,
            )
            .with_label("prometheus.io/scrape", "true")
            .build(),
        spec: Some(ServiceSpec {
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
    rolegroup: &RoleGroupRef<OpenPolicyAgent>,
    opa: &OpenPolicyAgent,
    server_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
) -> Result<ConfigMap> {
    let config = server_config
        .get(&PropertyNameKind::File(CONFIG_FILE.to_string()))
        .map(Cow::Borrowed)
        .unwrap_or_default();
    let mut cm = ConfigMapBuilder::new();
    cm.metadata(
        ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(rolegroup.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(
                opa,
                APP_NAME,
                opa_version(opa)?,
                &rolegroup.role,
                &rolegroup.role_group,
            )
            .build(),
    );
    if let Some(rego_reference) = config.get(REGO_RULE_REFERENCE) {
        cm.add_data(CONFIG_FILE, build_config_file(rego_reference));
    }
    cm.build().with_context(|_| BuildRoleGroupConfigSnafu {
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
fn build_server_rolegroup_daemonset(
    rolegroup_ref: &RoleGroupRef<OpenPolicyAgent>,
    opa: &OpenPolicyAgent,
    server_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
) -> Result<DaemonSet> {
    let opa_version = opa_version(opa)?;
    let image = format!(
        "docker.stackable.tech/stackable/opa:{}-stackable0",
        opa_version
    );
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
    let container_opa = ContainerBuilder::new("opa")
        .image(image)
        .command(build_opa_start_command())
        .add_env_vars(env)
        .add_container_port(APP_PORT_NAME, APP_PORT.into())
        .add_volume_mount("config", "/stackable/config")
        .build();
    Ok(DaemonSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(
                opa,
                APP_NAME,
                opa_version,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            )
            .with_annotation("prometheus.io/scrape", "true")
            .with_annotation("prometheus.io/port", APP_PORT.to_string())
            .with_annotation("prometheus.io/path", "metrics")
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
            template: PodBuilder::new()
                .metadata_builder(|m| {
                    m.with_recommended_labels(
                        opa,
                        APP_NAME,
                        opa_version,
                        &rolegroup_ref.role,
                        &rolegroup_ref.role_group,
                    )
                    .with_annotation("prometheus.io/scrape", "true")
                    .with_annotation("prometheus.io/port", APP_PORT.to_string())
                    .with_annotation("prometheus.io/path", "metrics")
                })
                .add_container(container_opa)
                .add_volume(Volume {
                    name: "config".to_string(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: Some(rolegroup_ref.object_name()),
                        ..ConfigMapVolumeSource::default()
                    }),
                    ..Volume::default()
                })
                .build_template(),
            ..DaemonSetSpec::default()
        }),
        status: None,
    })
}

pub fn opa_version(opa: &OpenPolicyAgent) -> Result<&str> {
    opa.spec.version.as_deref().context(ObjectHasNoVersionSnafu)
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> ReconcilerAction {
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(5)),
    }
}

fn build_config_file(rego_rule_reference: &str) -> String {
    format!(
        "
services:
  - name: stackable
    url: {}

bundles:
  stackable:
    service: stackable
    resource: opa/bundle.tar.gz
    persist: true
    polling:
      min_delay_seconds: 10
      max_delay_seconds: 20",
        rego_rule_reference
    )
}

fn build_opa_start_command() -> Vec<String> {
    vec![
        "/stackable/opa/opa".to_string(),
        "run".to_string(),
        "-s".to_string(),
        "-a".to_string(),
        format!("0.0.0.0:{}", APP_PORT),
        "-c".to_string(),
        "/stackable/config/config.yaml".to_string(),
    ]
}

fn service_ports() -> Vec<ServicePort> {
    vec![ServicePort {
        name: Some(APP_PORT_NAME.to_string()),
        port: APP_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}
