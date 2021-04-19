use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::{client, error};

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_OPERATOR_LOG");
    let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;

    stackable_operator::crd::ensure_crd_created::<OpenPolicyAgent>(&client).await?;

    //stackable_opa_operator::create_controller(client);

    Ok(())
}
