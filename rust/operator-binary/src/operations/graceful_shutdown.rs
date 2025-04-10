use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{SERVER_GRACEFUL_SHUTDOWN_SAFETY_OVERHEAD, v1alpha1};
use stackable_operator::builder::pod::PodBuilder;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to set terminationGracePeriod"))]
    SetTerminationGracePeriod {
        source: stackable_operator::builder::pod::Error,
    },
}

pub fn add_graceful_shutdown_config(
    merged_config: &v1alpha1::OpaConfig,
    pod_builder: &mut PodBuilder,
) -> Result<(), Error> {
    // This must be always set by the merge mechanism, as we provide a default value,
    // users can not disable graceful shutdown.
    if let Some(graceful_shutdown_timeout) = merged_config.graceful_shutdown_timeout {
        pod_builder
            .termination_grace_period(
                &(graceful_shutdown_timeout + SERVER_GRACEFUL_SHUTDOWN_SAFETY_OVERHEAD),
            )
            .context(SetTerminationGracePeriodSnafu)?;
    }

    Ok(())
}
