//! When a fact happened, as distinct from when it was said or stored.
//!
//! A conversation says "I ran a half marathon last Saturday" on 8 May 2023. The fact's event time is
//! Saturday 6 May 2023 (a single day), resolved against the message's own time, which is kept as the
//! `anchor`. `recorded_at` on the stored fact is neither of these: it is when memcore wrote the row.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

/// How precisely an event time is known. The range `[start, end]` always covers the whole unit —
/// a `Month` value starts at the first instant of the month and ends at the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalGranularity {
    Day,
    Week,
    Month,
    Year,
    /// A span the conversation did not pin down ("a few years ago", "recently").
    Approximate,
}

impl TemporalGranularity {
    pub const ALL: [Self; 5] = [
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Year,
        Self::Approximate,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
            Self::Approximate => "approximate",
        }
    }
}

impl fmt::Display for TemporalGranularity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TemporalGranularity {
    type Err = MemcoreError;

    fn from_str(value: &str) -> MemcoreResult<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == normalized)
            .ok_or_else(|| {
                MemcoreError::ValidationError(format!("invalid temporal granularity: {value}"))
            })
    }
}

/// A resolved event time: the span it covers, how precise it is, and the time it was resolved
/// against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalValue {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub granularity: TemporalGranularity,
    /// The client time the expression was resolved against: the cited message's time, else the
    /// session's. Relative expressions ("yesterday") are resolved against this, never against
    /// wall-clock or receipt time. An absolute date settled without any client time (P2-20)
    /// carries its own `start` here, since nothing was resolved against a message time.
    pub anchor: DateTime<Utc>,
}

impl TemporalValue {
    /// Builds a value, rejecting a span that ends before it starts.
    pub fn new(
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        granularity: TemporalGranularity,
        anchor: DateTime<Utc>,
    ) -> MemcoreResult<Self> {
        if end < start {
            return Err(MemcoreError::ValidationError(format!(
                "event time ends ({end}) before it starts ({start})"
            )));
        }
        Ok(Self {
            start,
            end,
            granularity,
            anchor,
        })
    }

    /// Whether the span contains `instant`.
    pub fn contains(&self, instant: DateTime<Utc>) -> bool {
        self.start <= instant && instant <= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn granularity_labels_round_trip() {
        for granularity in TemporalGranularity::ALL {
            assert_eq!(
                granularity.as_str().parse::<TemporalGranularity>().unwrap(),
                granularity
            );
            let json = serde_json::to_string(&granularity).unwrap();
            assert_eq!(json, format!("\"{}\"", granularity.as_str()));
        }
        assert_eq!(
            " Month ".parse::<TemporalGranularity>().unwrap(),
            TemporalGranularity::Month
        );
        assert!("fortnight".parse::<TemporalGranularity>().is_err());
    }

    #[test]
    fn a_span_that_ends_before_it_starts_is_rejected() {
        let anchor = at(2023, 5, 8);
        assert!(
            TemporalValue::new(
                at(2023, 5, 7),
                at(2023, 5, 6),
                TemporalGranularity::Day,
                anchor
            )
            .is_err()
        );
        let value = TemporalValue::new(
            at(2023, 5, 6),
            at(2023, 5, 6),
            TemporalGranularity::Day,
            anchor,
        )
        .unwrap();
        assert!(value.contains(at(2023, 5, 6)));
        assert!(!value.contains(at(2023, 5, 7)));
    }
}
