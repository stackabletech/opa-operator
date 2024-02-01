use hyper::StatusCode;
use reqwest::{RequestBuilder, Response, Url};
use serde::de::DeserializeOwned;
use snafu::{ResultExt, Snafu};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to execute request"))]
    HttpRequest { source: reqwest::Error },

    #[snafu(display("failed to parse json response"))]
    ParseJson { source: reqwest::Error },

    #[snafu(display("response was an HTTP error: {text}"))]
    HttpErrorResponse {
        status: StatusCode,
        url: Url,
        text: String,
    },

    #[snafu(display("response was an HTTP error with undecodable text"))]
    HttpErrorResponseUndecodableText {
        status: StatusCode,
        url: Url,
        encoding_error: reqwest::Error,
    },
}

pub async fn send_json_request<T: DeserializeOwned>(req: RequestBuilder) -> Result<T, Error> {
    // make the request
    let response = req.send().await.context(HttpRequestSnafu)?;
    // check for client or server errors
    let non_error_response = error_for_status(response).await?;
    // parse the result
    let result = non_error_response.json().await.context(ParseJsonSnafu)?;
    Ok(result)
}

/// takes a Response and checks whether it is an error. If so, parse the reqwest Error
/// and create our own error type with more context added. We do this because the plain
/// reqwest error does not give any response body context.
async fn error_for_status(response: Response) -> Result<Response, Error> {
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let url = response.url().to_owned();
        match response.text().await {
            Ok(text) => HttpErrorResponseSnafu { status, url, text }.fail()?,
            Err(encoding_error) => HttpErrorResponseUndecodableTextSnafu {
                status,
                url,
                encoding_error,
            }
            .fail()?,
        }
    } else {
        Ok(response)
    }
}
