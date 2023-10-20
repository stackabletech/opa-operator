mod backend;
mod http_error;
mod util;

use std::{
    collections::HashMap,
    net::AddrParseError,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use futures::{future, pin_mut, FutureExt};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;

pub const APP_NAME: &str = "opa-user-info-fetcher";

#[derive(clap::Parser)]
pub struct Args {
    #[clap(long, env)]
    config: PathBuf,
    #[clap(long, env)]
    credentials_dir: PathBuf,
    #[clap(flatten)]
    common: stackable_operator::cli::ProductOperatorRun,
}

#[derive(Clone)]
struct AppState {
    config: Arc<crd::Config>,
    http: reqwest::Client,
    credentials: Arc<Credentials>,
}

struct Credentials {
    username: String,
    password: String,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile {
        source: std::io::Error,
        path: PathBuf,
    },
    #[snafu(display("unable to parse config file"))]
    ParseConfig { source: serde_json::Error },
    #[snafu(display("failed to parse listen address"))]
    ParseListenAddr { source: AddrParseError },
    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },
    #[snafu(display("failed to run server"))]
    RunServer { source: hyper::Error },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    stackable_operator::logging::initialize_logging(
        "OPA_OPERATOR_LOG",
        APP_NAME,
        args.common.tracing_target,
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

    let config = Arc::<crd::Config>::new(
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?,
    );
    let credentials = Arc::new(match &config.backend {
        // FIXME: factor this out into each backend
        crd::Backend::None {} => Credentials {
            username: "".to_string(),
            password: "".to_string(),
        },
        crd::Backend::Keycloak(_) => Credentials {
            username: read_config_file(&args.credentials_dir.join("username")).await?,
            password: read_config_file(&args.credentials_dir.join("password")).await?,
        },
    });
    let http = reqwest::Client::default();
    let app = Router::new()
        .route("/user", post(get_user_info))
        .with_state(AppState {
            config,
            http,
            credentials,
        });
    axum::Server::bind(&"127.0.0.1:9476".parse().context(ParseListenAddrSnafu)?)
        .serve(app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembershipRequest {
    username: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupRef {
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleRef {
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    groups: Vec<GroupRef>,
    roles: Vec<RoleRef>,
    custom_attributes: HashMap<String, String>,
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("failed to get user information from Keycloak"))]
    Keycloak { source: backend::keycloak::Error },
}
impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        match self {
            Self::Keycloak { source } => source.status_code(),
        }
    }
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<GroupMembershipRequest>,
) -> Result<Json<UserInfo>, http_error::JsonResponse<GetUserInfoError>> {
    let AppState {
        config,
        http,
        credentials,
    } = state;
    Ok(Json(match &config.backend {
        crd::Backend::None {} => UserInfo {
            groups: vec![],
            roles: vec![],
            custom_attributes: HashMap::new(),
        },
        crd::Backend::Keycloak(keycloak) => {
            backend::keycloak::get_user_info(req, &http, &credentials, keycloak)
                .await
                .context(get_user_info_error::KeycloakSnafu)?
        }
    }))
}
