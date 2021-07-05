mod error;
mod pod_utils;

use crate::error::Error;
use crate::error::Error::RoleGroupMissing;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, Node, Pod, PodSpec, Volume, VolumeMount,
};
use kube::api::ListParams;
use kube::Api;
use product_config::ProductConfigManager;
use serde::{Deserialize, Serialize};
use stackable_opa_crd::{OpaConfig, OpaRole, OpaSpec, OpenPolicyAgent, APP_NAME, MANAGED_BY};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::k8s_utils::LabelOptionalValueMap;
use stackable_operator::labels::{
    APP_COMPONENT_LABEL, APP_INSTANCE_LABEL, APP_MANAGED_BY_LABEL, APP_NAME_LABEL,
    APP_ROLE_GROUP_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::{
    get_role_and_group_labels, list_eligible_nodes_for_role_and_group, RoleGroup,
};
use stackable_operator::{k8s_utils, role_utils};
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use strum::IntoEnumIterator;
use tracing::{debug, info, trace, warn};

type OpaReconcileResult = ReconcileResult<error::Error>;

struct OpaState {
    context: ReconciliationContext<OpenPolicyAgent>,
    existing_pods: Vec<Pod>,
    eligible_nodes: HashMap<String, HashMap<String, Vec<Node>>>,
    validated_role_config: ValidatedRoleConfigByPropertyKind,
}

impl OpaState {
    pub fn deletion_labels(&self) -> BTreeMap<String, Option<Vec<String>>> {
        let roles = OpaRole::iter()
            .map(|role| role.to_string())
            .collect::<Vec<_>>();
        let mut mandatory_labels = BTreeMap::new();

        mandatory_labels.insert(String::from(APP_COMPONENT_LABEL), Some(roles));
        mandatory_labels.insert(
            String::from(APP_INSTANCE_LABEL),
            Some(vec![self.context.name()]),
        );
        mandatory_labels.insert(
            APP_VERSION_LABEL.to_string(),
            Some(vec![self.context.resource.spec.version.to_string()]),
        );
        mandatory_labels
    }

    async fn create_config_map(
        &self,
        name: &str,
        role_group: &str,
        labels: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        let config = match self.context.resource.spec.servers.selectors.get(role_group) {
            Some(selector) => &selector.config,
            None => {
                return Err(RoleGroupMissing {
                    role_group: role_group.to_string(),
                })
            }
        };

        let config = create_config_file(config);

        let mut data = BTreeMap::new();
        data.insert("config.yaml".to_string(), config);

        // And now create the actual ConfigMap
        // TODO: use builder
        let mut config_map = ConfigMap::default();
        /*stackable_operator::config_map::create_config_map(
            &self.context.resource,
            &name,
            data.clone(),
        )?;*/

        // add required labels
        config_map.metadata.labels = labels.clone();

        match self
            .context
            .client
            .get::<ConfigMap>(name, Some(&self.context.namespace()))
            .await
        {
            Ok(existing_config_map) => {
                if let Some(existing_config_map_data) = existing_config_map.data {
                    if existing_config_map_data == data {
                        debug!(
                            "ConfigMap [{}] already exists with identical data, skipping creation!",
                            name
                        );
                    } else {
                        debug!(
                            "ConfigMap [{}] already exists, but differs, recreating it!",
                            name
                        );
                        self.context.client.update(&config_map).await?;
                    }
                }
            }
            Err(e) => {
                // TODO: This is shit, but works for now. If there is an actual error in comes with
                //   K8S, it will most probably also occur further down and be properly handled
                debug!("Error getting ConfigMap [{}]: [{:?}]", name, e);
                self.context.client.create(&config_map).await?;
            }
        }

        Ok(())
    }

    async fn create_missing_pods(&mut self) -> OpaReconcileResult {
        // The iteration happens in two stages here, to accommodate the way our operators think
        // about nodes and roles.
        // The hierarchy is:
        // - Roles (for example Datanode, Namenode, Opa Server)
        //   - Role groups for this role (user defined)
        //      - Individual nodes
        for role in OpaRole::iter() {
            if let Some(nodes_for_role) = self.eligible_nodes.get(&role) {
                let role_str = &role.to_string();
                for (role_group, nodes) in nodes_for_role {
                    debug!(
                        "Identify missing pods for [{}] role and group [{}]",
                        role_str, role_group
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
                        get_role_and_group_labels(role_str, role_group)
                    );
                    let nodes_that_need_pods = k8s_utils::find_nodes_that_need_pods(
                        nodes,
                        &self.existing_pods,
                        &get_role_and_group_labels(role_str, role_group),
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
                            role_str,
                            role_group
                        );

                        // Create a pod for this node, role and group combination
                        let pod = build_pod(
                            &self.context.resource,
                            node_name,
                            &labels,
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

        Box::pin(async move {
            self.context
                .delete_illegal_pods(
                    self.existing_pods.as_slice(),
                    &self.deletion_labels(),
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
                        .wait_for_running_and_ready_pods(&self.existing_pods.as_slice()),
                )
                .await?
                .then(self.context.delete_excess_pods(
                    list_eligible_nodes_for_role_and_group(&self.eligible_nodes).as_slice(),
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
struct OpaStrategy {
    config: Arc<ProductConfigManager>,
}

impl OpaStrategy {
    pub fn new(config: ProductConfigManager) -> OpaStrategy {
        OpaStrategy {
            config: Arc::new(config),
        }
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

        let mut eligible_nodes = HashMap::new();

        eligible_nodes.insert(
            OpaRole::Server.to_string(),
            role_utils::find_nodes_that_fit_selectors(
                &context.client,
                None,
                &context.resource.servers,
            )
            .await?,
        );

        Ok(OpaState {
            validated_role_config: validated_product_config(&context.resource, &self.config)?,
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

    let product_config =
        ProductConfigManager::from_yaml_file("deploy/config-spec/properties.yaml").unwrap();
    let strategy = OpaStrategy::new(product_config);

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;
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
