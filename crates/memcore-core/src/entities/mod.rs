//! Entity resolution (deterministic first).
//!
//! A mention the annotation names for a fact resolves, in order, to:
//! 1. a **speaker**: a name equal (normalised) to a message speaker's name in the window is that
//!    speaker's person entity, created if needed. Speakers are authoritative and two speakers are
//!    never merged: a speaker's name never becomes an alias of another entity;
//! 2. an **exact name or alias**: an entity of a compatible type whose canonical name or an alias
//!    equals the mention (normalised), or whose canonical name equals one of the mention's
//!    aliases;
//! 3. a **candidate match**: the most similar entity of a compatible type by name embedding, at or
//!    above the predeclared threshold. With model confirmation on (at most one request per new
//!    entity) a confirmed candidate absorbs the mention as a `merge` alias, which records the
//!    similarity so the merge can be reversed; otherwise
//! 4. a **new entity**.
//!
//! Resolution only ever reads and writes the one tenant user's entities.

pub mod annotation;

use std::collections::HashMap;

use chrono::Utc;
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ports::{EmbeddingProvider, EntityMatchInput, EntityMention, EntityStore, LlmProvider};
use crate::{AliasSource, Entity, EntityAlias, EntityType, TenantContext, normalize_entity_name};

/// Predeclared default for `MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD` (fixed before any entity
/// measurement).
pub const DEFAULT_ENTITY_NAME_SIMILARITY_THRESHOLD: f32 = 0.70;

/// Model identifier recorded when the embedding model is not configured (tests, mocks).
pub const UNSPECIFIED_EMBEDDING_MODEL: &str = "unspecified";

/// Entity layer settings (`MEMCORE_ENTITIES_ENABLED` turns the layer on).
#[derive(Debug, Clone, PartialEq)]
pub struct EntityConfig {
    pub name_similarity_threshold: f32,
    /// Ask the model to confirm a candidate match; off, a candidate match creates a new entity.
    pub llm_confirmation: bool,
    /// The configured embedding model (`<provider>/<model>`). Name embeddings record it, and
    /// only stored embeddings of this model and dimension are compared.
    pub embedding_model: String,
}

impl Default for EntityConfig {
    fn default() -> Self {
        Self {
            name_similarity_threshold: DEFAULT_ENTITY_NAME_SIMILARITY_THRESHOLD,
            llm_confirmation: true,
            embedding_model: UNSPECIFIED_EMBEDDING_MODEL.to_string(),
        }
    }
}

/// What entity processing did for one add request (reported only when the layer is on).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityProcessingSummary {
    /// Annotation requests sent (one per extraction window holding stored facts).
    pub annotation_requests: usize,
    /// Annotation requests that failed; their facts got no entities.
    pub failed_annotations: usize,
    /// Windows whose used annotation needed a syntax repair.
    #[serde(default)]
    pub repaired_annotations: usize,
    /// Windows that used their one retry.
    #[serde(default)]
    pub retried_annotations: usize,
    /// Mentions rejected because they are not grounded in the source evidence or name no
    /// particular referent.
    #[serde(default)]
    pub rejected_mentions: usize,
    /// Mentions resolved (speaker, exact, merged or new).
    pub mentions: usize,
    pub created: usize,
    /// Mentions that resolved to an existing entity by speaker name or exact name / alias.
    pub reused: usize,
    /// Mentions merged into an existing entity after a confirmed candidate match.
    pub merged: usize,
    /// Model confirmation requests sent.
    pub confirmations: usize,
    pub links: usize,
    pub relations: usize,
}

/// A merge made while resolving, for the audit event.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityMergeRecord {
    pub entity_id: Uuid,
    pub canonical_name: String,
    pub alias: String,
    pub similarity: f32,
    pub fact_id: Uuid,
}

/// How a mention resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionKind {
    Speaker,
    Exact,
    Merged,
    Created,
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let norm = |values: &[f32]| values.iter().map(|value| value * value).sum::<f32>().sqrt();
    let denominator = norm(left) * norm(right);
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

fn has_name(entity: &Entity, normalized: &str) -> bool {
    entity
        .names()
        .any(|name| normalize_entity_name(name) == normalized)
}

/// Resolves mentions against one tenant user's entities within one request, keeping the working
/// set in memory so later mentions see entities created or changed by earlier ones.
pub struct EntityResolver<'a> {
    store: &'a dyn EntityStore,
    llm: &'a dyn LlmProvider,
    embedder: &'a dyn EmbeddingProvider,
    tenant: &'a TenantContext,
    config: EntityConfig,
    entities: Vec<Entity>,
    embeddings: HashMap<String, Vec<f32>>,
    pub summary: EntityProcessingSummary,
    pub merges: Vec<EntityMergeRecord>,
}

impl<'a> EntityResolver<'a> {
    pub async fn load(
        store: &'a dyn EntityStore,
        llm: &'a dyn LlmProvider,
        embedder: &'a dyn EmbeddingProvider,
        tenant: &'a TenantContext,
        config: EntityConfig,
    ) -> MemcoreResult<Self> {
        let entities = store.list_all_entities(tenant).await?;
        Ok(Self {
            store,
            llm,
            embedder,
            tenant,
            config,
            entities,
            embeddings: HashMap::new(),
            summary: EntityProcessingSummary::default(),
            merges: Vec::new(),
        })
    }

    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    async fn embed(&mut self, name: &str) -> MemcoreResult<Vec<f32>> {
        let key = normalize_entity_name(name);
        if let Some(embedding) = self.embeddings.get(&key) {
            return Ok(embedding.clone());
        }
        let embedding = self.embedder.embed_text(name).await?;
        self.embeddings.insert(key, embedding.clone());
        Ok(embedding)
    }

    /// Normalised names of every speaker entity other than `except`.
    fn other_speaker_names(&self, except: Option<Uuid>) -> Vec<String> {
        self.entities
            .iter()
            .filter(|entity| entity.is_speaker && Some(entity.id) != except)
            .flat_map(|entity| {
                entity
                    .names()
                    .map(normalize_entity_name)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Adds `names` as annotation aliases of the entity at `index` when new, never a name that
    /// belongs to another speaker. Returns whether anything changed.
    fn add_aliases(&mut self, index: usize, names: &[String], window_speakers: &[String]) -> bool {
        let entity_id = self.entities[index].id;
        let mut protected = self.other_speaker_names(Some(entity_id));
        if !self.entities[index].is_speaker {
            protected.extend(
                window_speakers
                    .iter()
                    .map(|name| normalize_entity_name(name)),
            );
        }
        let now = Utc::now();
        let mut changed = false;
        for name in names {
            let normalized = normalize_entity_name(name);
            if normalized.is_empty()
                || protected.contains(&normalized)
                || has_name(&self.entities[index], &normalized)
            {
                continue;
            }
            self.entities[index].aliases.push(EntityAlias {
                name: name.clone(),
                source: AliasSource::Annotation,
                added_at: now,
                similarity: None,
            });
            changed = true;
        }
        if changed {
            self.entities[index].updated_at = now;
        }
        changed
    }

    async fn save(&mut self, index: usize) -> MemcoreResult<()> {
        let stored = self
            .store
            .upsert_entity(self.tenant, self.entities[index].clone())
            .await?;
        self.entities[index] = stored;
        Ok(())
    }

    async fn create(
        &mut self,
        name: &str,
        entity_type: EntityType,
        is_speaker: bool,
        aliases: &[String],
        window_speakers: &[String],
    ) -> MemcoreResult<usize> {
        let now = Utc::now();
        let embedding = self.embed(name).await?;
        self.entities.push(Entity {
            id: Uuid::new_v4(),
            org_id: self.tenant.org_id.clone(),
            user_id: self.tenant.user_id.clone(),
            canonical_name: name.to_string(),
            entity_type,
            aliases: Vec::new(),
            summary: None,
            is_speaker,
            name_embedding_model: Some(self.config.embedding_model.clone()),
            name_embedding_dimensions: Some(embedding.len()),
            name_embedding: Some(embedding),
            created_at: now,
            updated_at: now,
        });
        let index = self.entities.len() - 1;
        self.add_aliases(index, aliases, window_speakers);
        self.entities[index].updated_at = now;
        self.save(index).await?;
        self.summary.created += 1;
        Ok(index)
    }

    /// Resolves one mention from the fact `fact_id` (`context` is its text) and returns the entity
    /// ID and how it resolved. `None` for a mention with no usable name.
    pub async fn resolve(
        &mut self,
        mention: &EntityMention,
        window_speakers: &[String],
        fact_id: Uuid,
        context: &str,
    ) -> MemcoreResult<Option<(Uuid, ResolutionKind)>> {
        let normalized = normalize_entity_name(&mention.name);
        if normalized.is_empty() {
            return Ok(None);
        }
        self.summary.mentions += 1;
        let aliases: Vec<String> = mention
            .aliases
            .iter()
            .filter(|alias| normalize_entity_name(alias) != normalized)
            .cloned()
            .collect();

        // 1. Speakers are authoritative person entities.
        if let Some(speaker) = window_speakers
            .iter()
            .find(|speaker| normalize_entity_name(speaker) == normalized)
        {
            // A speaker is matched by canonical name only, so an alias another entity picked up
            // can never fold a second speaker into it.
            let existing = self
                .entities
                .iter()
                .position(|entity| {
                    entity.is_speaker && normalize_entity_name(&entity.canonical_name) == normalized
                })
                .or_else(|| {
                    self.entities.iter().position(|entity| {
                        entity.entity_type.compatible_with(EntityType::Person)
                            && normalize_entity_name(&entity.canonical_name) == normalized
                    })
                });
            let index = match existing {
                Some(index) => {
                    let promote = !self.entities[index].is_speaker;
                    if promote {
                        self.entities[index].is_speaker = true;
                        self.entities[index].entity_type = EntityType::Person;
                        self.entities[index].updated_at = Utc::now();
                    }
                    if self.add_aliases(index, &aliases, window_speakers) || promote {
                        self.save(index).await?;
                    }
                    self.summary.reused += 1;
                    index
                }
                None => {
                    self.create(speaker, EntityType::Person, true, &aliases, window_speakers)
                        .await?
                }
            };
            return Ok(Some((self.entities[index].id, ResolutionKind::Speaker)));
        }

        // 2. Exact canonical name or alias, compatible type.
        let alias_norms: Vec<String> = aliases.iter().map(|a| normalize_entity_name(a)).collect();
        let exact = self
            .entities
            .iter()
            .position(|entity| {
                entity.entity_type.compatible_with(mention.entity_type)
                    && has_name(entity, &normalized)
            })
            .or_else(|| {
                self.entities.iter().position(|entity| {
                    entity.entity_type.compatible_with(mention.entity_type)
                        && alias_norms.contains(&normalize_entity_name(&entity.canonical_name))
                })
            });
        if let Some(index) = exact {
            let mut names = vec![mention.name.clone()];
            names.extend(aliases.iter().cloned());
            if self.entities[index].entity_type == EntityType::Other
                && mention.entity_type != EntityType::Other
            {
                self.entities[index].entity_type = mention.entity_type;
                self.entities[index].updated_at = Utc::now();
                self.add_aliases(index, &names, window_speakers);
                self.save(index).await?;
            } else if self.add_aliases(index, &names, window_speakers) {
                self.save(index).await?;
            }
            self.summary.reused += 1;
            return Ok(Some((self.entities[index].id, ResolutionKind::Exact)));
        }

        // 3. Candidate by name similarity, compatible type; the model confirms when on. Only
        //    stored embeddings of the configured model and this dimension are compared;
        //    any other is skipped as a candidate, never compared across embedding spaces.
        let embedding = self.embed(&mention.name).await?;
        let model = self.config.embedding_model.clone();
        let candidate = self
            .entities
            .iter()
            .enumerate()
            .filter(|(_, entity)| entity.entity_type.compatible_with(mention.entity_type))
            .filter_map(|(index, entity)| {
                entity
                    .comparable_name_embedding(&model, embedding.len())
                    .map(|stored| (index, cosine_similarity(&embedding, stored)))
            })
            .filter(|(_, similarity)| *similarity >= self.config.name_similarity_threshold)
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        // Ties: the older entity.
                        self.entities[right.0]
                            .created_at
                            .cmp(&self.entities[left.0].created_at)
                    })
            });
        if let Some((index, similarity)) = candidate
            && self.config.llm_confirmation
        {
            let entity = &self.entities[index];
            self.summary.confirmations += 1;
            let confirmed = self
                .llm
                .confirm_entity_match(EntityMatchInput {
                    tenant: self.tenant.clone(),
                    mention: mention.name.clone(),
                    mention_type: mention.entity_type,
                    context: context.to_string(),
                    candidate_name: entity.canonical_name.clone(),
                    candidate_type: entity.entity_type,
                    candidate_aliases: entity.aliases.iter().map(|a| a.name.clone()).collect(),
                })
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        event = "entity_match_confirmation_failed",
                        error = %error,
                        "entity match confirmation failed; the mention becomes a new entity"
                    );
                    false
                });
            if confirmed {
                let now = Utc::now();
                self.entities[index].aliases.push(EntityAlias {
                    name: mention.name.clone(),
                    source: AliasSource::Merge,
                    added_at: now,
                    similarity: Some(similarity),
                });
                self.entities[index].updated_at = now;
                self.add_aliases(index, &aliases, window_speakers);
                self.save(index).await?;
                self.summary.merged += 1;
                self.merges.push(EntityMergeRecord {
                    entity_id: self.entities[index].id,
                    canonical_name: self.entities[index].canonical_name.clone(),
                    alias: mention.name.clone(),
                    similarity,
                    fact_id,
                });
                return Ok(Some((self.entities[index].id, ResolutionKind::Merged)));
            }
        }

        // 4. A new entity.
        let index = self
            .create(
                &mention.name,
                mention.entity_type,
                false,
                &aliases,
                window_speakers,
            )
            .await?;
        Ok(Some((self.entities[index].id, ResolutionKind::Created)))
    }

    /// The entity a relation endpoint names among `resolved` (the fact's own mentions, by
    /// normalised name or alias).
    pub fn lookup(&self, resolved: &[Uuid], name: &str) -> Option<Uuid> {
        let normalized = normalize_entity_name(name);
        resolved.iter().copied().find(|id| {
            self.entities
                .iter()
                .any(|entity| entity.id == *id && has_name(entity, &normalized))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_handles_edges() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
