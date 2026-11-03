//! The read-only entity API, the entity block of v3 user
//! exports, and the per-request entity summary of an add.

use chrono::{DateTime, Utc};
use memcore_core::{
    AliasSource, Entity, EntityAlias, EntityDetail, EntityExport, EntityProcessingSummary,
    EntityRelation, EntityRole, EntityType, FactEntityLink, ListEntitiesOutput,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityTypeResponse {
    Person,
    Animal,
    Place,
    Organization,
    Object,
    Event,
    Other,
}

impl From<EntityType> for EntityTypeResponse {
    fn from(value: EntityType) -> Self {
        match value {
            EntityType::Person => Self::Person,
            EntityType::Animal => Self::Animal,
            EntityType::Place => Self::Place,
            EntityType::Organization => Self::Organization,
            EntityType::Object => Self::Object,
            EntityType::Event => Self::Event,
            EntityType::Other => Self::Other,
        }
    }
}

impl From<EntityTypeResponse> for EntityType {
    fn from(value: EntityTypeResponse) -> Self {
        match value {
            EntityTypeResponse::Person => Self::Person,
            EntityTypeResponse::Animal => Self::Animal,
            EntityTypeResponse::Place => Self::Place,
            EntityTypeResponse::Organization => Self::Organization,
            EntityTypeResponse::Object => Self::Object,
            EntityTypeResponse::Event => Self::Event,
            EntityTypeResponse::Other => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityRoleResponse {
    Subject,
    Object,
    Mentioned,
}

impl From<EntityRole> for EntityRoleResponse {
    fn from(value: EntityRole) -> Self {
        match value {
            EntityRole::Subject => Self::Subject,
            EntityRole::Object => Self::Object,
            EntityRole::Mentioned => Self::Mentioned,
        }
    }
}

impl From<EntityRoleResponse> for EntityRole {
    fn from(value: EntityRoleResponse) -> Self {
        match value {
            EntityRoleResponse::Subject => Self::Subject,
            EntityRoleResponse::Object => Self::Object,
            EntityRoleResponse::Mentioned => Self::Mentioned,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AliasSourceResponse {
    Annotation,
    Merge,
}

/// One name an entity goes by, and how it was added (a `merge` alias can be split back out).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EntityAliasResponse {
    pub name: String,
    pub source: AliasSourceResponse,
    pub added_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
}

impl From<&EntityAlias> for EntityAliasResponse {
    fn from(alias: &EntityAlias) -> Self {
        Self {
            name: alias.name.clone(),
            source: match alias.source {
                AliasSource::Annotation => AliasSourceResponse::Annotation,
                AliasSource::Merge => AliasSourceResponse::Merge,
            },
            added_at: alias.added_at,
            similarity: alias.similarity,
        }
    }
}

impl From<EntityAliasResponse> for EntityAlias {
    fn from(alias: EntityAliasResponse) -> Self {
        Self {
            name: alias.name,
            source: match alias.source {
                AliasSourceResponse::Annotation => AliasSource::Annotation,
                AliasSourceResponse::Merge => AliasSource::Merge,
            },
            added_at: alias.added_at,
            similarity: alias.similarity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EntityResponse {
    pub id: Uuid,
    pub canonical_name: String,
    pub entity_type: EntityTypeResponse,
    pub aliases: Vec<EntityAliasResponse>,
    pub summary: Option<String>,
    /// A person named as a message speaker.
    pub is_speaker: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Entity> for EntityResponse {
    fn from(entity: &Entity) -> Self {
        Self {
            id: entity.id,
            canonical_name: entity.canonical_name.clone(),
            entity_type: entity.entity_type.into(),
            aliases: entity
                .aliases
                .iter()
                .map(EntityAliasResponse::from)
                .collect(),
            summary: entity.summary.clone(),
            is_speaker: entity.is_speaker,
            created_at: entity.created_at,
            updated_at: entity.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListEntitiesQuery {
    #[serde(default)]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListEntitiesResponse {
    pub status: &'static str,
    pub entities: Vec<EntityResponse>,
    pub next_cursor: Option<String>,
}

impl From<ListEntitiesOutput> for ListEntitiesResponse {
    fn from(output: ListEntitiesOutput) -> Self {
        Self {
            status: "success",
            entities: output.entities.iter().map(EntityResponse::from).collect(),
            next_cursor: output.next_cursor,
        }
    }
}

/// A fact linked to the entity (deleted facts are not listed).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EntityFactResponse {
    pub fact_id: Uuid,
    pub role: EntityRoleResponse,
    /// The name the fact used for the entity.
    pub mention: String,
    pub content: String,
    /// Not invalidated or superseded.
    pub active: bool,
    pub valid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EntityRelationResponse {
    pub id: Uuid,
    pub source_entity_id: Uuid,
    pub source_name: Option<String>,
    pub relation: String,
    pub target_entity_id: Uuid,
    pub target_name: Option<String>,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub source_fact_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EntityDetailResponse {
    pub status: &'static str,
    pub entity: EntityResponse,
    pub facts: Vec<EntityFactResponse>,
    pub relations: Vec<EntityRelationResponse>,
}

impl From<EntityDetail> for EntityDetailResponse {
    fn from(detail: EntityDetail) -> Self {
        Self {
            status: "success",
            entity: EntityResponse::from(&detail.entity),
            facts: detail
                .facts
                .into_iter()
                .map(|fact| EntityFactResponse {
                    fact_id: fact.fact_id,
                    role: fact.role.into(),
                    mention: fact.mention,
                    content: fact.content,
                    active: fact.active,
                    valid_at: fact.valid_at,
                })
                .collect(),
            relations: detail
                .relations
                .into_iter()
                .map(|view| EntityRelationResponse {
                    id: view.relation.id,
                    source_entity_id: view.relation.source_entity_id,
                    source_name: view.source_name,
                    relation: view.relation.relation,
                    target_entity_id: view.relation.target_entity_id,
                    target_name: view.target_name,
                    valid_at: view.relation.valid_at,
                    invalid_at: view.relation.invalid_at,
                    source_fact_id: view.relation.source_fact_id,
                })
                .collect(),
        }
    }
}

/// What entity processing did for an add request (present only when entities are enabled).
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct EntityProcessingSummaryResponse {
    pub annotation_requests: usize,
    pub failed_annotations: usize,
    /// Annotations used after a deterministic syntax repair.
    pub repaired_annotations: usize,
    /// Annotation requests that used their one retry after an unusable output.
    pub retried_annotations: usize,
    /// Mentions rejected because they are not grounded in the source messages or fact or
    /// name no particular referent.
    pub rejected_mentions: usize,
    pub mentions: usize,
    pub created: usize,
    pub reused: usize,
    pub merged: usize,
    pub confirmations: usize,
    pub links: usize,
    pub relations: usize,
}

impl From<EntityProcessingSummary> for EntityProcessingSummaryResponse {
    fn from(summary: EntityProcessingSummary) -> Self {
        Self {
            annotation_requests: summary.annotation_requests,
            failed_annotations: summary.failed_annotations,
            repaired_annotations: summary.repaired_annotations,
            retried_annotations: summary.retried_annotations,
            rejected_mentions: summary.rejected_mentions,
            mentions: summary.mentions,
            created: summary.created,
            reused: summary.reused,
            merged: summary.merged,
            confirmations: summary.confirmations,
            links: summary.links,
            relations: summary.relations,
        }
    }
}

/// An exported entity (v3 exports).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportEntityItemResponse {
    pub id: Uuid,
    pub canonical_name: String,
    pub entity_type: EntityTypeResponse,
    pub aliases: Vec<EntityAliasResponse>,
    pub summary: Option<String>,
    pub is_speaker: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Embedding of the canonical name, with the model and dimension that produced it.
    /// Optional on import; an embedding from another model or dimension is never compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding_dimensions: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportEntityLinkResponse {
    pub fact_id: Uuid,
    pub entity_id: Uuid,
    pub role: EntityRoleResponse,
    pub mention: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportEntityRelationResponse {
    pub id: Uuid,
    pub source_entity_id: Uuid,
    pub relation: String,
    pub target_entity_id: Uuid,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub source_fact_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// The entity layer of a v3 export.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportEntitiesResponse {
    pub entities: Vec<ExportEntityItemResponse>,
    pub links: Vec<ExportEntityLinkResponse>,
    pub relations: Vec<ExportEntityRelationResponse>,
}

impl From<&EntityExport> for ExportEntitiesResponse {
    fn from(export: &EntityExport) -> Self {
        Self {
            entities: export
                .entities
                .iter()
                .map(|entity| ExportEntityItemResponse {
                    id: entity.id,
                    canonical_name: entity.canonical_name.clone(),
                    entity_type: entity.entity_type.into(),
                    aliases: entity
                        .aliases
                        .iter()
                        .map(EntityAliasResponse::from)
                        .collect(),
                    summary: entity.summary.clone(),
                    is_speaker: entity.is_speaker,
                    created_at: entity.created_at,
                    updated_at: entity.updated_at,
                    name_embedding: entity.name_embedding.clone(),
                    name_embedding_model: entity.name_embedding_model.clone(),
                    name_embedding_dimensions: entity.name_embedding_dimensions,
                })
                .collect(),
            links: export
                .links
                .iter()
                .map(|link| ExportEntityLinkResponse {
                    fact_id: link.fact_id,
                    entity_id: link.entity_id,
                    role: link.role.into(),
                    mention: link.mention.clone(),
                    created_at: link.created_at,
                })
                .collect(),
            relations: export
                .relations
                .iter()
                .map(|relation| ExportEntityRelationResponse {
                    id: relation.id,
                    source_entity_id: relation.source_entity_id,
                    relation: relation.relation.clone(),
                    target_entity_id: relation.target_entity_id,
                    valid_at: relation.valid_at,
                    invalid_at: relation.invalid_at,
                    source_fact_id: relation.source_fact_id,
                    created_at: relation.created_at,
                })
                .collect(),
        }
    }
}

/// The core entity layer of an API export, under the export's organization and user.
pub fn entity_export_from_response(
    response: ExportEntitiesResponse,
    org_id: &str,
    user_id: &str,
) -> EntityExport {
    EntityExport {
        entities: response
            .entities
            .into_iter()
            .map(|entity| Entity {
                id: entity.id,
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                canonical_name: entity.canonical_name,
                entity_type: entity.entity_type.into(),
                aliases: entity.aliases.into_iter().map(EntityAlias::from).collect(),
                summary: entity.summary,
                is_speaker: entity.is_speaker,
                name_embedding: entity.name_embedding,
                name_embedding_model: entity.name_embedding_model,
                name_embedding_dimensions: entity.name_embedding_dimensions,
                created_at: entity.created_at,
                updated_at: entity.updated_at,
            })
            .collect(),
        links: response
            .links
            .into_iter()
            .map(|link| FactEntityLink {
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                fact_id: link.fact_id,
                entity_id: link.entity_id,
                role: link.role.into(),
                mention: link.mention,
                created_at: link.created_at,
            })
            .collect(),
        relations: response
            .relations
            .into_iter()
            .map(|relation| EntityRelation {
                id: relation.id,
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                source_entity_id: relation.source_entity_id,
                relation: relation.relation,
                target_entity_id: relation.target_entity_id,
                valid_at: relation.valid_at,
                invalid_at: relation.invalid_at,
                source_fact_id: relation.source_fact_id,
                created_at: relation.created_at,
            })
            .collect(),
    }
}
