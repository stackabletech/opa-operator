use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    io::{Cursor, Read},
    num::ParseIntError,
    str::FromStr,
};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use hyper::StatusCode;
use ldap3::{ldap_escape, Ldap, LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry};
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

    #[snafu(display("failed to search LDAP for groups of user"))]
    FindUserGroupsLdap { source: LdapError },

    #[snafu(display("invalid user ID sent by client"))]
    ParseIdByClient { source: uuid::Error },

    #[snafu(display("invalid user ID sent by LDAP"))]
    ParseIdByLdap { source: uuid::Error },

    #[snafu(display("unable to find user {request}"))]
    UserNotFound { request: ErrorRenderUserInfoRequest },

    #[snafu(display("unable to parse user {user_dn:?}'s primary group's RID"))]
    InvalidPrimaryGroupRelativeId {
        source: ParseIntError,
        user_dn: String,
    },

    #[snafu(display("user {user_dn:?}'s SID has no subauthorities"))]
    UserSidHasNoSubauthorities { user_dn: String },

    #[snafu(display("failed to parse user {user_dn:?}'s SID"))]
    ParseUserSid {
        source: ParseSecurityIdError,
        user_dn: String,
    },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match *self {
            Error::ConnectLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::RequestLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::BindLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::FindUserLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::FindUserGroupsLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::ParseIdByClient { .. } => StatusCode::BAD_REQUEST,
            Error::ParseIdByLdap { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::UserNotFound { .. } => StatusCode::NOT_FOUND,
            Error::InvalidPrimaryGroupRelativeId { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::UserSidHasNoSubauthorities { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::ParseUserSid { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// Matching rules defined at https://learn.microsoft.com/en-us/windows/win32/adsi/search-filter-syntax#operators
/// Makes DN filters apply recursively to group membership
const LDAP_MATCHING_RULE_IN_CHAIN: &str = ":1.2.840.113556.1.4.1941:";

const LDAP_FIELD_OBJECT_ID: &str = "objectGUID";
const LDAP_FIELD_OBJECT_SECURITY_ID: &str = "objectSid";
const LDAP_FIELD_OBJECT_DISTINGUISHED_NAME: &str = "dn";
const LDAP_FIELD_USER_NAME: &str = "userPrincipalName";
const LDAP_FIELD_USER_PRIMARY_GROUP_RID: &str = "primaryGroupID";
const LDAP_FIELD_GROUP_MEMBER: &str = "member";

pub(crate) async fn get_user_info(
    request: &UserInfoRequest,
    ldap_server: &str,
    base_distinguished_name: &str,
    custom_attribute_mappings: &BTreeMap<String, String>,
) -> Result<UserInfo, Error> {
    let (ldap_conn, mut ldap) = LdapConnAsync::with_settings(
        LdapConnSettings::new().set_no_tls_verify(true),
        &format!("ldaps://{ldap_server}"),
    )
    .await
    .context(ConnectLdapSnafu)?;
    ldap3::drive!(ldap_conn);
    ldap.sasl_gssapi_bind(ldap_server)
        .await
        .context(RequestLdapSnafu)?
        .success()
        .context(BindLdapSnafu)?;
    let user_filter = match request {
        UserInfoRequest::UserInfoRequestById(id) => {
            format!(
                "{LDAP_FIELD_OBJECT_ID}={}",
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
    let requested_user_attrs = [
        LDAP_FIELD_OBJECT_SECURITY_ID,
        LDAP_FIELD_OBJECT_ID,
        LDAP_FIELD_USER_NAME,
        LDAP_FIELD_USER_PRIMARY_GROUP_RID,
    ]
    .into_iter()
    .chain(custom_attribute_mappings.values().map(String::as_str))
    .collect::<Vec<&str>>();
    let user = ldap
        .search(
            base_distinguished_name,
            Scope::Subtree,
            &format!("(&(objectClass=user)({user_filter}))"),
            requested_user_attrs,
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
    user_attributes(
        &mut ldap,
        base_distinguished_name,
        &user,
        custom_attribute_mappings,
    )
    .await
}

#[tracing::instrument(skip(ldap, base_dn, user, custom_attribute_mappings), fields(user.dn))]
async fn user_attributes(
    ldap: &mut Ldap,
    base_dn: &str,
    user: &SearchEntry,
    custom_attribute_mappings: &BTreeMap<String, String>,
) -> Result<UserInfo, Error> {
    let user_sid = user
        .bin_attrs
        .get(LDAP_FIELD_OBJECT_SECURITY_ID)
        .into_iter()
        .flatten()
        .next()
        .map(|sid| SecurityId::from_bytes(sid).context(ParseUserSidSnafu { user_dn: &user.dn }))
        .transpose()?;
    let id = user
        .bin_attrs
        .get(LDAP_FIELD_OBJECT_ID)
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
    let custom_attributes = custom_attribute_mappings
        .iter()
        .filter_map(|(uif_key, ldap_key)| {
            Some((
                uif_key.clone(),
                serde_json::Value::Array(match ldap_key.as_str() {
                    // Some fields require special handling
                    LDAP_FIELD_OBJECT_DISTINGUISHED_NAME => {
                        vec![serde_json::Value::String(user.dn.clone())]
                    }
                    LDAP_FIELD_OBJECT_ID => {
                        vec![serde_json::Value::String(id?.to_string())]
                    }
                    LDAP_FIELD_OBJECT_SECURITY_ID => {
                        vec![serde_json::Value::String(user_sid.as_ref()?.to_string())]
                    }

                    // Otherwise, try to read the string value(s)
                    _ => user
                        .attrs
                        .get(ldap_key)?
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                }),
            ))
        })
        .collect::<HashMap<_, _>>();
    let groups = if let Some(user_sid) = &user_sid {
        user_group_distinguished_names(ldap, base_dn, user, user_sid).await?
    } else {
        tracing::debug!(user.dn, "user has no SID, cannot fetch groups...");
        Vec::new()
    };

    Ok(UserInfo {
        id: id.map(|id| id.to_string()),
        username,
        groups,
        custom_attributes,
    })
}

/// Gets the distinguished names of all of `user`'s groups, both primary and secondary.
#[tracing::instrument(skip(ldap, base_dn, user, user_sid))]
async fn user_group_distinguished_names(
    ldap: &mut Ldap,
    base_dn: &str,
    user: &SearchEntry,
    user_sid: &SecurityId,
) -> Result<Vec<String>, Error> {
    // User group memberships are tricky, because users have exactly one *primary* and any number of *secondary* groups.
    // Additionally groups can be members of other groups.
    // Secondary groups are easy to read, either from reading the user's "memberOf" field, or by matching the user against
    // the groups' "member" field. Here we use the latter method, which lets us make it recursive using the
    // LDAP_MATCHING_RULE_IN_CHAIN rule.
    let secondary_groups_filter =
        format!("({LDAP_FIELD_GROUP_MEMBER}{LDAP_MATCHING_RULE_IN_CHAIN}=<SID={user_sid}>)");

    // The user's *primary* group is trickier.. It is only available as a "RID" (relative ID),
    // which is a sibling relative to the user's SID.
    let Some(primary_group_relative_id) = user
        .attrs
        .get(LDAP_FIELD_USER_PRIMARY_GROUP_RID)
        .into_iter()
        .flatten()
        .next()
        .map(|rid| {
            rid.parse::<u32>()
                .context(InvalidPrimaryGroupRelativeIdSnafu { user_dn: &user.dn })
        })
        .transpose()?
    else {
        tracing::debug!("user has no primary group");
        return Ok(Vec::new());
    };
    let mut primary_group_sid = user_sid.clone();
    *primary_group_sid
        .subauthorities
        .last_mut()
        .context(UserSidHasNoSubauthoritiesSnafu { user_dn: &user.dn })? =
        primary_group_relative_id;
    let primary_group_filter = format!("({LDAP_FIELD_OBJECT_SECURITY_ID}={primary_group_sid})");

    // We can't trivially make the primary group query recursive... but since we know the primary group's SID,
    // we can add a separate recursive filter for all of its parents.
    let primary_group_parents_filter = format!(
        "({LDAP_FIELD_GROUP_MEMBER}{LDAP_MATCHING_RULE_IN_CHAIN}=<SID={primary_group_sid}>)"
    );

    // Let's put it all together, and make it go...
    let groups_filter =
        format!("(|{primary_group_filter}{primary_group_parents_filter}{secondary_groups_filter})");
    Ok(ldap
        .search(
            base_dn,
            Scope::Subtree,
            &format!("(&(objectClass=group){groups_filter})"),
            [LDAP_FIELD_OBJECT_DISTINGUISHED_NAME],
        )
        .await
        .context(RequestLdapSnafu)?
        .success()
        .context(FindUserGroupsLdapSnafu)?
        .0
        .into_iter()
        .map(|group| SearchEntry::construct(group).dn)
        .collect::<Vec<_>>())
}

/// Escapes raw byte sequences for use in LDAP filter strings.
fn ldap_escape_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for byte in bytes {
        // 02 -> zero-pad to length 2
        write!(out, "\\{byte:02X}").expect("writing to string buffer failed");
    }
    out
}

#[derive(Snafu, Debug)]
#[snafu(module)]
pub enum ParseSecurityIdError {
    #[snafu(display("read failed"), context(false))]
    Read { source: std::io::Error },

    #[snafu(display("unknown SID format revision {revision}"))]
    InvalidRevision { revision: u8 },

    #[snafu(display("SID is longer than expected"))]
    TooLong,
}

/// An ActiveDirectory SID (Security ID) identifier for a user or group.
#[derive(Debug, Clone)]
struct SecurityId {
    revision: u8,
    identifier_authority: u64,
    subauthorities: Vec<u32>,
}

impl SecurityId {
    /// Parses a SID from the binary SID--Packet representation.
    fn from_bytes(bytes: &[u8]) -> Result<Self, ParseSecurityIdError> {
        use parse_security_id_error::*;
        let mut cursor = Cursor::new(bytes);

        // Format documented in https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/f992ad60-0fe4-4b87-9fed-beb478836861
        let revision = cursor.read_u8()?;
        match revision {
            1 => {
                assert_eq!(revision, 1);
                let subauthority_count = cursor.read_u8()?;
                // From experimentation, yes this is a mix of big- and little endian values. Just roll with it...
                let identifier_authority = cursor.read_u48::<BigEndian>()?;
                let subauthorities = (0..subauthority_count)
                    .map(|_| cursor.read_u32::<LittleEndian>())
                    .collect::<Result<Vec<_>, _>>()?;
                if cursor.bytes().next().is_some() {
                    return TooLongSnafu.fail();
                }

                Ok(Self {
                    revision,
                    identifier_authority,
                    subauthorities,
                })
            }
            _ => InvalidRevisionSnafu { revision }.fail(),
        }
    }
}

impl Display for SecurityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            revision,
            identifier_authority,
            subauthorities,
        } = self;
        // Format documented in https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/c92a27b1-c772-4fa7-a432-15df5f1b66a1
        write!(f, "S-{revision}-")?;

        // Yes, this is technically part of the spec..
        if *identifier_authority < 1 << 32 {
            write!(f, "{identifier_authority}")?;
        } else {
            write!(f, "{identifier_authority:X}")?;
        }

        for subauthority in subauthorities {
            write!(f, "-{subauthority}")?;
        }
        Ok(())
    }
}
