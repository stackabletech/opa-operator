mod error;

use crate::error::Error;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{ConfigMap, EnvVar, Node, Pod};
use kube::api::ListParams;
use kube::error::ErrorResponse;
use kube::Api;
use kube::ResourceExt;
use product_config::types::PropertyNameKind;
use product_config::ProductConfigManager;
use stackable_opa_crd::{
    OpaRole, OpenPolicyAgent, APP_NAME, CONFIG_FILE, PORT, REPO_RULE_REFERENCE,
};
use stackable_operator::builder::{
    ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder,
};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::error::OperatorResult;
use stackable_operator::labels::{
    build_common_labels_for_all_managed_resources, get_recommended_labels, APP_COMPONENT_LABEL,
    APP_INSTANCE_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::product_config_utils::{
    config_for_role_and_group, transform_all_roles_to_config, validate_all_roles_and_groups_config,
    ValidatedRoleConfigByPropertyKind,
};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::{
    get_role_and_group_labels, list_eligible_nodes_for_role_and_group,
};
use stackable_operator::{k8s_utils, pod_utils, role_utils};
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
    pub fn required_pod_labels(&self) -> BTreeMap<String, Option<Vec<String>>> {
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

    /// Create or update a config map.
    /// - Create if no config map of that name exists
    /// - Update if config map exists but the content differs
    /// - Do nothing if the config map exists and the content is identical
    async fn create_config_map(&self, config_map: ConfigMap) -> Result<(), Error> {
        let cm_name = match config_map.metadata.name.as_deref() {
            None => return Err(Error::InvalidConfigMap),
            Some(name) => name,
        };

        match self
            .context
            .client
            .get::<ConfigMap>(cm_name, Some(&self.context.namespace()))
            .await
        {
            Ok(ConfigMap {
                data: existing_config_map_data,
                ..
            }) if existing_config_map_data == config_map.data => {
                debug!(
                    "ConfigMap [{}] already exists with identical data, skipping creation!",
                    cm_name
                );
            }
            Ok(_) => {
                debug!(
                    "ConfigMap [{}] already exists, but differs, updating it!",
                    cm_name
                );
                self.context.client.update(&config_map).await?;
            }
            Err(stackable_operator::error::Error::KubeError {
                source: kube::error::Error::Api(ErrorResponse { reason, .. }),
            }) if reason == "NotFound" => {
                debug!("Error getting ConfigMap [{}]: [{:?}]", cm_name, reason);
                self.context.client.create(&config_map).await?;
            }
            Err(e) => return Err(Error::OperatorError { source: e }),
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
            if let Some(nodes_for_role) = self.eligible_nodes.get(&role.to_string()) {
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

                        let (pod, config_maps) = self
                            .create_pod_and_config_maps(
                                &role,
                                role_group,
                                &node_name,
                                config_for_role_and_group(
                                    role_str,
                                    role_group,
                                    &self.validated_role_config,
                                )?,
                            )
                            .await?;

                        self.context.client.create(&pod).await?;

                        for config_map in config_maps {
                            self.create_config_map(config_map).await?;
                        }
                    }
                }
            }
        }
        Ok(ReconcileFunctionAction::Continue)
    }

    /// This method creates a pod and required config map(s) for a certain role and role_group.
    /// The validated_config from the product-config is used to create the config map data, as
    /// well as setting the ENV variables in the containers or adapt / expand the CLI parameters.
    /// First we iterate through the validated_config and extract files (which represents one or
    /// more config map(s)), env variables for the pod containers and cli parameters for the
    /// container start command and arguments.
    async fn create_pod_and_config_maps(
        &self,
        role: &OpaRole,
        role_group: &str,
        node_name: &str,
        validated_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    ) -> Result<(Pod, Vec<ConfigMap>), Error> {
        let mut config_maps = vec![];
        let mut env_vars = vec![];
        let mut start_command = vec![];

        let pod_name = pod_utils::get_pod_name(
            APP_NAME,
            &self.context.name(),
            role_group,
            &role.to_string(),
            node_name,
        );

        let cm_name = format!("{}-config", pod_name);
        let mut cm_data = BTreeMap::new();

        for (property_name_kind, config) in validated_config {
            match property_name_kind {
                // we collect the data for the config map here and build it later
                PropertyNameKind::File(file_name) => {
                    if file_name.as_str() == CONFIG_FILE {
                        if let Some(repo_reference) = config.get(REPO_RULE_REFERENCE) {
                            cm_data
                                .insert(file_name.to_string(), build_config_file(repo_reference));
                        }
                    }
                }
                // we collect env variables here and add it to the pod container later
                PropertyNameKind::Env => {
                    for (property_name, property_value) in config {
                        if property_name.is_empty() {
                            warn!("Received empty property_name for ENV... skipping");
                            continue;
                        }

                        env_vars.push(EnvVar {
                            name: property_name.clone(),
                            value: Some(property_value.to_string()),
                            value_from: None,
                        });
                    }
                }
                PropertyNameKind::Cli => {
                    if let Some(port) = config.get(PORT) {
                        start_command = build_opa_start_command(Some(port.clone()));
                    }
                }
            }
        }

        config_maps.push(
            ConfigMapBuilder::new()
                .metadata(
                    ObjectMetaBuilder::new()
                        .name(cm_name.clone())
                        .ownerreference_from_resource(
                            &self.context.resource,
                            Some(true),
                            Some(true),
                        )?
                        .namespace(&self.context.client.default_namespace)
                        .build()?,
                )
                .data(cm_data)
                .build()?,
        );

        let version = &self.context.resource.spec.version.to_string();

        let labels = get_recommended_labels(
            &self.context.resource,
            APP_NAME,
            version,
            &role.to_string(),
            role_group,
        );

        let mut container_builder = ContainerBuilder::new("opa");
        container_builder.image(format!(
            "opa:{}",
            &self.context.resource.spec.version.to_string()
        ));
        container_builder.command(start_command);
        container_builder.add_configmapvolume(cm_name, "conf".to_string());

        for env in env_vars {
            if let Some(val) = env.value {
                container_builder.add_env_var(env.name, val);
            }
        }

        let pod = PodBuilder::new()
            .metadata(
                ObjectMetaBuilder::new()
                    .name(pod_name)
                    .namespace(&self.context.client.default_namespace)
                    .with_labels(labels)
                    .ownerreference_from_resource(&self.context.resource, Some(true), Some(true))?
                    .build()?,
            )
            .add_stackable_agent_tolerations()
            .add_container(container_builder.build())
            .node_name(node_name)
            .build()?;

        Ok((pod, config_maps))
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
                    &self.required_pod_labels(),
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
        let existing_pods = context
            .list_owned(build_common_labels_for_all_managed_resources(
                APP_NAME,
                &context.resource.name(),
            ))
            .await?;
        trace!("Found [{}] pods", existing_pods.len());

        let mut eligible_nodes = HashMap::new();

        eligible_nodes.insert(
            OpaRole::Server.to_string(),
            role_utils::find_nodes_that_fit_selectors(
                &context.client,
                None,
                &context.resource.spec.servers,
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

/// Validates the provided custom resource configuration fpr the provided roles with the
/// product-config.
pub fn validated_product_config(
    resource: &OpenPolicyAgent,
    product_config: &ProductConfigManager,
) -> OperatorResult<ValidatedRoleConfigByPropertyKind> {
    let mut roles = HashMap::new();
    roles.insert(
        OpaRole::Server.to_string(),
        (
            vec![
                PropertyNameKind::File(CONFIG_FILE.to_string()),
                PropertyNameKind::Cli,
            ],
            resource.spec.servers.clone().into(),
        ),
    );

    let role_config = transform_all_roles_to_config(resource, roles);

    validate_all_roles_and_groups_config(
        &resource.spec.version.to_string(),
        &role_config,
        &product_config,
        false,
        false,
    )
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

fn build_config_file(repo_rule_reference: &str) -> String {
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
        repo_rule_reference
    )
}

fn build_opa_start_command(port: Option<String>) -> Vec<String> {
    let mut command = vec![String::from("./opa run")];

    // --server
    command.push("-s".to_string());

    if let Some(port) = port {
        // --addr
        command.push(format!("-a 0.0.0.0:{}", port))
    }

    // --config-file
    command.push("-c {{configroot}}/conf/config.yaml".to_string());

    command
}
