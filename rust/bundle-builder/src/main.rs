use std::{
    num::TryFromIntError,
    sync::{Arc, Mutex},
};

use axum::{extract::State, http, response::IntoResponse, routing::get, Router};
use clap::Parser;
use flate2::write::GzEncoder;
use futures::{
    future::{self, BoxFuture},
    pin_mut, FutureExt, StreamExt, TryFutureExt,
};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    k8s_openapi::api::core::v1::ConfigMap,
    kube::{
        self,
        api::ObjectMeta,
        runtime::{
            reflector::{self, ObjectRef, Store},
            watcher,
        },
    },
};
use tokio::net::TcpListener;
use tracing::{error, info};

const OPERATOR_NAME: &str = "opa.stackable.tech";
pub const APP_NAME: &str = "opa-bundle-builder";

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
    InitKube { source: kube::Error },
    #[snafu(display("failed to get listener address"))]
    GetListenerAddr { source: std::io::Error },
    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },
    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },
    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    stackable_operator::logging::initialize_logging(
        "OPA_BUNDLE_BUILDER_LOG",
        APP_NAME,
        args.common.tracing_target,
    );

    let kube = kube::Client::try_default().await.context(InitKubeSnafu)?;

    let (store, store_w) = reflector::store();
    let rebuild_bundle = || {
        build_bundle(store.clone())
            .inspect_err(|error| {
                error!(
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
            kube::Api::<ConfigMap>::default_namespaced(kube),
            watcher::Config::default().labels(&format!("{OPERATOR_NAME}/bundle")),
        ),
    )
    .for_each(|ev| async {
        match ev {
            Ok(watcher::Event::Applied(o)) => {
                info!(object = %ObjectRef::from_obj(&o), "saw updated object")
            }
            Ok(watcher::Event::Deleted(o)) => {
                info!(object = %ObjectRef::from_obj(&o), "saw deleted object")
            }
            Ok(watcher::Event::Restarted(os)) => {
                let objects = os
                    .iter()
                    .map(ObjectRef::from_obj)
                    .map(|o| o.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                info!(objects, "restarted reflector")
            }
            Err(error) => {
                error!(
                    error = &error as &dyn std::error::Error,
                    "failed to update reflector"
                )
            }
        }
        *bundle.lock().unwrap() = rebuild_bundle();
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
    info!(%address, "listening");

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

    #[snafu(display("file {file_name:?} in {config_map} is too large ({file_size} bytes)"))]
    FileSizeOverflow {
        source: TryFromIntError,
        config_map: ObjectRef<ConfigMap>,
        file_name: String,
        file_size: usize,
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
    fn file_header(
        config_map: &ObjectRef<ConfigMap>,
        file_name: &str,
        data: &[u8],
    ) -> Result<tar::Header, BundleError> {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        let file_size = data.len();
        header.set_size(
            file_size
                .try_into()
                .with_context(|_| FileSizeOverflowSnafu {
                    config_map: config_map.clone(),
                    file_name,
                    file_size,
                })?,
        );
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        Ok(header)
    }

    info!("building bundle");
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), flate2::Compression::default()));
    for cm in store.state() {
        let ObjectMeta {
            name: Some(cm_ns),
            namespace: Some(cm_name),
            ..
        } = &cm.metadata
        else {
            return ConfigMapMetadataMissingSnafu.fail();
        };
        let cm_ref = ObjectRef::from_obj(&*cm);
        for (file_name, data) in cm.data.iter().flatten() {
            let mut header = file_header(&cm_ref, file_name, data.as_bytes())?;
            tar.append_data(
                &mut header,
                format!("configmap/{cm_ns}/{cm_name}/{file_name}"),
                data.as_bytes(),
            )
            .with_context(|_| AddFileToTarballSnafu {
                config_map: cm_ref.clone(),
                file_name,
            })?;
        }
    }
    let tar = tar
        .into_inner()
        .context(BuildTarballSnafu)?
        .finish()
        .context(BuildTarballSnafu)?;
    info!("finished building bundle");
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
