use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use hyper::StatusCode;
use info_fetcher_commons::utils::{self, credentials::FileCredential, secret::Secret};
use ldap3::{LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry, ldap_escape};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_opa_operator::crd::user_info_fetcher::v1alpha2;
use stackable_operator::crd::authentication::ldap;

use crate::{ErrorRenderUserInfoRequest, UserInfo, UserInfoRequest, http_error};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to configure TLS"))]
    ConfigureTls { source: utils::tls::Error },

    #[snafu(display("failed to connect to LDAP"))]
    ConnectLdap { source: LdapError },

    #[snafu(display("failed to send LDAP request"))]
    RequestLdap { source: LdapError },

    #[snafu(display("failed to bind LDAP credentials"))]
    BindLdap { source: LdapError },

    #[snafu(display("failed to search LDAP for users"))]
    FindUserLdap { source: LdapError },

    #[snafu(display("unable to find user {request}"))]
    UserNotFound { request: ErrorRenderUserInfoRequest },

    #[snafu(display("failed to parse LDAP endpoint URL"))]
    ParseLdapEndpointUrl { source: ldap::v1alpha1::Error },

    #[snafu(display("unable to get username attribute \"{attribute}\" from LDAP user"))]
    MissingUsernameAttribute { attribute: String },

    #[snafu(display("failed to read the LDAP bind user"))]
    ReadBindUser { source: utils::credentials::Error },

    #[snafu(display("failed to read the LDAP bind password"))]
    ReadBindPassword { source: utils::credentials::Error },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match *self {
            Error::ConfigureTls { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::ConnectLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::RequestLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::BindLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::FindUserLdap { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::UserNotFound { .. } => StatusCode::NOT_FOUND,
            Error::ParseLdapEndpointUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::MissingUsernameAttribute { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::ReadBindUser { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Error::ReadBindPassword { .. } => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// OpenLDAP backend with resolved credentials.
///
/// This struct combines the CRD configuration with credentials loaded from the filesystem.
/// Credentials are loaded once at startup and stored internally.
pub struct ResolvedOpenLdapBackend {
    config: v1alpha2::OpenLdapBackend,
    bind_user: String,

    /// The bind password, re-read from its mounted file when the LDAP server rejects the bind, so
    /// that rotating it in the Secret takes effect without restarting the Pod.
    bind_password: FileCredential,
}

impl ResolvedOpenLdapBackend {
    /// Resolves an OpenLDAP backend by loading credentials from the filesystem.
    ///
    /// Reads bind credentials from paths specified in the configuration.
    pub async fn resolve(config: v1alpha2::OpenLdapBackend) -> Result<Self, Error> {
        let ldap_provider = config.to_ldap_provider();
        // Bind credentials are guaranteed to be present because they are required in the CRD
        let (user_path, password_path) = ldap_provider
            .bind_credentials_mount_paths()
            .expect("bind credentials must be configured for OpenLDAP backend");

        let bind_user = utils::credentials::read_credential_file(Path::new(&user_path))
            .await
            .context(ReadBindUserSnafu)?;
        let bind_password = FileCredential::load(Path::new(&password_path))
            .await
            .context(ReadBindPasswordSnafu)?;

        Ok(Self {
            config,
            bind_user,
            bind_password,
        })
    }

    #[tracing::instrument(skip(self))]
    pub(crate) async fn get_user_info(&self, request: &UserInfoRequest) -> Result<UserInfo, Error> {
        // A rejected bind is the only sign that the password changed underneath us. The rotated one
        // is already on disk by then, see [`FileCredential`]. The whole lookup is retried rather
        // than just the bind, because the connection is bound once and then searched on.
        self.bind_password
            .use_with_retry(
                "LDAP",
                |error| matches!(error, Error::BindLdap { .. }),
                |bind_password| self.get_user_info_with(request, bind_password),
            )
            .await
    }

    /// The lookup itself, made with an already-read `bind_password`.
    async fn get_user_info_with(
        &self,
        request: &UserInfoRequest,
        bind_password: Secret,
    ) -> Result<UserInfo, Error> {
        let ldap_provider = self.config.to_ldap_provider();

        let ldap_url = ldap_provider
            .endpoint_url()
            .context(ParseLdapEndpointUrlSnafu)?;

        let ldap_tls = utils::tls::configure_native_tls(&ldap_provider.tls)
            .await
            .context(ConfigureTlsSnafu)?;
        let (ldap_conn, mut ldap) = LdapConnAsync::with_settings(
            LdapConnSettings::new().set_connector(ldap_tls),
            ldap_url.as_str(),
        )
        .await
        .context(ConnectLdapSnafu)?;
        ldap3::drive!(ldap_conn);

        ldap.simple_bind(&self.bind_user, bind_password.expose())
            .await
            .context(RequestLdapSnafu)?
            .success()
            .context(BindLdapSnafu)?;

        let user_id_attribute = &self.config.user_id_attribute;
        let user_name_attribute = &self.config.user_name_attribute;
        let user_filter = match request {
            UserInfoRequest::UserInfoRequestById(id) => {
                format!("{}={}", ldap_escape(user_id_attribute), ldap_escape(&id.id))
            }
            UserInfoRequest::UserInfoRequestByName(username) => {
                format!(
                    "{}={}",
                    ldap_escape(user_name_attribute),
                    ldap_escape(&username.username)
                )
            }
        };

        let user_search_dn = &ldap_provider.search_base;
        let requested_user_attrs = [user_id_attribute.as_str(), user_name_attribute.as_str()]
            .into_iter()
            .chain(
                self.config
                    .custom_attribute_mappings
                    .values()
                    .map(String::as_str),
            )
            .collect::<Vec<&str>>();
        tracing::debug!(
            user_filter,
            ?requested_user_attrs,
            "requesting user from LDAP"
        );
        let user = ldap
            .search(
                user_search_dn,
                Scope::Subtree,
                &user_filter,
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
        tracing::debug!(?user, "got user from LDAP");

        // Search for groups that contain this user
        let groups = search_user_groups(&mut ldap, &user, &self.config).await?;

        let attrs = user_attributes(
            user_id_attribute,
            user_name_attribute,
            &user,
            groups,
            &self.config.custom_attribute_mappings,
        )
        .await?;
        Ok(attrs)
    }
}

/// Searches for groups that contain the given user.
///
/// This function performs an LDAP search to find all groups where the user is a member.
/// The search strategy depends on the `group_member_attribute`:
/// - `member`: Searches for groups where `member=<user_dn>` (DN-based, for `groupOfNames`)
/// - `memberUid`: Searches for groups where `memberUid=<username>`
///   (username-based, for `posixGroup`)
#[tracing::instrument(skip(ldap, user, config), fields(user.dn))]
async fn search_user_groups(
    ldap: &mut ldap3::Ldap,
    user: &SearchEntry,
    config: &v1alpha2::OpenLdapBackend,
) -> Result<Vec<String>, Error> {
    let group_member_attribute = &config.group_member_attribute;
    let groups_search_base = config
        .groups_search_base
        .as_ref()
        .unwrap_or(&config.search_base);

    // Determine the search value based on the attribute type
    let search_value = if group_member_attribute == "memberUid" {
        // Use username for posixGroup style
        user.attrs
            .get(&config.user_name_attribute)
            .and_then(|values| values.first())
            .map(|s| s.as_str())
            .context(MissingUsernameAttributeSnafu {
                attribute: config.user_name_attribute.clone(),
            })?
    } else {
        // Use full DN for groupOfNames style
        &user.dn
    };

    let group_filter = format!(
        "{}={}",
        ldap_escape(group_member_attribute),
        ldap_escape(search_value)
    );

    tracing::debug!(
        group_filter,
        groups_search_base,
        "searching for user's groups"
    );

    let group_results = ldap
        .search(
            groups_search_base,
            Scope::Subtree,
            &group_filter,
            vec!["cn"],
        )
        .await
        .context(RequestLdapSnafu)?
        .success()
        .context(FindUserLdapSnafu)?
        .0;

    let groups = group_results
        .into_iter()
        .map(SearchEntry::construct)
        .filter_map(|group| {
            group
                .attrs
                .get("cn")
                .and_then(|values| values.first())
                .cloned()
        })
        .collect();

    tracing::debug!(?groups, "found user groups");
    Ok(groups)
}

#[tracing::instrument(
    skip(user_id_attribute, user_name_attribute, user, custom_attribute_mappings),
    fields(user.dn),
)]
async fn user_attributes(
    user_id_attribute: &str,
    user_name_attribute: &str,
    user: &SearchEntry,
    groups: Vec<String>,
    custom_attribute_mappings: &BTreeMap<String, String>,
) -> Result<UserInfo, Error> {
    let id = user
        .attrs
        .get(user_id_attribute)
        .and_then(|values| values.first())
        .cloned();
    let username = user
        .attrs
        .get(user_name_attribute)
        .and_then(|values| values.first())
        .cloned();

    let custom_attributes = custom_attribute_mappings
        .iter()
        .filter_map(|(uif_key, ldap_key)| {
            let Some(values) = user.attrs.get(ldap_key) else {
                if user.bin_attrs.contains_key(ldap_key) {
                    tracing::warn!(
                        ?uif_key,
                        ?ldap_key,
                        "LDAP custom attribute is only returned as binary, which is not supported",
                    );
                }
                return None;
            };
            Some((
                uif_key.clone(),
                serde_json::Value::Array(
                    values
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect::<Vec<_>>(),
                ),
            ))
        })
        .collect::<HashMap<_, _>>();

    Ok(UserInfo {
        id,
        username,
        groups,
        custom_attributes,
    })
}
