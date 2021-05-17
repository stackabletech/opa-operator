use crate::error::Error::{
    ObjectWithoutName, OpaServerMissing, OperatorFrameworkError, PodMissingLabels,
    PodWithoutHostname,
};
use crate::error::OpaOperatorResult;
use crate::util::TicketReferences::ErrOpaPodWithoutName;
use crate::{OpaSpec, OpenPolicyAgent, APP_NAME, MANAGED_BY};
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::client::Client;
use stackable_operator::error::OperatorResult;
use stackable_operator::labels::{
    APP_INSTANCE_LABEL, APP_MANAGED_BY_LABEL, APP_NAME_LABEL, APP_ROLE_GROUP_LABEL,
};
use std::collections::BTreeMap;
use std::string::ToString;
use strum_macros::Display;
use tracing::{debug, warn};
use url::Url;

const OPA_URL_VERSION: &str = "v1";

#[derive(Display)]
pub enum TicketReferences {
    ErrOpaPodWithoutName,
}

/// Contains all necessary information identify a Stackable managed
/// Open Policy Agent (OPA) and build a connection string for it.
/// The main purpose for this struct is for other operators that need to reference
/// an OPA to use in their CRDs.
/// This has the benefit of keeping references to OPA consistent
/// throughout the entire stack.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct OpaReference {
    pub namespace: String,
    pub name: String,
}

/// Helper enum to build urls against OPA API.
/// The OPA rest API consists of 4 endpoints: Policy, Data, Query and Compile.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub enum OpaApi {
    /// e.g. GET /v1/policies/<id>
    Policy { id: String },
    /// e.g. GET /v1/data/<package_path>/<rule>
    Data {
        /// e.g. /kafka/authz
        package_path: String,
        /// e.g. "/allow"
        rule: String,
    },
    /// e.g. GET /v1/query?q=data.servers[i].ports[_] = "p2"; data.servers[i].name = name
    Query { params: BTreeMap<String, String> },
    /// e.g. POST /v1/compile
    Compile {},
}

#[derive(strum_macros::Display, strum_macros::EnumString)]
pub enum OpaApiProtocol {
    #[strum(serialize = "http")]
    HTTP,
    #[strum(serialize = "https")]
    HTTPS,
}

impl OpaApi {
    pub fn get_url(
        &self,
        protocol: &OpaApiProtocol,
        node_name: &str,
        port: u16,
    ) -> OpaOperatorResult<String> {
        let url = match self {
            OpaApi::Policy { id } => {
                format!(
                    "{}://{}:{}/{}/{}/{}",
                    protocol.to_string(),
                    node_name,
                    port,
                    OPA_URL_VERSION,
                    "policies",
                    id
                )
            }
            OpaApi::Data { package_path, rule } => {
                format!(
                    "{}://{}:{}/{}/{}/{}/{}",
                    protocol.to_string(),
                    node_name,
                    port,
                    OPA_URL_VERSION,
                    "data",
                    package_path,
                    rule
                )
            }
            OpaApi::Query { params } => {
                format!(
                    "{}://{}:{}/{}/{}/{}",
                    protocol.to_string(),
                    node_name,
                    port,
                    OPA_URL_VERSION,
                    "query?q=",
                    param_map_to_string(params)
                )
            }
            OpaApi::Compile { .. } => {
                format!(
                    "{}://{}:{}/{}/{}",
                    protocol.to_string(),
                    node_name,
                    port,
                    OPA_URL_VERSION,
                    "compile",
                )
            }
        };

        let parsed_url = Url::parse(&url)?;

        Ok(parsed_url.to_string())
    }
}

/// Contains all necessary information to establish a connection with OPA
/// In contrast to e.g. the ZooKeeper Operator, this will only be one node as reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaConnectionInformation {
    // A connection string as defined by OPA
    // For example:
    //  - http://server1:8181
    //  - https://secure-server:8181/v1/data/some-package/some-rule
    pub connection_string: String,
}

/// Returns connection information for a Open Policy Agent custom resource. In contrast
///
/// # Arguments
///
/// * `client` - A [`stackable_operator::client::Client`] used to access the Kubernetes cluster
/// * `opa_reference` - The name of the OPA custom resource
/// * `opa_api` - The desired OPA endpoint like Data, Query, Policy etc.
/// * `opa_api_protocol` - The desired OPA endpoint protocol like HTTP or HTTPS
/// * `node_name` - If node_name is provided we look for opa deployments on the same node name to improve lookup speed
///                 
pub async fn get_opa_connection_info(
    client: &Client,
    opa_reference: &OpaReference,
    opa_api: &OpaApi,
    opa_api_protocol: &OpaApiProtocol,
    node_name: Option<String>,
) -> OpaOperatorResult<OpaConnectionInformation> {
    let opa_cluster =
        check_opa_reference(client, &opa_reference.name, &opa_reference.namespace).await?;

    let opa_pods = client
        .list_with_label_selector(None, &get_match_labels(&opa_reference.name))
        .await?;

    let connection_string = get_opa_connection_string_from_pods(
        opa_cluster.spec,
        opa_pods,
        opa_api,
        opa_api_protocol,
        node_name,
    )?;

    Ok(OpaConnectionInformation { connection_string })
}

/// Transform the query param map to actual http parameters.
fn param_map_to_string(params: &BTreeMap<String, String>) -> String {
    let params_len = params.len();
    let mut params_as_string = "".to_string();
    let mut count = 0;
    for (key, value) in params {
        // TODO: escape?
        params_as_string.push_str(&format!("{}={}", key, value));

        if count != (params_len - 1) {
            params_as_string.push(';');
        }

        count += 1;
    }

    params_as_string
}

/// Build a label selector that applies only to pods belonging to the cluster instance referenced
/// by `name`
fn get_match_labels(name: &str) -> LabelSelector {
    let mut opa_pod_matchlabels = BTreeMap::new();
    opa_pod_matchlabels.insert(String::from(APP_NAME_LABEL), String::from(APP_NAME));
    opa_pod_matchlabels.insert(String::from(APP_MANAGED_BY_LABEL), String::from(MANAGED_BY));
    opa_pod_matchlabels.insert(String::from(APP_INSTANCE_LABEL), name.to_string());

    LabelSelector {
        match_labels: Some(opa_pod_matchlabels),
        ..Default::default()
    }
}

/// Check in kubernetes, whether the OPA object referenced by `opa_name` and `opa_namespace`
/// exists. If it exists the object will be returned.
async fn check_opa_reference(
    client: &Client,
    opa_name: &str,
    opa_namespace: &str,
) -> OpaOperatorResult<OpenPolicyAgent> {
    debug!(
        "Checking OpaReference if [{}] exists in namespace [{}].",
        opa_name, opa_namespace
    );
    let opa_cluster: OperatorResult<OpenPolicyAgent> =
        client.get(opa_name, Some(opa_namespace)).await;

    opa_cluster.map_err(|err| {
        warn!(?err,
                        "Referencing a OPA that does not exist (or some other error while fetching it): [{}/{}], we will requeue and check again",
                        opa_namespace,
                        opa_name
                    );
        OperatorFrameworkError {source: err}})
}

/// Builds the actual connection string after all necessary information has been retrieved.
/// As best practice and to reduce network traffic and increase response time, if a node_name
/// is provided and matches one of the pod node_name, this node_name is selected. Without a match
/// a random node is returned. TODO: for now the node_names are sorted and the first node is returned.
///
/// # Arguments
///
/// * `opa_spec` - OpaSpec to retrieve different port configuration
/// * `opa_pods` - Pods belonging to the OPA cluster
/// * `opa_api` - The desired OPA endpoint like Data, Query, Policy etc.
/// * `opa_api_protocol` - The desired OPA endpoint protocol like HTTP or HTTPS
/// * `desired_node_name` - If node_name is provided we look for opa deployments on the same node name to improve response time
///
fn get_opa_connection_string_from_pods(
    opa_spec: OpaSpec,
    opa_pods: Vec<Pod>,
    opa_api: &OpaApi,
    opa_api_protocol: &OpaApiProtocol,
    desired_node_name: Option<String>,
) -> OpaOperatorResult<String> {
    let mut server_and_port_list = Vec::new();

    for pod in opa_pods {
        let pod_name = match pod.metadata.name {
            None => {
                return Err(ObjectWithoutName {
                    reference: ErrOpaPodWithoutName.to_string(),
                })
            }
            Some(pod_name) => pod_name,
        };

        let node_name = match pod.spec.and_then(|spec| spec.node_name) {
            None => {
                debug!("Pod [{:?}] is does not have node_name set, might not be scheduled yet, aborting.. ",
                       pod_name);
                return Err(PodWithoutHostname { pod: pod_name });
            }
            Some(node_name) => node_name,
        };

        let role_group = match pod
            .metadata
            .labels
            .unwrap_or_default()
            .get(APP_ROLE_GROUP_LABEL)
        {
            None => {
                return Err(PodMissingLabels {
                    labels: vec![String::from(APP_ROLE_GROUP_LABEL)],
                    pod: pod_name,
                })
            }
            Some(role_group) => role_group.to_owned(),
        };

        let opa_port = get_opa_port(&opa_spec, &role_group)?;

        // if a node_name is provided we prefer OPA deployments that are located on the same machine
        if let Some(desired_host) = &desired_node_name {
            if &node_name == desired_host {
                let url = opa_api.get_url(opa_api_protocol, &node_name, opa_port)?;
                debug!(
                    "Found Opa deployment on provided node [{}]; Using this one [{}] ...",
                    node_name, url
                );
                return Ok(url);
            }
        }

        server_and_port_list.push((node_name, opa_port));
    }

    // Sort list by hostname to make resulting connection strings predictable
    // Shouldn't matter for connectivity but makes testing easier and avoids unnecessary
    // changes to the infrastructure
    server_and_port_list.sort_by(|(host1, _), (host2, _)| host1.cmp(host2));

    // TODO: randomly search for node? -> for now we just take the first one
    let server = server_and_port_list.pop();

    if let Some(server_url) = server {
        Ok(opa_api.get_url(opa_api_protocol, &server_url.0, server_url.1)?)
    } else {
        Err(OpaServerMissing)
    }
}

/// Retrieve the port for the specified role group from the cluster spec
/// Defaults to 8181
fn get_opa_port(opa_spec: &OpaSpec, role_group: &str) -> OpaOperatorResult<u16> {
    if let Some(selector) = opa_spec.servers.selectors.get(role_group) {
        if let Some(port) = selector.config.port {
            return Ok(port);
        }
    }

    Ok(8181)
}
