use std::path::Path;

use snafu::{ResultExt as _, Snafu};
use stackable_operator::commons::tls_verification::TlsClientDetails;
use tokio::{fs::File, io::AsyncReadExt};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to read ca certificates"))]
    ReadCaBundle { source: std::io::Error },

    #[snafu(display("failed to parse ca certificates (via reqwest)"))]
    ParseCaBundleReqwest { source: reqwest::Error },
}

/// Configures a [`reqwest`] client according to the specified TLS configuration
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

async fn read_file(path: &impl AsRef<Path>) -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::<u8>::new();
    File::open(path).await?.read_to_end(&mut buf).await?;
    Ok(buf)
}
