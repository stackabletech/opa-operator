use std::num::ParseIntError;

#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "ConfigMap of type [{cm_type}] is for pod with generate_name [{pod_name}] is missing."
    )]
    MissingConfigMapError {
        cm_type: &'static str,
        pod_name: String,
    },

    #[error("ConfigMap of type [{cm_type}] is missing the metadata.name. Maybe the config map was not created yet?")]
    MissingConfigMapNameError { cm_type: &'static str },

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
