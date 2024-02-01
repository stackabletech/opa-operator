use hyper::StatusCode;
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to execute request"))]
    HttpRequest { source: reqwest::Error },

    #[snafu(display("failed to parse json response"))]
    ParseJson { source: reqwest::Error },

    #[snafu(display("response was an HTTP error: {text}"))]
    HttpErrorResponse { status: StatusCode, text: String },

    #[snafu(display("response was an HTTP error with undecodable text"))]
    HttpErrorResponseUndecodableText {
        status: StatusCode,
        encoding_error: reqwest::Error,
    },
}

pub async fn send_json_request<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, Error> {
    // make the request
    let response = req.send().await.context(HttpRequestSnafu)?;
    // check for client or server errors
    let non_error_response = get_non_error_response(response).await?;
    // parse the result
    let result = non_error_response.json().await.context(ParseJsonSnafu)?;
    Ok(result)
}

/// takes a Response and checks whether it is an error. If so, parse the reqwest Error
/// and create our own error type with more context added. We do this because the plain
/// reqwest error does not give any response body context.
async fn get_non_error_response(response: Response) -> Result<Response, Error> {
    let status = response.status();
    // good response
    if status.is_success() || status.is_informational() || status.is_redirection() {
        Ok(response)
    }
    // error response branch -> get the text and raise an error
    else {
        match response.text().await {
            Ok(text) => HttpErrorResponseSnafu { status, text }.fail(),
            Err(encoding_error) => HttpErrorResponseUndecodableTextSnafu {
                status,
                encoding_error,
            }
            .fail(),
        }
    }
}

// TODO fix all callsites for this function
