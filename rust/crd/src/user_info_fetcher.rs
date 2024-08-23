use derivative::Derivative;
use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::authentication::tls::TlsClientDetails,
    schemars::{self, JsonSchema},
    time::Duration,
};

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// The backend directory service to use.
    #[serde(default)]
    pub backend: Backend,

    /// The backend directory service to use.
    #[serde(default)]
    pub resource_backend: ResourceBackend,

    /// Caching configuration.
    #[serde(default)]
    pub cache: Cache,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceBackend {
    /// Dummy backend that adds no extra user information.
    None {},

    /// Backend that fetches user information from DQuantum.
    DQuantum(DQuantumBackend),
    /// Backend that fetches user information from Gravitino.
    Gravitino(GravitinoBackend),
    Datahub(DatahubBackend),
}

impl Default for ResourceBackend {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DQuantumBackend {
    pub hostname: String,
    pub port: Option<u16>,

    #[serde(flatten)]
    pub tls: TlsClientDetails,

    /// Name of a Secret that contains client credentials of a Keycloak account with permission to read user metadata.
    ///
    /// Must contain the fields `clientId` and `clientSecret`.
    pub client_credentials_secret: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GravitinoBackend {
    pub hostname: String,
    pub port: Option<u16>,

    #[serde(flatten)]
    pub tls: TlsClientDetails,

    /// Name of a Secret that contains client credentials of a Keycloak account with permission to read user metadata.
    ///
    /// Must contain the fields `clientId` and `clientSecret`.
    pub client_credentials_secret: String,
}


#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatahubBackend {
    pub hostname: String,
    pub port: Option<u16>,

    #[serde(flatten)]
    pub tls: TlsClientDetails,

    /// Name of a Secret that contains a token to access the DataHub api with.
    ///
    /// Must contain the field `bearerToken`.
    pub bearer_token_secret: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Backend {
    /// Dummy backend that adds no extra user information.
    None {},

    /// Backend that fetches user information from Keycloak.
    Keycloak(KeycloakBackend),

    /// Backend that fetches user information from the Gaia-X
    /// Cross Federation Services Components (XFSC) Authentication & Authorization Service.
    ExperimentalXfscAas(AasBackend),
}

impl Default for Backend {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeycloakBackend {
    /// Hostname of the identity provider, e.g. `my.keycloak.corp`.
    pub hostname: String,

    /// Port of the identity provider. If TLS is used defaults to `443`, otherwise to `80`.
    pub port: Option<u16>,

    /// Root HTTP path of the identity provider. Defaults to `/`.
    #[serde(default = "default_root_path")]
    pub root_path: String,

    /// Use a TLS connection. If not specified no TLS will be used.
    #[serde(flatten)]
    pub tls: TlsClientDetails,

    /// Name of a Secret that contains client credentials of a Keycloak account with permission to read user metadata.
    ///
    /// Must contain the fields `clientId` and `clientSecret`.
    pub client_credentials_secret: String,

    /// The Keycloak realm that OPA's Keycloak account (as specified by `credentialsSecretName` exists in).
    ///
    /// Typically `master`.
    pub admin_realm: String,

    /// The Keycloak realm that user metadata should be resolved from.
    pub user_realm: String,
}

fn default_root_path() -> String {
    "/".to_string()
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AasBackend {
    /// Hostname of the identity provider, e.g. `my.aas.corp`.
    pub hostname: String,

    /// Port of the identity provider. Defaults to port 5000.
    #[serde(default = "aas_default_port")]
    pub port: u16,
}

fn aas_default_port() -> u16 {
    5000
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, Derivative)]
#[derivative(Default)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    /// How long metadata about each user should be cached for.
    #[derivative(Default(value = "Cache::default_entry_time_to_live()"))]
    #[serde(default = "Cache::default_entry_time_to_live")]
    pub entry_time_to_live: Duration,
}

impl Cache {
    const fn default_entry_time_to_live() -> Duration {
        Duration::from_minutes_unchecked(1)
    }
}
