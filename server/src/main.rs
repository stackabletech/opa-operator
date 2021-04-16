use stackable_opa_crd::OpaServer;
use stackable_operator::{client, error};

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_OPERATOR_LOG");
    let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;

    stackable_operator::crd::ensure_crd_created::<OpaServer>(client.clone()).await?;

    //stackable_opa_operator::create_controller(client.clone());

    Ok(())
}
