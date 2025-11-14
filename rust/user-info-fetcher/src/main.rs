use std::{
    collections::HashMap,
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use futures::{FutureExt, future, pin_mut};
use moka::future::Cache;
use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha1;
use stackable_operator::{
    cli::CommonOptions, commons::tls_verification::TlsClientDetails, telemetry::Tracing,
};
use tokio::net::TcpListener;

mod backend;
mod http_error;
mod utils;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const APP_NAME: &str = "opa-user-info-fetcher";

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
    config: Arc<v1alpha1::Config>,
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

    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to initialize stackable-telemetry"))]
    TracingInit {
        source: stackable_operator::telemetry::tracing::Error,
    },
}

async fn read_config_file(path: &Path) -> Result<String, StartupError> {
    tokio::fs::read_to_string(path)
        .await
        .context(ReadConfigFileSnafu { path })
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<(), StartupError> {
    let args = Args::parse();

    // NOTE (@NickLarsenNZ): Before stackable-telemetry was used:
    // - The console log level was set by `OPA_OPERATOR_LOG`, and is now `CONSOLE_LOG` (when using Tracing::pre_configured).
    // - The file log level was set by `OPA_OPERATOR_LOG`, and is now set via `FILE_LOG` (when using Tracing::pre_configured).
    // - The file log directory was set by `OPA_OPERATOR_LOG_DIRECTORY`, and is now set by `ROLLING_LOGS_DIR` (or via `--rolling-logs <DIRECTORY>`).
    let _tracing_guard = Tracing::pre_configured(built_info::PKG_NAME, args.common.telemetry)
        .init()
        .context(TracingInitSnafu)?;

    tracing::info!(
        built_info.pkg_version = built_info::PKG_VERSION,
        built_info.git_version = built_info::GIT_VERSION,
        built_info.target = built_info::TARGET,
        built_info.built_time_utc = built_info::BUILT_TIME_UTC,
        built_info.rustc_version = built_info::RUSTC_VERSION,
        "Starting user-info-fetcher",
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

    let config = Arc::<v1alpha1::Config>::new(
        serde_json::from_str(&read_config_file(&args.config).await?).context(ParseConfigSnafu)?,
    );
    let credentials = Arc::new(match &config.backend {
        // TODO: factor this out into each backend (e.g. when we add LDAP support)
        v1alpha1::Backend::None {} => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        v1alpha1::Backend::Keycloak(_) => Credentials {
            client_id: read_config_file(&args.credentials_dir.join("clientId")).await?,
            client_secret: read_config_file(&args.credentials_dir.join("clientSecret")).await?,
        },
        v1alpha1::Backend::ExperimentalXfscAas(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        v1alpha1::Backend::ActiveDirectory(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
        v1alpha1::Backend::Entra(_) => Credentials {
            client_id: read_config_file(&args.credentials_dir.join("clientId")).await?,
            client_secret: read_config_file(&args.credentials_dir.join("clientSecret")).await?,
        },
        v1alpha1::Backend::OpenLdap(_) => Credentials {
            client_id: "".to_string(),
            client_secret: "".to_string(),
        },
    });

    let mut client_builder = ClientBuilder::new();

    // TODO: I'm not so sure we should be doing all this keycloak specific stuff here.
    // We could factor it out in the provider specific implementation (e.g. when we add LDAP support).
    // I know it is for setting up the client, but an idea: make a trait for implementing backends
    // The trait can do all this for a genric client using an implementation on the trait (eg: get_http_client() which will call self.uses_tls())
    if let v1alpha1::Backend::Keycloak(keycloak) = &config.backend {
        client_builder = utils::tls::configure_reqwest(&keycloak.tls, client_builder)
            .await
            .context(ConfigureTlsSnafu)?;
    } else if let v1alpha1::Backend::Entra(entra) = &config.backend {
        client_builder = utils::tls::configure_reqwest(
            &TlsClientDetails {
                tls: entra.tls.clone(),
            },
            client_builder,
        )
        .await
        .context(ConfigureTlsSnafu)?;
    }
    let http = client_builder.build().context(ConstructHttpClientSnafu)?;

    let user_info_cache = {
        let v1alpha1::Cache { entry_time_to_live } = config.cache;
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

    #[snafu(display("failed to get user information from Entra"))]
    Entra { source: backend::entra::Error },

    #[snafu(display("failed to get user information from OpenLDAP"))]
    OpenLdap { source: backend::openldap::Error },
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
            Self::Entra { source } => source.status_code(),
            Self::OpenLdap { source } => source.status_code(),
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
                    v1alpha1::Backend::None {} => {
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
                    v1alpha1::Backend::Keycloak(keycloak) => {
                        backend::keycloak::get_user_info(&req, &http, &credentials, keycloak)
                            .await
                            .context(get_user_info_error::KeycloakSnafu)
                    }
                    v1alpha1::Backend::ExperimentalXfscAas(aas) => {
                        backend::xfsc_aas::get_user_info(&req, &http, aas)
                            .await
                            .context(get_user_info_error::ExperimentalXfscAasSnafu)
                    }
                    v1alpha1::Backend::ActiveDirectory(ad) => {
                        backend::active_directory::get_user_info(
                            &req,
                            &ad.ldap_server,
                            &ad.tls,
                            &ad.base_distinguished_name,
                            &ad.custom_attribute_mappings,
                            &ad.additional_group_attribute_filters,
                        )
                        .await
                        .context(get_user_info_error::ActiveDirectorySnafu)
                    }
                    v1alpha1::Backend::Entra(entra) => {
                        backend::entra::get_user_info(&req, &http, &credentials, entra)
                            .await
                            .context(get_user_info_error::EntraSnafu)
                    }
                    v1alpha1::Backend::OpenLdap(openldap) => {
                        backend::openldap::get_user_info(&req, openldap)
                            .await
                            .context(get_user_info_error::OpenLdapSnafu)
                    }
                }
            })
            .await?,
    ))
}
