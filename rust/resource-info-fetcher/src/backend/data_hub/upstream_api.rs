use serde::Deserialize;

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
    tag_urn: String,
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
    #[serde(rename = "owner")]
    owner_urn: String,
}

impl DataHubEntityResponse {
    pub fn tag_urns(&self) -> Vec<String> {
        self.aspects
            .global_tags
            .iter()
            .flat_map(|global_tag| &global_tag.value.tags)
            .map(|tag| tag.tag_urn.clone())
            .collect()
    }

    // pub fn tags(&self) -> Vec<String> {
    //     self.tag_urns()
    //         .iter()
    //         .map(|urn| urn.trim_start_matches("urn:li:tag:").to_owned())
    //         .collect()
    // }

    pub fn owner_urns(&self) -> Vec<String> {
        self.aspects
            .ownership
            .iter()
            .flat_map(|owner| &owner.value.owners)
            .map(|owner| owner.owner_urn.clone())
            .collect()
    }
}
