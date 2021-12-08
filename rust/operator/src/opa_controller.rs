//! Ensures that `Pod`s are configured and running for each [`ZookeeperCluster`]

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use crate::{
    discovery::{self, build_discovery_configmaps},
    // discovery::{self, build_discovery_configmaps},
    utils::apply_owned,
    APP_NAME,
};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{OpaRole, OpenPolicyAgent, REGO_RULE_REFERENCE};
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
    kube::{
        self,
        runtime::{
            controller::{Context, ReconcilerAction},
            reflector::ObjectRef,
        },
    },
    labels::{role_group_selector_labels, role_selector_labels},
    product_config::{types::PropertyNameKind, ProductConfigManager},
    product_config_utils::{transform_all_roles_to_config, validate_all_roles_and_groups_config},
    role_utils::RoleGroupRef,
};

const FIELD_MANAGER: &str = "zookeeper.stackable.tech/zookeepercluster";

pub const CONFIG_FILE: &str = "config.yaml";
pub const APP_PORT: u16 = 8181;

pub struct Ctx {
    pub kube: kube::Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object {} has no namespace", obj_ref))]
    ObjectHasNoNamespace { obj_ref: ObjectRef<OpenPolicyAgent> },
    #[snafu(display("object {} defines no version", obj_ref))]
    ObjectHasNoVersion { obj_ref: ObjectRef<OpenPolicyAgent> },
    #[snafu(display("{} has no server role", obj_ref))]
    NoServerRole { obj_ref: ObjectRef<OpenPolicyAgent> },
    #[snafu(display("failed to calculate role service name for {}", obj_ref))]
    RoleServiceNameNotFound { obj_ref: ObjectRef<OpenPolicyAgent> },
    #[snafu(display("failed to apply role Service for {}", opa))]
    ApplyRoleService {
        source: kube::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to build ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: kube::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to apply DaemonSet for {}", rolegroup))]
    ApplyRoleGroupDaemonSet {
        source: kube::Error,
        rolegroup: RoleGroupRef<OpenPolicyAgent>,
    },
    #[snafu(display("invalid product config for {}", opa))]
    InvalidProductConfig {
        source: stackable_operator::error::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
    #[snafu(display("object {} is missing metadata to build owner reference", opa))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::error::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to build discovery ConfigMap for {}", opa))]
    BuildDiscoveryConfig {
        source: discovery::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
    #[snafu(display("failed to apply discovery ConfigMap for {}", opa))]
    ApplyDiscoveryConfig {
        source: kube::Error,
        opa: ObjectRef<OpenPolicyAgent>,
    },
}
type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile_opa(opa: OpenPolicyAgent, ctx: Context<Ctx>) -> Result<ReconcilerAction> {
    tracing::info!("Starting reconcile");
    let opa_ref = ObjectRef::from_obj(&opa);
    let kube = ctx.get_ref().kube.clone();

    let opa_version = opa.spec.version.to_string();
    let validated_config = validate_all_roles_and_groups_config(
        &opa_version,
        &transform_all_roles_to_config(
            &opa,
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
        ),
        &ctx.get_ref().product_config,
        false,
        false,
    )
    .with_context(|| InvalidProductConfig {
        opa: opa_ref.clone(),
    })?;
    let role_server_config = validated_config
        .get(&OpaRole::Server.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let server_role_service = apply_owned(&kube, FIELD_MANAGER, &build_server_role_service(&opa)?)
        .await
        .with_context(|| ApplyRoleService {
            opa: opa_ref.clone(),
        })?;
    for (rolegroup_name, rolegroup_config) in role_server_config.iter() {
        let rolegroup = RoleGroupRef {
            cluster: opa_ref.clone(),
            role: OpaRole::Server.to_string(),
            role_group: rolegroup_name.to_string(),
        };

        apply_owned(
            &kube,
            FIELD_MANAGER,
            &build_server_rolegroup_config_map(&rolegroup, &opa, rolegroup_config)?,
        )
        .await
        .with_context(|| ApplyRoleGroupConfig {
            rolegroup: rolegroup.clone(),
        })?;
        apply_owned(
            &kube,
            FIELD_MANAGER,
            &build_server_rolegroup_daemonset(&rolegroup, &opa, rolegroup_config)?,
        )
        .await
        .with_context(|| ApplyRoleGroupDaemonSet {
            rolegroup: rolegroup.clone(),
        })?;
    }

    for discovery_cm in
        build_discovery_configmaps(&opa, &opa, &server_role_service).with_context(|| {
            BuildDiscoveryConfig {
                opa: opa_ref.clone(),
            }
        })?
    {
        apply_owned(&kube, FIELD_MANAGER, &discovery_cm)
            .await
            .with_context(|| ApplyDiscoveryConfig {
                opa: opa_ref.clone(),
            })?;
    }

    Ok(ReconcilerAction {
        requeue_after: None,
    })
}

/// The server-role service is the primary endpoint that should be used by clients that do not perform internal load balancing,
/// including targets outside of the cluster.
///
/// Note that you should generally *not* hard-code clients to use these services; instead, create a [`ZookeeperZnode`](`stackable_zookeeper_crd::ZookeeperZnode`)
/// and use the connection string that it gives you.
pub fn build_server_role_service(opa: &OpenPolicyAgent) -> Result<Service> {
    let role_name = OpaRole::Server.to_string();
    let role_svc_name =
        opa.server_role_service_name()
            .with_context(|| RoleServiceNameNotFound {
                obj_ref: ObjectRef::from_obj(opa),
            })?;
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&role_svc_name)
            .ownerreference_from_resource(opa, None, Some(true))
            .with_context(|| ObjectMissingMetadataForOwnerRef {
                opa: ObjectRef::from_obj(opa),
            })?
            .with_recommended_labels(opa, APP_NAME, &opa_version(opa)?, &role_name, "global")
            .build(),
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                name: Some("http".to_string()),
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
            .with_context(|| ObjectMissingMetadataForOwnerRef {
                opa: ObjectRef::from_obj(opa),
            })?
            .with_recommended_labels(
                opa,
                APP_NAME,
                &opa_version(opa)?,
                &rolegroup.role,
                &rolegroup.role_group,
            )
            .build(),
    );
    if let Some(rego_reference) = config.get(REGO_RULE_REFERENCE) {
        cm.add_data(CONFIG_FILE, build_config_file(rego_reference));
    }
    cm.build().with_context(|| BuildRoleGroupConfig {
        rolegroup: rolegroup.clone(),
    })
}

/// The rolegroup [`StatefulSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the corresponding [`Service`] (from [`build_rolegroup_service`]).
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
        .add_env_vars(env)
        .add_container_port("http", APP_PORT.into())
        .add_volume_mount("config", "/stackable/config")
        .build();
    Ok(DaemonSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(opa, None, Some(true))
            .with_context(|| ObjectMissingMetadataForOwnerRef {
                opa: ObjectRef::from_obj(opa),
            })?
            .with_recommended_labels(
                opa,
                APP_NAME,
                &opa_version,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            )
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
                        &opa_version,
                        &rolegroup_ref.role,
                        &rolegroup_ref.role_group,
                    )
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

pub fn opa_version(opa: &OpenPolicyAgent) -> Result<String> {
    Ok(opa.spec.version.to_string())
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
