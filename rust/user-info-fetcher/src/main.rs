use std::{
    collections::HashMap,
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use futures::{future, pin_mut, FutureExt};
use moka::future::Cache;
use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_crd::user_info_fetcher as crd;
use tokio::{fs::File, io::AsyncReadExt, net::TcpListener};

mod backend;
mod http_error;
mod util;

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
    user_info_cache: Cache<UserInfoRequest, UserInfo>,
}

struct Credentials {
    // TODO: Find a better way of sharing behavior between different backends
    client_id: String,
    client_secret: String,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("unable to read config file from {path:?}"))]
    ReadConfigFile {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("failed to parse config file"))]
    ParseConfig { source: serde_json::Error },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to construct http client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to open ca certificate"))]
    OpenCaCert { source: std::io::Error },

    #[snafu(display("failed to read ca certificate"))]
    ReadCaCert { source: std::io::Error },

    #[snafu(display("failed to parse ca certificate"))]
    ParseCaCert { source: reqwest::Error },
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
        // TODO: factor this out into each backend (e.g. when we add LDAP support)
        crd::Backend::None {} => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        crd::Backend::Keycloak(_) => Credentials {
            client_id: read_config_file(&args.credentials_dir.join("clientId")).await?,
            client_secret: read_config_file(&args.credentials_dir.join("clientSecret")).await?,
        },
        crd::Backend::ExperimentalXfscAas(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        crd::Backend::ActiveDirectory(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
    });

    let mut client_builder = ClientBuilder::new();

    // TODO: I'm not so sure we should be doing all this keycloak specific stuff here.
    // We could factor it out in the provider specific implementation (e.g. when we add LDAP support).
    // I know it is for setting up the client, but an idea: make a trait for implementing backends
    // The trait can do all this for a genric client using an implementation on the trait (eg: get_http_client() which will call self.uses_tls())
    if let crd::Backend::Keycloak(keycloak) = &config.backend {
        if keycloak.tls.uses_tls() && !keycloak.tls.uses_tls_verification() {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }
        if let Some(tls_ca_cert_mount_path) = keycloak.tls.tls_ca_cert_mount_path() {
            let mut buf = Vec::new();
            File::open(tls_ca_cert_mount_path)
                .await
                .context(OpenCaCertSnafu)?
                .read_to_end(&mut buf)
                .await
                .context(ReadCaCertSnafu)?;
            let ca_cert = reqwest::Certificate::from_pem(&buf).context(ParseCaCertSnafu)?;

            client_builder = client_builder
                .tls_built_in_root_certs(false)
                .add_root_certificate(ca_cert);
        }
    }
    let http = client_builder.build().context(ConstructHttpClientSnafu)?;

    let user_info_cache = {
        let crd::Cache { entry_time_to_live } = config.cache;
        Cache::builder()
            .name("user-info")
            .time_to_live(*entry_time_to_live)
            .build()
    };
    let app = Router::new()
        .route("/user", post(get_user_info))
        .with_state(AppState {
            config,
            http,
            credentials,
            user_info_cache,
        });
    let listener = TcpListener::bind("127.0.0.1:9476")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase", untagged)]
enum UserInfoRequest {
    UserInfoRequestById(UserInfoRequestById),
    UserInfoRequestByName(UserInfoRequestByName),
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct UserInfoRequestById {
    id: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct UserInfoRequestByName {
    username: String,
}

/// Renders [`UserInfoRequest`] for use in error messages.
///
/// An independent type rather than an impl on [`UserInfoRequest`], since it is
/// not suitable for use in other contexts.
#[derive(Debug, Clone)]
struct ErrorRenderUserInfoRequest(UserInfoRequest);
impl Display for ErrorRenderUserInfoRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            UserInfoRequest::UserInfoRequestById(UserInfoRequestById { id }) => {
                write!(f, "with id {id:?}")
            }
            UserInfoRequest::UserInfoRequestByName(UserInfoRequestByName { username }) => {
                write!(f, "with username {username:?}")
            }
        }
    }
}
impl From<&UserInfoRequest> for ErrorRenderUserInfoRequest {
    fn from(value: &UserInfoRequest) -> Self {
        Self(value.clone())
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    /// This might be null in case the id is not known (e.g. the backend does not have this info).
    id: Option<String>,
    /// This might be null in case the username is not known (e.g. the backend does not have this info).
    username: Option<String>,
    groups: Vec<String>,
    custom_attributes: HashMap<String, serde_json::Value>,
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("failed to get user information from Keycloak"))]
    Keycloak { source: backend::keycloak::Error },

    #[snafu(display(
        "failed to get user information from the XFSC Authentication & Authorization Service"
    ))]
    ExperimentalXfscAas { source: backend::xfsc_aas::Error },

    #[snafu(display("failed to get user information from Active Directory"))]
    ActiveDirectory {
        source: backend::active_directory::Error,
    },
}

impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        // todo: the warn here loses context about the scope in which the error occurred, eg: stackable_opa_user_info_fetcher::backend::keycloak
        // Also, we should make the log level (warn vs error) more dynamic in the backend's impl `http_error::Error for Error`
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::Keycloak { source } => source.status_code(),
            Self::ExperimentalXfscAas { source } => source.status_code(),
            Self::ActiveDirectory { source } => source.status_code(),
        }
    }
}

async fn get_user_info(
    State(state): State<AppState>,
    Json(req): Json<UserInfoRequest>,
) -> Result<Json<UserInfo>, http_error::JsonResponse<Arc<GetUserInfoError>>> {
    let AppState {
        config,
        http,
        credentials,
        user_info_cache,
    } = state;
    Ok(Json(
        user_info_cache
            .try_get_with_by_ref(&req, async {
                match &config.backend {
                    crd::Backend::None {} => {
                        let user_id = match &req {
                            UserInfoRequest::UserInfoRequestById(UserInfoRequestById { id }) => {
                                Some(id)
                            }
                            _ => None,
                        };
                        let username = match &req {
                            UserInfoRequest::UserInfoRequestByName(UserInfoRequestByName {
                                username,
                            }) => Some(username),
                            _ => None,
                        };
                        Ok(UserInfo {
                            id: user_id.cloned(),
                            username: username.cloned(),
                            groups: vec![],
                            custom_attributes: HashMap::new(),
                        })
                    }
                    crd::Backend::Keycloak(keycloak) => {
                        backend::keycloak::get_user_info(&req, &http, &credentials, keycloak)
                            .await
                            .context(get_user_info_error::KeycloakSnafu)
                    }
                    crd::Backend::ExperimentalXfscAas(aas) => {
                        backend::xfsc_aas::get_user_info(&req, &http, aas)
                            .await
                            .context(get_user_info_error::ExperimentalXfscAasSnafu)
                    }
                    crd::Backend::ActiveDirectory(ad) => backend::active_directory::get_user_info(
                        &req,
                        &ad.ldap_server,
                        &ad.base_distinguished_name,
                        &ad.custom_attribute_mappings,
                    )
                    .await
                    .context(get_user_info_error::ActiveDirectorySnafu),
                }
            })
            .await?,
    ))
}
