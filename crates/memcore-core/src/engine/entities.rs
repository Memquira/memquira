//! Entity layer in the engine.
//!
//! With `MEMCORE_ENTITIES_ENABLED` on, after an add request has stored its facts, one annotation
//! request per extraction window (the same windows fact extraction used) names the entities each
//! stored fact is about. The fact-extraction request itself is unchanged. Mentions are resolved
//! deterministically ([`crate::entities`]), linked to their facts, and the relations between them
//! stored. Off (the default), nothing here runs: no requests, no rows, and responses unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{AddMemoryInput, MemoryEngine, record_event_best_effort};
use crate::entities::annotation::{AnnotatedWindow, annotate_window};
use crate::entities::{EntityConfig, EntityProcessingSummary, EntityResolver};
use crate::pagination::{PageCursor, build_page, page_fetch_limit, parse_optional_cursor};
use crate::ports::{
    AnnotationAttempt, AnnotationFact, EntityAnnotationInput, EntityMention, EntityStore,
    MemoryMessage,
};
use crate::privacy::PiiRedactor;
use crate::{
    AliasSource, Entity, EntityRelation, EntityRole, Fact, FactEntityLink, MemoryEvent,
    MemoryEventOperation, TenantContext, normalize_entity_name,
};

/// Default and maximum page size of the entity list.
pub const DEFAULT_ENTITY_LIST_LIMIT: usize = 50;
pub const MAX_ENTITY_LIST_LIMIT: usize = 100;

/// Entity storage and settings (`MEMCORE_ENTITIES_ENABLED`).
#[derive(Clone)]
pub struct EntityComponents {
    pub store: Arc<dyn EntityStore>,
    pub config: EntityConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListEntitiesInput {
    pub tenant: TenantContext,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListEntitiesOutput {
    pub entities: Vec<Entity>,
    pub next_cursor: Option<String>,
}

/// A fact linked to an entity, as the entity read API shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityFactView {
    pub fact_id: Uuid,
    pub role: EntityRole,
    pub mention: String,
    pub content: String,
    /// active (not invalidated or superseded).
    pub active: bool,
    pub valid_at: Option<DateTime<Utc>>,
}

/// A relation with the names of both ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRelationView {
    pub relation: EntityRelation,
    pub source_name: Option<String>,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDetail {
    pub entity: Entity,
    /// Linked facts that are not deleted, oldest link first.
    pub facts: Vec<EntityFactView>,
    pub relations: Vec<EntityRelationView>,
}

fn entities_disabled() -> MemcoreError {
    MemcoreError::ValidationError("entities are not enabled".to_string())
}

/// Distinct speaker names of a window, in order of appearance.
fn window_speakers(messages: &[MemoryMessage], redact: bool) -> Vec<String> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .filter_map(|message| message.name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            if redact {
                PiiRedactor::redact_text(name)
            } else {
                name.to_string()
            }
        })
        .filter(|name| seen.insert(normalize_entity_name(name)))
        .collect()
}

/// The facts a request stored, each once (its latest version), grouped by the first window holding
/// a message it cites; a fact citing none goes to the first window.
fn facts_by_window(windows: &[Vec<MemoryMessage>], facts: &[Fact]) -> Vec<Vec<Fact>> {
    let mut latest: Vec<Fact> = Vec::new();
    for fact in facts {
        match latest.iter_mut().find(|stored| stored.id == fact.id) {
            Some(stored) => *stored = fact.clone(),
            None => latest.push(fact.clone()),
        }
    }
    let mut grouped = vec![Vec::new(); windows.len().max(1)];
    for fact in latest {
        let index = windows
            .iter()
            .position(|window| {
                window.iter().any(|message| {
                    message.message_id.as_ref().is_some_and(|id| {
                        fact.structure
                            .source_message_ids
                            .iter()
                            .any(|cited| cited == id)
                    })
                })
            })
            .unwrap_or(0);
        grouped[index].push(fact);
    }
    grouped
}

fn redacted_mention(mention: EntityMention, redact: bool) -> EntityMention {
    if !redact {
        return mention;
    }
    EntityMention {
        name: PiiRedactor::redact_text(&mention.name),
        aliases: mention
            .aliases
            .iter()
            .map(|alias| PiiRedactor::redact_text(alias))
            .collect(),
        ..mention
    }
}

impl MemoryEngine {
    /// Enables the entity layer (`MEMCORE_ENTITIES_ENABLED`).
    pub fn with_entities(mut self, entities: Option<EntityComponents>) -> Self {
        self.entities = entities;
        self
    }

    pub fn entities_enabled(&self) -> bool {
        self.entities.is_some()
    }

    /// The entity store to use for lifecycle operations while the layer is off (forget,
    /// fact cleanup, retention, export, import). Ignored when the layer is on.
    pub fn with_entity_store(mut self, store: Option<Arc<dyn EntityStore>>) -> Self {
        self.entity_store = store;
        self
    }

    /// The store holding the user's persisted entity data, whether the layer is on or off.
    pub(super) fn entity_data_store(&self) -> Option<&Arc<dyn EntityStore>> {
        self.entities
            .as_ref()
            .map(|components| &components.store)
            .or(self.entity_store.as_ref())
    }

    /// Removes the entity links and relations of these facts and every entity left without a link
    /// (whether the layer is on or off).
    pub(super) async fn delete_entity_data_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<()> {
        if let Some(store) = self.entity_data_store() {
            store.delete_for_facts(tenant, fact_ids).await?;
        }
        Ok(())
    }

    /// Annotates and resolves the entities of the facts an add request stored. Best effort: the
    /// facts are already stored, so a failure is logged and reported in the summary, never turned
    /// into a failed request.
    pub(super) async fn process_entities(
        &self,
        components: &EntityComponents,
        input: &AddMemoryInput,
        messages: Vec<MemoryMessage>,
        facts: &[Fact],
    ) -> EntityProcessingSummary {
        let mut summary = EntityProcessingSummary::default();
        if facts.is_empty() {
            return summary;
        }
        if let Err(error) = self
            .annotate_and_resolve(components, input, messages, facts, &mut summary)
            .await
        {
            summary.failed_annotations += 1;
            tracing::warn!(
                event = "entity_processing_failed",
                error = %error,
                "entity processing failed after the facts were stored"
            );
        }
        summary
    }

    async fn annotate_and_resolve(
        &self,
        components: &EntityComponents,
        input: &AddMemoryInput,
        messages: Vec<MemoryMessage>,
        facts: &[Fact],
        summary: &mut EntityProcessingSummary,
    ) -> MemcoreResult<()> {
        let redact = self.enable_pii_redaction;
        let messages = super::structured::with_effective_times(messages, &input.session);
        let (windows, concurrency) = match self.structured_extraction {
            Some(config) => (
                super::structured::extraction_windows(
                    &messages,
                    config.window_turns,
                    config.window_overlap,
                ),
                config.concurrency,
            ),
            None => (vec![messages], 1),
        };
        let grouped = facts_by_window(&windows, facts);
        let requests: Vec<(Vec<MemoryMessage>, Vec<Fact>)> = windows
            .into_iter()
            .zip(grouped)
            .filter(|(_, facts)| !facts.is_empty())
            .collect();
        summary.annotation_requests = requests.len();

        let annotation_inputs: Vec<EntityAnnotationInput> = requests
            .iter()
            .map(|(window, window_facts)| EntityAnnotationInput {
                tenant: input.tenant.clone(),
                messages: window.clone(),
                facts: window_facts
                    .iter()
                    .enumerate()
                    .map(|(index, fact)| AnnotationFact {
                        index,
                        content: fact.content.clone(),
                        subject: fact.structure.subject.clone(),
                    })
                    .collect(),
                speakers: window_speakers(window, redact),
                attempt: AnnotationAttempt::First,
            })
            .collect();
        // Owned inputs and a cloned provider handle keep the future `Send` for every lifetime.
        let results: Vec<AnnotatedWindow> = stream::iter(annotation_inputs)
            .map(|annotation_input| {
                let llm = Arc::clone(&self.llm_provider);
                async move { annotate_window(llm.as_ref(), annotation_input).await }
            })
            .buffered(concurrency.max(1))
            .collect()
            .await;

        let mut resolver = EntityResolver::load(
            components.store.as_ref(),
            self.llm_provider.as_ref(),
            self.embedding_provider.as_ref(),
            &input.tenant,
            components.config.clone(),
        )
        .await?;
        let mut links = 0usize;
        let mut relations = 0usize;
        for ((window, window_facts), annotated) in requests.iter().zip(results) {
            let audit = &annotated.audit;
            summary.retried_annotations += usize::from(audit.retry_used);
            summary.repaired_annotations += usize::from(audit.succeeded() && audit.repaired());
            summary.rejected_mentions += audit.rejected_mentions + audit.unanchored_mentions;
            tracing::info!(
                event = "entity_annotation_audit",
                facts = window_facts.len(),
                first_attempt = ?audit.first_attempt,
                first_repairs = ?audit.first_repairs,
                retry_used = audit.retry_used,
                retry_result = ?audit.retry_result,
                retry_repairs = ?audit.retry_repairs,
                failure_reason = audit.failure_reason.as_deref(),
                rejected_mentions = audit.rejected_mentions,
                rejected_aliases = audit.rejected_aliases,
                unanchored_mentions = audit.unanchored_mentions,
                "entity annotation request audited"
            );
            let Some(output) = annotated.output else {
                summary.failed_annotations += 1;
                tracing::warn!(
                    event = "entity_annotation_failed",
                    reason = audit.failure_reason.as_deref(),
                    facts = window_facts.len(),
                    "entity annotation failed; these facts get no entities"
                );
                continue;
            };
            let speakers = window_speakers(window, redact);
            let mut seen = HashSet::new();
            for annotation in output.facts {
                let Some(fact) = window_facts.get(annotation.fact_index) else {
                    continue;
                };
                if !seen.insert(annotation.fact_index) {
                    continue;
                }
                let mut resolved: Vec<Uuid> = Vec::new();
                let mut linked: HashSet<(Uuid, EntityRole)> = HashSet::new();
                for mention in annotation.entities {
                    let mention = redacted_mention(mention, redact);
                    let Some((entity_id, _)) = resolver
                        .resolve(&mention, &speakers, fact.id, &fact.content)
                        .await?
                    else {
                        continue;
                    };
                    resolved.push(entity_id);
                    if linked.insert((entity_id, mention.role)) {
                        components
                            .store
                            .link_fact(
                                &input.tenant,
                                FactEntityLink {
                                    org_id: input.tenant.org_id.clone(),
                                    user_id: input.tenant.user_id.clone(),
                                    fact_id: fact.id,
                                    entity_id,
                                    role: mention.role,
                                    mention: mention.name.clone(),
                                    created_at: Utc::now(),
                                },
                            )
                            .await?;
                        links += 1;
                    }
                }
                let mut stated: HashSet<(Uuid, String, Uuid)> = HashSet::new();
                for relation in annotation.relations {
                    let (Some(source), Some(target)) = (
                        resolver.lookup(&resolved, &relation.subject),
                        resolver.lookup(&resolved, &relation.object),
                    ) else {
                        continue;
                    };
                    if source == target
                        || !stated.insert((source, relation.relation.clone(), target))
                    {
                        continue;
                    }
                    components
                        .store
                        .insert_relation(
                            &input.tenant,
                            EntityRelation {
                                id: Uuid::new_v4(),
                                org_id: input.tenant.org_id.clone(),
                                user_id: input.tenant.user_id.clone(),
                                source_entity_id: source,
                                relation: relation.relation.clone(),
                                target_entity_id: target,
                                valid_at: fact.valid_at,
                                invalid_at: fact.invalid_at,
                                source_fact_id: Some(fact.id),
                                created_at: Utc::now(),
                            },
                        )
                        .await?;
                    relations += 1;
                }
            }
        }

        for merge in &resolver.merges {
            record_event_best_effort(
                &self.event_store,
                &input.tenant,
                MemoryEvent::new(
                    input.tenant.org_id.clone(),
                    input.tenant.user_id.clone(),
                    Some(merge.fact_id),
                    MemoryEventOperation::EntityMerge,
                    Some(merge.canonical_name.clone()),
                    Some(merge.alias.clone()),
                    self.audit_provider_name.clone(),
                    self.audit_model_name.clone(),
                    json!({
                        "entity_id": merge.entity_id,
                        "alias": merge.alias,
                        "similarity": merge.similarity,
                        "confirmed_by": "model",
                    }),
                ),
            )
            .await;
        }
        *summary = EntityProcessingSummary {
            annotation_requests: summary.annotation_requests,
            failed_annotations: summary.failed_annotations,
            repaired_annotations: summary.repaired_annotations,
            retried_annotations: summary.retried_annotations,
            rejected_mentions: summary.rejected_mentions,
            links,
            relations,
            ..resolver.summary
        };
        Ok(())
    }

    /// The user's entity layer for an export (v3): every entity (with its name embedding and its
    /// provenance), its fact links and relations.
    pub(super) async fn export_entities(
        &self,
        tenant: &TenantContext,
        store: &dyn EntityStore,
    ) -> MemcoreResult<crate::EntityExport> {
        let entities = store.list_all_entities(tenant).await?;
        let mut links = Vec::new();
        let mut relations: Vec<EntityRelation> = Vec::new();
        for entity in &entities {
            links.extend(store.links_for_entity(tenant, entity.id).await?);
            for relation in store.relations_for_entity(tenant, entity.id).await? {
                if !relations.iter().any(|stored| stored.id == relation.id) {
                    relations.push(relation);
                }
            }
        }
        // Name embeddings travel with their provenance, so an import never has to guess
        // which embedding space they belong to.
        Ok(crate::EntityExport {
            entities,
            links,
            relations,
        })
    }

    /// Imports an exported entity layer under `tenant` with new entity IDs (so an append import
    /// never collides), following the facts to their imported IDs (`fact_ids`). Links to facts that
    /// were not imported are dropped; name embeddings keep their provenance. Returns the entities
    /// stored.
    pub(super) async fn import_entities(
        &self,
        tenant: &TenantContext,
        store: &dyn EntityStore,
        exported: &crate::EntityExport,
        fact_ids: &HashMap<Uuid, Uuid>,
    ) -> MemcoreResult<usize> {
        let mut entity_ids: HashMap<Uuid, Uuid> = HashMap::new();
        for entity in &exported.entities {
            // The exported name embedding and its provenance are kept as they are; one from another
            // model or dimension is simply never compared.
            let stored = store
                .upsert_entity(
                    tenant,
                    Entity {
                        id: Uuid::new_v4(),
                        org_id: tenant.org_id.clone(),
                        user_id: tenant.user_id.clone(),
                        ..entity.clone()
                    },
                )
                .await?;
            entity_ids.insert(entity.id, stored.id);
        }
        for link in &exported.links {
            let (Some(fact_id), Some(entity_id)) =
                (fact_ids.get(&link.fact_id), entity_ids.get(&link.entity_id))
            else {
                continue;
            };
            store
                .link_fact(
                    tenant,
                    FactEntityLink {
                        org_id: tenant.org_id.clone(),
                        user_id: tenant.user_id.clone(),
                        fact_id: *fact_id,
                        entity_id: *entity_id,
                        ..link.clone()
                    },
                )
                .await?;
        }
        for relation in &exported.relations {
            let (Some(source), Some(target)) = (
                entity_ids.get(&relation.source_entity_id),
                entity_ids.get(&relation.target_entity_id),
            ) else {
                continue;
            };
            store
                .insert_relation(
                    tenant,
                    EntityRelation {
                        id: Uuid::new_v4(),
                        org_id: tenant.org_id.clone(),
                        user_id: tenant.user_id.clone(),
                        source_entity_id: *source,
                        target_entity_id: *target,
                        source_fact_id: relation
                            .source_fact_id
                            .and_then(|id| fact_ids.get(&id).copied()),
                        ..relation.clone()
                    },
                )
                .await?;
        }
        Ok(entity_ids.len())
    }

    fn entity_components(&self) -> MemcoreResult<&EntityComponents> {
        self.entities.as_ref().ok_or_else(entities_disabled)
    }

    /// A page of the user's entities, newest first. 400 when entities are disabled.
    pub async fn list_entities(
        &self,
        input: ListEntitiesInput,
    ) -> MemcoreResult<ListEntitiesOutput> {
        super::validate_tenant(&input.tenant)?;
        let components = self.entity_components()?;
        let limit = match input.limit {
            0 => DEFAULT_ENTITY_LIST_LIMIT,
            limit if limit > MAX_ENTITY_LIST_LIMIT => {
                return Err(MemcoreError::ValidationError(format!(
                    "limit must be at most {MAX_ENTITY_LIST_LIMIT}"
                )));
            }
            limit => limit,
        };
        let cursor = parse_optional_cursor(input.cursor)?;
        let entities = components
            .store
            .list_entities_page(&input.tenant, page_fetch_limit(limit), cursor.as_ref())
            .await?;
        let page = build_page(entities, limit, |entity: &Entity| PageCursor {
            last_id: entity.id.to_string(),
            last_sort_value: entity.created_at,
        })?;
        Ok(ListEntitiesOutput {
            entities: page.items,
            next_cursor: page.next_cursor,
        })
    }

    /// One of the user's entities with its linked facts and relations; `None` when the user has no
    /// such entity. 400 when entities are disabled.
    pub async fn get_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Option<EntityDetail>> {
        super::validate_tenant(tenant)?;
        let components = self.entity_components()?;
        let Some(entity) = components.store.get_entity(tenant, entity_id).await? else {
            return Ok(None);
        };
        let mut facts = Vec::new();
        for link in components.store.links_for_entity(tenant, entity_id).await? {
            if let Some(fact) = self.fact_store.get_fact(tenant, link.fact_id).await? {
                facts.push(EntityFactView {
                    fact_id: fact.id,
                    role: link.role,
                    mention: link.mention,
                    active: fact.is_active(),
                    valid_at: fact.valid_at,
                    content: fact.content,
                });
            }
        }
        let mut names: HashMap<Uuid, Option<String>> = HashMap::new();
        names.insert(entity.id, Some(entity.canonical_name.clone()));
        let mut relations = Vec::new();
        for relation in components
            .store
            .relations_for_entity(tenant, entity_id)
            .await?
        {
            for id in [relation.source_entity_id, relation.target_entity_id] {
                if let std::collections::hash_map::Entry::Vacant(slot) = names.entry(id) {
                    let name = components
                        .store
                        .get_entity(tenant, id)
                        .await?
                        .map(|other| other.canonical_name);
                    slot.insert(name);
                }
            }
            relations.push(EntityRelationView {
                source_name: names[&relation.source_entity_id].clone(),
                target_name: names[&relation.target_entity_id].clone(),
                relation,
            });
        }
        Ok(Some(EntityDetail {
            entity: Entity {
                name_embedding: None,
                ..entity
            },
            facts,
            relations,
        }))
    }

    /// Reverses a merge: the merged alias leaves the entity and becomes an
    /// entity of its own, and the fact links made under that name move with it. Returns the new
    /// entity.
    pub async fn split_entity_alias(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
        alias: &str,
    ) -> MemcoreResult<Entity> {
        super::validate_tenant(tenant)?;
        let components = self.entity_components()?;
        let mut entity = components
            .store
            .get_entity(tenant, entity_id)
            .await?
            .ok_or_else(|| MemcoreError::NotFound("entity not found".to_string()))?;
        let normalized = normalize_entity_name(alias);
        let position = entity
            .aliases
            .iter()
            .position(|record| {
                record.source == AliasSource::Merge
                    && normalize_entity_name(&record.name) == normalized
            })
            .ok_or_else(|| {
                MemcoreError::ValidationError("no merged alias with that name".to_string())
            })?;
        let removed = entity.aliases.remove(position);
        let embedding = self.embedding_provider.embed_text(&removed.name).await?;
        let dimensions = embedding.len();
        let now = Utc::now();
        entity.updated_at = now;
        let entity = components.store.upsert_entity(tenant, entity).await?;

        let split = components
            .store
            .upsert_entity(
                tenant,
                Entity {
                    id: Uuid::new_v4(),
                    org_id: tenant.org_id.clone(),
                    user_id: tenant.user_id.clone(),
                    canonical_name: removed.name.clone(),
                    entity_type: entity.entity_type,
                    aliases: Vec::new(),
                    summary: None,
                    is_speaker: false,
                    name_embedding: Some(embedding),
                    name_embedding_model: Some(components.config.embedding_model.clone()),
                    name_embedding_dimensions: Some(dimensions),
                    created_at: now,
                    updated_at: now,
                },
            )
            .await?;
        let mut moved = 0usize;
        for link in components.store.links_for_entity(tenant, entity.id).await? {
            if normalize_entity_name(&link.mention) != normalized {
                continue;
            }
            components.store.unlink_fact(tenant, &link).await?;
            components
                .store
                .link_fact(
                    tenant,
                    FactEntityLink {
                        entity_id: split.id,
                        ..link
                    },
                )
                .await?;
            moved += 1;
        }
        record_event_best_effort(
            &self.event_store,
            tenant,
            MemoryEvent::new(
                tenant.org_id.clone(),
                tenant.user_id.clone(),
                None,
                MemoryEventOperation::EntitySplit,
                Some(entity.canonical_name.clone()),
                Some(split.canonical_name.clone()),
                self.audit_provider_name.clone(),
                self.audit_model_name.clone(),
                json!({
                    "entity_id": entity.id,
                    "split_entity_id": split.id,
                    "alias": removed.name,
                    "similarity": removed.similarity,
                    "moved_links": moved,
                }),
            ),
        )
        .await;
        Ok(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::MessageRole;

    fn message(id: &str, name: &str) -> MemoryMessage {
        MemoryMessage {
            role: MessageRole::User,
            content: "text".to_string(),
            name: Some(name.to_string()),
            timestamp: None,
            message_id: Some(id.to_string()),
        }
    }

    #[test]
    fn speakers_are_distinct_in_order() {
        let window = vec![
            message("1", "Ines"),
            message("2", "Tomasz"),
            message("3", "ines"),
        ];
        assert_eq!(window_speakers(&window, false), vec!["Ines", "Tomasz"]);
    }
}
