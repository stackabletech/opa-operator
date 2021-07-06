/// All pod names follow a simple pattern: spark-<cluster_name>-<role_group>-<node_type>-<node_name>
///
/// # Arguments
/// * `app_name` - The name of the cluster application (Spark, Kafka ...)
/// * `context_name` - The name of the cluster as specified in the custom resource
/// * `role` - The cluster role (e.g. master, worker, history-server)
/// * `role_group` - The role group of the selector
/// * `node_name` - The node or host name
///
// TODO: Remove (move to) for operator-rs method
pub fn build_pod_name(
    app_name: &str,
    context_name: &str,
    role: &str,
    role_group: &str,
    node_name: &str,
) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        app_name, context_name, role_group, role, node_name
    )
    .to_lowercase()
}

pub fn create_opa_start_command(port: Option<String>) -> Vec<String> {
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
