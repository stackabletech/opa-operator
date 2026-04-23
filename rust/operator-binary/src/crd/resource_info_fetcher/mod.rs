use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::tls_verification::{CaCert, Tls, TlsClientDetails, TlsServerVerification, TlsVerification},
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
}
