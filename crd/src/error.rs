#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Pod has no hostname assignment, this is most probably a transient failure and should be retried: [{pod}]")]
    PodWithoutHostname { pod: String },

    #[error("Pod [{pod}] is missing the following required labels: [{labels:?}]")]
    PodMissingLabels { pod: String, labels: Vec<String> },

    #[error("Did not find any suitable OPA server. Please confirm that at least one OPA pod is up and running.")]
    OpaServerMissing,

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

    #[error("Url Framework reported error: {source}")]
    UrlError {
        #[from]
        source: url::ParseError,
    },
}

pub type OpaOperatorResult<T> = std::result::Result<T, Error>;
