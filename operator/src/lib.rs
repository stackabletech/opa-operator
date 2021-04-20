mod error;
use crate::error::Error;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, Node, Pod, PodSpec, Volume, VolumeMount,
};
use kube::api::ListParams;
use kube::Api;
use stackable_opa_crd::{OpaConfig, OpaSpec, OpenPolicyAgent};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::k8s_utils::LabelOptionalValueMap;
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::RoleGroup;
use stackable_operator::{k8s_utils, metadata, role_utils};
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::time::Duration;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::EnumIter;
use tracing::{debug, info, trace, warn};

pub const CLUSTER_NAME_LABEL: &str = "app.kubernetes.io/instance";
pub const NODE_GROUP_LABEL: &str = "authz.stackable.tech/node-group-name";
pub const NODE_TYPE_LABEL: &str = "authz.stackable.tech/node-type";

type OpaReconcileResult = ReconcileResult<error::Error>;

#[derive(EnumIter, Debug, Display, PartialEq, Eq, Hash)]
pub enum OpaNodeType {
    Server,
}

struct OpaState {
    context: ReconciliationContext<OpenPolicyAgent>,
    existing_pods: Vec<Pod>,
    eligible_nodes: HashMap<OpaNodeType, HashMap<String, Vec<Node>>>,
}

impl OpaState {
    pub fn get_full_pod_node_map(&self) -> Vec<(Vec<Node>, LabelOptionalValueMap)> {
        let mut eligible_nodes_map = vec![];
        debug!(
            "Looking for excess pods that need to be deleted for cluster [{}]",
            self.context.name()
        );
        for node_type in OpaNodeType::iter() {
            if let Some(eligible_nodes_for_role) = self.eligible_nodes.get(&node_type) {
                for (group_name, eligible_nodes) in eligible_nodes_for_role {
                    // Create labels to identify eligible nodes
                    trace!(
                        "Adding [{}] nodes to eligible node list for role [{}] and group [{}].",
                        eligible_nodes.len(),
                        node_type,
                        group_name
                    );
                    eligible_nodes_map.push((
                        eligible_nodes.clone(),
                        get_node_and_group_labels(group_name, &node_type),
                    ))
                }
            }
        }
        eligible_nodes_map
    }

    pub fn get_deletion_labels(&self) -> BTreeMap<String, Option<Vec<String>>> {
        let roles = OpaNodeType::iter()
            .map(|role| role.to_string())
            .collect::<Vec<_>>();
        let mut mandatory_labels = BTreeMap::new();

        mandatory_labels.insert(String::from(NODE_TYPE_LABEL), Some(roles));
        mandatory_labels.insert(String::from(CLUSTER_NAME_LABEL), None);
        mandatory_labels
    }

    async fn create_config_map(&self, name: &str, config: &OpaConfig) -> Result<(), Error> {
        match self
            .context
            .client
            .get::<ConfigMap>(name, Some(&"default".to_string()))
            .await
        {
            Ok(_) => {
                debug!("ConfigMap [{}] already exists, skipping creation!", name);
                return Ok(());
            }
            Err(e) => {
                // TODO: This is shit, but works for now. If there is an actual error in comms with
                //   K8S, it will most probably also occur further down and be properly handled
                debug!("Error getting ConfigMap [{}]: [{:?}]", name, e);
            }
        }
        let config = create_config_file(config);

        let mut data = BTreeMap::new();
        data.insert("config.yaml".to_string(), config);

        // And now create the actual ConfigMap
        let cm =
            stackable_operator::config_map::create_config_map(&self.context.resource, &name, data)?;
        self.context.client.create(&cm).await?;
        Ok(())
    }

    async fn create_missing_pods(&mut self) -> OpaReconcileResult {
        // The iteration happens in two stages here, to accommodate the way our operators think
        // about nodes and roles.
        // The hierarchy is:
        // - Roles (for example Datanode, Namenode, Opa Server)
        //   - Node groups for this role (user defined)
        //      - Individual nodes
        for node_type in OpaNodeType::iter() {
            if let Some(nodes_for_role) = self.eligible_nodes.get(&node_type) {
                for (role_group, nodes) in nodes_for_role {
                    // extract selector for opa config
                    let opa_config = match self
                        .context
                        .resource
                        .spec
                        .servers
                        .selectors
                        .get(role_group)
                    {
                        Some(selector) => &selector.config,
                        None => {
                            warn!(
                                    "No config found in selector for role [{}], this should not happen!",
                                    &role_group
                                );
                            continue;
                        }
                    };

                    // Create config map for this rolegroup
                    let pod_name =
                        format!("opa-{}-{}-{}", self.context.name(), role_group, node_type)
                            .to_lowercase();

                    let cm_name = format!("{}-config", pod_name);
                    debug!("pod_name: [{}], cm_name: [{}]", pod_name, cm_name);

                    self.create_config_map(&cm_name, opa_config).await?;

                    debug!("pod_name: [{}]", pod_name);
                    debug!(
                        "Identify missing pods for [{}] role and group [{}]",
                        node_type, role_group
                    );
                    trace!(
                        "candidate_nodes[{}]: [{:?}]",
                        nodes.len(),
                        nodes
                            .iter()
                            .map(|node| node.metadata.name.as_ref().unwrap())
                            .collect::<Vec<_>>()
                    );
                    trace!(
                        "existing_pods[{}]: [{:?}]",
                        &self.existing_pods.len(),
                        &self
                            .existing_pods
                            .iter()
                            .map(|pod| pod.metadata.name.as_ref().unwrap())
                            .collect::<Vec<_>>()
                    );
                    trace!(
                        "labels: [{:?}]",
                        get_node_and_group_labels(role_group, &node_type)
                    );
                    let nodes_that_need_pods = k8s_utils::find_nodes_that_need_pods(
                        nodes,
                        &self.existing_pods,
                        &get_node_and_group_labels(role_group, &node_type),
                    );

                    for node in nodes_that_need_pods {
                        let node_name = if let Some(node_name) = &node.metadata.name {
                            node_name
                        } else {
                            warn!("No name found in metadata, this should not happen! Skipping node: [{:?}]", node);
                            continue;
                        };
                        debug!(
                            "Creating pod on node [{}] for [{}] role and group [{}]",
                            node.metadata
                                .name
                                .as_deref()
                                .unwrap_or("<no node name found>"),
                            node_type,
                            role_group
                        );

                        let mut node_labels = BTreeMap::new();
                        node_labels.insert(String::from(NODE_TYPE_LABEL), node_type.to_string());
                        node_labels
                            .insert(String::from(NODE_GROUP_LABEL), String::from(role_group));
                        node_labels.insert(String::from(CLUSTER_NAME_LABEL), self.context.name());

                        // Create a pod for this node, role and group combination
                        let pod = build_pod(
                            &self.context.resource,
                            node_name,
                            &node_labels,
                            &pod_name,
                            &cm_name,
                            opa_config,
                        )?;
                        self.context.client.create(&pod).await?;
                    }
                }
            }
        }
        Ok(ReconcileFunctionAction::Continue)
    }
}

impl ReconciliationState for OpaState {
    type Error = error::Error;

    fn reconcile(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<ReconcileFunctionAction, Self::Error>> + Send + '_>>
    {
        info!("========================= Starting reconciliation =========================");
        debug!("Deletion Labels: [{:?}]", &self.get_deletion_labels());

        Box::pin(async move {
            self.context
                .delete_illegal_pods(
                    self.existing_pods.as_slice(),
                    &self.get_deletion_labels(),
                    ContinuationStrategy::OneRequeue,
                )
                .await?
                .then(
                    self.context
                        .wait_for_terminating_pods(self.existing_pods.as_slice()),
                )
                .await?
                .then(
                    self.context
                        .wait_for_running_and_ready_pods(&self.existing_pods),
                )
                .await?
                .then(self.context.delete_excess_pods(
                    self.get_full_pod_node_map().as_slice(),
                    &self.existing_pods,
                    ContinuationStrategy::OneRequeue,
                ))
                .await?
                .then(self.create_missing_pods())
                .await
        })
    }
}

#[derive(Debug)]
struct OpaStrategy {}

impl OpaStrategy {
    pub fn new() -> OpaStrategy {
        OpaStrategy {}
    }
}

#[async_trait]
impl ControllerStrategy for OpaStrategy {
    type Item = OpenPolicyAgent;
    type State = OpaState;
    type Error = error::Error;

    async fn init_reconcile_state(
        &self,
        context: ReconciliationContext<Self::Item>,
    ) -> Result<Self::State, Self::Error> {
        let existing_pods = context.list_pods().await?;
        trace!("Found [{}] pods", existing_pods.len());

        let opa_spec: OpaSpec = context.resource.spec.clone();

        let mut eligible_nodes = HashMap::new();

        let role_groups: Vec<RoleGroup> = opa_spec
            .servers
            .selectors
            .iter()
            .map(|(group_name, selector_config)| RoleGroup {
                name: group_name.to_string(),
                selector: selector_config.clone().selector.unwrap(),
            })
            .collect();

        eligible_nodes.insert(
            OpaNodeType::Server,
            role_utils::find_nodes_that_fit_selectors(
                &context.client,
                None,
                role_groups.as_slice(),
            )
            .await?,
        );

        Ok(OpaState {
            context,
            existing_pods,
            eligible_nodes,
        })
    }
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(client: Client) {
    let opa_api: Api<OpenPolicyAgent> = client.get_all_api();
    let pods_api: Api<Pod> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();

    let controller = Controller::new(opa_api)
        .owns(pods_api, ListParams::default())
        .owns(configmaps_api, ListParams::default());

    let strategy = OpaStrategy::new();

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;
}

fn get_node_and_group_labels(group_name: &str, node_type: &OpaNodeType) -> LabelOptionalValueMap {
    let mut node_labels = BTreeMap::new();
    node_labels.insert(String::from(NODE_TYPE_LABEL), Some(node_type.to_string()));
    node_labels.insert(
        String::from(NODE_GROUP_LABEL),
        Some(String::from(group_name)),
    );
    node_labels
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
            tolerations: Some(stackable_operator::krustlet::create_tolerations()),
            containers: vec![Container {
                image: Some(format!("opa:{}", resource.spec.version.to_string())),
                name: "opa".to_string(),
                command: Some(create_opa_start_command(config)),
                volume_mounts: Some(vec![VolumeMount {
                    mount_path: "config".to_string(),
                    name: "config-volume".to_string(),
                    ..VolumeMount::default()
                }]),
                ..Container::default()
            }],
            volumes: Some(vec![Volume {
                name: "config-volume".to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: Some(cm_name.to_string()),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            }]),
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

fn create_config_file(config: &OpaConfig) -> String {
    format!(
        "services:
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
        config.repo_rule_reference
    )
}
