use std::{collections::HashMap, str::FromStr};

use hyper::StatusCode;
use ldap3::{ldap_escape, LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry};
use snafu::{OptionExt, ResultExt, Snafu};
use uuid::Uuid;

use crate::{http_error, ErrorRenderUserInfoRequest, UserInfo, UserInfoRequest};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to connect to LDAP"))]
    ConnectLdap { source: LdapError },

    #[snafu(display("failed to send LDAP request"))]
    RequestLdap { source: LdapError },

    #[snafu(display("failed to bind LDAP credentials"))]
    BindLdap { source: LdapError },

    #[snafu(display("failed to search LDAP for users"))]
    FindUserLdap { source: LdapError },

    #[snafu(display("invalid user ID sent by client"))]
    ParseIdByClient { source: uuid::Error },

    #[snafu(display("invalid user ID sent by LDAP"))]
    ParseIdByLdap { source: uuid::Error },

    #[snafu(display("unable to find user {request}"))]
    UserNotFound { request: ErrorRenderUserInfoRequest },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match *self {
            Error::ConnectLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::RequestLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::BindLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::FindUserLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::ParseIdByClient { .. } => StatusCode::BAD_REQUEST,
            Error::ParseIdByLdap { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::UserNotFound { .. } => StatusCode::NOT_FOUND,
        }
    }
}

const LDAP_FIELD_USER_ID: &str = "objectGUID";
const LDAP_FIELD_USER_NAME: &str = "userPrincipalName";

pub(crate) async fn get_user_info(
    request: &UserInfoRequest,
    ldap_server: &str,
) -> Result<UserInfo, Error> {
    let (ldap_conn, mut ldap) =
        LdapConnAsync::with_settings(LdapConnSettings::new().set_no_tls_verify(true), ldap_server)
            .await
            .context(ConnectLdapSnafu)?;
    ldap3::drive!(ldap_conn);
    ldap.simple_bind("asdf@sble.test", "Qwer1234")
        .await
        .context(RequestLdapSnafu)?
        .success()
        .context(BindLdapSnafu)?;
    let filter = match request {
        UserInfoRequest::UserInfoRequestById(id) => {
            format!(
                "{LDAP_FIELD_USER_ID}={}",
                ldap_escape_bytes(
                    &Uuid::from_str(&id.id)
                        .context(ParseIdByClientSnafu)?
                        .to_bytes_le()
                )
            )
        }
        UserInfoRequest::UserInfoRequestByName(username) => {
            format!("{LDAP_FIELD_USER_NAME}={}", ldap_escape(&username.username))
        }
    };
    let user = ldap
        .search(
            "DC=sble,DC=test",
            Scope::Subtree,
            &format!("(&(objectClass=user)({filter}))"),
            ["*"],
        )
        .await
        .context(RequestLdapSnafu)?
        .success()
        .context(FindUserLdapSnafu)?
        .0
        .into_iter()
        .next()
        .context(UserNotFoundSnafu { request })?;
    let user = SearchEntry::construct(user);
    let id = user
        .bin_attrs
        .get(LDAP_FIELD_USER_ID)
        .and_then(|values| values.first())
        .map(|uuid|
             // AD stores UUIDs as little-endian bytestrings
             // Technically, byte order doesn't matter to us as long as it matches the filter, but
             // we should try to be consistent with how MS tools display the UUIDs
             Uuid::from_slice_le(uuid).context(ParseIdByLdapSnafu))
        .transpose()?;
    let username = user
        .attrs
        .get(LDAP_FIELD_USER_NAME)
        .and_then(|values| values.first())
        .cloned();
    Ok(UserInfo {
        id: id.map(|id| id.to_string()),
        username,
        groups: Vec::new(),
        custom_attributes: HashMap::new(),
    })
}

/// Escapes raw byte sequences for use in LDAP filter strings
fn ldap_escape_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for byte in bytes {
        // 02 -> zero-pad to length 2
        write!(out, "\\{byte:02X}").expect("writing to string buffer failed");
    }
    out
}
