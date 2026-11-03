//! Entity annotation and match-confirmation prompts, shared by
//! every provider.
//!
//! Annotation is its own request per extraction window: the fact-extraction request is unchanged.
//! The example names are invented; nothing here may come from a third-party dataset (a scan
//! checks every prompt).

use std::borrow::Cow;

use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    AnnotationAttempt, EntityAnnotationInput, EntityAnnotationOutput, EntityMatchInput,
    EntityMention, FactAnnotation, JsonRepair, RelationMention, unusable_annotation_output,
};
use memcore_core::{EntityRole, EntityType};
use serde::Deserialize;
use serde_json::{Value, json};

/// System instructions for entity annotation (specificity contract, particular-referent
/// rule). Every example is invented and shares nothing with an evaluation set.
pub const ENTITY_ANNOTATION_INSTRUCTIONS: &str = r#"You identify the particular entities in a person's stored memory facts.
You receive part of a conversation (messages with speaker names), the speaker names, and the facts stored from it, each with an index.
An entity is one particular referent: this person, this organisation, this place, this animal, this object or this event, told apart from every other one of its kind. A word is not an entity just because it is a noun in the fact.
Test every candidate by asking "which one?". List it only when the conversation answers that question: by a name or title, by a relationship to a named person that fits exactly one referent, by a unique description, or by its own story.
For each fact, list only the entities the fact is about:
- person: the speakers and every named person. An unnamed person counts only when a relationship to a speaker picks out exactly one person; name that person by the relationship, for example "Ines's mother" (never "my mother"). An occupation or social role on its own ("a nurse", "the waiter", "some neighbours") is not an entity.
- animal: a particular animal, named or identified through its owner, for example "Biscuit" or "Tomasz's dog".
- place: a particular named place such as a city, country, region, street, venue, park, shop or school.
- organization: a particular named organisation (company, school, club, band, charity, team), or a specific group a speaker belongs to.
- object: only an object the conversation makes one particular object: it has a name or title (a book, song, artwork, named vessel or named product), or a story or unique description that sets it apart, such as "Ines's grandfather's clock" that has stood in the family hall for a century. Owning, using, buying, liking, losing or describing an ordinary thing does not make it an entity: "Tomasz's new armchair", "a red colander" and "Ines's phone charger" are not entities.
- event: one distinguishable occurrence, named or clearly a single occasion, such as a named festival, race or conference, or "Tomasz's graduation". An activity or a kind of occasion ("hiking", "a meeting", "holidays") is not an entity.
- other: any other particular, named thing.
Never list activities, pastimes, sports, topics, feelings, ideas, attributes, quantities, dates, times, durations, or broad groups and categories.
A fact can name particular entities and ordinary things together: list only the particular entities. Many facts have no entity other than the speaker; a fact about ordinary things only has an empty entities list.
Every entity must be named in the messages or in the fact; never add an entity that is not there.
Each entity is one referent: never join two people or things in one name ("Ines and Tomasz" is two entities), and never give an entity another entity's name as an alias.
When the same entity appears in several facts of this request, list it in every one of those facts, always under exactly the same name.
name: the fullest name the conversation gives for the entity ("Tomasz Nowak" when the full name is given, else "Tomasz"). aliases: other names the conversation uses for the same entity (nicknames, short forms, "Mom").
role: "subject" for who or what the fact is mainly about, "object" for an entity the subject acts on or relates to, "mentioned" otherwise.
relations: relations the fact states between two of its listed entities, as subject, relation, object, with names exactly as listed and a short lowercase relation such as "owns", "lives in", "works at", "sister of".
Example: fact "Ines adopted a beagle named Biscuit and takes him sailing." -> entities Ines (person, subject), Biscuit (animal, object); relation Ines, "owns", Biscuit. "beagle" and "sailing" are not entities.
Example: fact "Tomasz bought a kettle and some tea." -> entities Tomasz (person, subject) only.
Example: facts "Ines repainted her grandfather's clock." and "The clock stood in her grandfather's hall for a century." -> "Ines's grandfather's clock" (object) in both facts, under the same name.
Return every fact index exactly once. A fact without a particular entity has an empty entities list.
Return valid JSON only matching the schema."#;

/// Appended to the instructions for the one retry after an unusable output. The request is
/// otherwise identical: same messages, same facts, no added context.
pub const ENTITY_ANNOTATION_RETRY_NOTE: &str = "Your previous reply to this request could not be used: it was not one complete, valid JSON object matching the schema. Reply again with exactly one complete JSON object that matches the schema, with nothing before or after it.";

/// The annotation instructions for a request's attempt.
pub fn entity_annotation_instructions(attempt: AnnotationAttempt) -> Cow<'static, str> {
    match attempt {
        AnnotationAttempt::First => Cow::Borrowed(ENTITY_ANNOTATION_INSTRUCTIONS),
        AnnotationAttempt::Retry => Cow::Owned(format!(
            "{ENTITY_ANNOTATION_INSTRUCTIONS}\n{ENTITY_ANNOTATION_RETRY_NOTE}"
        )),
    }
}

/// System instructions for confirming an ambiguous entity match.
pub const ENTITY_MATCH_INSTRUCTIONS: &str = r#"You decide whether a name in a person's memory refers to the same real-world entity as a stored entity.
Answer same = true only when the mention clearly refers to that stored entity, for example a nickname or short form of the same person, or the same pet named another way.
Answer same = false when they could be different entities, for example two different people who share a first name, or a relative of someone rather than that person.
Return valid JSON only matching the schema."#;

const ENTITY_TYPES: [&str; 7] = [
    "person",
    "animal",
    "place",
    "organization",
    "object",
    "event",
    "other",
];

/// JSON schema of an annotation.
pub fn entity_annotation_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "fact_index": { "type": "integer" },
                        "entities": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string" },
                                    "entity_type": { "type": "string", "enum": ENTITY_TYPES },
                                    "role": { "type": "string", "enum": ["subject", "object", "mentioned"] },
                                    "aliases": { "type": "array", "items": { "type": "string" } }
                                },
                                "required": ["name", "entity_type", "role", "aliases"],
                                "additionalProperties": false
                            }
                        },
                        "relations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "subject": { "type": "string" },
                                    "relation": { "type": "string" },
                                    "object": { "type": "string" }
                                },
                                "required": ["subject", "relation", "object"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["fact_index", "entities", "relations"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["facts"],
        "additionalProperties": false
    })
}

/// JSON schema of a match confirmation.
pub fn entity_match_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "same": { "type": "boolean" },
            "reason": { "type": "string" }
        },
        "required": ["same", "reason"],
        "additionalProperties": false
    })
}

/// The user payload of an annotation request.
pub fn entity_annotation_payload(input: &EntityAnnotationInput) -> Value {
    json!({
        "speakers": input.speakers,
        "messages": input.messages.iter().map(|message| json!({
            "id": message.message_id,
            "speaker": message.name,
            "role": message.role,
            "time": message.timestamp.map(|time| time.format("%Y-%m-%d").to_string()),
            "text": message.content,
        })).collect::<Vec<_>>(),
        "facts": input.facts.iter().map(|fact| json!({
            "index": fact.index,
            "content": fact.content,
            "subject": fact.subject,
        })).collect::<Vec<_>>(),
    })
}

/// The user payload of a match confirmation.
pub fn entity_match_payload(input: &EntityMatchInput) -> Value {
    json!({
        "mention": { "name": input.mention, "entity_type": input.mention_type.as_str(), "fact": input.context },
        "stored_entity": {
            "name": input.candidate_name,
            "entity_type": input.candidate_type.as_str(),
            "aliases": input.candidate_aliases,
        },
    })
}

#[derive(Debug, Deserialize)]
struct AnnotationResponse {
    #[serde(default)]
    facts: Vec<FactResponse>,
}

#[derive(Debug, Deserialize)]
struct FactResponse {
    fact_index: i64,
    #[serde(default)]
    entities: Vec<MentionResponse>,
    #[serde(default)]
    relations: Vec<RelationResponse>,
}

#[derive(Debug, Deserialize)]
struct MentionResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    entity_type: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RelationResponse {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    relation: String,
    #[serde(default)]
    object: String,
}

fn clean(value: &str) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

/// Parses an annotation. The output is first parsed strictly; if that fails, only the
/// unambiguous syntax repairs of [`repair_json_syntax`] are tried, and the repaired text is parsed
/// strictly again. Anything else (duplicate keys, wrong value types, non-string keys, missing
/// internal commas or colons) is an unusable output: a provider error recognised by
/// [`memcore_core::ports::unusable_annotation_class`].
///
/// Negative indexes, entities without a name and relations with an empty part are dropped; unknown
/// types become `other` and unknown roles `mentioned` (memcore validates the rest).
pub fn parse_entity_annotation_response(text: &str) -> MemcoreResult<EntityAnnotationOutput> {
    let text = text.trim();
    let (parsed, repairs) = match serde_json::from_str::<AnnotationResponse>(text) {
        Ok(parsed) => (parsed, Vec::new()),
        Err(error) => {
            let Some((repaired, repairs)) = repair_json_syntax(text) else {
                return Err(unusable(&error, ""));
            };
            match serde_json::from_str::<AnnotationResponse>(&repaired) {
                Ok(parsed) => (parsed, repairs),
                Err(error) => return Err(unusable(&error, " after repair")),
            }
        }
    };
    let facts = parsed
        .facts
        .into_iter()
        .filter_map(|fact| {
            let fact_index = usize::try_from(fact.fact_index).ok()?;
            let entities = fact
                .entities
                .into_iter()
                .filter_map(|mention| {
                    Some(EntityMention {
                        name: clean(&mention.name)?,
                        entity_type: EntityType::parse(&mention.entity_type),
                        role: EntityRole::parse(&mention.role),
                        aliases: mention
                            .aliases
                            .iter()
                            .filter_map(|alias| clean(alias))
                            .collect(),
                    })
                })
                .collect();
            let relations = fact
                .relations
                .into_iter()
                .filter_map(|relation| {
                    Some(RelationMention {
                        subject: clean(&relation.subject)?,
                        relation: clean(&relation.relation)?.to_lowercase(),
                        object: clean(&relation.object)?,
                    })
                })
                .collect();
            Some(FactAnnotation {
                fact_index,
                entities,
                relations,
            })
        })
        .collect();
    Ok(EntityAnnotationOutput { facts, repairs })
}

fn unusable(error: &serde_json::Error, when: &str) -> MemcoreError {
    let class = match error.classify() {
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
        serde_json::error::Category::Io => "io",
    };
    // serde's message for a wrong value type can quote the model's text; only the position is kept.
    unusable_annotation_output(
        class,
        format!(
            "unusable{when} at line {} column {}",
            error.line(),
            error.column()
        ),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Open,
    Comma,
    Colon,
    Close,
    Key,
    StringValue,
    Scalar,
}

/// The deterministic repairs, applied only where the intended structure is unambiguous:
/// - material after a complete top-level object is dropped;
/// - a comma directly before `}` or `]`, following a complete value, is dropped;
/// - when the text ends outside a string right after a complete string value or a closing
///   delimiter, the missing closing delimiters are appended.
///
/// Nothing is ever inserted inside the structure (no commas, colons, quotes or keys), so duplicate
/// keys, wrong value types, non-string keys and missing internal delimiters stay unusable. Returns
/// `None` when no repair applies or a closing delimiter does not match its opener.
pub fn repair_json_syntax(text: &str) -> Option<(String, Vec<JsonRepair>)> {
    let text = text.trim();
    if !text.starts_with('{') {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut stack: Vec<char> = Vec::new();
    let mut repairs: Vec<JsonRepair> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut last = Token::Open;
    // The token before the most recent comma, and where that comma sits in `out`.
    let mut comma: Option<(usize, Token)> = None;
    let mut complete_at: Option<usize> = None;

    for (position, c) in text.char_indices() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                let is_key =
                    stack.last() == Some(&'{') && matches!(last, Token::Open | Token::Comma);
                last = if is_key {
                    Token::Key
                } else {
                    Token::StringValue
                };
                out.push(c);
            }
            '{' | '[' => {
                stack.push(c);
                last = Token::Open;
                out.push(c);
            }
            '}' | ']' => {
                let opener = if c == '}' { '{' } else { '[' };
                if stack.pop() != Some(opener) {
                    return None;
                }
                if last == Token::Comma {
                    let (at, before) = comma?;
                    if !matches!(before, Token::StringValue | Token::Scalar | Token::Close) {
                        return None;
                    }
                    out.remove(at);
                    if !repairs.contains(&JsonRepair::TrailingComma) {
                        repairs.push(JsonRepair::TrailingComma);
                    }
                }
                last = Token::Close;
                out.push(c);
                if stack.is_empty() {
                    complete_at = Some(position + c.len_utf8());
                    break;
                }
            }
            ',' => {
                comma = Some((out.len(), last));
                last = Token::Comma;
                out.push(c);
            }
            ':' => {
                last = Token::Colon;
                out.push(c);
            }
            c if c.is_whitespace() => out.push(c),
            _ => {
                last = Token::Scalar;
                out.push(c);
            }
        }
    }

    match complete_at {
        Some(end) => {
            if !text[end..].trim().is_empty() {
                repairs.push(JsonRepair::TrailingMaterial);
            }
        }
        None => {
            // A string cut open, a dangling key, comma or colon, or a bare scalar (which may itself
            // be cut short) leaves the intended structure unknown.
            if in_string || !matches!(last, Token::StringValue | Token::Close) {
                return None;
            }
            while let Some(opener) = stack.pop() {
                out.push(if opener == '{' { '}' } else { ']' });
            }
            repairs.push(JsonRepair::MissingClosers);
        }
    }
    (!repairs.is_empty()).then_some((out, repairs))
}

#[derive(Debug, Deserialize)]
struct MatchResponse {
    same: bool,
}

/// Parses a match confirmation; malformed JSON is a provider error.
pub fn parse_entity_match_response(text: &str) -> MemcoreResult<bool> {
    serde_json::from_str::<MatchResponse>(text.trim())
        .map(|response| response.same)
        .map_err(|error| {
            MemcoreError::ProviderError(format!("invalid entity match JSON from model: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_annotation_leniently() {
        let text = r#"{"facts":[
            {"fact_index":0,"entities":[
                {"name":"  Ines ","entity_type":"person","role":"subject","aliases":["Nessa",""]},
                {"name":"Biscuit","entity_type":"pet","role":"owner","aliases":[]},
                {"name":"","entity_type":"place","role":"object","aliases":[]}],
             "relations":[{"subject":"Ines","relation":"Owns","object":"Biscuit"},
                          {"subject":"Ines","relation":"","object":"Biscuit"}]},
            {"fact_index":-1,"entities":[],"relations":[]}]}"#;
        let output = parse_entity_annotation_response(text).unwrap();
        assert_eq!(output.facts.len(), 1);
        let fact = &output.facts[0];
        assert_eq!(fact.entities.len(), 2);
        assert_eq!(fact.entities[0].name, "Ines");
        assert_eq!(fact.entities[0].aliases, vec!["Nessa"]);
        assert_eq!(fact.entities[1].entity_type, EntityType::Other);
        assert_eq!(fact.entities[1].role, EntityRole::Mentioned);
        assert_eq!(fact.relations.len(), 1);
        assert_eq!(fact.relations[0].relation, "owns");
        assert!(parse_entity_annotation_response("not json").is_err());
    }

    #[test]
    fn parses_a_match_confirmation() {
        assert!(parse_entity_match_response(r#"{"same":true,"reason":"nickname"}"#).unwrap());
        assert!(!parse_entity_match_response(r#"{"same":false,"reason":"x"}"#).unwrap());
        assert!(parse_entity_match_response("{}").is_err());
    }

    #[test]
    fn the_payload_keys_differ_from_the_consolidation_payload() {
        let input = EntityAnnotationInput {
            tenant: memcore_core::TenantContext::new("org", "user").unwrap(),
            messages: vec![],
            facts: vec![],
            speakers: vec!["Ines".to_string()],
            attempt: AnnotationAttempt::Retry,
        };
        let payload = entity_annotation_payload(&input);
        assert!(payload.get("candidate_fact").is_none());
        assert!(payload.get("existing_facts").is_none());
        assert_eq!(payload["speakers"], json!(["Ines"]));
        // The retry sends exactly the same evidence; only the instructions differ.
        let first = EntityAnnotationInput {
            attempt: AnnotationAttempt::First,
            ..input.clone()
        };
        assert_eq!(entity_annotation_payload(&first), payload);
    }

    #[test]
    fn the_retry_instructions_only_add_the_retry_note() {
        let first = entity_annotation_instructions(AnnotationAttempt::First);
        let retry = entity_annotation_instructions(AnnotationAttempt::Retry);
        assert_eq!(first, ENTITY_ANNOTATION_INSTRUCTIONS);
        assert_eq!(
            retry,
            format!(
                "{ENTITY_ANNOTATION_INSTRUCTIONS}
{ENTITY_ANNOTATION_RETRY_NOTE}"
            )
        );
    }

    #[test]
    fn repairs_only_unambiguous_syntax() {
        let repaired = repair_json_syntax;
        assert_eq!(
            repaired(r#"{"facts":[]} and more"#),
            Some((
                r#"{"facts":[]}"#.to_string(),
                vec![JsonRepair::TrailingMaterial]
            ))
        );
        assert_eq!(
            repaired(r#"{"facts":[1,2,],}"#),
            Some((
                r#"{"facts":[1,2]}"#.to_string(),
                vec![JsonRepair::TrailingComma]
            ))
        );
        assert_eq!(
            repaired(r#"{"facts":[{"a":"b"}"#),
            Some((
                r#"{"facts":[{"a":"b"}]}"#.to_string(),
                vec![JsonRepair::MissingClosers]
            ))
        );
        // Commas inside strings are never touched.
        assert_eq!(repaired(r#"{"a":"x,]"}"#), None);
        for ambiguous in [
            r#"{"facts":[{"a":"b"},{"#, // cut after an opener
            r#"{"facts":[{"a":"b"},"#,  // dangling comma at the end
            r#"{"facts":[{"a":"#,       // dangling colon
            r#"{"facts":[{"a""#,        // dangling key
            r#"{"facts":[{"a":12"#,     // a number may itself be cut short
            r#"{"facts":[{"a":"b"#,     // cut inside a string
            r#"{"facts":[{"a":"b"]}"#,  // mismatched closer
            r#"{"facts":[,]}"#,         // comma after an opener
            r#"{"facts":[1,,]}"#,       // double comma
            r#"[{"facts":[]}]"#,        // top level is not an object
            r#"```json {"facts":[]}"#,  // leading material is not repaired
        ] {
            assert_eq!(repaired(ambiguous), None, "{ambiguous}");
        }
    }

    #[test]
    fn unusable_outputs_are_classified_without_model_text() {
        let error =
            parse_entity_annotation_response(r#"{"facts":[{"fact_index":"zyxq"}]}"#).unwrap_err();
        assert_eq!(
            memcore_core::ports::unusable_annotation_class(&error),
            Some("data")
        );
        assert!(!error.to_string().contains("zyxq"));
        let error =
            parse_entity_annotation_response("{\"facts\":[{\"fact_index\":0 \"entities\":[]}]}")
                .unwrap_err();
        assert_eq!(
            memcore_core::ports::unusable_annotation_class(&error),
            Some("syntax")
        );
    }
}
