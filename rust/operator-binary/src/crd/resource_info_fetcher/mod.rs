use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::tls_verification::{CaCert, Tls, TlsServerVerification, TlsVerification},
    schemars::{self, JsonSchema},
    shared::time::Duration,
    versioned::versioned,
};

mod v1alpha1_impl;

#[versioned(version(name = "v1alpha1"))]
pub mod versioned {
    #[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Config {
        /// The resource-catalog backend to fetch metadata from.
        #[serde(default)]
        pub backend: Backend,

        /// Caching configuration.
        #[serde(default)]
        pub cache: Cache,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub enum Backend {
        /// Dummy backend that returns empty `ResourceInfo` for every request.
        None {},

        /// Backend that fetches dataset metadata from a DataHub instance via GraphQL.
        DataHub(DataHubBackend),
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DataHubBackend {
        /// Full GraphQL endpoint URL, e.g. `http://datahub-gms:8080/api/graphql`.
        pub graphql_endpoint: String,

        /// Name of a Secret containing DataHub credentials.
        ///
        /// The Secret must contain EITHER (a) keys `username` + `password` (Basic
        /// auth; X-DataHub-Actor is set to `urn:li:corpuser:{username}`) OR (b) keys
        /// `token` + `actor` (Bearer auth; X-DataHub-Actor is set to
        /// `urn:li:corpuser:{actor}`). If both sets are present, Basic wins.
        pub credentials_secret: String,

        /// Optional TLS configuration for the GraphQL endpoint. Defaults to WebPki
        /// verification when TLS is in use.
        #[serde(default = "default_tls_web_pki")]
        pub tls: Option<Tls>,
    }

    #[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Cache {
        /// How long a resolved `ResourceInfo` stays in the sidecar's in-memory
        /// cache before the backend is queried again.
        #[serde(default = "default_entry_time_to_live")]
        pub entry_time_to_live: Duration,
    }
}

const fn default_entry_time_to_live() -> Duration {
    Duration::from_minutes_unchecked(1)
}

fn default_tls_web_pki() -> Option<Tls> {
    Some(Tls {
        verification: TlsVerification::Server(TlsServerVerification {
            ca_cert: CaCert::WebPki {},
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::v1alpha1;

    #[test]
    fn config_default_serde_roundtrip() {
        let cfg = v1alpha1::Config::default();
        let json = serde_json::to_value(&cfg).unwrap();
        let back: v1alpha1::Config = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn config_with_none_backend_parses() {
        let json = serde_json::json!({
            "backend": {"none": {}},
            "cache": {"entryTimeToLive": "60s"},
        });
        let cfg: v1alpha1::Config = serde_json::from_value(json).unwrap();
        assert!(matches!(cfg.backend, v1alpha1::Backend::None {}));
    }

    #[test]
    fn config_with_datahub_backend_parses() {
        let json = serde_json::json!({
            "backend": {
                "dataHub": {
                    "graphqlEndpoint": "http://datahub-gms:8080/api/graphql",
                    "credentialsSecret": "datahub-creds"
                }
            }
        });
        let cfg: v1alpha1::Config = serde_json::from_value(json).unwrap();
        let v1alpha1::Backend::DataHub(dh) = cfg.backend else {
            panic!("expected DataHub backend");
        };
        assert_eq!(dh.graphql_endpoint, "http://datahub-gms:8080/api/graphql");
        assert_eq!(dh.credentials_secret, "datahub-creds");
    }
}
