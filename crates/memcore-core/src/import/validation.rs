use memcore_common::{MemcoreError, MemcoreResult};
use serde_json::Value;
use uuid::Uuid;

use crate::export::{
    USER_EXPORT_FORMAT_VERSION, USER_EXPORT_FORMAT_VERSION_V2, USER_EXPORT_FORMAT_VERSION_V3,
    UserMemoryExport,
};
use crate::import::{
    DUPLICATE_FACT_ID, ImportMode, ImportValidationIssue, ImportValidationSummary,
};
use crate::{Fact, MemoryEvent, TenantContext};

const FORBIDDEN_METADATA_KEYS: &[&str] = &[
    "api_key",
    "key_hash",
    "raw_key",
    "bearer",
    "bearer_token",
    "secret",
];

fn validation_issue(
    code: &str,
    message: impl Into<String>,
    path: Option<String>,
) -> ImportValidationIssue {
    ImportValidationIssue {
        code: code.to_string(),
        message: message.into(),
        path,
    }
}

/// Collects all import validation issues without performing writes.
pub fn collect_import_validation(
    export: &UserMemoryExport,
    tenant: &TenantContext,
    restore_events: bool,
) -> ImportValidationSummary {
    let mut errors = Vec::new();
    let warnings = Vec::new();

    if export.format_version != USER_EXPORT_FORMAT_VERSION
        && export.format_version != USER_EXPORT_FORMAT_VERSION_V2
        && export.format_version != USER_EXPORT_FORMAT_VERSION_V3
    {
        errors.push(validation_issue(
            "UNSUPPORTED_FORMAT_VERSION",
            format!("unsupported format_version: {}", export.format_version),
            Some("export.format_version".to_string()),
        ));
    }

    if export.org_id != tenant.org_id {
        errors.push(validation_issue(
            "ORG_ID_MISMATCH",
            "export org_id does not match tenant org_id",
            Some("export.org_id".to_string()),
        ));
    }

    if export.user_id != tenant.user_id {
        errors.push(validation_issue(
            "USER_ID_MISMATCH",
            "export user_id does not match path user_id",
            Some("export.user_id".to_string()),
        ));
    }

    for (index, fact) in export.facts.iter().enumerate() {
        collect_fact_validation_issues(fact, tenant, index, &mut errors);
    }

    if let Some(entities) = &export.entities {
        let owned =
            |org_id: &str, user_id: &str| org_id == tenant.org_id && user_id == tenant.user_id;
        for (index, entity) in entities.entities.iter().enumerate() {
            if !owned(&entity.org_id, &entity.user_id) {
                errors.push(validation_issue(
                    "ENTITY_TENANT_MISMATCH",
                    "entity org_id/user_id does not match the tenant",
                    Some(format!("export.entities.entities[{index}]")),
                ));
            }
            if entity.canonical_name.trim().is_empty() {
                errors.push(validation_issue(
                    "ENTITY_NAME_EMPTY",
                    "entity canonical_name cannot be empty",
                    Some(format!("export.entities.entities[{index}]")),
                ));
            }
        }
        for (index, link) in entities.links.iter().enumerate() {
            if !owned(&link.org_id, &link.user_id) {
                errors.push(validation_issue(
                    "ENTITY_TENANT_MISMATCH",
                    "entity link org_id/user_id does not match the tenant",
                    Some(format!("export.entities.links[{index}]")),
                ));
            }
        }
        for (index, relation) in entities.relations.iter().enumerate() {
            if !owned(&relation.org_id, &relation.user_id) {
                errors.push(validation_issue(
                    "ENTITY_TENANT_MISMATCH",
                    "entity relation org_id/user_id does not match the tenant",
                    Some(format!("export.entities.relations[{index}]")),
                ));
            }
        }
    }

    for (index, episode) in export.episodes.iter().flatten().enumerate() {
        if episode.org_id != tenant.org_id || episode.user_id != tenant.user_id {
            errors.push(validation_issue(
                "EPISODE_TENANT_MISMATCH",
                "episode org_id/user_id does not match the tenant",
                Some(format!("export.episodes[{index}]")),
            ));
        }
        if episode.content.trim().is_empty() {
            errors.push(validation_issue(
                "EPISODE_CONTENT_EMPTY",
                "episode content cannot be empty",
                Some(format!("export.episodes[{index}].content")),
            ));
        }
    }

    if restore_events {
        for (index, event) in export.memory_events.iter().enumerate() {
            collect_event_validation_issues(event, tenant, index, &mut errors);
        }
    }

    collect_truncation_metadata_issues(export, &mut errors);
    collect_completeness_manifest_issues(export, &mut errors);
    collect_duplicate_fact_id_issues(export, &mut errors);

    ImportValidationSummary {
        valid: errors.is_empty(),
        errors,
        warnings,
    }
}

/// Each exported fact has its own source id, in
/// every export (with or without a completeness manifest) and every mode, so the id map of an
/// import is never ambiguous. Reported by position only.
fn collect_duplicate_fact_id_issues(
    export: &UserMemoryExport,
    errors: &mut Vec<ImportValidationIssue>,
) {
    let mut first_index: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::with_capacity(export.facts.len());
    for (index, fact) in export.facts.iter().enumerate() {
        if let Some(earlier) = first_index.get(&fact.id) {
            errors.push(validation_issue(
                DUPLICATE_FACT_ID,
                format!(
                    "{DUPLICATE_FACT_ID}: export.facts[{index}] has the same id as export.facts[{earlier}]"
                ),
                Some(format!("export.facts[{index}].id")),
            ));
        } else {
            first_index.insert(fact.id, index);
        }
    }
}

/// Sections an export can mark as truncated.
const TRUNCATABLE_SECTIONS: &[&str] = &["facts", "memory_events", "episodes"];

/// Malformed truncation metadata is an error in every mode, so it is never read as "complete".
fn collect_truncation_metadata_issues(
    export: &UserMemoryExport,
    errors: &mut Vec<ImportValidationIssue>,
) {
    let Some(sections) = &export.truncated else {
        return;
    };
    if sections.is_empty() {
        errors.push(validation_issue(
            "TRUNCATION_METADATA_INVALID",
            "export.truncated is present but empty; a complete export omits it",
            Some("export.truncated".to_string()),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for (index, section) in sections.iter().enumerate() {
        let path = Some(format!("export.truncated[{index}]"));
        if !TRUNCATABLE_SECTIONS.contains(&section.section.as_str()) {
            errors.push(validation_issue(
                "TRUNCATION_METADATA_INVALID",
                format!(
                    "unknown truncated section `{}` (expected one of: {})",
                    section.section,
                    TRUNCATABLE_SECTIONS.join(", ")
                ),
                path.clone(),
            ));
        }
        if section.limit == 0 {
            errors.push(validation_issue(
                "TRUNCATION_METADATA_INVALID",
                "truncated section limit must be greater than 0",
                path.clone(),
            ));
        }
        if !seen.insert(section.section.as_str()) {
            errors.push(validation_issue(
                "TRUNCATION_METADATA_INVALID",
                format!("truncated section `{}` is listed twice", section.section),
                path,
            ));
        }
    }
}

/// A complete-export manifest must be exactly consistent with the archive, in every
/// mode and before any write: a missing, extra or corrupted page makes the counts disagree, and an
/// archive cannot be both complete and truncated. Legacy exports without a manifest are not checked
/// here and are never treated as complete.
fn collect_completeness_manifest_issues(
    export: &UserMemoryExport,
    errors: &mut Vec<ImportValidationIssue>,
) {
    let Some(manifest) = &export.completeness else {
        return;
    };
    let mut issue = |message: String, path: &str| {
        errors.push(validation_issue(
            "COMPLETE_EXPORT_MANIFEST_INVALID",
            message,
            Some(format!("export.completeness{path}")),
        ));
    };
    if manifest.mode != crate::EXPORT_COMPLETE_MODE {
        issue(
            format!("unknown completeness mode `{}`", manifest.mode),
            ".mode",
        );
    }
    if export.truncated.is_some() {
        issue(
            "an export cannot be both complete and truncated".to_string(),
            "",
        );
    }
    if manifest.verified_reads < 2 {
        issue(
            format!(
                "verified_reads must be >= 2 (got {})",
                manifest.verified_reads
            ),
            ".verified_reads",
        );
    }
    if manifest.page_size == 0 || manifest.page_size > crate::COMPLETE_EXPORT_MAX_PAGE_SIZE {
        issue(
            format!(
                "page_size must be between 1 and {} (got {})",
                crate::COMPLETE_EXPORT_MAX_PAGE_SIZE,
                manifest.page_size
            ),
            ".page_size",
        );
    }
    let entities = export.entities.as_ref();
    let actual = crate::ExportSectionCounts {
        facts: export.facts.len(),
        memory_events: export.memory_events.len(),
        episodes: export.episodes.as_ref().map_or(0, Vec::len),
        entities: entities.map_or(0, |e| e.entities.len()),
        entity_links: entities.map_or(0, |e| e.links.len()),
        entity_relations: entities.map_or(0, |e| e.relations.len()),
    };
    if export.episodes.is_none() || entities.is_none() {
        issue(
            "a complete export carries every section (episodes and entities, possibly empty)"
                .to_string(),
            "",
        );
    }
    if actual != manifest.counts {
        issue(
            format!(
                "section counts do not match the archive (manifest {:?}, archive {:?}): a page is missing, duplicated or corrupted",
                manifest.counts, actual
            ),
            ".counts",
        );
    }
    let fact_ids: std::collections::HashSet<_> = export.facts.iter().map(|f| f.id).collect();
    let event_ids: std::collections::HashSet<_> =
        export.memory_events.iter().map(|e| e.id).collect();
    let episode_ids: std::collections::HashSet<_> =
        export.episodes.iter().flatten().map(|e| e.id).collect();
    let entity_ids: std::collections::HashSet<_> = entities
        .iter()
        .flat_map(|e| &e.entities)
        .map(|e| e.id)
        .collect();
    let link_ids: std::collections::HashSet<_> = entities
        .iter()
        .flat_map(|e| &e.links)
        .map(|l| (l.entity_id, l.fact_id))
        .collect();
    let relation_ids: std::collections::HashSet<_> = entities
        .iter()
        .flat_map(|e| &e.relations)
        .map(|r| r.id)
        .collect();
    if fact_ids.len() != export.facts.len()
        || event_ids.len() != export.memory_events.len()
        || episode_ids.len() != actual.episodes
        || entity_ids.len() != actual.entities
        || link_ids.len() != actual.entity_links
        || relation_ids.len() != actual.entity_relations
    {
        issue("duplicate record ids in a complete export".to_string(), "");
    }
}

/// A well-formed truncation marker means the export is NOT a complete backup. A `replace`
/// import would delete every record of the user that is missing from it, so it is refused before
/// any write; an `append` import keeps its semantics and only gets a warning.
pub fn collect_truncation_mode_validation(
    export: &UserMemoryExport,
    mode: ImportMode,
    summary: &mut ImportValidationSummary,
) {
    let Some(sections) = export.truncated.as_ref().filter(|s| !s.is_empty()) else {
        return;
    };
    let listed = sections
        .iter()
        .map(|s| format!("{} (limit {})", s.section, s.limit))
        .collect::<Vec<_>>()
        .join(", ");
    match mode {
        ImportMode::Replace => {
            summary.errors.push(validation_issue(
                "TRUNCATED_EXPORT_REPLACE_REJECTED",
                format!(
                    "this export is marked truncated ({listed}): it is not a complete backup, and a \
                     replace import would permanently delete the user's records that are missing \
                     from it. Use mode=append to add the exported records, or import a complete \
                     export"
                ),
                Some("export.truncated".to_string()),
            ));
            summary.valid = false;
        }
        ImportMode::Append => summary.warnings.push(validation_issue(
            "TRUNCATED_EXPORT_PARTIAL",
            format!(
                "this export is marked truncated ({listed}); append adds only the exported records"
            ),
            Some("export.truncated".to_string()),
        )),
    }
}

fn collect_fact_validation_issues(
    fact: &Fact,
    tenant: &TenantContext,
    index: usize,
    errors: &mut Vec<ImportValidationIssue>,
) {
    let base_path = format!("export.facts[{index}]");

    if fact.org_id != tenant.org_id || fact.user_id != tenant.user_id {
        errors.push(validation_issue(
            "FACT_TENANT_MISMATCH",
            "fact org_id/user_id does not match import tenant",
            Some(base_path.clone()),
        ));
    }

    if fact.content.trim().is_empty() {
        errors.push(validation_issue(
            "EMPTY_FACT_CONTENT",
            "fact content cannot be empty",
            Some(format!("{base_path}.content")),
        ));
    }

    if !(0.0..=1.0).contains(&fact.confidence) {
        errors.push(validation_issue(
            "INVALID_CONFIDENCE",
            "fact confidence must be between 0.0 and 1.0",
            Some(format!("{base_path}.confidence")),
        ));
    }

    if !(0.0..=1.0).contains(&fact.importance) {
        errors.push(validation_issue(
            "INVALID_IMPORTANCE",
            "fact importance must be between 0.0 and 1.0",
            Some(format!("{base_path}.importance")),
        ));
    }

    if contains_forbidden_secret_fields(&fact.metadata) {
        errors.push(validation_issue(
            "FORBIDDEN_SECRET_METADATA",
            "fact metadata contains forbidden secret fields",
            Some(format!("{base_path}.metadata")),
        ));
    }
}

fn collect_event_validation_issues(
    event: &MemoryEvent,
    tenant: &TenantContext,
    index: usize,
    errors: &mut Vec<ImportValidationIssue>,
) {
    let base_path = format!("export.memory_events[{index}]");

    if event.org_id != tenant.org_id || event.user_id != tenant.user_id {
        errors.push(validation_issue(
            "EVENT_TENANT_MISMATCH",
            "memory event org_id/user_id does not match import tenant",
            Some(base_path.clone()),
        ));
    }

    if contains_forbidden_secret_fields(&event.metadata) {
        errors.push(validation_issue(
            "FORBIDDEN_SECRET_METADATA",
            "memory event metadata contains forbidden secret fields",
            Some(format!("{base_path}.metadata")),
        ));
    }
}

/// Validates export envelope and tenant alignment before import.
pub fn validate_export_for_import(
    export: &UserMemoryExport,
    tenant: &TenantContext,
) -> MemcoreResult<()> {
    let summary = collect_import_validation(export, tenant, false);
    if !summary.valid {
        return Err(MemcoreError::ValidationError(
            summary
                .first_error_message()
                .unwrap_or_else(|| "import validation failed".to_string()),
        ));
    }
    Ok(())
}

/// Validates a single exported fact for import.
pub fn validate_fact_for_import(fact: &Fact, tenant: &TenantContext) -> MemcoreResult<()> {
    let mut errors = Vec::new();
    collect_fact_validation_issues(fact, tenant, 0, &mut errors);
    if let Some(issue) = errors.into_iter().next() {
        return Err(MemcoreError::ValidationError(issue.message));
    }
    Ok(())
}

/// Validates a single exported memory event for import.
pub fn validate_event_for_import(event: &MemoryEvent, tenant: &TenantContext) -> MemcoreResult<()> {
    let mut errors = Vec::new();
    collect_event_validation_issues(event, tenant, 0, &mut errors);
    if let Some(issue) = errors.into_iter().next() {
        return Err(MemcoreError::ValidationError(issue.message));
    }
    Ok(())
}

/// Returns true when JSON contains keys that must never be imported (API keys, hashes, tokens).
pub fn contains_forbidden_secret_fields(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let normalized = key.to_ascii_lowercase();
                if FORBIDDEN_METADATA_KEYS
                    .iter()
                    .any(|forbidden| normalized.contains(forbidden))
                {
                    return true;
                }
                if contains_forbidden_secret_fields(nested) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => items.iter().any(contains_forbidden_secret_fields),
        _ => false,
    }
}

/// Resolves the fact id to use on import. In append mode, generates a new id when the
/// exported id already exists for this tenant.
pub fn resolve_import_fact_id(exported_id: Uuid, id_exists: bool, mode: super::ImportMode) -> Uuid {
    if id_exists && matches!(mode, super::ImportMode::Append) {
        Uuid::new_v4()
    } else {
        exported_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ImportMode;
    use crate::{MemorySource, MemoryType};
    use chrono::Utc;
    use serde_json::json;

    fn tenant() -> TenantContext {
        TenantContext::new("org_a", "user_a").expect("tenant")
    }

    fn sample_fact() -> Fact {
        Fact::new(
            Uuid::new_v4(),
            "org_a",
            "user_a",
            MemoryType::Profile,
            "hello",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            Utc::now(),
            Utc::now(),
            json!({}),
        )
        .expect("fact")
    }

    fn sample_export(facts: Vec<Fact>) -> UserMemoryExport {
        UserMemoryExport::new("org_a", "user_a", facts, vec![])
    }

    #[test]
    fn forbidden_metadata_keys_are_detected() {
        assert!(contains_forbidden_secret_fields(&json!({ "api_key": "x" })));
        assert!(contains_forbidden_secret_fields(
            &json!({ "nested": { "key_hash": "y" } })
        ));
        assert!(!contains_forbidden_secret_fields(
            &json!({ "source": "import" })
        ));
    }

    #[test]
    fn resolve_import_fact_id_regenerates_on_append_collision() {
        let original = Uuid::new_v4();
        let resolved = resolve_import_fact_id(original, true, ImportMode::Append);
        assert_ne!(resolved, original);
    }

    #[test]
    fn validate_fact_rejects_mismatched_tenant() {
        let mut fact = sample_fact();
        fact.user_id = "other".to_string();
        let err = validate_fact_for_import(&fact, &tenant()).expect_err("should fail");
        assert!(matches!(err, MemcoreError::ValidationError(_)));
    }

    #[test]
    fn collect_import_validation_reports_user_id_mismatch() {
        let export = UserMemoryExport::new("org_a", "user_b", vec![sample_fact()], vec![]);
        let summary = collect_import_validation(&export, &tenant(), false);
        assert!(!summary.valid);
        assert!(
            summary
                .errors
                .iter()
                .any(|issue| issue.code == "USER_ID_MISMATCH")
        );
    }

    #[test]
    fn collect_import_validation_reports_invalid_importance() {
        let mut fact = sample_fact();
        fact.importance = 2.0;
        let export = sample_export(vec![fact]);
        let summary = collect_import_validation(&export, &tenant(), false);
        assert!(!summary.valid);
        assert!(
            summary
                .errors
                .iter()
                .any(|issue| issue.code == "INVALID_IMPORTANCE")
        );
    }
}
