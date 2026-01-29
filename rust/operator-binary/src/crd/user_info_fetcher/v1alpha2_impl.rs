use stackable_operator::{crd::authentication::ldap, shared::time::Duration};

use crate::crd::user_info_fetcher::v1alpha2;

// TODO (@Techassi): Most of these impls are the exact same across v1alpha1 and v1alpha2. Explore
// and design a more elegant solution for it.
impl Default for v1alpha2::Backend {
    fn default() -> Self {
        Self::None {}
    }
}

impl Default for v1alpha2::Cache {
    fn default() -> Self {
        Self {
            entry_time_to_live: Self::default_entry_time_to_live(),
        }
    }
}

impl v1alpha2::Cache {
    pub const fn default_entry_time_to_live() -> Duration {
        Duration::from_minutes_unchecked(1)
    }
}

impl v1alpha2::OpenLdapBackend {
    /// Returns an LDAP [`AuthenticationProvider`](ldap::v1alpha1::AuthenticationProvider) for
    /// connecting to the OpenLDAP server.
    ///
    /// Converts this OpenLdap backend configuration into a standard LDAP authentication provider
    /// that can be used by the user-info-fetcher to establish connections and query user data.
    pub fn to_ldap_provider(&self) -> ldap::v1alpha1::AuthenticationProvider {
        ldap::v1alpha1::AuthenticationProvider {
            hostname: self.hostname.clone(),
            port: self.port,
            search_base: self.search_base.clone(),
            search_filter: String::new(),
            ldap_field_names: ldap::v1alpha1::FieldNames::default(),
            bind_credentials: Some(self.bind_credentials.clone()),
            tls: self.tls.clone(),
        }
    }
}
