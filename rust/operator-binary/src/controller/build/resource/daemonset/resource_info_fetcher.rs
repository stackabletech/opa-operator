use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{
        self,
        pod::{PodBuilder, volume::VolumeBuilder},
    },
    commons::tls_verification::TlsClientDetailsError,
    k8s_openapi::api::core::v1::SecretVolumeSource,
    utils::cluster_info::KubernetesClusterInfo,
    v2::builder::pod::container::new_container_builder,
};

use crate::{
    controller::{
        ValidatedCluster, ValidatedOpaConfig,
        build::{
            self,
            resource::daemonset::{
                CONFIG_DIR, CONFIG_VOLUME_NAME, RESOURCE_INFO_FETCHER_CREDENTIALS_DIR,
                RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME, add_stackable_rust_cli_env_vars,
                container_name, sidecar_container_log_level, sidecar_resource_requirements,
            },
        },
    },
    crd::{Container, resource_info_fetcher},
};

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

        cb_rif
            .image_from_product_image(&cluster.image) // inherit the pull policy and pull secrets, and then...
            .image(resource_info_fetcher_image) // ...override the image
            .command(vec!["stackable-opa-resource-info-fetcher".to_string()])
            .add_env_var(
                "CONFIG",
                format!(
                    "{CONFIG_DIR}/{file}",
                    file = build::properties::ConfigFileName::ResourceInfoFetcher
                ),
            )
            .add_env_var("CREDENTIALS_DIR", RESOURCE_INFO_FETCHER_CREDENTIALS_DIR)
            .add_volume_mount(CONFIG_VOLUME_NAME.as_ref(), CONFIG_DIR)
            .context(AddVolumeMountSnafu)?
            .resources(sidecar_resource_requirements());
        add_stackable_rust_cli_env_vars(
            &mut cb_rif,
            cluster_info,
            sidecar_container_log_level(merged_config, &Container::ResourceInfoFetcher).to_string(),
            &Container::ResourceInfoFetcher,
        );

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
                    .add_volume_mount(
                        RESOURCE_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref(),
                        RESOURCE_INFO_FETCHER_CREDENTIALS_DIR,
                    )
                    .context(AddVolumeMountSnafu)?;
                data_hub
                    .tls
                    .add_volumes_and_mounts(pb, vec![&mut cb_rif])
                    .context(TlsVolumeAndMountsSnafu)?;
            }
        }

        pb.add_container(cb_rif.build());
    }

    Ok(())
}
