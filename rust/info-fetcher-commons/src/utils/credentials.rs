//! Reading the credentials the info-fetcher backends authenticate with.

use std::path::{Path, PathBuf};

use snafu::{ResultExt, Snafu};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to read the credential file {path:?}"))]
    ReadFile {
        source: std::io::Error,
        path: PathBuf,
    },
}

/// Reads a single credential (an access token, a client secret, an LDAP bind password) from the
/// file it is mounted at.
///
/// Surrounding whitespace is trimmed. Credentials come from Secrets, whose values are routinely
/// produced with `echo`, `kubectl create secret --from-file` or an editor, all of which append a
/// trailing newline that is not part of the credential. Every use we make of one carries that
/// newline through verbatim (into a `Basic` or `Bearer` header, into a form-encoded body as `%0A`,
/// into an LDAP bind), so the credential is rejected by the other side, with an error that says
/// nothing about the newline being the cause.
pub async fn read_credential_file(path: &Path) -> Result<String, Error> {
    let credential = tokio::fs::read_to_string(path)
        .await
        .with_context(|_| ReadFileSnafu { path })?;

    Ok(credential.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `contents` to a credential file that only the test named `test` uses.
    async fn credential_file(test: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("info-fetcher-commons-credentials-{test}"));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("the temporary directory must be creatable");

        let path = dir.join("credential");
        tokio::fs::write(&path, contents)
            .await
            .expect("the credential file must be writable");

        path
    }

    /// The whole point of reading credentials through this function: the trailing newline a Secret
    /// is routinely created with is not part of the credential.
    #[tokio::test]
    async fn surrounding_whitespace_is_trimmed() {
        let path = credential_file("trimmed", "  the-secret\n").await;

        let credential = read_credential_file(&path)
            .await
            .expect("the credential file is readable");

        assert_eq!(credential, "the-secret");
    }

    /// Whitespace within a credential is part of it, so only the surrounding whitespace goes.
    #[tokio::test]
    async fn whitespace_inside_a_credential_is_kept() {
        let path = credential_file("inner-whitespace", "the secret\n").await;

        let credential = read_credential_file(&path)
            .await
            .expect("the credential file is readable");

        assert_eq!(credential, "the secret");
    }

    /// A missing credential file is the most likely failure (a Secret that does not have the key we
    /// expect), so the error has to name the path we looked at.
    #[tokio::test]
    async fn a_missing_file_is_reported_with_the_path() {
        let path = std::env::temp_dir()
            .join("info-fetcher-commons-credentials-missing")
            .join("credential");

        let error = read_credential_file(&path)
            .await
            .expect_err("the credential file does not exist");

        assert!(
            error.to_string().contains(&path.display().to_string()),
            "the error must name the path, but was: {error}"
        );
    }
}
