use derivative::Derivative;
use serde::{Deserialize, Serialize};
use stackable_operator::{
    schemars::{self, JsonSchema},
    time::Duration,
};

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// The backend directory service to use.
    #[serde(default)]
    pub backend: Backend,
    /// Caching configuration.
    #[serde(default)]
    pub cache: Cache,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Backend {
    /// Dummy backend that adds no extra user information.
    None {},
    /// Backend that fetches user information from Keycloak.
    Keycloak(KeycloakBackend),
}

impl Default for Backend {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeycloakBackend {
    /// URL of the Keycloak installation.
    pub url: String,
    /// Name of a Secret that contains credentials to a Keycloak account with permission to read user metadata.
    ///
    /// Must contain the fields `username` and `password`.
    pub credentials_secret_name: String,
    /// The Keycloak realm that OPA's Keycloak account (as specified by `credentials_secret_name` exists in).
    ///
    /// Typically `master`.
    pub admin_realm: String,
    /// The Keycloak realm that user metadata should be resolved from.
    pub user_realm: String,
    /// The Keycloak client ID for OPA to log in with. It must be allowed to use Direct Grants.
    pub client_id: String,
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
