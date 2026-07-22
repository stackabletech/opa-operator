use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::{networking::HostName, tls_verification::TlsClientDetails},
    schemars::{self, JsonSchema},
    v2::types::kubernetes::SecretName,
    versioned::versioned,
};

use crate::crd::cache::Cache;

#[versioned(version(name = "v1alpha1"))]
pub mod versioned {
    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Config {
        /// The backend directory service to use.
        pub backend: Backend,

        /// Caching configuration.
        #[serde(default)]
        pub cache: Cache,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Backend {
        /// Backend that fetches resource information from DataHub.
        DataHub(DataHubBackend),
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DataHubBackend {
        /// Hostname of DataHub
        pub hostname: HostName,

        /// Port of DataHub. If TLS is used defaults to `443`, otherwise to `80`.
        pub port: Option<u16>,

        /// Use a TLS connection. If not specified then no TLS will be used.
        #[serde(flatten)]
        pub tls: TlsClientDetails,

        /// Name of a Secret containing a DataHub Personal Access Token (PAT) that is authorized
        /// to read resource metadata.
        ///
        /// Must contain the field `token`.
        pub credentials_secret_name: SecretName,

        /// The env in DataHub, defaults to `PROD`
        #[serde(default = "default_data_hub_env")]
        pub env: String,
    }
}

// We actually use it as serde(default) value, but Rust fails to see that
#[allow(dead_code)]
fn default_data_hub_env() -> String {
    "PROD".to_owned()
}
