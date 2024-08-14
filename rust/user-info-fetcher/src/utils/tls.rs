use std::{io::Cursor, path::Path};

use snafu::{ResultExt as _, Snafu};
use stackable_operator::commons::authentication::tls::TlsClientDetails;
use tokio::{fs::File, io::AsyncReadExt};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to read ca certificates"))]
    ReadCaBundle { source: std::io::Error },

    #[snafu(display("failed to parse ca certificates (via reqwest)"))]
    ParseCaBundleReqwest { source: reqwest::Error },

    #[snafu(display("failed to split ca certificate bundle"))]
    SplitCaBundle { source: std::io::Error },

    #[snafu(display("failed to parse ca certificate (via native_tls)"))]
    ParseCaCertNativeTls { source: native_tls::Error },

    #[snafu(display("failed to build native_tls connector"))]
    BuildNativeTlsConnector { source: native_tls::Error },
}

/// Configures a [`reqwest`] client according to the specified TLS configuration
// NOTE: MUST be kept in sync with all configure_* functions
pub async fn configure_reqwest(
    tls: &TlsClientDetails,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::ClientBuilder, Error> {
    Ok(if tls.uses_tls() && !tls.uses_tls_verification() {
        builder.danger_accept_invalid_certs(true)
    } else if let Some(tls_ca_cert_mount_path) = tls.tls_ca_cert_mount_path() {
        reqwest::Certificate::from_pem_bundle(
            &read_file(&tls_ca_cert_mount_path)
                .await
                .context(ReadCaBundleSnafu)?,
        )
        .context(ParseCaBundleReqwestSnafu)?
        .into_iter()
        .fold(
            builder.tls_built_in_root_certs(false),
            reqwest::ClientBuilder::add_root_certificate,
        )
    } else {
        builder
    })
}

/// Configures a [`native_tls`] connector according to the specified TLS configuration
// NOTE: MUST be kept in sync with all configure_* functions
pub async fn configure_native_tls(
    tls: &TlsClientDetails,
) -> Result<native_tls::TlsConnector, Error> {
    let mut builder = native_tls::TlsConnector::builder();
    if tls.uses_tls() && !tls.uses_tls_verification() {
        builder.danger_accept_invalid_certs(true);
    } else if let Some(tls_ca_cert_mount_path) = tls.tls_ca_cert_mount_path() {
        builder.disable_built_in_roots(true);
        // native-tls doesn't support parsing CA *bundles*, so split them using rustls first
        for ca_cert in rustls_pemfile::certs(&mut Cursor::new(
            read_file(&tls_ca_cert_mount_path)
                .await
                .context(ReadCaBundleSnafu)?,
        )) {
            builder.add_root_certificate(
                native_tls::Certificate::from_der(&ca_cert.context(SplitCaBundleSnafu)?)
                    .context(ParseCaCertNativeTlsSnafu)?,
            );
        }
    }
    builder.build().context(BuildNativeTlsConnectorSnafu)
}

async fn read_file(path: &impl AsRef<Path>) -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::<u8>::new();
    File::open(path).await?.read_to_end(&mut buf).await?;
    Ok(buf)
}
