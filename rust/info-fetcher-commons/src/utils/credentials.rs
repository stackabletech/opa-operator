//! Reading the credentials the info-fetcher backends authenticate with.

use std::{
    future::Future,
    path::{Path, PathBuf},
};

use snafu::{ResultExt, Snafu};
use tokio::sync::RwLock;

use crate::utils::secret::Secret;

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

/// A credential that is mounted as a file, held in memory, and re-read when whoever it authenticates
/// us with rejects it.
///
/// Kubernetes propagates a changed Secret into the container within about a minute, so the file on
/// disk is already correct long before anything restarts the process. Nothing re-reads it though, so
/// a rotated or revoked credential would otherwise not take effect until the Pod restarts, and there
/// is nothing that would restart it: the platform's restart controller only watches StatefulSets,
/// while OPA runs as a DaemonSet. Re-reading on rejection closes that gap without a restart.
pub struct FileCredential {
    /// [`None`] for a credential that did not come from a file and therefore cannot be refreshed.
    path: Option<PathBuf>,

    cached: RwLock<Secret>,
}

impl FileCredential {
    /// Reads the credential from `path`, see [`read_credential_file`].
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let secret = read_credential_file(&path).await?;

        Ok(Self {
            path: Some(path),
            cached: RwLock::new(secret.into()),
        })
    }

    /// A credential with no file behind it, which therefore never changes and is never re-read.
    pub fn fixed(secret: impl Into<Secret>) -> Self {
        Self {
            path: None,
            cached: RwLock::new(secret.into()),
        }
    }

    /// The credential as it was last read.
    pub async fn get(&self) -> Secret {
        self.cached.read().await.clone()
    }

    /// Re-reads the credential from disk, returning it only when it differs from the one held.
    ///
    /// [`None`] means there is nothing new to try: either the file is unchanged, or there is no file
    /// to read. Whoever rejected the credential would reject the same value again, so the caller
    /// should not spend a second request on it.
    pub async fn reload(&self) -> Result<Option<Secret>, Error> {
        let Some(path) = &self.path else {
            return Ok(None);
        };

        let refreshed = Secret::from(read_credential_file(path).await?);

        // Held across the comparison so that two callers reloading at once agree on which of them
        // saw the change.
        let mut cached = self.cached.write().await;
        if *cached == refreshed {
            return Ok(None);
        }

        *cached = refreshed.clone();

        Ok(Some(refreshed))
    }

    /// Runs `use_credential` with the credential, retrying exactly once with a freshly read one if
    /// `rejected` says the other side turned it down.
    ///
    /// The retry is skipped when the file has not changed, because the same value would only be
    /// rejected again, and a credential that is genuinely revoked would otherwise double every
    /// request we make with it. `issuer` only names who did the rejecting in the log message.
    pub async fn use_with_retry<R, U, UFut, T, E>(
        &self,
        issuer: &str,
        rejected: R,
        use_credential: U,
    ) -> Result<T, E>
    where
        R: Fn(&E) -> bool,
        U: Fn(Secret) -> UFut,
        UFut: Future<Output = Result<T, E>>,
        E: std::error::Error + 'static,
    {
        match use_credential(self.get().await).await {
            Err(error) if rejected(&error) => match self.reload().await {
                Ok(Some(refreshed)) => {
                    tracing::warn!(
                        error = &error as &dyn std::error::Error,
                        issuer,
                        "the credential was rejected and has since changed on disk; retrying once \
                        with the new one"
                    );

                    use_credential(refreshed).await
                }
                Ok(None) => {
                    tracing::warn!(
                        error = &error as &dyn std::error::Error,
                        issuer,
                        "the credential was rejected and is unchanged on disk, so it is not retried. \
                        Updating it in the Secret is picked up without restarting the Pod"
                    );

                    Err(error)
                }
                Err(reload_error) => {
                    tracing::warn!(
                        error = &reload_error as &dyn std::error::Error,
                        issuer,
                        "the credential was rejected and could not be re-read from disk; keeping \
                        the one already held"
                    );

                    Err(error)
                }
            },
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`FileCredential`] backed by a file only the test named `test` uses.
    async fn file_credential(test: &str, contents: &str) -> (PathBuf, FileCredential) {
        let path = credential_file(test, contents).await;
        let credential = FileCredential::load(path.clone())
            .await
            .expect("the credential file is readable");

        (path, credential)
    }

    /// The reason the type exists: a credential rotated in the Secret has to be picked up without
    /// restarting the process.
    #[tokio::test]
    async fn reloading_picks_up_a_rotated_credential() {
        let (path, credential) = file_credential("reload-rotated", "old-secret\n").await;
        assert_eq!(credential.get().await.expose(), "old-secret");

        tokio::fs::write(&path, "new-secret\n")
            .await
            .expect("the credential file is writable");

        let refreshed = credential
            .reload()
            .await
            .expect("the credential file is readable")
            .expect("a changed credential is reported as changed");

        assert_eq!(refreshed.expose(), "new-secret");
        assert_eq!(credential.get().await.expose(), "new-secret");
    }

    /// An unchanged file has nothing new to offer, and saying so is what stops the caller spending a
    /// second request on a credential that was just rejected.
    #[tokio::test]
    async fn reloading_an_unchanged_credential_reports_no_change() {
        let (_path, credential) = file_credential("reload-unchanged", "the-secret\n").await;

        let refreshed = credential.reload().await.expect("the file is readable");

        assert!(refreshed.is_none());
    }

    /// A credential that never came from a file cannot be refreshed.
    #[tokio::test]
    async fn a_fixed_credential_never_changes() {
        let credential = FileCredential::fixed("the-secret");

        assert_eq!(credential.get().await.expose(), "the-secret");
        assert!(
            credential
                .reload()
                .await
                .expect("a fixed credential reloads without touching the filesystem")
                .is_none()
        );
    }

    /// The error a backend reports when the other side rejected the credential.
    #[derive(Debug, Snafu)]
    #[snafu(display("rejected: {rejected}"))]
    struct Rejected {
        rejected: bool,
    }

    /// Records what each attempt was given, so a test can assert both the number of attempts and
    /// which credential each of them used.
    #[derive(Default)]
    struct Attempts(std::sync::Mutex<Vec<String>>);

    impl Attempts {
        /// Fails every attempt made with `rejects`, so the retry is driven by the credential rather
        /// than by the attempt count.
        async fn attempt(&self, credential: Secret, rejects: &str) -> Result<String, Rejected> {
            let credential = credential.expose().to_owned();
            self.0
                .lock()
                .expect("the attempt log is not poisoned")
                .push(credential.clone());

            match credential == rejects {
                true => Err(Rejected { rejected: true }),
                false => Ok(credential),
            }
        }

        fn log(&self) -> Vec<String> {
            self.0
                .lock()
                .expect("the attempt log is not poisoned")
                .clone()
        }
    }

    /// A rejected credential that has since been rotated must be retried with the new value, so that
    /// rotating the Secret heals the backend without a restart.
    #[tokio::test]
    async fn a_rejected_credential_is_reread_and_the_call_retried_once() {
        let (path, credential) = file_credential("retry-rotated", "old-secret").await;
        let attempts = Attempts::default();

        // Rotated behind our back, exactly as kubelet would after the Secret changed.
        tokio::fs::write(&path, "new-secret")
            .await
            .expect("the credential file is writable");

        let used = credential
            .use_with_retry(
                "the backend",
                |error: &Rejected| error.rejected,
                |credential| attempts.attempt(credential, "old-secret"),
            )
            .await
            .expect("the retry with the rotated credential succeeds");

        assert_eq!(used, "new-secret");
        assert_eq!(attempts.log(), ["old-secret", "new-secret"]);
    }

    /// A credential that was revoked rather than rotated is unchanged on disk, so retrying it would
    /// only be rejected again. Every request would otherwise cost the backend two.
    #[tokio::test]
    async fn a_rejected_credential_that_did_not_change_is_not_retried() {
        let (_path, credential) = file_credential("retry-unchanged", "the-secret").await;
        let attempts = Attempts::default();

        let error = credential
            .use_with_retry(
                "the backend",
                |error: &Rejected| error.rejected,
                |credential| attempts.attempt(credential, "the-secret"),
            )
            .await
            .expect_err("the credential is rejected and unchanged");

        assert!(error.rejected);
        assert_eq!(attempts.log(), ["the-secret"]);
    }

    /// Any other failure says nothing about the credential, so re-reading it would only cost a round
    /// trip.
    #[tokio::test]
    async fn other_failures_are_not_retried() {
        let (path, credential) = file_credential("retry-other-failure", "old-secret").await;
        let attempts = Attempts::default();

        tokio::fs::write(&path, "new-secret")
            .await
            .expect("the credential file is writable");

        credential
            .use_with_retry(
                "the backend",
                // Nothing counts as a rejection, so the failure below must not drive a re-read.
                |_error| false,
                |credential| attempts.attempt(credential, "old-secret"),
            )
            .await
            .expect_err("the attempt fails");

        assert_eq!(attempts.log(), ["old-secret"]);
    }

    /// A credential whose file disappeared leaves us with the one we have, and the original
    /// rejection is what the caller needs to see.
    #[tokio::test]
    async fn an_unreadable_credential_file_leaves_the_held_one_in_place() {
        let (path, credential) = file_credential("retry-unreadable", "the-secret").await;
        let attempts = Attempts::default();

        tokio::fs::remove_file(&path)
            .await
            .expect("the credential file is removable");

        let error = credential
            .use_with_retry(
                "the backend",
                |error: &Rejected| error.rejected,
                |credential| attempts.attempt(credential, "the-secret"),
            )
            .await
            .expect_err("the credential is rejected and cannot be re-read");

        assert!(error.rejected);
        assert_eq!(attempts.log(), ["the-secret"]);
        assert_eq!(credential.get().await.expose(), "the-secret");
    }

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
