mod controller;
mod discovery;

use futures::StreamExt;
use stackable_opa_crd::OpenPolicyAgent;
use stackable_operator::cli::Command;
use stackable_operator::client::Client;
use stackable_operator::error::OperatorResult;
use stackable_operator::k8s_openapi::api::apps::v1::DaemonSet;
use stackable_operator::kube::Api;
use stackable_operator::product_config::ProductConfigManager;
use stackable_operator::{
    client, error,
    k8s_openapi::api::core::v1::{ConfigMap, Service},
    kube::{
        api::ListParams,
        runtime::{controller::Context, Controller},
        CustomResourceExt,
    },
};
use structopt::StructOpt;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[derive(StructOpt)]
#[structopt(about = built_info::PKG_DESCRIPTION, author = stackable_operator::cli::AUTHOR)]
struct Opts {
    #[structopt(subcommand)]
    cmd: Command,
}

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("OPA_OPERATOR_LOG");

    let opts = Opts::from_args();
    match opts.cmd {
        Command::Crd => println!("{}", serde_yaml::to_string(&OpenPolicyAgent::crd())?),
        Command::Run { product_config } => {
            stackable_operator::utils::print_startup_string(
                built_info::PKG_DESCRIPTION,
                built_info::PKG_VERSION,
                built_info::GIT_VERSION,
                built_info::TARGET,
                built_info::BUILT_TIME_UTC,
                built_info::RUSTC_VERSION,
            );
            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/opa-operator/config-spec/properties.yaml",
            ])?;
            let client = client::create_client(Some("opa.stackable.tech".to_string())).await?;
            create_controller(client, product_config).await?;
        }
    };

    Ok(())
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
async fn create_controller(
    client: Client,
    product_config: ProductConfigManager,
) -> OperatorResult<()> {
    let opa_api: Api<OpenPolicyAgent> = client.get_all_api();
    let daemonsets_api: Api<DaemonSet> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();
    let services_api: Api<Service> = client.get_all_api();

    let controller = Controller::new(opa_api, ListParams::default())
        .owns(daemonsets_api, ListParams::default())
        .owns(configmaps_api, ListParams::default())
        .owns(services_api, ListParams::default());

    controller
        .run(
            controller::reconcile_opa,
            controller::error_policy,
            Context::new(controller::Ctx {
                client,
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
