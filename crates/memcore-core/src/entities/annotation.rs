//! One window's entity annotation with bounded recovery and evidence
//! grounding.
//!
//! 1. The provider parses the output strictly and applies only the unambiguous syntax repairs.
//! 2. An output that is still unusable, or was cut off at the output-token limit, gets at most ONE
//!    retry: the same messages and facts, instructions asking only for a valid result, no added
//!    context. Any other error (a provider outage, a guardrail) is not retried here; the provider
//!    router owns transport retries.
//! 3. If the retry fails too, the window gets no annotation; its facts stay stored.
//! 4. Every mention of a usable output must be grounded in the window's messages or the fact it
//!    annotates: a name or alias that appears there, or a relationship name ("Ines's
//!    mother") whose owner is grounded and whose relationship word appears. Ungrounded aliases are
//!    dropped; a mention with no grounded name is rejected.
//! 5. Every kept mention must carry a sign of one particular referent: a word of its name
//!    other than a leading article that is capitalised (a name or title; words in scripts without
//!    case count too), or a possessive owner that itself qualifies ("Ines's mother"). A name made
//!    only of lowercase common words ("armchair", "a red colander") names a kind of thing, whatever
//!    type the model gave it, and is rejected. A speaker's own name always qualifies, however it is
//!    written. This is a structural check; there is no word list.
//!
//! The audit records each step for the request. It holds no model text.

use memcore_common::MemcoreError;
use serde::{Deserialize, Serialize};

use crate::normalize_entity_name;
use crate::ports::{
    AnnotationAttempt, EntityAnnotationInput, EntityAnnotationOutput, EntityMention, JsonRepair,
    LlmProvider, unusable_annotation_class,
};

/// How one annotation request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationAttemptStatus {
    /// Parsed strictly.
    Parsed,
    /// Parsed after a syntax repair.
    Repaired,
    /// Malformed or not matching the schema, after any allowed repair.
    Unusable,
    /// Cut off at the output-token limit.
    Truncated,
    /// Any other error (not retried here).
    ProviderError,
}

impl AnnotationAttemptStatus {
    fn usable(self) -> bool {
        matches!(self, Self::Parsed | Self::Repaired)
    }
}

/// The audit of one window's annotation, without model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationAudit {
    pub first_attempt: AnnotationAttemptStatus,
    /// Repairs applied to the first attempt's output.
    pub first_repairs: Vec<JsonRepair>,
    pub retry_used: bool,
    pub retry_result: Option<AnnotationAttemptStatus>,
    /// Repairs applied to the retry's output.
    pub retry_repairs: Vec<JsonRepair>,
    /// Why the window got no annotation: `unusable_output:<class>`, `truncated` or
    /// `provider_error:<code>`; `None` when an annotation was used.
    pub failure_reason: Option<String>,
    /// Mentions rejected because no name or alias is grounded in the source evidence.
    pub rejected_mentions: usize,
    /// Aliases dropped from kept mentions because they are not grounded.
    pub rejected_aliases: usize,
    /// Grounded mentions rejected because their name carries no sign of a particular referent
    #[serde(default)]
    pub unanchored_mentions: usize,
}

impl AnnotationAudit {
    /// Whether the window's annotation was used.
    pub fn succeeded(&self) -> bool {
        self.failure_reason.is_none()
    }

    /// Whether a syntax repair produced the annotation that was used.
    pub fn repaired(&self) -> bool {
        match self.retry_result {
            Some(status) => status == AnnotationAttemptStatus::Repaired,
            None => self.first_attempt == AnnotationAttemptStatus::Repaired,
        }
    }
}

/// One window's annotation: the grounded output when one was usable, and its audit.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotatedWindow {
    pub output: Option<EntityAnnotationOutput>,
    pub audit: AnnotationAudit,
}

fn classify(
    result: &Result<EntityAnnotationOutput, MemcoreError>,
) -> (AnnotationAttemptStatus, Option<String>) {
    match result {
        Ok(output) if output.repairs.is_empty() => (AnnotationAttemptStatus::Parsed, None),
        Ok(_) => (AnnotationAttemptStatus::Repaired, None),
        Err(error) if error.is_provider_output_truncated() => (
            AnnotationAttemptStatus::Truncated,
            Some("truncated".to_string()),
        ),
        Err(error) => match unusable_annotation_class(error) {
            Some(class) => (
                AnnotationAttemptStatus::Unusable,
                Some(format!("unusable_output:{class}")),
            ),
            None => (
                AnnotationAttemptStatus::ProviderError,
                Some(format!("provider_error:{}", error.code())),
            ),
        },
    }
}

/// Annotates one window: the first request, at most one retry for an unusable or truncated
/// output, then evidence grounding.
pub async fn annotate_window(
    llm: &dyn LlmProvider,
    input: EntityAnnotationInput,
) -> AnnotatedWindow {
    let first = llm
        .annotate_entities(EntityAnnotationInput {
            attempt: AnnotationAttempt::First,
            ..input.clone()
        })
        .await;
    let (first_status, first_failure) = classify(&first);
    let mut audit = AnnotationAudit {
        first_attempt: first_status,
        first_repairs: first
            .as_ref()
            .map(|output| output.repairs.clone())
            .unwrap_or_default(),
        retry_used: false,
        retry_result: None,
        retry_repairs: Vec::new(),
        failure_reason: None,
        rejected_mentions: 0,
        rejected_aliases: 0,
        unanchored_mentions: 0,
    };
    let result = if first_status.usable() {
        first
    } else if matches!(
        first_status,
        AnnotationAttemptStatus::Unusable | AnnotationAttemptStatus::Truncated
    ) {
        audit.retry_used = true;
        let retry = llm
            .annotate_entities(EntityAnnotationInput {
                attempt: AnnotationAttempt::Retry,
                ..input.clone()
            })
            .await;
        let (retry_status, retry_failure) = classify(&retry);
        audit.retry_result = Some(retry_status);
        audit.retry_repairs = retry
            .as_ref()
            .map(|output| output.repairs.clone())
            .unwrap_or_default();
        audit.failure_reason = retry_failure;
        retry
    } else {
        audit.failure_reason = first_failure;
        first
    };
    let output = match result {
        Ok(output) => {
            let (output, rejected_mentions, rejected_aliases) = ground_annotation(&input, output);
            audit.rejected_mentions = rejected_mentions;
            audit.rejected_aliases = rejected_aliases;
            let (output, unanchored) = keep_particular_referents(output, &input.speakers);
            audit.unanchored_mentions = unanchored;
            Some(output)
        }
        Err(_) => None,
    };
    AnnotatedWindow { output, audit }
}

/// Keeps only mentions whose name carries a sign of one particular referent; a speaker's own
/// name always qualifies. Returns the output and the number of rejected mentions.
pub fn keep_particular_referents(
    mut output: EntityAnnotationOutput,
    speakers: &[String],
) -> (EntityAnnotationOutput, usize) {
    let speakers: Vec<String> = speakers.iter().map(|s| normalize_entity_name(s)).collect();
    let mut rejected = 0;
    for annotation in &mut output.facts {
        let before = annotation.entities.len();
        annotation.entities.retain(|mention| {
            speakers.contains(&normalize_entity_name(&mention.name))
                || names_a_particular_referent(&mention.name)
        });
        rejected += before - annotation.entities.len();
    }
    (output, rejected)
}

/// A capitalised (or caseless-script) word other than a leading article, or a possessive whose
/// owner itself qualifies.
fn names_a_particular_referent(name: &str) -> bool {
    let named = name.split_whitespace().enumerate().any(|(index, word)| {
        let article = index == 0 && matches!(word.to_lowercase().as_str(), "the" | "a" | "an");
        !article && word.chars().any(|c| c.is_alphabetic() && !c.is_lowercase())
    });
    named || split_possessive(name).is_some_and(|(owner, _)| names_a_particular_referent(owner))
}

/// Keeps only mentions grounded in the source evidence of their fact. Returns the output,
/// the number of rejected mentions and the number of dropped aliases.
pub fn ground_annotation(
    input: &EntityAnnotationInput,
    mut output: EntityAnnotationOutput,
) -> (EntityAnnotationOutput, usize, usize) {
    let window = window_evidence(input);
    let mut rejected_mentions = 0;
    let mut rejected_aliases = 0;
    for annotation in &mut output.facts {
        let mut evidence = window.clone();
        if let Some(fact) = input
            .facts
            .iter()
            .find(|fact| fact.index == annotation.fact_index)
        {
            evidence.extend(tokens(&fact.content));
            if let Some(subject) = &fact.subject {
                evidence.push(String::new());
                evidence.extend(tokens(subject));
            }
        }
        let mentions = std::mem::take(&mut annotation.entities);
        for mention in mentions {
            match ground_mention(mention, &evidence) {
                Some((mention, dropped)) => {
                    rejected_aliases += dropped;
                    annotation.entities.push(mention);
                }
                None => rejected_mentions += 1,
            }
        }
    }
    (output, rejected_mentions, rejected_aliases)
}

fn ground_mention(
    mut mention: EntityMention,
    evidence: &[String],
) -> Option<(EntityMention, usize)> {
    let aliases = std::mem::take(&mut mention.aliases);
    let total = aliases.len();
    mention.aliases = aliases
        .into_iter()
        .filter(|alias| grounded(alias, evidence))
        .collect();
    let dropped = total - mention.aliases.len();
    if grounded(&mention.name, evidence) || !mention.aliases.is_empty() {
        Some((mention, dropped))
    } else {
        None
    }
}

/// The window's evidence tokens: each message's speaker name and text. An empty token separates
/// messages, so a name never matches across two of them.
fn window_evidence(input: &EntityAnnotationInput) -> Vec<String> {
    let mut evidence = Vec::new();
    for speaker in &input.speakers {
        evidence.extend(tokens(speaker));
        evidence.push(String::new());
    }
    for message in &input.messages {
        if let Some(name) = &message.name {
            evidence.extend(tokens(name));
            evidence.push(String::new());
        }
        evidence.extend(tokens(&message.content));
        evidence.push(String::new());
    }
    evidence
}

fn tokens(text: &str) -> Vec<String> {
    normalize_entity_name(text)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// A name is grounded when its words appear together in the evidence, or when it is a
/// relationship name ("Ines's mother", "Ines's grandfather's clock") whose owner is grounded and
/// whose last part appears in the evidence.
fn grounded(name: &str, evidence: &[String]) -> bool {
    let words = without_article(tokens(name));
    if words.is_empty() {
        return false;
    }
    if contains_run(evidence, &words) {
        return true;
    }
    match split_possessive(name) {
        Some((owner, head)) => {
            let head = without_article(tokens(head));
            !head.is_empty() && contains_run(evidence, &head) && grounded(owner, evidence)
        }
        None => false,
    }
}

/// A leading article never decides grounding ("the beagle" is grounded by "a beagle").
fn without_article(mut words: Vec<String>) -> Vec<String> {
    if words.len() > 1 && matches!(words[0].as_str(), "the" | "a" | "an") {
        words.remove(0);
    }
    words
}

/// Splits a name at its last possessive: `("Ines's grandfather", "clock")` for
/// "Ines's grandfather's clock".
fn split_possessive(name: &str) -> Option<(&str, &str)> {
    let name = name.trim();
    let mut split = None;
    for marker in ["'s ", "\u{2019}s ", "s' ", "s\u{2019} "] {
        if let Some(at) = name.rfind(marker)
            && split.is_none_or(|(best, _)| at > best)
        {
            split = Some((at, marker));
        }
    }
    let (at, marker) = split?;
    // "s' " keeps its "s" with the owner ("the Novaks' house" -> "the Novaks").
    let owner_end = if marker.starts_with('s') { at + 1 } else { at };
    Some((&name[..owner_end], &name[at + marker.len()..]))
}

fn contains_run(haystack: &[String], needle: &[String]) -> bool {
    needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{AnnotationFact, MemoryMessage, MessageRole};
    use crate::{EntityRole, EntityType, TenantContext};

    fn input(messages: &[(&str, &str)], facts: &[&str]) -> EntityAnnotationInput {
        EntityAnnotationInput {
            tenant: TenantContext::new("org", "user").unwrap(),
            messages: messages
                .iter()
                .map(|(speaker, text)| MemoryMessage {
                    role: MessageRole::User,
                    content: text.to_string(),
                    name: Some(speaker.to_string()),
                    timestamp: None,
                    message_id: None,
                })
                .collect(),
            facts: facts
                .iter()
                .enumerate()
                .map(|(index, content)| AnnotationFact {
                    index,
                    content: content.to_string(),
                    subject: None,
                })
                .collect(),
            speakers: messages
                .iter()
                .map(|(speaker, _)| speaker.to_string())
                .collect(),
            attempt: AnnotationAttempt::First,
        }
    }

    fn mention(name: &str, aliases: &[&str]) -> EntityMention {
        EntityMention {
            name: name.to_string(),
            entity_type: EntityType::Person,
            role: EntityRole::Mentioned,
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        }
    }

    fn kept(
        input: &EntityAnnotationInput,
        mentions: Vec<EntityMention>,
    ) -> (Vec<String>, usize, usize) {
        let output = EntityAnnotationOutput {
            facts: vec![crate::ports::FactAnnotation {
                fact_index: 0,
                entities: mentions,
                relations: vec![],
            }],
            repairs: vec![],
        };
        let (output, rejected, aliases) = ground_annotation(input, output);
        (
            output.facts[0]
                .entities
                .iter()
                .map(|m| m.name.clone())
                .collect(),
            rejected,
            aliases,
        )
    }

    #[test]
    fn names_in_the_window_or_the_fact_are_grounded() {
        let input = input(
            &[(
                "Oriel",
                "We drove to Lake Vessary with my aunt's dog Quibbet.",
            )],
            &["Oriel took Quibbet to Lake Vessary."],
        );
        let (names, rejected, _) = kept(
            &input,
            vec![
                mention("Oriel", &[]),
                mention("Quibbet", &[]),
                mention("lake vessary", &[]),
                mention("Oriel's aunt", &[]),
                mention("Oriel's aunt's dog", &[]),
            ],
        );
        assert_eq!(names.len(), 5, "{names:?}");
        assert_eq!(rejected, 0);
    }

    #[test]
    fn speculative_names_are_rejected_and_ungrounded_aliases_dropped() {
        let input = input(
            &[("Oriel", "My cousin Brannoch lent me his bike.")],
            &["Oriel borrowed a bike from Brannoch."],
        );
        let (names, rejected, aliases) = kept(
            &input,
            vec![
                mention("Quillsby Harbour", &[]),
                mention("Brannoch", &["Bran the Bold"]),
                mention("Oriel's uncle", &[]),
                mention("Oriel and Brannoch", &[]),
            ],
        );
        assert_eq!(names, vec!["Brannoch"]);
        assert_eq!((rejected, aliases), (3, 1));
        let (names, _, aliases) = kept(&input, vec![mention("the bike", &["a bike"])]);
        assert_eq!(
            (names, aliases),
            (vec!["the bike".to_string()], 0),
            "articles are ignored"
        );
    }

    #[test]
    fn a_name_never_matches_across_two_messages() {
        let input = input(
            &[("Oriel", "I met Tamsin"), ("Deverel", "Ashby was there")],
            &["x"],
        );
        let (names, rejected, _) = kept(&input, vec![mention("Tamsin Ashby", &["Tamsin"])]);
        assert_eq!(
            names,
            vec!["Tamsin Ashby"],
            "kept through its grounded alias"
        );
        assert_eq!(rejected, 0);
        let (names, _, _) = kept(&input, vec![mention("Tamsin Ashby", &[])]);
        assert!(names.is_empty());
    }

    #[test]
    fn only_names_of_a_particular_referent_are_kept() {
        for name in [
            "Quorvath",
            "the Bellamira Trust",
            "Nadrel's aunt",
            "Nadrel's aunt's sewing box",
            "eGrove",
            "\u{6771}\u{4eac}",
        ] {
            assert!(names_a_particular_referent(name), "{name}");
        }
        for name in [
            "armchair",
            "a red colander",
            "The colander",
            "the neighbour's hamster",
            "someone's teapot",
            "",
        ] {
            assert!(!names_a_particular_referent(name), "{name}");
        }
        let output = EntityAnnotationOutput {
            facts: vec![crate::ports::FactAnnotation {
                fact_index: 0,
                entities: vec![
                    mention("Quorvath", &[]),
                    mention("armchair", &[]),
                    mention("nadrel", &[]),
                ],
                relations: vec![],
            }],
            repairs: vec![],
        };
        let (output, rejected) = keep_particular_referents(output, &["Nadrel".to_string()]);
        assert_eq!(rejected, 1);
        let names: Vec<&str> = output.facts[0]
            .entities
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["Quorvath", "nadrel"],
            "a speaker's name always qualifies"
        );
    }

    #[test]
    fn possessive_names_split_at_the_last_owner() {
        assert_eq!(
            split_possessive("Ines's grandfather's clock"),
            Some(("Ines's grandfather", "clock"))
        );
        assert_eq!(
            split_possessive("the Novaks' house"),
            Some(("the Novaks", "house"))
        );
        assert_eq!(split_possessive("Biscuit"), None);
    }

    struct Scripted(
        std::sync::Mutex<Vec<Result<EntityAnnotationOutput, MemcoreError>>>,
        std::sync::Mutex<Vec<AnnotationAttempt>>,
    );

    #[async_trait::async_trait]
    impl LlmProvider for Scripted {
        async fn extract_facts(
            &self,
            _input: crate::ports::FactExtractionInput,
        ) -> memcore_common::MemcoreResult<crate::FactExtractionOutput> {
            unreachable!()
        }

        async fn classify_fact_operation(
            &self,
            _input: crate::ports::FactClassificationInput,
        ) -> memcore_common::MemcoreResult<crate::FactOperationDecision> {
            unreachable!()
        }

        async fn summarize_memory(
            &self,
            _input: crate::ports::SummarizationInput,
        ) -> memcore_common::MemcoreResult<String> {
            unreachable!()
        }

        async fn annotate_entities(
            &self,
            input: EntityAnnotationInput,
        ) -> memcore_common::MemcoreResult<EntityAnnotationOutput> {
            self.1.lock().unwrap().push(input.attempt);
            self.0.lock().unwrap().remove(0)
        }
    }

    fn scripted(results: Vec<Result<EntityAnnotationOutput, MemcoreError>>) -> Scripted {
        Scripted(
            std::sync::Mutex::new(results),
            std::sync::Mutex::new(Vec::new()),
        )
    }

    fn usable(repairs: Vec<JsonRepair>) -> Result<EntityAnnotationOutput, MemcoreError> {
        Ok(EntityAnnotationOutput {
            facts: vec![crate::ports::FactAnnotation {
                fact_index: 0,
                entities: vec![mention("Oriel", &[])],
                relations: vec![],
            }],
            repairs,
        })
    }

    fn bad() -> Result<EntityAnnotationOutput, MemcoreError> {
        Err(crate::ports::unusable_annotation_output(
            "syntax",
            "unusable at line 1 column 9",
        ))
    }

    #[tokio::test]
    async fn a_usable_first_output_is_not_retried() {
        let llm = scripted(vec![usable(vec![JsonRepair::TrailingComma])]);
        let window = annotate_window(&llm, input(&[("Oriel", "hi")], &["Oriel said hi."])).await;
        assert!(window.output.is_some());
        assert_eq!(
            window.audit.first_attempt,
            AnnotationAttemptStatus::Repaired
        );
        assert_eq!(window.audit.first_repairs, vec![JsonRepair::TrailingComma]);
        assert!(!window.audit.retry_used && window.audit.succeeded() && window.audit.repaired());
        assert_eq!(*llm.1.lock().unwrap(), vec![AnnotationAttempt::First]);
    }

    #[tokio::test]
    async fn an_unusable_output_is_retried_once() {
        let llm = scripted(vec![bad(), usable(vec![])]);
        let window = annotate_window(&llm, input(&[("Oriel", "hi")], &["Oriel said hi."])).await;
        assert!(window.output.is_some());
        assert_eq!(
            window.audit.first_attempt,
            AnnotationAttemptStatus::Unusable
        );
        assert!(window.audit.retry_used);
        assert_eq!(
            window.audit.retry_result,
            Some(AnnotationAttemptStatus::Parsed)
        );
        assert!(window.audit.succeeded());
        assert_eq!(
            *llm.1.lock().unwrap(),
            vec![AnnotationAttempt::First, AnnotationAttempt::Retry]
        );
    }

    #[tokio::test]
    async fn a_failed_retry_gives_no_annotation_and_no_third_request() {
        let truncated = Err(MemcoreError::provider_output_truncated(
            "annotate_entities",
            Some(10),
        ));
        let llm = scripted(vec![truncated, bad(), usable(vec![])]);
        let window = annotate_window(&llm, input(&[("Oriel", "hi")], &["Oriel said hi."])).await;
        assert!(window.output.is_none());
        assert_eq!(
            window.audit.first_attempt,
            AnnotationAttemptStatus::Truncated
        );
        assert_eq!(
            window.audit.retry_result,
            Some(AnnotationAttemptStatus::Unusable)
        );
        assert_eq!(
            window.audit.failure_reason.as_deref(),
            Some("unusable_output:syntax")
        );
        assert_eq!(llm.1.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_provider_error_is_not_retried_here() {
        let llm = scripted(vec![
            Err(MemcoreError::ProviderError(
                "http request failed".to_string(),
            )),
            usable(vec![]),
        ]);
        let window = annotate_window(&llm, input(&[("Oriel", "hi")], &["Oriel said hi."])).await;
        assert!(window.output.is_none());
        assert!(!window.audit.retry_used);
        assert_eq!(
            window.audit.failure_reason.as_deref(),
            Some("provider_error:provider_error")
        );
        assert_eq!(llm.1.lock().unwrap().len(), 1);
    }
}
