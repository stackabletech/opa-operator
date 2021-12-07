mod discovery;
mod opa_controller;
mod utils;

use futures::StreamExt;
use stackable_opa_crd::{OpenPolicyAgent, APP_NAME};
use stackable_operator::client::Client;
use stackable_operator::error::OperatorResult;
use stackable_operator::k8s_openapi::api::core::v1::{ConfigMap, Pod};
use stackable_operator::kube::api::ListParams;
use stackable_operator::kube::runtime::controller::Context;
use stackable_operator::kube::runtime::Controller;
use stackable_operator::kube::Api;
use stackable_operator::product_config::ProductConfigManager;

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(client: Client, product_config_path: &str) -> OperatorResult<()> {
    let opa_api: Api<OpenPolicyAgent> = client.get_all_api();
    let pods_api: Api<Pod> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();

    let controller = Controller::new(opa_api, ListParams::default())
        .owns(pods_api, ListParams::default())
        .owns(configmaps_api, ListParams::default());

    let product_config = ProductConfigManager::from_yaml_file(product_config_path).unwrap();

    controller
        .run(
            opa_controller::reconcile_opa,
            opa_controller::error_policy,
            Context::new(opa_controller::Ctx {
                kube: client.as_kube_client(),
                product_config,
            }),
        )
        .for_each(|res| async {
            match res {
                Ok((obj, _)) => tracing::info!(object = %obj, "Reconciled object"),
                Err(err) => {
                    tracing::error!(
                        error = &err as &dyn std::error::Error,
                        "Failed to reconcile object",
                    )
                }
            }
        })
        .await;

    Ok(())
}
