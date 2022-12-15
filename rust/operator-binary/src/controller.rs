//! Ensures that `Pod`s are configured and running for each [`OpaCluster`]

use crate::discovery::{self, build_discovery_configmaps};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_crd::{OpaCluster, OpaRole, OpaStorageConfig, APP_NAME, OPERATOR_NAME};
use stackable_operator::{
    builder::{ConfigMapBuilder, ContainerBuilder, FieldPathEnvVar, ObjectMetaBuilder, PodBuilder},
    commons::{
        product_image_selection::ResolvedProductImage,
        resources::{NoRuntimeLimits, Resources},
    },
    k8s_openapi::{
        api::{
            apps::v1::{DaemonSet, DaemonSetSpec},
            core::v1::{
                ConfigMap, ConfigMapVolumeSource, EmptyDirVolumeSource, EnvVar, HTTPGetAction,
                PodSecurityContext, Probe, Service, ServiceAccount, ServicePort, ServiceSpec,
                Volume,
            },
            rbac::v1::{ClusterRole, RoleBinding, RoleRef, Subject},
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
        Resource,
    },
    kube::runtime::{controller::Action, reflector::ObjectRef},
    labels::{role_group_selector_labels, role_selector_labels, ObjectLabels},
    logging::controller::ReconcilerError,
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
use strum::{EnumDiscriminants, IntoStaticStr};

pub const OPA_CONTROLLER_NAME: &str = "opacluster";

pub const CONFIG_FILE: &str = "config.yaml";
pub const APP_PORT: u16 = 8081;
pub const APP_PORT_NAME: &str = "http";
pub const METRICS_PORT_NAME: &str = "metrics";
pub const BUNDLES_ACTIVE_DIR: &str = "/bundles/active";
pub const BUNDLES_INCOMING_DIR: &str = "/bundles/incoming";
pub const BUNDLES_TMP_DIR: &str = "/bundles/tmp";
pub const BUNDLE_BUILDER_PORT: i32 = 3030;

const DOCKER_IMAGE_BASE_NAME: &str = "opa";
const DOCKER_BUNDLE_BUILDER_IMAGE_BASE_NAME: &str = "opa-bundle-builder";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
    pub opa_bundle_builder_clusterrole: String,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("failed to calculate role service name"))]
    RoleServiceNameNotFound,
    #[snafu(display("failed to apply role Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to apply role ServiceAccount"))]
    ApplyRoleServiceAccount {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to apply global RoleBinding"))]
    ApplyRoleRoleBinding {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to apply Service for {}", rolegroup))]
    ApplyRoleGroupService {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },
    #[snafu(display("failed to build ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },
    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
    },
    #[snafu(display("failed to apply DaemonSet for {}", rolegroup))]
    ApplyRoleGroupDaemonSet {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<OpaCluster>,
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
    #[snafu(display("failed to resolve and merge resource config for role and role group"))]
    FailedToResolveResourceConfig { source: stackable_opa_crd::Error },
}
type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_opa(opa: Arc<OpaCluster>, ctx: Arc<Ctx>) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let opa_ref = ObjectRef::from_obj(&*opa);
    let client = ctx.client.clone();
    let resolved_product_image = opa.spec.image.resolve(DOCKER_IMAGE_BASE_NAME);
    let resolved_bundle_builder_image = opa
        .spec
        .bundle_builder_image
        .resolve(DOCKER_BUNDLE_BUILDER_IMAGE_BASE_NAME);

    let validated_config = validate_all_roles_and_groups_config(
        &resolved_product_image.product_version,
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
        &ctx.product_config,
        false,
        false,
    )
    .context(InvalidProductConfigSnafu)?;
    let role_server_config = validated_config
        .get(&OpaRole::Server.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let server_role_service = build_server_role_service(&opa, &resolved_product_image)?;
    let server_role_service = client
        .apply_patch(
            OPA_CONTROLLER_NAME,
            &server_role_service,
            &server_role_service,
        )
        .await
        .context(ApplyRoleServiceSnafu)?;

    let (opa_builder_role_serviceaccount, opa_builder_role_rolebinding) =
        build_opa_builder_serviceaccount(
            &opa,
            &resolved_product_image,
            &ctx.opa_bundle_builder_clusterrole,
        )?;

    client
        .apply_patch(
            OPA_CONTROLLER_NAME,
            &opa_builder_role_serviceaccount,
            &opa_builder_role_serviceaccount,
        )
        .await
        .context(ApplyRoleServiceAccountSnafu)?;
    client
        .apply_patch(
            OPA_CONTROLLER_NAME,
            &opa_builder_role_rolebinding,
            &opa_builder_role_rolebinding,
        )
        .await
        .context(ApplyRoleRoleBindingSnafu)?;

    for (rolegroup_name, rolegroup_config) in role_server_config.iter() {
        let rolegroup = RoleGroupRef {
            cluster: opa_ref.clone(),
            role: OpaRole::Server.to_string(),
            role_group: rolegroup_name.to_string(),
        };

        let resources = opa
            .resolve_resource_config_for_role_and_rolegroup(&OpaRole::Server, &rolegroup)
            .context(FailedToResolveResourceConfigSnafu)?;

        let rg_configmap =
            build_server_rolegroup_config_map(&opa, &resolved_product_image, &rolegroup)?;
        let rg_daemonset = build_server_rolegroup_daemonset(
            &opa,
            &resolved_product_image,
            &resolved_bundle_builder_image,
            &rolegroup,
            rolegroup_config,
            &resources,
        )?;
        let rg_service = build_rolegroup_service(&opa, &resolved_product_image, &rolegroup)?;

        client
            .apply_patch(OPA_CONTROLLER_NAME, &rg_configmap, &rg_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(OPA_CONTROLLER_NAME, &rg_daemonset, &rg_daemonset)
            .await
            .with_context(|_| ApplyRoleGroupDaemonSetSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(OPA_CONTROLLER_NAME, &rg_service, &rg_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup.clone(),
            })?;
    }

    for discovery_cm in
        build_discovery_configmaps(&*opa, &*opa, &resolved_product_image, &server_role_service)
            .context(BuildDiscoveryConfigSnafu)?
    {
        client
            .apply_patch(OPA_CONTROLLER_NAME, &discovery_cm, &discovery_cm)
            .await
            .context(ApplyDiscoveryConfigSnafu)?;
    }

    Ok(Action::await_change())
}

fn build_opa_builder_serviceaccount(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    opa_bundle_builder_clusterrole: &str,
) -> Result<(ServiceAccount, RoleBinding)> {
    let role_name = OpaRole::Server.to_string();
    let sa_name = format!("{}-{}", opa.metadata.name.as_ref().unwrap(), role_name);
    let sa = ServiceAccount {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(&sa_name)
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                opa,
                &resolved_product_image.app_version_label,
                &role_name,
                "global",
            ))
            .build(),
        ..ServiceAccount::default()
    };
    let binding_name = &sa_name;
    let binding = RoleBinding {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(opa)
            .name(binding_name)
            .ownerreference_from_resource(opa, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                opa,
                &resolved_product_image.app_version_label,
                &role_name,
                "global",
            ))
            .build(),
        role_ref: RoleRef {
            api_group: ClusterRole::GROUP.to_string(),
            kind: ClusterRole::KIND.to_string(),
            name: opa_bundle_builder_clusterrole.to_string(),
        },
        subjects: Some(vec![Subject {
            api_group: Some(ServiceAccount::GROUP.to_string()),
            kind: ServiceAccount::KIND.to_string(),
            name: sa_name,
            namespace: sa.metadata.namespace.clone(),
        }]),
    };
    Ok((sa, binding))
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
            ports: Some(vec![ServicePort {
                name: Some(APP_PORT_NAME.to_string()),
                port: APP_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(role_selector_labels(opa, APP_NAME, &role_name)),
            type_: Some("NodePort".to_string()),
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
) -> Result<ConfigMap> {
    ConfigMapBuilder::new()
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
        .add_data(CONFIG_FILE, build_config_file())
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
fn build_server_rolegroup_daemonset(
    opa: &OpaCluster,
    resolved_product_image: &ResolvedProductImage,
    resolved_bundle_builder_image: &ResolvedProductImage,
    rolegroup_ref: &RoleGroupRef<OpaCluster>,
    server_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    resources: &Resources<OpaStorageConfig, NoRuntimeLimits>,
) -> Result<DaemonSet> {
    let sa_name = format!(
        "{}-{}",
        opa.metadata.name.as_ref().unwrap(),
        rolegroup_ref.role
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
        .expect("invalid hard-coded container name")
        .image_from_product_image(resolved_product_image)
        .command(build_opa_start_command())
        .add_env_vars(env)
        .add_container_port(APP_PORT_NAME, APP_PORT.into())
        .add_volume_mount("config", "/stackable/config")
        .resources(resources.clone().into())
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
        })
        .build();

    let container_bundle_builder = ContainerBuilder::new("opa-bundle-builder")
        .expect("invalid hard-coded container name")
        // TODO: use image_from_product_image as soon as the bundle builder versioning is fixed
        //.image_from_product_image(resolved_bundle_builder_image)
        .image(extract_image_for_bundle_builder(
            &resolved_bundle_builder_image.image,
        ))
        .image_pull_policy(&resolved_bundle_builder_image.image_pull_policy)
        .command(vec![String::from("/stackable-opa-bundle-builder")])
        .add_env_var_from_field_path("WATCH_NAMESPACE", FieldPathEnvVar::Namespace)
        .add_volume_mount("bundles", "/bundles")
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
        })
        .build();

    let init_container = ContainerBuilder::new("init-container")
        .expect("invalid hard-coded container name")
        // TODO: use image_from_product_image as soon as the bundle builder versioning is fixed
        //.image_from_product_image(resolved_bundle_builder_image)
        .image(extract_image_for_bundle_builder(
            &resolved_bundle_builder_image.image,
        ))
        .image_pull_policy(&resolved_bundle_builder_image.image_pull_policy)
        .command(vec!["bash".to_string()])
        .args(vec![
            "-euo".to_string(),
            "pipefail".to_string(),
            "-x".to_string(),
            "-c".to_string(),
            [
                format!("mkdir -p {BUNDLES_ACTIVE_DIR}"),
                format!("mkdir -p {BUNDLES_INCOMING_DIR}"),
                format!("mkdir -p {BUNDLES_TMP_DIR}"),
            ]
            .join(" && "),
        ])
        .add_volume_mount("bundles", "/bundles")
        .build();

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
            template: PodBuilder::new()
                .metadata_builder(|m| {
                    m.with_recommended_labels(build_recommended_labels(
                        opa,
                        &resolved_product_image.app_version_label,
                        &rolegroup_ref.role,
                        &rolegroup_ref.role_group,
                    ))
                })
                .add_init_container(init_container)
                .add_container(container_opa)
                .add_container(container_bundle_builder)
                .image_pull_secrets_from_product_image(&resolved_product_image)
                // This will merge / extend existing secrets from "resolved_product_image"
                .image_pull_secrets_from_product_image(&resolved_bundle_builder_image)
                .add_volume(Volume {
                    name: "config".to_string(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: Some(rolegroup_ref.object_name()),
                        ..ConfigMapVolumeSource::default()
                    }),
                    ..Volume::default()
                })
                .add_volume(Volume {
                    name: "bundles".to_string(),
                    empty_dir: Some(EmptyDirVolumeSource::default()),
                    ..Volume::default()
                })
                .service_account_name(sa_name)
                .security_context(PodSecurityContext {
                    run_as_user: Some(1000),
                    run_as_group: Some(1000),
                    fs_group: Some(1000),
                    ..PodSecurityContext::default()
                })
                .build_template(),
            ..DaemonSetSpec::default()
        }),
        status: None,
    })
}

pub fn error_policy(_obj: Arc<OpaCluster>, _error: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(5))
}

fn build_config_file() -> &'static str {
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
      max_delay_seconds: 20"
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

// This is a workaround until the OPA bundle builder adheres to the stackable versioning
fn extract_image_for_bundle_builder(image: &str) -> &str {
    // Remove the -stackablex.x.x version
    // TODO: Improve. This will fail if a custom repo or anything else contains "-stackable"
    image.split("-stackable").next().unwrap_or(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_image_for_bundle_builder() {
        let stackable_image =
            "docker.stackable.tech/stackable/opa-bundle-builder:0.11.0-stackable0.1.0";
        assert_eq!(
            "docker.stackable.tech/stackable/opa-bundle-builder:0.11.0",
            extract_image_for_bundle_builder(stackable_image)
        );

        let custom_image = "other.company.tech/company/bundle-builder:0.11.0";
        assert_eq!(
            "other.company.tech/company/bundle-builder:0.11.0",
            extract_image_for_bundle_builder(custom_image)
        );

        let nightly_image = "docker.stackable.tech/stackable/opa-bundle-builder:0.11.0-nightly";
        assert_eq!(
            "docker.stackable.tech/stackable/opa-bundle-builder:0.11.0-nightly",
            extract_image_for_bundle_builder(nightly_image)
        );
    }
}
