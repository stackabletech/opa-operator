use serde::{Deserialize, Serialize};

use stackable_operator::{
    commons::{networking::HostName, tls_verification::TlsClientDetails},
    schemars::{self, JsonSchema},
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceBackend {
    /// Dummy backend that adds no extra user information.
    None {},
    /// Backend that fetches user information from DQuantum.
    DQuantum(DQuantumBackend),
}

impl Default for ResourceBackend {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DQuantumBackend {
    pub url: String,

    #[serde(flatten)]
    pub tls: TlsClientDetails,

    /// Name of a Secret that contains client credentials of a Keycloak account with permission to read user metadata.
    ///
    /// Must contain the fields `clientId` and `clientSecret`.
    pub client_credentials_secret: String,

    pub hierarchy: TableEntity,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Relation {
    Forward {
        #[serde(flatten)]
        entity: Entity,
        relation_name: String,
    },
    Backward {
        #[serde(flatten)]
        entity: Entity,
        relation_name: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableEntity {
    entity_name: String,
    entity_id: u8,
    parents: Option<Box<Relation>>,
    children: Option<Box<Relation>>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    entity_name: String,
    entity_id: u8,
    #[serde(flatten)]
    relation: Option<Box<Relation>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_info_fetcher::Relation::{Backward, Forward};
    use serde_yaml;

    #[test]
    fn it_works() {
        let parents = Some(Box::new(Backward { entity: Entity {
            entity_name: "data_collection".to_string(),
            entity_id: 150,
            relation: None,
        }, relation_name: "data_assets".to_string() }));

        let children = Some(Box::new(Backward { entity: Entity {
            entity_name: "data_element".to_string(),
            entity_id: 168,
            relation: None,
        }, relation_name: "data_asset".to_string() }));

        let table_node = TableEntity{
            entity_name: "data_asset".to_string(),
            entity_id: 151,
            parents,
            children,
        };

        /*let entity = Entity {
            entity_name: "data_collection".to_string(),
            entity_id: 150,
            relation: Some(Box::new(Forward {
                relation_name: "data_assets".to_string(),
                entity: Entity {
                    entity_name: "data_asset".to_string(),
                    entity_id: 151,
                    relation: Some(Box::new(Backward {
                        entity: Entity {
                            entity_name: "data_element".to_string(),
                            entity_id: 168,
                            relation: None,
                        },
                        relation_name: "data_asset".to_string(),
                    })),
                },

            })),
        };
        */

        let test = serde_yaml::to_string(&table_node).unwrap();
        println!("{}", test);
    }
}
