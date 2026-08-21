use std::str::FromStr;

use snafu::{ResultExt, Snafu};
use stackable_opa_operator::crd::{Container, user_info_fetcher};
use stackable_operator::{
    builder::{
        self,
        pod::{PodBuilder, volume::VolumeBuilder},
    },
    commons::{
        secret_class::{
            SecretClassVolume, SecretClassVolumeProvisionParts, SecretClassVolumeScope,
        },
        tls_verification::{TlsClientDetails, TlsClientDetailsError},
    },
    constant,
    crd::authentication::ldap,
    k8s_openapi::api::core::v1::SecretVolumeSource,
    utils::cluster_info::KubernetesClusterInfo,
    v2::builder::pod::container::{EnvVarName, EnvVarSet, new_container_builder},
};

use crate::controller::{
    ValidatedCluster, ValidatedOpaConfig,
    build::{
        self,
        resource::daemonset::{
            CONFIG_DIR, CONFIG_VOLUME_NAME, LOG_VOLUME_NAME, STACKABLE_LOG_DIR,
            USER_INFO_FETCHER_CREDENTIALS_DIR, USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME,
            USER_INFO_FETCHER_KERBEROS_DIR, USER_INFO_FETCHER_KERBEROS_VOLUME_NAME, container_name,
            sidecar_container_log_level, sidecar_resource_requirements,
            stackable_rust_cli_env_vars,
        },
    },
};

constant!(CONFIG: EnvVarName = "CONFIG");
constant!(CREDENTIALS_DIR: EnvVarName = "CREDENTIALS_DIR");
constant!(KRB5_CONFIG: EnvVarName = "KRB5_CONFIG");
constant!(KRB5_CLIENT_KTNAME: EnvVarName = "KRB5_CLIENT_KTNAME");
constant!(KRB5CCNAME: EnvVarName = "KRB5CCNAME");

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build volume spec for the User Info Fetcher TLS config"))]
    KerberosVolume {
        source: stackable_operator::builder::pod::Error,
    },

    #[snafu(display("failed to build volume mount spec for the User Info Fetcher TLS config"))]
    KerberosVolumeMount {
        source: stackable_operator::builder::pod::container::Error,
    },

    #[snafu(display("failed to convert the User Info Fetcher Kerberos SecretClass into a volume"))]
    ConvertKerberosSecretClassVolume {
        source: stackable_operator::commons::secret_class::SecretClassVolumeError,
    },

    #[snafu(display(
        "failed to build volume or volume mount spec for the User Info Fetcher TLS config"
    ))]
    TlsVolumeAndMounts { source: TlsClientDetailsError },

    #[snafu(display(
        "failed to build volume or volume mount spec for the User Info Fetcher LDAP config"
    ))]
    LdapVolumeAndMounts { source: ldap::v1alpha1::Error },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub fn add_user_info_fetcher_sidecar(
    pb: &mut PodBuilder,
    cluster: &ValidatedCluster,
    merged_config: &ValidatedOpaConfig,
    user_info_fetcher_image: &str,
    cluster_info: &KubernetesClusterInfo,
) -> Result<()> {
    if let Some(user_info) = &cluster.cluster_config.user_info {
        let user_info_fetcher_container_name = container_name(&Container::UserInfoFetcher);
        let mut cb_user_info_fetcher = new_container_builder(&user_info_fetcher_container_name);

        // All operator-set environment variables of the user-info-fetcher container, collected
        // into an `EnvVarSet` so that every name occurs only once. The backend match below may
        // extend it before it is added to the container.
        let mut env_vars = EnvVarSet::new()
            .with_value(
                &CONFIG,
                format!(
                    "{CONFIG_DIR}/{file}",
                    file = build::properties::ConfigFileName::UserInfoFetcher
                ),
            )
            .with_value(&CREDENTIALS_DIR, USER_INFO_FETCHER_CREDENTIALS_DIR)
            .merge(stackable_rust_cli_env_vars(
                cluster_info,
                sidecar_container_log_level(merged_config, &Container::UserInfoFetcher).to_string(),
                &Container::UserInfoFetcher,
            ));

        cb_user_info_fetcher
            .image_from_product_image(&cluster.image) // inherit the pull policy and pull secrets, and then...
            .image(user_info_fetcher_image) // ...override the image
            .command(vec!["stackable-opa-user-info-fetcher".to_string()])
            .add_volume_mount(CONFIG_VOLUME_NAME.as_ref(), CONFIG_DIR)
            .context(AddVolumeMountSnafu)?
            // The sidecar writes its file logs below this directory (see
            // `stackable_rust_cli_env_vars`). They have to land on the shared log volume,
            // because that is the only place the Vector agent collects them from.
            .add_volume_mount(LOG_VOLUME_NAME.as_ref(), STACKABLE_LOG_DIR)
            .context(AddVolumeMountSnafu)?
            .resources(sidecar_resource_requirements());

        match &user_info.backend {
            user_info_fetcher::v1alpha2::Backend::None {} => {}
            user_info_fetcher::v1alpha2::Backend::ExperimentalXfscAas(_) => {}
            user_info_fetcher::v1alpha2::Backend::ActiveDirectory(ad) => {
                pb.add_volume(
                    SecretClassVolume::new(
                        ad.kerberos_secret_class_name.to_string(),
                        Some(SecretClassVolumeScope {
                            pod: false,
                            node: false,
                            services: vec![cluster.name.to_string()],
                            listener_volumes: Vec::new(),
                        }),
                    )
                    .to_volume(
                        USER_INFO_FETCHER_KERBEROS_VOLUME_NAME.as_ref(),
                        // The user-info-fetcher needs both the keytab (private) and the Kerberos config (public).
                        SecretClassVolumeProvisionParts::PublicPrivate,
                    )
                    .context(ConvertKerberosSecretClassVolumeSnafu)?,
                )
                .context(KerberosVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_KERBEROS_VOLUME_NAME.as_ref(),
                        USER_INFO_FETCHER_KERBEROS_DIR,
                    )
                    .context(KerberosVolumeMountSnafu)?;
                env_vars = env_vars
                    .with_value(
                        &KRB5_CONFIG,
                        format!("{USER_INFO_FETCHER_KERBEROS_DIR}/krb5.conf"),
                    )
                    .with_value(
                        &KRB5_CLIENT_KTNAME,
                        format!("{USER_INFO_FETCHER_KERBEROS_DIR}/keytab"),
                    )
                    .with_value(&KRB5CCNAME, "MEMORY:");
                ad.tls
                    .add_volumes_and_mounts(pb, vec![&mut cb_user_info_fetcher])
                    .context(TlsVolumeAndMountsSnafu)?;
            }
            user_info_fetcher::v1alpha2::Backend::Keycloak(keycloak) => {
                pb.add_volume(
                    VolumeBuilder::new(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref())
                        .secret(SecretVolumeSource {
                            secret_name: Some(keycloak.client_credentials_secret.to_string()),
                            ..Default::default()
                        })
                        .build(),
                )
                .context(AddVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref(),
                        USER_INFO_FETCHER_CREDENTIALS_DIR,
                    )
                    .context(AddVolumeMountSnafu)?;
                keycloak
                    .tls
                    .add_volumes_and_mounts(pb, vec![&mut cb_user_info_fetcher])
                    .context(TlsVolumeAndMountsSnafu)?;
            }
            user_info_fetcher::v1alpha2::Backend::Entra(entra) => {
                pb.add_volume(
                    VolumeBuilder::new(USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref())
                        .secret(SecretVolumeSource {
                            secret_name: Some(entra.client_credentials_secret.to_string()),
                            ..Default::default()
                        })
                        .build(),
                )
                .context(AddVolumeSnafu)?;
                cb_user_info_fetcher
                    .add_volume_mount(
                        USER_INFO_FETCHER_CREDENTIALS_VOLUME_NAME.as_ref(),
                        USER_INFO_FETCHER_CREDENTIALS_DIR,
                    )
                    .context(AddVolumeMountSnafu)?;

                TlsClientDetails {
                    tls: entra.tls.clone(),
                }
                .add_volumes_and_mounts(pb, vec![&mut cb_user_info_fetcher])
                .context(TlsVolumeAndMountsSnafu)?;
            }
            user_info_fetcher::v1alpha2::Backend::OpenLdap(openldap) => {
                // Reuse the logic from the LDAP `AuthenticationProvider` which handles
                // volume mounting of TLS secrets and LDAP bind credentials
                openldap
                    .to_ldap_provider()
                    .add_volumes_and_mounts(pb, vec![&mut cb_user_info_fetcher])
                    .context(LdapVolumeAndMountsSnafu)?;
            }
        }

        cb_user_info_fetcher.add_env_vars(env_vars);
        pb.add_container(cb_user_info_fetcher.build());
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
        let _ = *KRB5_CONFIG;
        let _ = *KRB5_CLIENT_KTNAME;
        let _ = *KRB5CCNAME;
    }
}
