//! Prompts shared by every LLM provider, kept in one place so provider implementations cannot
//! drift apart in what they ask a model for.

pub mod consolidation;
pub mod entities;
pub mod extraction;

pub use consolidation::{
    CONSOLIDATION_INSTRUCTIONS, consolidation_json_schema, consolidation_payload,
    parse_consolidation_response,
};
pub use entities::{
    ENTITY_ANNOTATION_INSTRUCTIONS, ENTITY_ANNOTATION_RETRY_NOTE, ENTITY_MATCH_INSTRUCTIONS,
    entity_annotation_instructions, entity_annotation_json_schema, entity_annotation_payload,
    entity_match_json_schema, entity_match_payload, parse_entity_annotation_response,
    parse_entity_match_response, repair_json_syntax,
};
pub use extraction::{
    StructuredExtractionPolicy, fact_extraction_instructions, fact_extraction_json_schema,
    structured_fact_extraction_instructions, structured_fact_extraction_instructions_for,
    structured_fact_extraction_json_schema,
};
