use derivative::Derivative;
use serde::{Deserialize, Serialize};
use stackable_operator::{
    schemars::{self, JsonSchema},
    time::Duration,
};

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub backend: Backend,
    #[serde(default)]
    pub cache: Cache,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Backend {
    None {},
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
    pub url: String,
    pub credentials_secret_name: String,
    pub admin_realm: String,
    pub user_realm: String,
    pub client_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize, Derivative)]
#[derivative(Default)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    #[derivative(Default(value = "Cache::default_entry_time_to_live()"))]
    #[serde(default = "Cache::default_entry_time_to_live")]
    pub entry_time_to_live: Duration,
}

impl Cache {
    const fn default_entry_time_to_live() -> Duration {
        Duration::from_minutes_unchecked(1)
    }
}
