use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    Episode, ExportSectionTruncation, Fact, MemoryEvent, MemorySource, MemoryType,
    UserMemoryExport, message_role_from_str, message_role_to_str,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::memories::MemoryTypeResponse;
use super::memory_events::{MemoryEventItemResponse, MemoryEventOperationResponse};

fn default_include_events_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportUserQuery {
    #[serde(default = "default_include_events_true")]
    pub include_events: bool,
    #[serde(default)]
    pub include_deleted: bool,
    /// Opt-in COMPLETE export (every record, bounded keyset pages, verified by two
    /// identical reads, `completeness` manifest). Default false: the capped export is unchanged.
    #[serde(default)]
    pub complete: bool,
    /// Complete exports only: keyset page size, 1..=1000 (default 500).
    #[serde(default)]
    pub page_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExportUserResponse {
    pub status: &'static str,
    pub export: UserMemoryExportResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserMemoryExportResponse {
    pub org_id: String,
    pub user_id: String,
    pub exported_at: DateTime<Utc>,
    pub format_version: String,
    pub facts: Vec<ExportFactItemResponse>,
    pub memory_events: Vec<MemoryEventItemResponse>,
    /// v2 exports only (episodes enabled). Absent from v1 exports, which are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes: Option<Vec<ExportEpisodeItemResponse>>,
    /// v3 exports only (entities enabled): entities, fact links and relations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<super::entities::ExportEntitiesResponse>,
    /// Present only when the export is INCOMPLETE: each listed section reached its documented
    /// per-export limit (facts 100, memory events 100, episodes 10,000) and later records were
    /// not exported. Complete exports omit the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Vec<ExportTruncatedSectionResponse>>,
    /// Present only on a complete export (`complete=true`): exact section counts that an
    /// import verifies before any write. Legacy and capped exports never carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<ExportCompletenessResponse>,
}

/// The complete-export manifest.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportCompletenessResponse {
    /// Always `complete`.
    pub mode: String,
    pub include_deleted: bool,
    pub page_size: usize,
    /// Identical full reads the export required (2).
    pub verified_reads: usize,
    pub counts: ExportSectionCountsResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportSectionCountsResponse {
    pub facts: usize,
    pub memory_events: usize,
    pub episodes: usize,
    pub entities: usize,
    pub entity_links: usize,
    pub entity_relations: usize,
}

impl From<&memcore_core::ExportCompleteness> for ExportCompletenessResponse {
    fn from(c: &memcore_core::ExportCompleteness) -> Self {
        Self {
            mode: c.mode.clone(),
            include_deleted: c.include_deleted,
            page_size: c.page_size,
            verified_reads: c.verified_reads,
            counts: ExportSectionCountsResponse {
                facts: c.counts.facts,
                memory_events: c.counts.memory_events,
                episodes: c.counts.episodes,
                entities: c.counts.entities,
                entity_links: c.counts.entity_links,
                entity_relations: c.counts.entity_relations,
            },
        }
    }
}

impl From<ExportCompletenessResponse> for memcore_core::ExportCompleteness {
    fn from(c: ExportCompletenessResponse) -> Self {
        Self {
            mode: c.mode,
            include_deleted: c.include_deleted,
            page_size: c.page_size,
            verified_reads: c.verified_reads,
            counts: memcore_core::ExportSectionCounts {
                facts: c.counts.facts,
                memory_events: c.counts.memory_events,
                episodes: c.counts.episodes,
                entities: c.counts.entities,
                entity_links: c.counts.entity_links,
                entity_relations: c.counts.entity_relations,
            },
        }
    }
}

/// One export section that stopped at its per-export limit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportTruncatedSectionResponse {
    /// `facts`, `memory_events` or `episodes`.
    pub section: String,
    /// Exactly this many records of the section were exported.
    pub limit: usize,
}

/// An exported episode (source-message-level memory; content as stored, i.e. PII-redacted when
/// redaction was on).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportEpisodeItemResponse {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// `user`, `assistant` or `system`.
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    pub recorded_at: DateTime<Utc>,
    #[serde(default)]
    pub metadata: Value,
}

impl From<&Episode> for ExportEpisodeItemResponse {
    fn from(episode: &Episode) -> Self {
        Self {
            id: episode.id,
            session_id: episode.session_id.clone(),
            message_id: episode.message_id.clone(),
            speaker: episode.speaker.clone(),
            role: message_role_to_str(&episode.role).to_string(),
            content: episode.content.clone(),
            observed_at: episode.observed_at,
            recorded_at: episode.recorded_at,
            metadata: episode.metadata.clone(),
        }
    }
}

fn episode_from_export_item(
    item: ExportEpisodeItemResponse,
    org_id: &str,
    user_id: &str,
) -> MemcoreResult<Episode> {
    let role = message_role_from_str(&item.role).ok_or_else(|| {
        MemcoreError::ValidationError(format!("unknown episode role: {}", item.role))
    })?;
    Ok(Episode {
        id: item.id,
        org_id: org_id.to_string(),
        user_id: user_id.to_string(),
        session_id: item.session_id,
        message_id: item.message_id,
        speaker: item.speaker,
        role,
        content: item.content,
        observed_at: item.observed_at,
        recorded_at: item.recorded_at,
        embedded: false,
        metadata: item.metadata,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportFactItemResponse {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub content: String,
    pub summary: Option<String>,
    pub memory_type: MemoryTypeResponse,
    pub source: ExportMemorySourceResponse,
    pub confidence: f32,
    pub importance: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub recorded_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: Value,
    /// Structured fields. Omitted when empty, so exports of facts written without
    /// structured extraction are unchanged, and optional on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time: Option<super::memories::EventTimeResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_message_ids: Vec<String>,
    /// When memcore marked the fact superseded; bookkeeping time, not a
    /// real-world time. A superseded fact is no longer active. Omitted when unset; optional on
    /// import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<DateTime<Utc>>,
    /// The fact that superseded this one, when it was stored (provenance only).
    /// Omitted when unset; optional on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportMemorySourceResponse {
    UserMessage,
    AssistantMessage,
    Manual,
    ApiImport,
    System,
}

impl From<MemorySource> for ExportMemorySourceResponse {
    fn from(value: MemorySource) -> Self {
        match value {
            MemorySource::UserMessage => Self::UserMessage,
            MemorySource::AssistantMessage => Self::AssistantMessage,
            MemorySource::Manual => Self::Manual,
            MemorySource::ApiImport => Self::ApiImport,
            MemorySource::System => Self::System,
        }
    }
}

impl From<&Fact> for ExportFactItemResponse {
    fn from(fact: &Fact) -> Self {
        Self {
            id: fact.id,
            org_id: fact.org_id.clone(),
            user_id: fact.user_id.clone(),
            content: fact.content.clone(),
            summary: fact.summary.clone(),
            memory_type: fact.memory_type.into(),
            source: fact.source.into(),
            confidence: fact.confidence,
            importance: fact.importance,
            valid_at: fact.valid_at,
            invalid_at: fact.invalid_at,
            recorded_at: fact.recorded_at,
            updated_at: fact.updated_at,
            metadata: fact.metadata.clone(),
            subject: fact.structure.subject.clone(),
            event_time: fact
                .structure
                .event_time
                .as_ref()
                .map(super::memories::EventTimeResponse::from),
            event_time_text: fact.structure.event_time_text.clone(),
            observed_at: fact.structure.observed_at,
            session_id: fact.structure.session_id.clone(),
            source_message_ids: fact.structure.source_message_ids.clone(),
            superseded_at: fact.superseded_at,
            superseded_by: fact.superseded_by,
        }
    }
}

impl From<MemoryTypeResponse> for MemoryType {
    fn from(value: MemoryTypeResponse) -> Self {
        match value {
            MemoryTypeResponse::Profile => Self::Profile,
            MemoryTypeResponse::Preference => Self::Preference,
            MemoryTypeResponse::Project => Self::Project,
            MemoryTypeResponse::Conversation => Self::Conversation,
            MemoryTypeResponse::Task => Self::Task,
            MemoryTypeResponse::Entity => Self::Entity,
            MemoryTypeResponse::Skill => Self::Skill,
            MemoryTypeResponse::System => Self::System,
            MemoryTypeResponse::Goal => Self::Goal,
            MemoryTypeResponse::Relationship => Self::Relationship,
            MemoryTypeResponse::Event => Self::Event,
            MemoryTypeResponse::Knowledge => Self::Knowledge,
            MemoryTypeResponse::Instruction => Self::Instruction,
            MemoryTypeResponse::Other => Self::Other,
        }
    }
}

impl From<ExportMemorySourceResponse> for MemorySource {
    fn from(value: ExportMemorySourceResponse) -> Self {
        match value {
            ExportMemorySourceResponse::UserMessage => Self::UserMessage,
            ExportMemorySourceResponse::AssistantMessage => Self::AssistantMessage,
            ExportMemorySourceResponse::Manual => Self::Manual,
            ExportMemorySourceResponse::ApiImport => Self::ApiImport,
            ExportMemorySourceResponse::System => Self::System,
        }
    }
}

impl TryFrom<ExportFactItemResponse> for Fact {
    type Error = memcore_common::MemcoreError;

    fn try_from(item: ExportFactItemResponse) -> MemcoreResult<Self> {
        let structure = memcore_core::FactStructure {
            subject: item.subject.clone(),
            event_time: item
                .event_time
                .as_ref()
                .map(memcore_core::TemporalValue::try_from)
                .transpose()?,
            event_time_text: item.event_time_text.clone(),
            observed_at: item.observed_at,
            session_id: item.session_id.clone(),
            source_message_ids: item.source_message_ids.clone(),
        };
        Fact::new(
            item.id,
            item.org_id,
            item.user_id,
            item.memory_type.into(),
            item.content,
            item.summary,
            item.source.into(),
            item.confidence,
            item.importance,
            item.valid_at,
            item.invalid_at,
            item.recorded_at,
            item.updated_at,
            item.metadata,
        )
        .map(|fact| Fact {
            superseded_at: item.superseded_at,
            superseded_by: item.superseded_by,
            ..fact.with_structure(structure)
        })
    }
}

/// Converts an API export payload (from `GET .../export`) into the core import model.
pub fn user_memory_export_from_response(
    response: UserMemoryExportResponse,
) -> MemcoreResult<UserMemoryExport> {
    let org_id = response.org_id.clone();
    let user_id = response.user_id.clone();

    let facts = response
        .facts
        .into_iter()
        .map(Fact::try_from)
        .collect::<MemcoreResult<Vec<_>>>()?;

    let memory_events = response
        .memory_events
        .into_iter()
        .map(|event| memory_event_from_api_item(event, &org_id, &user_id))
        .collect();

    let episodes = response
        .episodes
        .map(|items| {
            items
                .into_iter()
                .map(|item| episode_from_export_item(item, &org_id, &user_id))
                .collect::<MemcoreResult<Vec<_>>>()
        })
        .transpose()?;

    Ok(UserMemoryExport {
        org_id: response.org_id,
        user_id: response.user_id,
        exported_at: response.exported_at,
        format_version: response.format_version,
        entities: response.entities.map(|entities| {
            super::entities::entity_export_from_response(entities, &org_id, &user_id)
        }),
        facts,
        memory_events,
        episodes,
        truncated: response.truncated.map(|sections| {
            sections
                .into_iter()
                .map(|s| ExportSectionTruncation {
                    section: s.section,
                    limit: s.limit,
                })
                .collect()
        }),
        completeness: response.completeness.map(Into::into),
    })
}

fn memory_event_from_api_item(
    item: MemoryEventItemResponse,
    org_id: &str,
    user_id: &str,
) -> MemoryEvent {
    let operation: memcore_core::MemoryEventOperation = item.operation.into();
    MemoryEvent {
        id: item.id,
        org_id: org_id.to_string(),
        user_id: user_id.to_string(),
        fact_id: item.fact_id,
        operation,
        input_text: None,
        previous_content: item.previous_content,
        new_content: item.new_content,
        provider_name: item.provider_name,
        model_name: item.model_name,
        metadata: item.metadata,
        created_at: item.created_at,
    }
}

impl From<MemoryEventOperationResponse> for memcore_core::MemoryEventOperation {
    fn from(value: MemoryEventOperationResponse) -> Self {
        match value {
            MemoryEventOperationResponse::Add => Self::Add,
            MemoryEventOperationResponse::Update => Self::Update,
            MemoryEventOperationResponse::Delete => Self::Delete,
            MemoryEventOperationResponse::NoOp => Self::NoOp,
            MemoryEventOperationResponse::ForgetUser => Self::ForgetUser,
            MemoryEventOperationResponse::Invalidate => Self::Invalidate,
            MemoryEventOperationResponse::EntityMerge => Self::EntityMerge,
            MemoryEventOperationResponse::EntitySplit => Self::EntitySplit,
        }
    }
}

impl From<UserMemoryExport> for ExportUserResponse {
    fn from(export: UserMemoryExport) -> Self {
        Self {
            status: "success",
            export: UserMemoryExportResponse {
                org_id: export.org_id,
                user_id: export.user_id,
                exported_at: export.exported_at,
                format_version: export.format_version,
                facts: export
                    .facts
                    .iter()
                    .map(ExportFactItemResponse::from)
                    .collect(),
                memory_events: export
                    .memory_events
                    .iter()
                    .map(MemoryEventItemResponse::from)
                    .collect(),
                truncated: export.truncated.as_ref().map(|sections| {
                    sections
                        .iter()
                        .map(|s| ExportTruncatedSectionResponse {
                            section: s.section.clone(),
                            limit: s.limit,
                        })
                        .collect()
                }),
                completeness: export
                    .completeness
                    .as_ref()
                    .map(ExportCompletenessResponse::from),
                entities: export
                    .entities
                    .as_ref()
                    .map(super::entities::ExportEntitiesResponse::from),
                episodes: export.episodes.as_ref().map(|episodes| {
                    episodes
                        .iter()
                        .map(ExportEpisodeItemResponse::from)
                        .collect()
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    /// P1-04 export/import: structured fields survive a round trip through the export format.
    #[test]
    fn structured_fields_round_trip_through_export_and_import() {
        use chrono::TimeZone;
        let anchor = Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap();
        let structure = memcore_core::FactStructure {
            subject: Some("Priya".to_string()),
            event_time: Some(
                memcore_core::TemporalValue::new(
                    Utc.with_ymd_and_hms(2023, 5, 7, 0, 0, 0).unwrap(),
                    Utc.with_ymd_and_hms(2023, 5, 7, 23, 59, 59).unwrap(),
                    memcore_core::TemporalGranularity::Day,
                    anchor,
                )
                .unwrap(),
            ),
            event_time_text: Some("yesterday".to_string()),
            observed_at: Some(anchor),
            session_id: Some("session_3".to_string()),
            source_message_ids: vec!["D3:1".to_string()],
        };
        let now = Utc::now();
        let fact = Fact::new(
            uuid::Uuid::new_v4(),
            "org_a",
            "user_a",
            memcore_core::MemoryType::Event,
            "Priya adopted a dog.",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            serde_json::json!({}),
        )
        .unwrap()
        .with_structure(structure.clone());

        let exported = ExportFactItemResponse::from(&fact);
        let json = serde_json::to_value(&exported).unwrap();
        assert_eq!(json["subject"], "Priya");
        let reparsed: ExportFactItemResponse = serde_json::from_value(json).unwrap();
        let imported = Fact::try_from(reparsed).unwrap();
        assert_eq!(imported.structure, structure);
    }

    /// Facts without structure export exactly the pre-Part-B keys.
    #[test]
    fn a_fact_without_structure_exports_no_new_keys() {
        let now = Utc::now();
        let fact = Fact::new(
            uuid::Uuid::new_v4(),
            "org_a",
            "user_a",
            memcore_core::MemoryType::Profile,
            "Likes tea.",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            serde_json::json!({}),
        )
        .unwrap();
        let json = serde_json::to_value(ExportFactItemResponse::from(&fact)).unwrap();
        for key in [
            "subject",
            "event_time",
            "event_time_text",
            "observed_at",
            "session_id",
            "source_message_ids",
        ] {
            assert!(json.get(key).is_none(), "{key}");
        }
    }

    use super::*;
    use memcore_core::{MemoryEventOperation, USER_EXPORT_FORMAT_VERSION};

    #[test]
    fn export_response_uses_format_version_constant() {
        let export = UserMemoryExport::new("org_a", "user_a", Vec::new(), Vec::new());
        let response = ExportUserResponse::from(export);
        assert_eq!(response.export.format_version, USER_EXPORT_FORMAT_VERSION);
    }

    #[test]
    fn memory_event_dto_omits_input_text() {
        let event = memcore_core::MemoryEvent::new(
            "org_a",
            "user_a",
            None,
            MemoryEventOperation::Add,
            None,
            Some("new".to_string()),
            None,
            None,
            serde_json::json!({}),
        );
        let dto = MemoryEventItemResponse::from(&event);
        let json = serde_json::to_value(&dto).expect("serialize");
        assert!(json.get("input_text").is_none());
    }
}
