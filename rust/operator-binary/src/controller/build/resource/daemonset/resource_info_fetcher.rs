use std::str::FromStr;

use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{Container, resource_info_fetcher};
use stackable_operator::{
    builder::{
        self,
        pod::{PodBuilder, volume::VolumeBuilder},
    },
    commons::tls_verification::TlsClientDetailsError,
    constant,
    k8s_openapi::api::core::v1::SecretVolumeSource,
    utils::cluster_info::KubernetesClusterInfo,
    v2::builder::pod::container::{EnvVarName, EnvVarSet, new_container_builder},
};

use crate::controller::{
    ValidatedCluster, ValidatedOpaConfig,
    build::{
        self,
        resource::daemonset::{
            CONFIG_DIR, CONFIG_VOLUME_NAME, LOG_VOLUME_NAME, RESOURCE_INFO_FETCHER_CREDENTIALS_DIR,
            RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME, STACKABLE_LOG_DIR, container_name,
            read_only_mount, sidecar_container_log_level, sidecar_resource_requirements,
            stackable_rust_cli_env_vars,
        },
    },
};

constant!(CONFIG: EnvVarName = "CONFIG");
constant!(CREDENTIALS_DIR: EnvVarName = "CREDENTIALS_DIR");

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display(
        "failed to build volume or volume mount spec for the Resource Info Fetcher TLS config"
    ))]
    TlsVolumeAndMounts { source: TlsClientDetailsError },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub fn add_resource_info_fetcher_sidecar(
    pb: &mut PodBuilder,
    cluster: &ValidatedCluster,
    merged_config: &ValidatedOpaConfig,
    resource_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<()> {
    if let Some(resource_info) = &cluster.cluster_config.resource_info {
        let rif_container_name = container_name(&Container::ResourceInfoFetcher);
        let mut cb_rif = new_container_builder(&rif_container_name);

        // All operator-set environment variables of the resource-info-fetcher container, collected
        // into an `EnvVarSet` so that every name occurs only once.
        let env_vars = EnvVarSet::new()
            .with_value(
                &CONFIG,
                format!(
                    "{CONFIG_DIR}/{file}",
                    file = build::properties::ConfigFileName::ResourceInfoFetcher
                ),
            )
            .with_value(&CREDENTIALS_DIR, RESOURCE_INFO_FETCHER_CREDENTIALS_DIR)
            .merge(stackable_rust_cli_env_vars(
                cluster_info,
                sidecar_container_log_level(merged_config, &Container::ResourceInfoFetcher)
                    .to_string(),
                &Container::ResourceInfoFetcher,
            ));

        cb_rif
            .image_from_product_image(&cluster.image) // inherit the pull policy and pull secrets, and then...
            .image(resource_info_fetcher_image) // ...override the image
            .command(vec!["stackable-opa-resource-info-fetcher".to_string()])
            .add_volume_mounts([read_only_mount(CONFIG_VOLUME_NAME.as_ref(), CONFIG_DIR)])
            .context(AddVolumeMountSnafu)?
            // The sidecar writes its file logs below this directory (see
            // `stackable_rust_cli_env_vars`). They have to land on the shared log volume,
            // because that is the only place the Vector agent collects them from.
            .add_volume_mount(LOG_VOLUME_NAME.as_ref(), STACKABLE_LOG_DIR)
            .context(AddVolumeMountSnafu)?
            .resources(sidecar_resource_requirements());

        match &resource_info.backend {
            resource_info_fetcher::v1alpha1::Backend::DataHub(data_hub) => {
                pb.add_volume(
                    VolumeBuilder::new(RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref())
                        .secret(SecretVolumeSource {
                            secret_name: Some(data_hub.credentials_secret_name.to_string()),
                            ..Default::default()
                        })
                        .build(),
                )
                .context(AddVolumeSnafu)?;
                cb_rif
                    .add_volume_mounts([read_only_mount(
                        RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref(),
                        RESOURCE_INFO_FETCHER_CREDENTIALS_DIR,
                    )])
                    .context(AddVolumeMountSnafu)?;
                data_hub
                    .tls
                    .add_volumes_and_mounts(pb, vec![&mut cb_rif])
                    .context(TlsVolumeAndMountsSnafu)?;
            }
        }

        cb_rif.add_env_vars(env_vars);
        pb.add_container(cb_rif.build());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *CONFIG;
        let _ = *CREDENTIALS_DIR;
    }
}
