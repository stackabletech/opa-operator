use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use futures::{FutureExt, future, pin_mut};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::resource_info_fetcher::v1alpha1;
use stackable_operator::{cli::CommonOptions, telemetry::Tracing};
use tokio::net::TcpListener;

mod backend;
mod http_error;
mod utils;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-resource-info-fetcher";
pub const SUPPORTED_KIND: &str = "dataset";

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
pub struct AppState {
    backend: Arc<backend::ResolvedBackend>,
    cache: Cache<ResourceInfoRequest, ResourceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfoRequest {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub glossary_terms: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub data_products: Vec<String>,
    #[serde(default)]
    pub custom_properties: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub custom_attributes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub glossary_terms: Vec<String>,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile { source: std::io::Error, path: PathBuf },

    #[snafu(display("failed to parse config file"))]
    ParseConfig { source: serde_json::Error },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit { source: stackable_operator::telemetry::tracing::Error },

    #[snafu(display("failed to resolve DataHub backend"))]
    ResolveDataHubBackend { source: backend::datahub::Error },

    #[snafu(display("failed to resolve OpenMetadata backend"))]
    ResolveOpenMetadataBackend { source: backend::openmetadata::Error },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
}

async fn resolve_backend(
    backend_config: v1alpha1::Backend,
    credentials_dir: &Path,
) -> Result<backend::ResolvedBackend, StartupError> {
    match backend_config {
        v1alpha1::Backend::None {} => Ok(backend::ResolvedBackend::None),
        v1alpha1::Backend::DataHub(config) => {
            let resolved =
                backend::datahub::ResolvedDataHubBackend::resolve(config, credentials_dir)
                    .await
                    .context(ResolveDataHubBackendSnafu)?;
            Ok(backend::ResolvedBackend::DataHub(resolved))
        }
        v1alpha1::Backend::OpenMetadata(config) => {
            let resolved =
                backend::openmetadata::ResolvedOpenMetadataBackend::resolve(config, credentials_dir)
                    .await
                    .context(ResolveOpenMetadataBackendSnafu)?;
            Ok(backend::ResolvedBackend::OpenMetadata(resolved))
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
            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let config: v1alpha1::Config =
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?;

    let backend = Arc::new(resolve_backend(config.backend, &args.credentials_dir).await?);

    let cache = {
        let v1alpha1::Cache { entry_time_to_live } = config.cache;
        Cache::builder()
            .name("resource-info")
            .time_to_live(*entry_time_to_live)
            .build()
    };

    let app = Router::new()
        .route("/resource", post(get_resource_info))
        .with_state(AppState { backend, cache });

    let listener = TcpListener::bind("127.0.0.1:9477")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)
}

async fn get_resource_info(
    State(state): State<AppState>,
    Json(req): Json<ResourceInfoRequest>,
) -> Result<Json<ResourceInfo>, http_error::JsonResponse<Arc<backend::GetResourceInfoError>>> {
    if req.kind != SUPPORTED_KIND {
        return Err(http_error::JsonResponse {
            error: Arc::new(backend::GetResourceInfoError::UnsupportedKind {
                kind: req.kind.clone(),
            }),
        });
    }

    let AppState { backend, cache } = state;
    Ok(Json(
        cache
            .try_get_with_by_ref(&req, async {
                backend.get_resource_info(&req).await
            })
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_info_request_deserialize() {
        let json = serde_json::json!({
            "kind": "dataset",
            "id": "hive.db.table",
            "attributes": {"platform": "trino", "environment": "PROD"},
        });
        let req: ResourceInfoRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.kind, "dataset");
        assert_eq!(req.id, "hive.db.table");
        assert_eq!(req.attributes.get("platform").map(String::as_str), Some("trino"));
    }

    #[test]
    fn resource_info_serialize_roundtrip() {
        let mut info = ResourceInfo::default();
        info.tags.push("pii".to_owned());
        info.owners.push("user:alice@example.com".to_owned());
        info.fields.insert(
            "customer_id".to_owned(),
            FieldInfo {
                type_: "STRING".to_owned(),
                tags: vec!["pii".to_owned()],
                glossary_terms: vec![],
            },
        );
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["tags"], serde_json::json!(["pii"]));
        assert_eq!(json["owners"], serde_json::json!(["user:alice@example.com"]));
        assert_eq!(json["fields"]["customer_id"]["type"], serde_json::json!("STRING"));
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    fn test_app() -> axum::Router {
        let backend = std::sync::Arc::new(backend::ResolvedBackend::None);
        let cache = moka::future::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(60))
            .build();
        axum::Router::new()
            .route("/resource", axum::routing::post(get_resource_info))
            .with_state(AppState { backend, cache })
    }

    #[tokio::test]
    async fn dataset_against_none_backend_returns_empty_info() {
        let app = test_app();
        let body = serde_json::json!({
            "kind": "dataset",
            "id": "hive.db.table",
            "attributes": {"platform": "trino", "environment": "PROD"},
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resource")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let info: ResourceInfo = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(info, ResourceInfo::default());
    }

    #[tokio::test]
    async fn unsupported_kind_returns_400() {
        let app = test_app();
        let body = serde_json::json!({"kind": "widget", "id": "x", "attributes": {}});
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resource")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
