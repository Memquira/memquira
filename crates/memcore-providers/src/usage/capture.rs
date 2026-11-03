//! Per-call provider usage capture.
//!
//! The router runs each logical provider call (retries included) inside [`with_usage_capture`].
//! Inside it, the resilient wrapper records its character-count ESTIMATE and the OpenAI-compatible
//! client records the token counts the provider REPORTED in each HTTP response (`usage`). The
//! router then records one usage event whose tokens are labelled with their source, so an estimate
//! is never presented as a provider-reported count. Unlike the per-candidate token slot, the
//! capture is per call, so concurrent calls never mix their usage.

use std::cell::RefCell;
use std::future::Future;

use serde_json::Value;

use super::types::ProviderTokenUsage;

/// What a provider response said about usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageReportStatus {
    /// No response carried a usage report (or no response was received).
    #[default]
    NotReported,
    /// Every expected count was reported as a non-negative integer.
    Complete,
    /// Some expected counts were missing.
    Partial,
    /// A `usage` field was present but not a usable object / numbers.
    Malformed,
}

impl UsageReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotReported => "missing",
            Self::Complete => "reported",
            Self::Partial => "partial",
            Self::Malformed => "malformed",
        }
    }

    /// The less trustworthy of two statuses (used when one call made several requests).
    fn worst(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Complete => 0,
            Self::Partial => 1,
            Self::NotReported => 2,
            Self::Malformed => 3,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Provider-reported token counts (summed over the responses of one call).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportedUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub status: UsageReportStatus,
    /// Responses that carried a report of any kind.
    pub responses: u32,
}

fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

impl ReportedUsage {
    fn merge(self, other: Self) -> Self {
        Self {
            input_tokens: add(self.input_tokens, other.input_tokens),
            output_tokens: add(self.output_tokens, other.output_tokens),
            total_tokens: add(self.total_tokens, other.total_tokens),
            status: if self.responses == 0 {
                other.status
            } else {
                self.status.worst(other.status)
            },
            responses: self.responses + other.responses,
        }
    }
}

/// Parses an OpenAI-compatible `usage` object from a response body.
///
/// Chat completions: `prompt_tokens` / `completion_tokens` / `total_tokens`. Responses API:
/// `input_tokens` / `output_tokens` / `total_tokens`. Embeddings: `prompt_tokens` /
/// `total_tokens` (no output). `expects_output` is false for embeddings.
pub fn parse_reported_usage(body: &Value, expects_output: bool) -> ReportedUsage {
    let usage = match body.get("usage") {
        None | Some(Value::Null) => {
            return ReportedUsage {
                status: UsageReportStatus::NotReported,
                ..ReportedUsage::default()
            };
        }
        Some(Value::Object(map)) => map,
        Some(_) => {
            return ReportedUsage {
                status: UsageReportStatus::Malformed,
                responses: 1,
                ..ReportedUsage::default()
            };
        }
    };
    let mut malformed = false;
    let mut count = |keys: &[&str]| -> Option<u64> {
        for key in keys {
            match usage.get(*key) {
                None | Some(Value::Null) => continue,
                Some(value) => match value.as_u64() {
                    Some(n) => return Some(n),
                    None => {
                        malformed = true;
                        return None;
                    }
                },
            }
        }
        None
    };
    let input = count(&["prompt_tokens", "input_tokens"]);
    let output = count(&["completion_tokens", "output_tokens"]);
    let total = count(&["total_tokens"]);
    let status = if malformed {
        UsageReportStatus::Malformed
    } else if input.is_some() && (!expects_output || output.is_some()) {
        UsageReportStatus::Complete
    } else if input.is_some() || output.is_some() || total.is_some() {
        UsageReportStatus::Partial
    } else {
        UsageReportStatus::Malformed
    };
    ReportedUsage {
        input_tokens: input,
        output_tokens: if expects_output { output } else { None },
        total_tokens: total,
        status,
        responses: 1,
    }
}

/// Where an event's token counts came from (persisted as `metadata.token_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenSource {
    /// Every count the provider was expected to report, as reported.
    Reported,
    /// Only the counts the provider reported; the rest are absent, never estimated.
    PartiallyReported,
    /// memcore's character-count estimate (the provider reported nothing usable).
    Estimated,
    /// No counts at all.
    #[default]
    None,
}

impl TokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::PartiallyReported => "partially_reported",
            Self::Estimated => "estimated",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Default)]
struct Capture {
    estimate: Option<ProviderTokenUsage>,
    reported: ReportedUsage,
}

/// The usage observed during one logical provider call.
#[derive(Debug, Clone, Copy, Default)]
pub struct CapturedUsage {
    pub estimate: Option<ProviderTokenUsage>,
    pub reported: ReportedUsage,
}

impl CapturedUsage {
    /// Token counts for the usage event and their source. Reported counts win; a partial report
    /// keeps only what was reported; an estimate is used only when nothing usable was reported.
    pub fn resolve(&self) -> (Option<u64>, Option<u64>, TokenSource) {
        match self.reported.status {
            UsageReportStatus::Complete if self.reported.responses > 0 => (
                self.reported.input_tokens,
                self.reported.output_tokens,
                TokenSource::Reported,
            ),
            UsageReportStatus::Partial => (
                self.reported.input_tokens,
                self.reported.output_tokens,
                TokenSource::PartiallyReported,
            ),
            _ => match self.estimate {
                Some(estimate) => (
                    estimate.input_tokens,
                    estimate.output_tokens,
                    TokenSource::Estimated,
                ),
                None => (None, None, TokenSource::None),
            },
        }
    }
}

tokio::task_local! {
    static CAPTURE: RefCell<Capture>;
}

/// Runs `future` with a fresh usage capture and returns what it captured.
pub async fn with_usage_capture<F: Future>(future: F) -> (F::Output, CapturedUsage) {
    CAPTURE
        .scope(RefCell::new(Capture::default()), async move {
            let output = future.await;
            let captured = CAPTURE
                .try_with(|cell| {
                    let capture = cell.borrow();
                    CapturedUsage {
                        estimate: capture.estimate,
                        reported: capture.reported,
                    }
                })
                .unwrap_or_default();
            (output, captured)
        })
        .await
}

/// Records memcore's estimate for the current call (the last one wins; never summed).
pub fn capture_estimated_usage(usage: ProviderTokenUsage) {
    let _ = CAPTURE.try_with(|cell| cell.borrow_mut().estimate = Some(usage));
}

/// Adds one response's reported usage to the current call (summed over responses).
pub fn capture_reported_usage(report: ReportedUsage) {
    if report.responses == 0 && report.status == UsageReportStatus::NotReported {
        // A response without `usage`: remember it so a call is not labelled complete.
        let _ = CAPTURE.try_with(|cell| {
            let mut capture = cell.borrow_mut();
            if capture.reported.responses > 0 {
                capture.reported.status = capture.reported.status.worst(UsageReportStatus::Partial);
            }
        });
        return;
    }
    let _ = CAPTURE.try_with(|cell| {
        let mut capture = cell.borrow_mut();
        capture.reported = capture.reported.merge(report);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_chat_responses_and_embedding_usage() {
        let chat = parse_reported_usage(
            &json!({"usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}}),
            true,
        );
        assert_eq!(
            (
                chat.input_tokens,
                chat.output_tokens,
                chat.total_tokens,
                chat.status
            ),
            (Some(12), Some(5), Some(17), UsageReportStatus::Complete)
        );
        let responses = parse_reported_usage(
            &json!({"usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}}),
            true,
        );
        assert_eq!(responses.status, UsageReportStatus::Complete);
        assert_eq!(responses.input_tokens, Some(3));
        let embed = parse_reported_usage(
            &json!({"usage": {"prompt_tokens": 8, "total_tokens": 8}}),
            false,
        );
        assert_eq!(
            (embed.input_tokens, embed.output_tokens, embed.status),
            (Some(8), None, UsageReportStatus::Complete)
        );
    }

    #[test]
    fn missing_partial_and_malformed_usage() {
        assert_eq!(
            parse_reported_usage(&json!({"data": []}), false).status,
            UsageReportStatus::NotReported
        );
        assert_eq!(
            parse_reported_usage(&json!({"usage": null}), true).status,
            UsageReportStatus::NotReported
        );
        let partial = parse_reported_usage(&json!({"usage": {"total_tokens": 9}}), true);
        assert_eq!(partial.status, UsageReportStatus::Partial);
        assert_eq!((partial.input_tokens, partial.output_tokens), (None, None));
        let chat_no_output = parse_reported_usage(&json!({"usage": {"prompt_tokens": 9}}), true);
        assert_eq!(chat_no_output.status, UsageReportStatus::Partial);
        for bad in [
            json!({"usage": "lots"}),
            json!({"usage": {"prompt_tokens": -1}}),
            json!({"usage": {"prompt_tokens": "12"}}),
            json!({"usage": {"prompt_tokens": 1.5}}),
            json!({"usage": {}}),
        ] {
            assert_eq!(
                parse_reported_usage(&bad, true).status,
                UsageReportStatus::Malformed,
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn capture_resolves_reported_over_estimate_and_never_mixes_calls() {
        let ((), captured) = with_usage_capture(async {
            capture_estimated_usage(ProviderTokenUsage::from_counts(Some(100), Some(50)));
            capture_reported_usage(parse_reported_usage(
                &json!({"usage": {"prompt_tokens": 12, "completion_tokens": 5}}),
                true,
            ));
            // A retried attempt that also got a response: its usage is summed.
            capture_reported_usage(parse_reported_usage(
                &json!({"usage": {"prompt_tokens": 10, "completion_tokens": 1}}),
                true,
            ));
        })
        .await;
        assert_eq!(
            captured.resolve(),
            (Some(22), Some(6), TokenSource::Reported)
        );

        let ((), estimate_only) = with_usage_capture(async {
            capture_estimated_usage(ProviderTokenUsage::from_counts(Some(100), Some(50)));
            capture_reported_usage(parse_reported_usage(&json!({}), true));
        })
        .await;
        assert_eq!(
            estimate_only.resolve(),
            (Some(100), Some(50), TokenSource::Estimated)
        );

        let ((), partial) = with_usage_capture(async {
            capture_estimated_usage(ProviderTokenUsage::from_counts(Some(100), Some(50)));
            capture_reported_usage(parse_reported_usage(
                &json!({"usage": {"prompt_tokens": 7}}),
                true,
            ));
        })
        .await;
        assert_eq!(
            partial.resolve(),
            (Some(7), None, TokenSource::PartiallyReported),
            "a partial report is never filled in from the estimate"
        );

        // Outside a capture nothing panics and nothing is recorded.
        capture_estimated_usage(ProviderTokenUsage::from_counts(Some(1), None));
    }
}
