// TODO: Look into how to properly resolve `clippy::result_large_err`.
// This will need changes in our and upstream error types.
#![allow(clippy::result_large_err)]
use std::sync::Arc;

use clap::Parser;
use futures::StreamExt;
use product_config::ProductConfigManager;
use stackable_opa_operator::crd::{OPERATOR_NAME, OpaCluster, OpaClusterVersion, v1alpha1};
use stackable_operator::{
    YamlSchema,
    cli::{Command, ProductOperatorRun},
    client::{self, Client},
    k8s_openapi::api::{
        apps::v1::DaemonSet,
        core::v1::{ConfigMap, Service},
    },
    kube::{
        Api,
        core::DeserializeGuard,
        runtime::{
            Controller,
            events::{Recorder, Reporter},
            watcher,
        },
    },
    logging::controller::report_controller_reconciled,
    namespace::WatchNamespace,
    shared::yaml::SerializeOptions,
    telemetry::Tracing,
    utils::cluster_info::KubernetesClusterInfo,
};

use crate::controller::OPA_FULL_CONTROLLER_NAME;

mod controller;
mod discovery;
mod operations;
mod product_logging;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[derive(Parser)]
#[clap(about, author)]
struct Opts {
    #[clap(subcommand)]
    cmd: Command<OpaRun>,
}

#[derive(clap::Parser)]
struct OpaRun {
    /// The full image tag of the operator, used to deploy the user_info_fetcher.
    #[clap(long, env)]
    operator_image: String,

    #[clap(flatten)]
    common: ProductOperatorRun,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    match opts.cmd {
        Command::Crd => {
            OpaCluster::merged_crd(OpaClusterVersion::V1Alpha1)?
                .print_yaml_schema(built_info::PKG_VERSION, SerializeOptions::default())?;
        }
        Command::Run(OpaRun {
            operator_image,
            common:
                ProductOperatorRun {
                    product_config,
                    watch_namespace,
                    telemetry_arguments,
                    cluster_info_opts,
                },
        }) => {
            // NOTE (@NickLarsenNZ): Before stackable-telemetry was used:
            // - The console log level was set by `OPA_OPERATOR_LOG`, and is now `CONSOLE_LOG` (when using Tracing::pre_configured).
            // - The file log level was set by `OPA_OPERATOR_LOG`, and is now set via `FILE_LOG` (when using Tracing::pre_configured).
            // - The file log directory was set by `OPA_OPERATOR_LOG_DIRECTORY`, and is now set by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
            let _tracing_guard =
                Tracing::pre_configured(built_info::PKG_NAME, telemetry_arguments).init()?;

            tracing::info!(
                built_info.pkg_version = built_info::PKG_VERSION,
                built_info.git_version = built_info::GIT_VERSION,
                built_info.target = built_info::TARGET,
                built_info.built_time_utc = built_info::BUILT_TIME_UTC,
                built_info.rustc_version = built_info::RUSTC_VERSION,
                "Starting {description}",
                description = built_info::PKG_DESCRIPTION
            );
            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/opa-operator/config-spec/properties.yaml",
            ])?;

            let client =
                client::initialize_operator(Some(OPERATOR_NAME.to_string()), &cluster_info_opts)
                    .await?;
            let kubernetes_cluster_info = client.kubernetes_cluster_info.clone();
            create_controller(
                client,
                product_config,
                watch_namespace,
                operator_image.clone(),
                operator_image,
                kubernetes_cluster_info,
            )
            .await;
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
    watch_namespace: WatchNamespace,
    opa_bundle_builder_image: String,
    user_info_fetcher_image: String,
    cluster_info: KubernetesClusterInfo,
) {
    let opa_api: Api<DeserializeGuard<v1alpha1::OpaCluster>> = watch_namespace.get_api(&client);
    let daemonsets_api: Api<DeserializeGuard<DaemonSet>> = watch_namespace.get_api(&client);
    let configmaps_api: Api<DeserializeGuard<ConfigMap>> = watch_namespace.get_api(&client);
    let services_api: Api<DeserializeGuard<Service>> = watch_namespace.get_api(&client);

    let controller = Controller::new(opa_api, watcher::Config::default())
        .owns(daemonsets_api, watcher::Config::default())
        .owns(configmaps_api, watcher::Config::default())
        .owns(services_api, watcher::Config::default());

    let event_recorder = Arc::new(Recorder::new(
        client.as_kube_client(),
        Reporter {
            controller: OPA_FULL_CONTROLLER_NAME.to_string(),
            instance: None,
        },
    ));
    controller
        .run(
            controller::reconcile_opa,
            controller::error_policy,
            Arc::new(controller::Ctx {
                client: client.clone(),
                product_config,
                opa_bundle_builder_image,
                user_info_fetcher_image,
                cluster_info,
            }),
        )
        // We can let the reporting happen in the background
        .for_each_concurrent(
            16, // concurrency limit
            |result| {
                // The event_recorder needs to be shared across all invocations, so that
                // events are correctly aggregated
                let event_recorder = event_recorder.clone();
                async move {
                    report_controller_reconciled(
                        &event_recorder,
                        OPA_FULL_CONTROLLER_NAME,
                        &result,
                    )
                    .await;
                }
            },
        )
        .await;
}
