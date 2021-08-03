use std::num::ParseIntError;

#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // TODO: move to operator-rs
    #[error("Invalid Configmap. No name found which is required to query the ConfigMap.")]
    InvalidConfigMap,

    #[error("Kubernetes reported error: {source}")]
    KubeError {
        #[from]
        source: kube::Error,
    },

    #[error("Operator Framework reported error: {source}")]
    OperatorFrameworkError {
        #[from]
        source: stackable_operator::error::Error,
    },

    #[error("Parse caused error: {source}")]
    ParseIntError {
        #[from]
        source: ParseIntError,
    },

    #[error("Serde json reported error: {source}")]
    SerdeError {
        #[from]
        source: serde_json::Error,
    },
}
