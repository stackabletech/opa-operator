use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use futures::{FutureExt, future};
use info_fetcher_commons::{
    config::{ConfigError, read_config_file},
    http_error,
};
use moka::future::Cache;
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1::{self};
use stackable_operator::{cli::CommonOptions, telemetry::Tracing};
use tokio::net::TcpListener;

use crate::api::{GetResourceInfoError, ResourceInfoBackend, ResourceInfoRequest};

mod api;
mod backend;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-resource-info-fetcher";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(flatten)]
    common: CommonOptions,

    #[clap(long, env)]
    config: PathBuf,

    #[clap(long, env)]
    credentials_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    backend: Arc<ResolvedBackend>,
    // Note: Although we might no talk JSON to the underlying backend, we always return JSON as a
    // result to the caller, so we can cache that.
    resource_info_cache: Cache<ResourceInfoRequest, serde_json::Value>,
}

/// Backend with resolved credentials.
///
/// This enum wraps backend-specific implementations that have already loaded their credentials
/// and initialized their HTTP clients.
enum ResolvedBackend {
    DataHub(backend::data_hub::ResolvedDataHubBackend),
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_operator::telemetry::tracing::Error,
    },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("unable to parse config file from {path:?}"))]
    ParseConfigFile { source: ConfigError, path: PathBuf },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to resolve DataHub backend"))]
    ResolveDataHubBackend { source: backend::data_hub::Error },
}

/// Resolves a backend configuration by loading credentials and creating the appropriate backend implementation.
///
/// This function reads credentials from the filesystem once at startup and returns a backend that
/// contains both the configuration and the resolved credentials.
async fn resolve_backend(
    backend: v1alpha1::Backend,
    credentials_dir: &Path,
) -> Result<ResolvedBackend, StartupError> {
    match backend {
        v1alpha1::Backend::DataHub(config) => {
            let resolved =
                backend::data_hub::ResolvedDataHubBackend::resolve(config, credentials_dir)
                    .await
                    .context(ResolveDataHubBackendSnafu)?;
            Ok(ResolvedBackend::DataHub(resolved))
        }
    }
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    let _tracing_guard = Tracing::pre_configured(built_info::PKG_NAME, args.common.telemetry)
        .init()
        .context(TracingInitSnafu)?;

    tracing::info!(
        built_info.pkg_version = built_info::PKG_VERSION,
        built_info.git_version = built_info::GIT_VERSION,
        built_info.target = built_info::TARGET,
        built_info.built_time_utc = built_info::BUILT_TIME_UTC,
        built_info.rustc_version = built_info::RUSTC_VERSION,
        "Starting resource-info-fetcher",
    );

    let shutdown_requested = tokio::signal::ctrl_c().map(|_| ());
    #[cfg(unix)]
    let shutdown_requested = {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context(RegisterSigtermSnafu)?;
        async move {
            use futures::pin_mut;

            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let config: v1alpha1::Config = read_config_file(&args.config)
        .with_context(|_| ParseConfigFileSnafu { path: args.config })?;
    let backend = Arc::new(resolve_backend(config.backend, &args.credentials_dir).await?);
    let resource_info_cache = {
        Cache::builder()
            .name("resource-info")
            .time_to_live(*config.cache.entry_time_to_live)
            .build()
    };
    let app = Router::new()
        .route("/resource", post(get_resource_info))
        .with_state(AppState {
            backend,
            resource_info_cache,
        });
    let listener = TcpListener::bind("127.0.0.1:9477")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)?;
    Ok(())
}

async fn get_resource_info(
    State(state): State<AppState>,
    Json(req): Json<ResourceInfoRequest>,
) -> Result<Json<serde_json::Value>, http_error::JsonResponse<Arc<GetResourceInfoError>>> {
    let AppState {
        backend,
        resource_info_cache,
    } = state;
    let resource_info = resource_info_cache
        .try_get_with_by_ref(&req, async {
            match backend.as_ref() {
                ResolvedBackend::DataHub(data_hub) => {
                    let response = data_hub.get_resource_info(&req).await?;
                    serde_json::to_value(&response).map_err(|err| {
                        GetResourceInfoError::SerializeResponseAsJson { source: err }
                    })
                }
            }
        })
        .await?;

    Ok(Json(resource_info))
}
