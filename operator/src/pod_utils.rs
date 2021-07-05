use std::collections::BTreeMap;

use stackable_operator::labels;

use crate::error::Error;
use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, Container, Pod, PodSpec, Volume, VolumeMount,
};
use stackable_opa_crd::{OpaConfig, APP_NAME, MANAGED_BY};

/// Provide required labels for pods. We need to keep track of which workers are
/// connected to which masters. This is accomplished by hashing known master urls
/// and comparing to the pods. If the hash from pod and selector differ, that means
/// we had changes (added / removed) masters and therefore restart the workers.
/// Furthermore labels for component, role group, instance and version are provided.
///
/// # Arguments
/// * `role` - The cluster role (e.g. master, worker, history-server)
/// * `role_group` - The role group of the selector
/// * `cluster_name` - The name of the cluster as specified in the custom resource
/// * `version` - The current cluster version
///
pub fn build_labels(
    role: &str,
    role_group: &str,
    cluster_name: &str,
    version: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(String::from(labels::APP_NAME_LABEL), APP_NAME.to_string());
    labels.insert(
        labels::APP_MANAGED_BY_LABEL.to_string(),
        MANAGED_BY.to_string(),
    );
    labels.insert(String::from(labels::APP_COMPONENT_LABEL), role.to_string());
    labels.insert(
        String::from(labels::APP_ROLE_GROUP_LABEL),
        role_group.to_string(),
    );
    labels.insert(
        String::from(labels::APP_INSTANCE_LABEL),
        cluster_name.to_string(),
    );

    labels.insert(labels::APP_VERSION_LABEL.to_string(), version.to_string());

    labels
}

fn build_pod(
    resource: &OpenPolicyAgent,
    node: &str,
    labels: &BTreeMap<String, String>,
    pod_name: &str,
    cm_name: &str,
    config: &OpaConfig,
) -> Result<Pod, Error> {
    let pod = Pod {
        metadata: metadata::build_metadata(
            pod_name.to_string(),
            Some(labels.clone()),
            resource,
            false,
        )?,
        spec: Some(PodSpec {
            node_name: Some(node.to_string()),
            tolerations: stackable_operator::krustlet::create_tolerations(),
            containers: vec![Container {
                image: Some(format!("opa:{}", resource.spec.version.to_string())),
                name: "opa".to_string(),
                command: create_opa_start_command(config),
                volume_mounts: vec![VolumeMount {
                    mount_path: "config".to_string(),
                    name: "config-volume".to_string(),
                    ..VolumeMount::default()
                }],
                ..Container::default()
            }],
            volumes: vec![Volume {
                name: "config-volume".to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: Some(cm_name.to_string()),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            }],
            ..PodSpec::default()
        }),
        ..Pod::default()
    };
    Ok(pod)
}

fn create_opa_start_command(config: &OpaConfig) -> Vec<String> {
    let mut command = vec![String::from("./opa run")];

    // --server
    command.push("-s".to_string());

    if let Some(port) = config.port {
        // --addr
        command.push(format!("-a 0.0.0.0:{}", port.to_string()))
    }

    // --config-file
    command.push("-c {{configroot}}/config/config.yaml".to_string());

    command
}
