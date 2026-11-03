use memcore_core::d76_context::{
    D76_DEFAULT_TOKEN_BUDGET, D76ContextItem, D76ContextOutput, D76PackedContextOutput, D76Packing,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

fn default_token_budget() -> usize {
    D76_DEFAULT_TOKEN_BUDGET
}

fn default_render_mode() -> String {
    "d76".to_string()
}

/// `POST /api/v1/memories/context`: one user's server-side context, rendered by the server.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MemoryContextRequest {
    pub user_id: String,
    pub query: String,
    /// cl100k_base token budget for this user's context; default 1,500, 1–4,000.
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
    /// Only `d76` (the validated rendering).
    #[serde(default = "default_render_mode")]
    pub render_mode: String,
    /// Also return the selected items with their scores, lines and per-line tokens.
    #[serde(default)]
    pub include_items: bool,
    /// `prefix` (the default unless the server sets `MEMCORE_CONTEXT_DEFAULT_PACKING`: the
    /// rank-order prefix the reference was measured with, the first line that does not fit ends
    /// the context), `fill` (opt-in: a line that does not fit is skipped and later lines are still
    /// tried, in rank order, within the same budget) or `evidence` (opt-in: `fill` over a
    /// fact-anchored order, see `d76_context::render_evidence`). Omitted: the server default.
    #[serde(default)]
    pub packing: Option<String>,
    /// Client message ids whose **episodes** are left out of this context,
    /// e.g. the `message_id`s of episodes already returned in another memory user's context for the
    /// same answer prompt (a conversation stored for both participants keeps each message as an
    /// episode of both, so concatenated contexts repeat them verbatim). Exact ids only; facts are
    /// never removed; the packing rule continues over the remaining candidates. At most 2,000 ids of
    /// at most 200 characters. Omitted or empty: the validated reference behaviour.
    #[serde(default)]
    pub exclude_message_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryContextResponse {
    pub status: &'static str,
    pub render_mode: &'static str,
    /// `e_merge` (facts and episodes) or `facts_only` (episodes disabled: (the facts-only form).
    pub source: &'static str,
    /// The selected lines joined with `\n`; `"<timestamp>: <text>"` per line.
    pub text: String,
    /// cl100k_base tokens of `text`.
    pub token_count: usize,
    pub token_budget: usize,
    /// True when the next candidate did not fit, i.e. the budget ended the context.
    pub budget_bound: bool,
    pub candidates_considered: usize,
    pub selected_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<MemoryContextItemResponse>>,
    /// `fill` or `evidence` when that packing was used (explicitly or as the server default); absent for `prefix`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packing: Option<&'static str>,
    /// With `fill` / `evidence`: the lines not kept (did not fit, or a redundant fact).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_skipped: Option<usize>,
    /// Episode candidates removed by `exclude_message_ids` (present only when ids were sent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_candidates: Option<usize>,
}

/// One selected item, in context order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryContextItemResponse {
    /// `fact` or `episode`.
    pub kind: &'static str,
    pub id: Uuid,
    pub vector_score: f32,
    pub timestamp: String,
    pub text: String,
    pub line: String,
    /// cl100k_base tokens of this line alone (`token_count` counts the joined text).
    pub tokens: usize,
    /// An episode's client message id (absent for facts and for messages sent without one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The client message ids a fact was extracted from (absent when none were recorded).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub source_message_ids: Vec<String>,
}

impl From<&D76ContextItem> for MemoryContextItemResponse {
    fn from(item: &D76ContextItem) -> Self {
        Self {
            kind: match item.kind {
                memcore_core::CandidateKind::Fact => "fact",
                memcore_core::CandidateKind::Episode => "episode",
            },
            id: item.id,
            vector_score: item.vector_score,
            timestamp: item.timestamp.clone(),
            text: item.text.clone(),
            line: item.line.clone(),
            tokens: item.tokens,
            message_id: None,
            source_message_ids: Vec::new(),
        }
    }
}

impl MemoryContextResponse {
    pub fn new(output: D76ContextOutput, include_items: bool) -> Self {
        let context = output.context;
        Self {
            status: "success",
            render_mode: "d76",
            source: if output.episodes_included {
                "e_merge"
            } else {
                "facts_only"
            },
            selected_count: context.items.len(),
            items: include_items.then(|| {
                context
                    .items
                    .iter()
                    .map(MemoryContextItemResponse::from)
                    .collect()
            }),
            text: context.text,
            token_count: context.token_count,
            token_budget: output.token_budget,
            budget_bound: context.budget_bound,
            candidates_considered: context.candidates_considered,
            packing: None,
            lines_skipped: None,
            excluded_candidates: None,
        }
    }

    /// A `Prefix` response is exactly [`Self::new`]; `Fill` and `Evidence` add
    /// `packing` and `lines_skipped`.
    pub fn from_packed(packed: D76PackedContextOutput, include_items: bool) -> Self {
        let packing = match packed.packing {
            D76Packing::Prefix => None,
            D76Packing::Fill => Some("fill"),
            D76Packing::Evidence => Some("evidence"),
        };
        let provenance = packed.item_provenance;
        let mut response = Self {
            packing,
            lines_skipped: packing.map(|_| packed.lines_skipped),
            ..Self::new(packed.output, include_items)
        };
        if let Some(items) = response.items.as_mut() {
            for (item, prov) in items.iter_mut().zip(provenance) {
                item.message_id = prov.message_id;
                item.source_message_ids = prov.source_message_ids;
            }
        }
        response
    }

    /// Records how many episode candidates `exclude_message_ids` removed.
    pub fn with_excluded(mut self, requested: bool, excluded: usize) -> Self {
        if requested {
            self.excluded_candidates = Some(excluded);
        }
        self
    }
}
