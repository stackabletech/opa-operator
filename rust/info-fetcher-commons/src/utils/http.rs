use std::time::Duration;

use hyper::StatusCode;
use reqwest::{ClientBuilder, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};
use tracing::{instrument, trace};

/// Overall deadline for a single outbound HTTP request.
/// Backends can issue several requests per lookup, so this bounds each one, not the lookup
/// as a whole.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Deadline for establishing the connection of an outbound request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A [`reqwest::ClientBuilder`] preconfigured with our outbound timeouts.
pub fn client_builder() -> ClientBuilder {
    ClientBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
}

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to execute request"))]
    HttpRequest { source: reqwest::Error },

    #[snafu(display("failed to parse json response"))]
    ParseJson { source: serde_json::Error },

    #[snafu(display("http response {status:?} for {url:?} with response body {text:?}"))]
    HttpErrorResponse {
        status: StatusCode,
        url: String,
        text: String,
    },

    #[snafu(display("http response {status:?} for {url:?} with an undecodable response body"))]
    HttpErrorResponseUndecodableText {
        status: StatusCode,
        url: String,
        encoding_error: reqwest::Error,
    },
}

#[instrument(skip_all)]
pub async fn send_json_request<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, Error> {
    // make the request
    let response = req.send().await.context(HttpRequestSnafu)?;
    // check for client or server errors
    let url = response.url().clone();
    let non_error_response = error_for_status(response).await?;
    // parse the result
    let json = non_error_response.text().await.context(HttpRequestSnafu)?;

    trace!(%url, json, "Got HTTP JSON response");

    serde_json::from_str(&json).context(ParseJsonSnafu)
}

/// Whether `error`, or any error it wraps, is a `401 Unauthorized` answer from a backend.
///
/// Walks the source chain because backends wrap [`Error`] in their own error types, so the 401 is
/// never the outermost error by the time a caller gets to decide whether to re-authenticate.
pub fn is_unauthorized(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |error| error.source())
        .filter_map(|error| error.downcast_ref::<Error>())
        .any(|error| error.status() == Some(StatusCode::UNAUTHORIZED))
}

impl Error {
    /// The status code the backend answered with, or [`None`] if the failure happened before there was
    /// a response to read a status off.
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::HttpErrorResponse { status, .. } => Some(*status),
            Self::HttpErrorResponseUndecodableText { status, .. } => Some(*status),
            Self::HttpRequest { .. } | Self::ParseJson { .. } => None,
        }
    }
}

/// Wraps a Response into a Result. If there is an HTTP Client or Server error,
/// extract the HTTP body (if possible) to be used as context in the returned Err.
/// This is done this because the `Response::error_for_status()` method Err variant
/// does not contain this information.
async fn error_for_status(response: Response) -> Result<Response, Error> {
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let url = response.url().to_string();
        return match response.text().await {
            Ok(text) => HttpErrorResponseSnafu {
                status,
                url,
                text: text.trim(),
            }
            .fail(),
            Err(encoding_error) => HttpErrorResponseUndecodableTextSnafu {
                status,
                url,
                encoding_error,
            }
            .fail(),
        };
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use snafu::IntoError;

    use super::*;

    /// Backends wrap transport errors in their own error types, sometimes several layers deep, so the
    /// check has to walk the source chain instead of inspecting the outermost error.
    #[derive(Snafu, Debug)]
    #[snafu(display("failed to fetch the user"))]
    struct FetchUser {
        source: Error,
    }

    #[derive(Snafu, Debug)]
    #[snafu(display("failed to get user info"))]
    struct GetUserInfo {
        source: FetchUser,
    }

    /// A backend response with `status`, wrapped the way a backend would wrap it.
    fn wrapped_response(status: StatusCode) -> GetUserInfo {
        let response = Error::HttpErrorResponse {
            status,
            url: "https://keycloak.example.com/admin/realms/my-realm/users/".to_owned(),
            text: "denied".to_owned(),
        };

        GetUserInfoSnafu.into_error(FetchUserSnafu.into_error(response))
    }

    #[test]
    fn a_wrapped_unauthorized_response_is_detected() {
        assert!(is_unauthorized(&wrapped_response(StatusCode::UNAUTHORIZED)));
    }

    /// Only a 401 means "your token is no good". A 403 says the token was understood and the actor is
    /// not allowed, which re-minting cannot fix.
    #[test]
    fn other_error_responses_are_not_unauthorized() {
        assert!(!is_unauthorized(&wrapped_response(StatusCode::FORBIDDEN)));
        assert!(!is_unauthorized(&wrapped_response(
            StatusCode::INTERNAL_SERVER_ERROR
        )));
    }

    /// A request that never got an answer has no status to look at.
    #[test]
    fn errors_without_a_response_are_not_unauthorized() {
        let error = Error::ParseJson {
            source: serde_json::from_str::<serde_json::Value>("not json")
                .expect_err("the input is not valid JSON"),
        };

        assert_eq!(error.status(), None);
        assert!(!is_unauthorized(&error));
    }
}
