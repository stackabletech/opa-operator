// TODO: Look into how to properly resolve `clippy::result_large_err`.
// This will need changes in our and upstream error types.
#![allow(clippy::result_large_err)]
use std::{
    collections::{BTreeMap, BTreeSet},
    num::TryFromIntError,
    sync::{Arc, Mutex},
};

use axum::{Router, extract::State, http, response::IntoResponse, routing::get};
use clap::Parser;
use flate2::write::GzEncoder;
use futures::{
    FutureExt, StreamExt, TryFutureExt,
    future::{self, BoxFuture},
    pin_mut,
};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::CommonOptions,
    k8s_openapi::api::core::v1::ConfigMap,
    kube::{
        api::ObjectMeta,
        runtime::{
            reflector::{self, ObjectRef, Store},
            watcher,
        },
    },
    namespace::WatchNamespace,
    telemetry::Tracing,
};
use tokio::net::TcpListener;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

const OPERATOR_NAME: &str = "opa.stackable.tech";
pub const APP_NAME: &str = "opa-bundle-builder";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Provides a specific namespace to watch (instead of watching all namespaces)
    #[arg(long, env, default_value = "")]
    pub watch_namespace: WatchNamespace,
}

type Bundle = Vec<u8>;
type BundleFuture = future::Shared<BoxFuture<'static, Arc<Result<Bundle, BundleError>>>>;

#[derive(Clone)]
struct AppState {
    bundle: Arc<Mutex<BundleFuture>>,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to initialize Kubernetes client"))]
    InitKube {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to get listener address"))]
    GetListenerAddr { source: std::io::Error },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_operator::telemetry::tracing::Error,
    },
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    // As stackable-operator pulls in ring and reqwest >= 0.13 pulls in aws_lc_rs, we need
    // to explicitly tell rustls what provider to use. As other operators use ring, we use
    // that for consistency reasons here as well.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install ring rustls provider");

    // NOTE (@NickLarsenNZ): Before stackable-telemetry was used:
    // - The console log level was set by `OPA_BUNDLE_BUILDER_LOG`, and is now `CONSOLE_LOG` (when using Tracing::pre_configured).
    // - The file log level was set by `OPA_BUNDLE_BUILDER_LOG`, and is now set via `FILE_LOG` (when using Tracing::pre_configured).
    // - The file log directory was set by `OPA_BUNDLE_BUILDER_LOG_DIRECTORY`, and is now set by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
    let _tracing_guard = Tracing::pre_configured(built_info::PKG_NAME, args.common.telemetry)
        .init()
        .context(TracingInitSnafu)?;

    tracing::info!(
        built_info.pkg_version = built_info::PKG_VERSION,
        built_info.git_version = built_info::GIT_VERSION,
        built_info.target = built_info::TARGET,
        built_info.built_time_utc = built_info::BUILT_TIME_UTC,
        built_info.rustc_version = built_info::RUSTC_VERSION,
        "Starting bundle-builder",
    );

    let client = stackable_operator::client::initialize_operator(None, &args.common.cluster_info)
        .await
        .context(InitKubeSnafu)?;

    let (store, store_w) = reflector::store();
    let rebuild_bundle = || {
        tracing::info!("bundle invalidated, will be rebuilt on next request");
        // Even if build_bundle is completely synchronous (currently),
        // storing a Future acts as a primitive laziness/debouncing mechanism,
        // the bundle will only actually be built once it is requested.
        build_bundle(store.clone())
            .inspect_err(|error| {
                tracing::error!(
                    error = error as &dyn std::error::Error,
                    "failed to rebuild bundle"
                )
            })
            .map(Arc::from)
            .boxed()
            .shared()
    };
    let bundle = Arc::new(Mutex::new(rebuild_bundle()));
    let reflector = std::pin::pin!(reflector::reflector(
        store_w,
        watcher(
            args.watch_namespace.get_api::<ConfigMap>(&client),
            watcher::Config::default().labels(&format!("{OPERATOR_NAME}/bundle")),
        ),
    )
    .for_each(|ev| async {
        let rebuild = match ev {
            Ok(watcher::Event::Apply(o)) => {
                tracing::info!(object = %ObjectRef::from_obj(&o), "saw updated object");
                true
            }
            Ok(watcher::Event::Delete(o)) => {
                tracing::info!(object = %ObjectRef::from_obj(&o), "saw deleted object");
                true
            }
            Ok(watcher::Event::Init) => {
                tracing::info!("restart initiated");
                false
            }
            Ok(watcher::Event::InitApply(o)) => {
                tracing::info!(object = %ObjectRef::from_obj(&o), "saw updated object (waiting for restart to complete before rebuilding)");
                false
            }
            Ok(watcher::Event::InitDone) => {
                tracing::info!("restart done");
                true
            }
            Err(error) => {
                tracing::error!(
                    error = &error as &dyn std::error::Error,
                    "failed to update reflector"
                );
                false
            }
        };
        if rebuild {
            tracing::info!("rebuilding bundle");
            *bundle.lock().unwrap() = rebuild_bundle();
        } else {
            tracing::debug!("change should have no effect, not rebuilding bundle");
        }
    })
    .map(Ok));

    let shutdown_requested = tokio::signal::ctrl_c().map(|_| ());
    #[cfg(unix)]
    let shutdown_requested = {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context(RegisterSigtermSnafu)?;
        async move {
            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let app = Router::new()
        .route("/opa/v1/opa/bundle.tar.gz", get(get_bundle))
        .route("/status", get(get_status))
        .with_state(AppState {
            bundle: bundle.clone(),
        });
    // FIXME: can we restrict access to localhost?
    // kubelet probes run from outside the container netns
    let listener = TcpListener::bind("0.0.0.0:3030")
        .await
        .context(BindListenerSnafu)?;
    let address = listener.local_addr().context(GetListenerAddrSnafu)?;
    tracing::info!(%address, "listening");

    let server = std::pin::pin!(async {
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_requested)
            .await
            .context(RunServerSnafu)
    });

    future::select(reflector, server).await.factor_first().0
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum BundleError {
    #[snafu(display("ConfigMap is missing required metadata"))]
    ConfigMapMetadataMissing,

    #[snafu(display("file {file_path:?} is too large ({file_size} bytes)"))]
    FileSizeOverflow {
        source: TryFromIntError,
        file_path: String,
        file_size: usize,
    },

    #[snafu(display("failed to add static file {file_path:?} to tarball"))]
    AddStaticRuleToTarball {
        source: std::io::Error,
        file_path: String,
    },

    #[snafu(display("failed to add file {file_name:?} from {config_map} to tarball"))]
    AddFileToTarball {
        source: std::io::Error,
        config_map: ObjectRef<ConfigMap>,
        file_name: String,
    },

    #[snafu(display("failed to build tarball"))]
    BuildTarball { source: std::io::Error },
}

impl BundleError {
    fn to_http_response(&self) -> impl IntoResponse {
        (
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "failed to build bundle, see opa-bundle-builder logs for more details",
        )
    }
}

async fn build_bundle(store: Store<ConfigMap>) -> Result<Vec<u8>, BundleError> {
    use bundle_error::*;
    fn file_header(file_path: &str, data: &[u8]) -> Result<tar::Header, BundleError> {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        let file_size = data.len();
        header.set_size(
            file_size
                .try_into()
                .with_context(|_| FileSizeOverflowSnafu {
                    file_path,
                    file_size,
                })?,
        );
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        Ok(header)
    }

    tracing::info!("building bundle");
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), flate2::Compression::default()));
    let mut resource_versions = BTreeMap::<String, String>::new();
    let mut bundle_file_paths = BTreeSet::<String>::new();

    for (file_path, data) in stackable_opa_regorule_library::REGORULES {
        let mut header = file_header(file_path, data.as_bytes())?;
        tar.append_data(&mut header, file_path, data.as_bytes())
            .context(AddStaticRuleToTarballSnafu {
                file_path: *file_path,
            })?;
        bundle_file_paths.insert(file_path.to_string());
    }

    for cm in store.state() {
        let ObjectMeta {
            name: Some(cm_ns),
            namespace: Some(cm_name),
            resource_version: Some(cm_version),
            ..
        } = &cm.metadata
        else {
            return ConfigMapMetadataMissingSnafu.fail();
        };
        let cm_ref = ObjectRef::from_obj(&*cm);
        for (file_name, data) in cm.data.iter().flatten() {
            let file_path = format!("configmap/{cm_ns}/{cm_name}/{file_name}");
            let mut header = file_header(&file_path, data.as_bytes())?;
            tar.append_data(&mut header, &file_path, data.as_bytes())
                .with_context(|_| AddFileToTarballSnafu {
                    config_map: cm_ref.clone(),
                    file_name,
                })?;
            bundle_file_paths.insert(file_path);
        }
        resource_versions.insert(cm_ref.to_string(), cm_version.clone());
    }
    let tar = tar
        .into_inner()
        .context(BuildTarballSnafu)?
        .finish()
        .context(BuildTarballSnafu)?;
    tracing::info!(bundle.files = ?bundle_file_paths, bundle.versions = ?resource_versions, "finished building bundle");
    Ok(tar)
}

async fn get_status(State(state): State<AppState>) -> impl IntoResponse {
    let bundle = future::Shared::clone(&*state.bundle.lock().unwrap());
    if let Err(err) = bundle.await.as_deref() {
        return Err(err.to_http_response());
    }
    Ok("ready")
}

async fn get_bundle(State(state): State<AppState>) -> impl IntoResponse {
    let bundle = future::Shared::clone(&*state.bundle.lock().unwrap());
    Ok((
        [(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/gzip"),
        )],
        match bundle.await.as_deref() {
            Ok(bundle) => bundle.to_vec(),
            Err(err) => return Err(err.to_http_response()),
        },
    ))
}
