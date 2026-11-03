//! The people, animals, places and things a user's facts are
//! about, the links from facts to them, and relations between them. Stored only while
//! `MEMCORE_ENTITIES_ENABLED` is on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What kind of thing an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Person,
    Animal,
    Place,
    Organization,
    Object,
    Event,
    Other,
}

impl EntityType {
    pub const ALL: [Self; 7] = [
        Self::Person,
        Self::Animal,
        Self::Place,
        Self::Organization,
        Self::Object,
        Self::Event,
        Self::Other,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Animal => "animal",
            Self::Place => "place",
            Self::Organization => "organization",
            Self::Object => "object",
            Self::Event => "event",
            Self::Other => "other",
        }
    }

    /// Lenient parse of a stored or model-written type; anything unknown is `Other`.
    pub fn parse(value: &str) -> Self {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .unwrap_or(Self::Other)
    }

    /// Two mentions may name the same entity only when their types agree or either is `Other`.
    pub fn compatible_with(self, other: Self) -> bool {
        self == other || self == Self::Other || other == Self::Other
    }
}

/// The part an entity plays in a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityRole {
    Subject,
    Object,
    Mentioned,
}

impl EntityRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Object => "object",
            Self::Mentioned => "mentioned",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "subject" => Self::Subject,
            "object" => Self::Object,
            _ => Self::Mentioned,
        }
    }
}

/// How an alias came to belong to an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasSource {
    /// Named as an alias by the annotation of a fact, or the entity's first surface name.
    Annotation,
    /// A mention merged into the entity by name similarity (and model confirmation when on).
    /// Reversible: the alias record keeps what was merged.
    Merge,
}

/// One name an entity goes by, with its provenance (merges are reversible).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityAlias {
    pub name: String,
    pub source: AliasSource,
    pub added_at: DateTime<Utc>,
    /// Name similarity to the canonical name when the alias came from a merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub canonical_name: String,
    pub entity_type: EntityType,
    pub aliases: Vec<EntityAlias>,
    pub summary: Option<String>,
    /// A person named as a message speaker: authoritative, and never merged with another speaker.
    pub is_speaker: bool,
    /// Embedding of the canonical name, for candidate matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding: Option<Vec<f32>>,
    /// The embedding model that produced `name_embedding` (`<provider>/<model>`). Embeddings
    /// are compared only when this and the dimension match the current configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding_model: Option<String>,
    /// The dimension of `name_embedding`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_embedding_dimensions: Option<usize>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Entity {
    /// The stored name embedding, only when it was produced by `model` and has `dimensions`
    /// values (and its recorded dimension agrees). Anything else, including a missing provenance,
    /// is never compared.
    pub fn comparable_name_embedding(&self, model: &str, dimensions: usize) -> Option<&[f32]> {
        let embedding = self.name_embedding.as_deref()?;
        (self.name_embedding_model.as_deref() == Some(model)
            && self.name_embedding_dimensions == Some(dimensions)
            && embedding.len() == dimensions)
            .then_some(embedding)
    }

    /// Every name the entity goes by: the canonical name, then its aliases.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.canonical_name.as_str())
            .chain(self.aliases.iter().map(|alias| alias.name.as_str()))
    }
}

/// A fact's link to an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactEntityLink {
    pub org_id: String,
    pub user_id: String,
    pub fact_id: Uuid,
    pub entity_id: Uuid,
    pub role: EntityRole,
    /// The name the fact used for the entity.
    pub mention: String,
    pub created_at: DateTime<Utc>,
}

/// A relation between two entities, stated by a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRelation {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub source_entity_id: Uuid,
    pub relation: String,
    pub target_entity_id: Uuid,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub source_fact_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Normalises a name for matching (the documented mapping rule):
/// case-folded, a possessive `'s` removed from every word ("Ines's mother" and "Ines mother"
/// match), punctuation stripped, whitespace collapsed. A plural possessive ("James'") keeps its
/// `s`.
pub fn normalize_entity_name(name: &str) -> String {
    let lowered = name.trim().to_lowercase().replace('\u{2019}', "'");
    lowered
        .split_whitespace()
        .map(|word| {
            let word = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '\'');
            word.strip_suffix("'s")
                .or_else(|| word.strip_suffix('\''))
                .unwrap_or(word)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalise_case_possessives_and_punctuation() {
        assert_eq!(normalize_entity_name("  Ines's "), "ines");
        assert_eq!(normalize_entity_name("INES\u{2019}s"), "ines");
        assert_eq!(normalize_entity_name("James'"), "james");
        assert_eq!(normalize_entity_name("Ines's mother"), "ines mother");
        assert_eq!(normalize_entity_name("Ines mother"), "ines mother");
        assert_eq!(normalize_entity_name("Tomasz's."), "tomasz");
        assert_eq!(normalize_entity_name("O'Brien"), "o brien");
        assert_eq!(
            normalize_entity_name("Dr. Ada   Byron-King"),
            "dr ada byron king"
        );
        assert_eq!(normalize_entity_name("!!!"), "");
    }

    fn embedded(model: Option<&str>, dimensions: Option<usize>, values: usize) -> Entity {
        let now = chrono::Utc::now();
        Entity {
            id: Uuid::new_v4(),
            org_id: "org".to_string(),
            user_id: "user".to_string(),
            canonical_name: "Ines".to_string(),
            entity_type: EntityType::Person,
            aliases: vec![],
            summary: None,
            is_speaker: false,
            name_embedding: Some(vec![0.5; values]),
            name_embedding_model: model.map(str::to_string),
            name_embedding_dimensions: dimensions,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn name_embeddings_are_comparable_only_within_one_model_and_dimension() {
        let model = "openai/embed-a";
        assert!(
            embedded(Some(model), Some(4), 4)
                .comparable_name_embedding(model, 4)
                .is_some()
        );
        assert!(
            embedded(Some("openai/embed-b"), Some(4), 4)
                .comparable_name_embedding(model, 4)
                .is_none()
        );
        assert!(
            embedded(Some(model), Some(4), 4)
                .comparable_name_embedding(model, 8)
                .is_none()
        );
        // Old or missing provenance, or a recorded dimension that disagrees with the vector.
        assert!(
            embedded(None, None, 4)
                .comparable_name_embedding(model, 4)
                .is_none()
        );
        assert!(
            embedded(Some(model), None, 4)
                .comparable_name_embedding(model, 4)
                .is_none()
        );
        assert!(
            embedded(Some(model), Some(8), 4)
                .comparable_name_embedding(model, 4)
                .is_none()
        );
        let mut none = embedded(Some(model), Some(4), 4);
        none.name_embedding = None;
        assert!(none.comparable_name_embedding(model, 4).is_none());
    }

    #[test]
    fn types_parse_leniently_and_other_is_compatible_with_everything() {
        assert_eq!(EntityType::parse(" Person "), EntityType::Person);
        assert_eq!(EntityType::parse("pet"), EntityType::Other);
        assert!(EntityType::Person.compatible_with(EntityType::Other));
        assert!(EntityType::Other.compatible_with(EntityType::Place));
        assert!(!EntityType::Person.compatible_with(EntityType::Animal));
        assert_eq!(EntityRole::parse("OBJECT"), EntityRole::Object);
        assert_eq!(EntityRole::parse("whatever"), EntityRole::Mentioned);
    }
}
