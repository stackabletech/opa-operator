use stackable_operator::{client, error};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_OPERATOR_LOG");

    info!("Starting Stackable Operator for OpenPolicyAgent");
    let client = client::create_client(Some("authz.stackable.tech".to_string())).await?;
    stackable_opa_operator::create_controller(client).await;
    Ok(())
}
