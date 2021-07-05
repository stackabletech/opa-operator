use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::crd::Crd;
use stackable_operator::{client, error};

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_OPERATOR_LOG");
    let client = client::create_client(Some("authz.stackable.tech".to_string())).await?;

    if let Err(error) = stackable_operator::crd::wait_until_crds_present(
        &client,
        vec![OpenPolicyAgent::RESOURCE_NAME],
        None,
    )
    .await
    {
        error!("Required CRDs missing, aborting: {:?}", error);
    };

    stackable_opa_operator::create_controller(client).await;

    Ok(())
}
