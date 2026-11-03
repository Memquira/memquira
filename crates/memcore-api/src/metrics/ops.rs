//! Application-level operation counters (safe labels only).

use metrics::counter;

/// Counts responses cut off at the output-token limit, by operation.
///
/// Kept apart from generic provider errors because the remedy differs: a truncation is fixed by
/// raising `MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS` (or the classification limit), not by waiting for
/// the provider. `operation` is a fixed name from the code, so the label set is closed.
pub fn record_provider_output_truncated(operation: &'static str) {
    counter!("memcore_provider_output_truncated_total", "operation" => operation).increment(1);
}

pub fn record_memory_create(status: &str) {
    counter!("memcore_memory_create_total", "status" => status.to_string()).increment(1);
}

/// Records what became of the candidate facts extraction produced.
///
/// Without this, a rejected memory is invisible: the request returns success and
/// the fact simply is not there. `memcore_fact_candidates_total{outcome}` and
/// `memcore_fact_discarded_total{reason}` make the drop rate and its causes
/// visible over time, not just in a single response body.
pub fn record_fact_outcomes(output: &memcore_core::AddMemoryOutput) {
    counter!("memcore_fact_candidates_total", "outcome" => "extracted")
        .increment(output.extracted as u64);
    counter!("memcore_fact_candidates_total", "outcome" => "stored")
        .increment(output.stored() as u64);
    counter!("memcore_fact_candidates_total", "outcome" => "deleted")
        .increment(output.deleted as u64);
    counter!("memcore_fact_candidates_total", "outcome" => "discarded")
        .increment(output.discarded.total() as u64);
    counter!("memcore_fact_candidates_total", "outcome" => "skipped")
        .increment(output.skipped.total() as u64);

    for reason in memcore_core::FactDiscardReason::ALL {
        let count = output.discarded.get(reason);
        if count > 0 {
            counter!("memcore_fact_discarded_total", "reason" => reason.as_str())
                .increment(count as u64);
        }
    }

    // Losses inside extraction, before any memory decision was taken. These are separate from
    // `memcore_fact_discarded_total`: a discarded fact was understood and then rejected, whereas a
    // skipped one was never usable. Both used to be invisible.
    for reason in memcore_core::CandidateSkipReason::ALL {
        let count = output.skipped.get(reason);
        if count > 0 {
            counter!("memcore_fact_candidates_skipped_total", "reason" => reason.as_str())
                .increment(count as u64);
        }
    }

    if output.event_time_unparsed > 0 {
        counter!("memcore_fact_event_time_unparsed_total")
            .increment(output.event_time_unparsed as u64);
    }

    // How each structured fact's event time was settled. A rising
    // `deterministic_override` share means the model is resolving relative dates wrongly; a rising
    // `rejected` share means it is producing impossible ones.
    for outcome in memcore_core::temporal::TemporalOutcome::ALL {
        let count = output.temporal.get(outcome);
        if count > 0 {
            counter!("memcore_temporal_resolution_total", "outcome" => outcome.as_str())
                .increment(count as u64);
        }
    }
    if output.invalid_source_message_ids > 0 {
        counter!("memcore_fact_source_ids_invalid_total")
            .increment(output.invalid_source_message_ids as u64);
    }
}

pub fn record_memory_delete(status: &str) {
    counter!("memcore_memory_delete_total", "status" => status.to_string()).increment(1);
}

pub fn record_memory_forget_user(status: &str) {
    counter!("memcore_memory_forget_user_total", "status" => status.to_string()).increment(1);
}

pub fn record_memory_search(status: &str) {
    counter!("memcore_memory_search_total", "status" => status.to_string()).increment(1);
}

pub fn record_context_request(status: &str) {
    counter!("memcore_context_requests_total", "status" => status.to_string()).increment(1);
}

pub fn record_import_request(status: &str) {
    counter!("memcore_import_requests_total", "status" => status.to_string()).increment(1);
}

pub fn record_export_request(status: &str) {
    counter!("memcore_export_requests_total", "status" => status.to_string()).increment(1);
}

pub fn record_quota_rejection(quota_type: &str) {
    counter!(
        "memcore_quota_rejections_total",
        "quota_type" => quota_type.to_string()
    )
    .increment(1);
}

/// Provider calls by bounded labels only (no free-form `model` label).
pub fn record_provider_request(
    provider: &'static str,
    role: &'static str,
    operation: &'static str,
    status: &'static str,
) {
    counter!(
        "memcore_provider_requests_total",
        "provider" => provider,
        "role" => role,
        "operation" => operation,
        "status" => status,
    )
    .increment(1);
}

/// Request classes for admission control and in-flight gauges.
pub const REQUEST_CLASSES: &[&str] = &["read", "write", "admin", "health", "other"];
/// Shed reasons will use.
pub const SHED_REASONS: &[&str] = &["capacity", "queue_timeout"];

/// Requests rejected by admission control (registered at zero so it is scrapeable).
pub fn record_request_shed(class: &'static str, reason: &'static str) {
    counter!("memcore_requests_shed_total", "class" => class, "reason" => reason).increment(1);
}

/// Registers every shed series at zero.
pub fn register_shed_counters() {
    for class in REQUEST_CLASSES {
        for reason in SHED_REASONS {
            counter!("memcore_requests_shed_total", "class" => *class, "reason" => *reason)
                .increment(0);
        }
    }
}

pub fn record_provider_failure(provider: &str, error_class: &str) {
    counter!(
        "memcore_provider_failures_total",
        "provider" => provider.to_string(),
        "error_class" => error_class.to_string(),
    )
    .increment(1);
}

pub fn record_provider_retries(provider: &str, count: u64) {
    if count == 0 {
        return;
    }
    counter!(
        "memcore_provider_retries_total",
        "provider" => provider.to_string(),
    )
    .increment(count);
}

pub fn record_provider_circuit_open(provider: &str) {
    counter!(
        "memcore_provider_circuit_breaker_open_total",
        "provider" => provider.to_string(),
    )
    .increment(1);
}

pub fn record_context_cache_hit(backend: &str) {
    counter!(
        "memcore_context_cache_hits_total",
        "backend" => backend.to_string()
    )
    .increment(1);
}

pub fn record_context_cache_miss(backend: &str) {
    counter!(
        "memcore_context_cache_misses_total",
        "backend" => backend.to_string()
    )
    .increment(1);
}

pub fn record_context_cache_stale_hit(backend: &str) {
    counter!(
        "memcore_context_cache_stale_hits_total",
        "backend" => backend.to_string()
    )
    .increment(1);
}

pub fn record_context_cache_refresh(backend: &str, status: &str) {
    if status == "failed" {
        counter!(
            "memcore_context_cache_refresh_failures_total",
            "backend" => backend.to_string()
        )
        .increment(1);
    }
    counter!(
        "memcore_context_cache_refresh_total",
        "backend" => backend.to_string(),
        "status" => status.to_string(),
    )
    .increment(1);
}

pub fn record_background_job_run(job_kind: &str, status: &str, duration_secs: f64) {
    counter!(
        "memcore_background_job_runs_total",
        "job_kind" => job_kind.to_string(),
        "status" => status.to_string(),
    )
    .increment(1);
    metrics::histogram!(
        "memcore_background_job_duration_seconds",
        "job_kind" => job_kind.to_string(),
    )
    .record(duration_secs);

    if status.eq_ignore_ascii_case("Failed") || status.eq_ignore_ascii_case("failed") {
        counter!(
            "memcore_background_job_failures_total",
            "job_kind" => job_kind.to_string(),
        )
        .increment(1);
    }
    if status.eq_ignore_ascii_case("Skipped") || status.eq_ignore_ascii_case("skipped") {
        counter!(
            "memcore_background_job_lock_skips_total",
            "job_kind" => job_kind.to_string(),
        )
        .increment(1);
    }
    if status.eq_ignore_ascii_case("Cancelled") || status.eq_ignore_ascii_case("cancelled") {
        counter!(
            "memcore_background_job_cancelled_total",
            "job_kind" => job_kind.to_string(),
        )
        .increment(1);
    }
}

pub fn record_background_job_retries(job_kind: &str, retries: u64) {
    if retries == 0 {
        return;
    }
    counter!(
        "memcore_background_job_retries_total",
        "job_kind" => job_kind.to_string(),
    )
    .increment(retries);
}
