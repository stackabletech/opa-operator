use std::{io::Cursor, path::Path};

use rustls_pki_types::{CertificateDer, pem::PemObject};
use snafu::{ResultExt as _, Snafu};
use stackable_operator::commons::tls_verification::TlsClientDetails;
use tokio::{fs::File, io::AsyncReadExt};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to read ca certificates"))]
    ReadCaBundle { source: std::io::Error },

    #[snafu(display("failed to parse ca certificates (via reqwest)"))]
    ParseCaBundleReqwest { source: reqwest::Error },

    #[snafu(display("failed to split ca certificate bundle"))]
    SplitCaBundle {
        source: rustls_pki_types::pem::Error,
    },

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
        let ca_certs = reqwest::Certificate::from_pem_bundle(
            &read_file(&tls_ca_cert_mount_path)
                .await
                .context(ReadCaBundleSnafu)?,
        )
        .context(ParseCaBundleReqwestSnafu)?;
        builder.tls_certs_only(ca_certs)
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
        let mut pem_bytes = Cursor::new(
            read_file(&tls_ca_cert_mount_path)
                .await
                .context(ReadCaBundleSnafu)?,
        );
        for ca_cert in CertificateDer::pem_reader_iter(&mut pem_bytes) {
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
