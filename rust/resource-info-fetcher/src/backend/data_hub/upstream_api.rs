use std::fmt::{self, Display};

use serde::{Deserialize, Serialize};

use crate::backend::data_hub::OwnerType;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Urn(pub String);

impl Display for Urn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tag(pub String);

impl Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataHubEntityResponse {
    aspects: Aspects,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aspects {
    global_tags: Option<AspectsGlobalTags>,
    ownership: Option<AspectsOwnership>,
    tag_properties: Option<AspectsTagProperties>,
    corp_user_info: Option<AspectsCorpUserInfo>,
    corp_group_info: Option<AspectsCorpGroupInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsGlobalTags {
    value: AspectsGlobalTagsValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsGlobalTagsValue {
    tags: Vec<AspectsGlobalTagsValueTag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsGlobalTagsValueTag {
    #[serde(rename = "tag")]
    tag_urn: Urn,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsOwnership {
    value: AspectsOwnershipValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsOwnershipValue {
    owners: Vec<AspectsOwnershipValueOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsOwnershipValueOwner {
    owner: String,

    #[serde(flatten)]
    type_: AspectsOwnershipValueOwnerType,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(untagged)]
pub enum AspectsOwnershipValueOwnerType {
    Urn {
        #[serde(rename = "typeUrn")]
        type_urn: OwnerTypeUrn,
    },
    Raw {
        #[serde(rename = "type")]
        type_: RawOwnerType,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OwnerTypeUrn {
    #[serde(rename = "urn:li:ownershipType:__system__business_owner")]
    BusinessOwner,
    #[serde(rename = "urn:li:ownershipType:__system__technical_owner")]
    TechnicalOwner,
    #[serde(rename = "urn:li:ownershipType:__system__data_steward")]
    DataSteward,
    #[serde(rename = "urn:li:ownershipType:__system__none")]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawOwnerType {
    #[serde(rename = "TECHNICAL_OWNER")]
    TechnicalOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsTagProperties {
    value: AspectsTagPropertiesValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsTagPropertiesValue {
    pub name: Tag,
    color_hex: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsCorpUserInfo {
    value: AspectsCorpUserInfoValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsCorpUserInfoValue {
    pub full_name: Option<String>,
    pub display_name: String,
    pub email: Option<String>,
    pub active: bool,

    /// We default this field to `true`, so that we set it during deserialization from DataHub. When
    /// "faking" a user that doesn't exist in DataHub, we set it explicitly to `false`.
    #[serde(default = "true_default")]
    pub data_hub_user: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsCorpGroupInfo {
    value: AspectsCorpGroupInfoValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AspectsCorpGroupInfoValue {
    display_name: String,
    description: String,
}

fn true_default() -> bool {
    true
}

impl DataHubEntityResponse {
    pub fn tag_urns(&self) -> Vec<Urn> {
        self.aspects
            .global_tags
            .iter()
            .flat_map(|global_tag| &global_tag.value.tags)
            .map(|tag| tag.tag_urn.clone())
            .collect()
    }

    /// Returns
    ///
    /// 1. The list of owner user URNs that are known to DataHub
    /// 3. The list of owner users that are not know to DataHub
    /// 2. The list of owner group URNs
    pub fn owners_for_type(&self, owner_type: &OwnerType) -> (Vec<Urn>, Vec<String>, Vec<Urn>) {
        let owners = self
            .aspects
            .ownership
            .iter()
            .flat_map(|owner| &owner.value.owners)
            .filter(|owner| &OwnerType::from(&owner.type_) == owner_type);

        let user_urns = owners
            .clone()
            .filter_map(|owner| match &owner.type_ {
                AspectsOwnershipValueOwnerType::Urn { type_urn }
                    if owner.owner.starts_with("urn:li:corpuser:") =>
                {
                    Some(Urn(owner.owner.to_owned()))
                }
                _ => None,
            })
            .collect();
        let users_without_urn = owners
            .clone()
            .filter_map(|owner| match &owner.type_ {
                AspectsOwnershipValueOwnerType::Raw { .. } => Some(owner.owner.to_owned()),
                AspectsOwnershipValueOwnerType::Urn { .. } => None,
            })
            .collect();
        let group_urns = owners
            .clone()
            .filter_map(|owner| match &owner.type_ {
                AspectsOwnershipValueOwnerType::Urn { type_urn }
                    if owner.owner.starts_with("urn:li:corpGroup:") =>
                {
                    Some(Urn(owner.owner.to_owned()))
                }
                _ => None,
            })
            .collect();

        (user_urns, users_without_urn, group_urns)
    }

    pub fn tag_properties(&self) -> Option<&AspectsTagPropertiesValue> {
        self.aspects
            .tag_properties
            .as_ref()
            .map(|properties| &properties.value)
    }

    pub fn user_info(&self) -> Option<&AspectsCorpUserInfoValue> {
        self.aspects.corp_user_info.as_ref().map(|info| &info.value)
    }

    pub fn group_info(&self) -> Option<&AspectsCorpGroupInfoValue> {
        self.aspects
            .corp_group_info
            .as_ref()
            .map(|info| &info.value)
    }
}
