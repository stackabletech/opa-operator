//! A string that must not end up in a log line.

use std::fmt;

use serde::Deserialize;

/// What [`Secret`] renders as instead of the value it holds.
const REDACTED: &str = "[redacted]";

/// A credential (an access token, a client secret) that refuses to print itself.
///
/// The fetchers log liberally, and a `#[derive(Debug)]` on a struct holding a bearer token is one
/// `?token` or `#[instrument]` away from writing that token to the log file the Vector agent ships
/// off the node. Wrapping the value means the leak has to be an explicit decision ([`Secret::expose`])
/// rather than an accident: the type has no [`Display`](fmt::Display), and its
/// [`Debug`](fmt::Debug) renders [`REDACTED`], so every struct that holds one can keep deriving
/// `Debug` safely.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Hands out the secret itself, for the one thing it is good for: sending it to the party it
    /// authenticates us with.
    ///
    /// Every call is a place the secret can leak, so there should be few of them and each should be
    /// obviously a request being built.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl<S: Into<String>> From<S> for Secret {
    fn from(secret: S) -> Self {
        Self(secret.into())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the type. Also covers the structs holding a [`Secret`], as their derived
    /// `Debug` renders their fields with this one.
    #[test]
    fn debug_does_not_render_the_secret() {
        let secret = Secret::from("hunter2");

        assert_eq!(format!("{secret:?}"), REDACTED);
    }

    /// A [`Secret`] nested in a struct that derives `Debug` (the reason the type exists) must stay
    /// redacted there too.
    #[test]
    fn debug_of_a_surrounding_struct_does_not_render_the_secret() {
        #[derive(Debug)]
        struct Holder {
            #[expect(
                dead_code,
                reason = "read through the derived Debug, which dead code analysis ignores"
            )]
            secret: Secret,
        }

        let rendered = format!(
            "{:?}",
            Holder {
                secret: Secret::from("hunter2"),
            }
        );

        assert!(
            !rendered.contains("hunter2"),
            "the secret must not be rendered, but was: {rendered}"
        );
    }

    /// Deserialized transparently, so a secret can be read straight out of a response body without
    /// ever existing as a bare [`String`].
    #[test]
    fn deserializes_from_the_bare_string() {
        let secret: Secret =
            serde_json::from_str(r#""hunter2""#).expect("a JSON string deserializes into a secret");

        assert_eq!(secret.expose(), "hunter2");
    }
}
