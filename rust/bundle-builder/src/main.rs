use std::{
    collections::{BTreeMap, BTreeSet},
    num::TryFromIntError,
    ops::Deref as _,
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
    cli::RollingPeriod,
    k8s_openapi::api::core::v1::ConfigMap,
    kube::{
        api::ObjectMeta,
        runtime::{
            reflector::{self, ObjectRef, Store},
            watcher,
        },
    },
};
use stackable_telemetry::{Tracing, tracing::settings::Settings};
use tokio::net::TcpListener;
use tracing::level_filters::LevelFilter;

const OPERATOR_NAME: &str = "opa.stackable.tech";
pub const APP_NAME: &str = "opa-bundle-builder";

// TODO (@NickLarsenNZ): Change the variable to `CONSOLE_LOG`
pub const ENV_VAR_CONSOLE_LOG: &str = "OPA_BUNDLE_BUILDER_LOG";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(flatten)]
    common: stackable_operator::cli::ProductOperatorRun,
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
        source: stackable_telemetry::tracing::Error,
    },
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    let _tracing_guard = Tracing::builder()
        .service_name("opa-bundle-builder")
        .with_console_output((
            ENV_VAR_CONSOLE_LOG,
            LevelFilter::INFO,
            !args.common.telemetry_arguments.no_console_output,
        ))
        // NOTE (@NickLarsenNZ): Before stackable-telemetry was used, the log directory was
        // set via an env: `OPA_BUNDLE_BUILDER_LOG_DIRECTORY`.
        // See: https://github.com/stackabletech/operator-rs/blob/f035997fca85a54238c8de895389cc50b4d421e2/crates/stackable-operator/src/logging/mod.rs#L40
        // Now it will be `ROLLING_LOGS` (or via `--rolling-logs <DIRECTORY>`).
        .with_file_output(
            args.common
                .telemetry_arguments
                .rolling_logs
                .map(|log_directory| {
                    let rotation_period = args
                        .common
                        .telemetry_arguments
                        .rolling_logs_period
                        .unwrap_or(RollingPeriod::Never)
                        .deref()
                        .clone();

                    Settings::builder()
                        .with_environment_variable(ENV_VAR_CONSOLE_LOG)
                        .with_default_level(LevelFilter::INFO)
                        .file_log_settings_builder(log_directory, "tracing-rs.log")
                        .with_rotation_period(rotation_period)
                        .build()
                }),
        )
        .with_otlp_log_exporter((
            "OTLP_LOG",
            LevelFilter::DEBUG,
            args.common.telemetry_arguments.otlp_logs,
        ))
        .with_otlp_trace_exporter((
            "OTLP_TRACE",
            LevelFilter::DEBUG,
            args.common.telemetry_arguments.otlp_traces,
        ))
        .build()
        .init()
        .context(TracingInitSnafu)?;

    let client =
        stackable_operator::client::initialize_operator(None, &args.common.cluster_info_opts)
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
            args.common.watch_namespace.get_api::<ConfigMap>(&client),
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
