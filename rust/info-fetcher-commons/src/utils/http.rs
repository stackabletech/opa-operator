use hyper::StatusCode;
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};
use tracing::{instrument, trace};

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
