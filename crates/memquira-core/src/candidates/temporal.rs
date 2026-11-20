//! Temporal ranking with the semantic-time correction.
//!
//! Only a fact's source-supported structured `event_time` is event time. Receipt time,
//! `observed_at`, source-message timestamps and episode message time never are: a message's time is
//! not the time of the events it describes. So episodes and facts without an event time are never
//! boosted, never counted by the `when` quota and never moved by ordinal ordering; they keep their
//! E-merge relevance. There is no hard temporal filter.

use std::cmp::Ordering;

use super::Candidate;
use crate::{QueryAnalysis, QuestionType, TemporalOrdinal};

/// `when` questions keep at least this many event-time facts in the window (when that many exist).
pub const WHEN_QUOTA: usize = 5;

/// Applies, in order: the range boost, temporal-ordinal ordering in the window and the `when` quota.
/// `list` is in E-merge order; `beta` is the boost; `window` is the rerank window (60).
pub fn apply_temporal_ranking(
    list: Vec<Candidate>,
    analysis: &QueryAnalysis,
    beta: f64,
    window: usize,
) -> Vec<Candidate> {
    let list = boost(list, analysis, beta);
    let list = order_by_event_time(list, analysis.temporal_ordinal, window);
    if analysis.question_type == QuestionType::When {
        when_quota(list, window, WHEN_QUOTA)
    } else {
        list
    }
}

/// Step 1: facts whose event time overlaps the question's range gain `beta` on their raw score
/// (widened to f64). Stable re-sort, so exact ties keep their prior order.
fn boost(list: Vec<Candidate>, analysis: &QueryAnalysis, beta: f64) -> Vec<Candidate> {
    let Some(constraint) = &analysis.temporal_constraint else {
        return list;
    };
    let mut scored: Vec<(f64, Candidate)> = list
        .into_iter()
        .map(|item| {
            let matches = item.has_event_time()
                && item
                    .event_time
                    .as_ref()
                    .is_some_and(|event| constraint.overlaps(event));
            let score = f64::from(item.vector_score) + if matches { beta } else { 0.0 };
            (score, item)
        })
        .collect();
    scored.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// Step 2: in the window, the slots held by event-time facts are refilled with those facts in time
/// order (`earliest`: start ascending; `latest`: end descending; ties keep their prior order). Every
/// other item keeps its slot; nothing enters or leaves the window.
fn order_by_event_time(
    mut list: Vec<Candidate>,
    ordinal: TemporalOrdinal,
    window: usize,
) -> Vec<Candidate> {
    if ordinal == TemporalOrdinal::None {
        return list;
    }
    let end = window.min(list.len());
    let slots: Vec<usize> = (0..end).filter(|&i| list[i].has_event_time()).collect();
    let mut timed: Vec<Candidate> = slots.iter().map(|&i| list[i].clone()).collect();
    timed.sort_by(|left, right| {
        let (Some(a), Some(b)) = (&left.event_time, &right.event_time) else {
            return Ordering::Equal;
        };
        match ordinal {
            TemporalOrdinal::Earliest => a.start.cmp(&b.start),
            TemporalOrdinal::Latest => b.end.cmp(&a.end),
            TemporalOrdinal::None => Ordering::Equal,
        }
    });
    for (slot, item) in slots.into_iter().zip(timed) {
        list[slot] = item;
    }
    list
}

/// Step 3: if the whole list has at least `quota` event-time facts but the window holds fewer, the
/// highest-ranked ones below the window are promoted to the end of the window, displacing the same
/// number of the window's lowest-ranked items that are not event-time facts; those move to directly
/// after the window. Nothing is manufactured.
fn when_quota(list: Vec<Candidate>, window: usize, quota: usize) -> Vec<Candidate> {
    let end = window.min(list.len());
    let total = list.iter().filter(|item| item.has_event_time()).count();
    let inside = list[..end]
        .iter()
        .filter(|item| item.has_event_time())
        .count();
    if total < quota || inside >= quota {
        return list;
    }
    let need = quota - inside;
    let promoted: Vec<usize> = (end..list.len())
        .filter(|&i| list[i].has_event_time())
        .take(need)
        .collect();
    let displaced: Vec<usize> = {
        let mut from_end: Vec<usize> = (0..end)
            .rev()
            .filter(|&i| !list[i].has_event_time())
            .take(promoted.len())
            .collect();
        from_end.reverse();
        from_end
    };
    let mut order: Vec<usize> = (0..end).filter(|i| !displaced.contains(i)).collect();
    order.extend(&promoted);
    order.extend(&displaced);
    order.extend((end..list.len()).filter(|i| !promoted.contains(i)));
    let mut slots: Vec<Option<Candidate>> = list.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("each index once"))
        .collect()
}

#[cfg(test)]
mod tests {
    //! The frozen fixtures T1–T10, with invented candidates.

    use chrono::{TimeZone, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;
    use crate::candidates::CandidateKind;
    use crate::{TemporalConstraint, TemporalGranularity, TemporalValue, analyze};

    fn at(y: i32, m: u32, d: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn event(start: (i32, u32, u32), end: (i32, u32, u32)) -> TemporalValue {
        TemporalValue::new(
            at(start.0, start.1, start.2),
            at(end.0, end.1, end.2),
            TemporalGranularity::Day,
            at(2023, 5, 1),
        )
        .unwrap()
    }

    fn candidate(
        name: &str,
        kind: CandidateKind,
        score: f32,
        event_time: Option<TemporalValue>,
        observed: Option<(i32, u32, u32)>,
    ) -> Candidate {
        Candidate {
            kind,
            id: Uuid::new_v4(),
            content: name.to_string(),
            vector_score: score,
            rank: 1,
            source_message_ids: Vec::new(),
            message_id: None,
            session_id: None,
            speaker: None,
            metadata: Value::Null,
            event_time,
            observed_at: observed.map(|(y, m, d)| at(y, m, d)),
            query_index: 0,
            rerank_score: None,
        }
    }

    fn fact(name: &str, score: f32, event_time: Option<TemporalValue>) -> Candidate {
        candidate(name, CandidateKind::Fact, score, event_time, None)
    }

    fn episode(name: &str, score: f32, observed: (i32, u32, u32)) -> Candidate {
        candidate(name, CandidateKind::Episode, score, None, Some(observed))
    }

    fn names(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|item| item.content.as_str()).collect()
    }

    fn with_constraint(start: (i32, u32, u32), end: (i32, u32, u32)) -> QueryAnalysis {
        QueryAnalysis {
            temporal_constraint: Some(TemporalConstraint {
                start: Some(at(start.0, start.1, start.2)),
                end: Some(
                    Utc.with_ymd_and_hms(end.0, end.1, end.2, 23, 59, 59)
                        .unwrap(),
                ),
                granularity: TemporalGranularity::Month,
            }),
            ..QueryAnalysis::default()
        }
    }

    #[test]
    fn t1_only_event_time_facts_are_boosted() {
        // F1: event 2021-03-10, stated 2023-05-01. F2: no event time, stated 2021-03-12.
        // E1: an episode sent 2021-03-15. Constraint: March 2021.
        let mut f1 = fact("F1", 0.50, Some(event((2021, 3, 10), (2021, 3, 10))));
        f1.observed_at = Some(at(2023, 5, 1));
        let mut f2 = fact("F2", 0.54, None);
        f2.observed_at = Some(at(2021, 3, 12));
        let e1 = episode("E1", 0.53, (2021, 3, 15));
        let analysis = with_constraint((2021, 3, 1), (2021, 3, 31));
        let ranked = apply_temporal_ranking(vec![f2, e1, f1], &analysis, 0.05, 60);
        // Only F1 gains 0.05 (0.55): it moves above F2 (0.54) and E1 (0.53); the others keep theirs.
        assert_eq!(names(&ranked), ["F1", "F2", "E1"]);
    }

    #[test]
    fn t2_event_time_counts_not_when_it_was_said() {
        let mut f3 = fact("F3", 0.40, Some(event((2019, 6, 1), (2019, 6, 30))));
        f3.observed_at = Some(at(2023, 2, 1));
        let other = fact("other", 0.42, None);
        let analysis = with_constraint((2019, 6, 1), (2019, 6, 30));
        let ranked = apply_temporal_ranking(vec![other, f3], &analysis, 0.05, 60);
        assert_eq!(names(&ranked), ["F3", "other"]);
    }

    #[test]
    fn t3_unknown_event_time_gets_no_boost_and_no_penalty() {
        let f4 = fact("F4", 0.60, None);
        let boosted = fact("boosted", 0.50, Some(event((2021, 7, 1), (2021, 7, 1))));
        let below = fact("below", 0.30, None);
        let analysis = with_constraint((2021, 1, 1), (2021, 12, 31));
        let ranked = apply_temporal_ranking(vec![f4, boosted, below], &analysis, 0.05, 60);
        assert_eq!(names(&ranked), ["F4", "boosted", "below"]);
        assert_eq!(
            ranked[0].vector_score, 0.60,
            "raw scores are never rewritten"
        );
    }

    fn ordinal_window() -> Vec<Candidate> {
        vec![
            fact("Fa", 0.9, Some(event((2022, 1, 1), (2022, 1, 1)))),
            episode("Eb", 0.8, (2019, 1, 1)),
            fact("Fc", 0.7, None),
            fact("Fd", 0.6, Some(event((2020, 1, 1), (2020, 1, 1)))),
        ]
    }

    #[test]
    fn t4_earliest_reorders_only_event_time_fact_slots() {
        let analysis = analyze("When did Rowena first visit Oslo?", None);
        assert_eq!(analysis.temporal_ordinal, TemporalOrdinal::Earliest);
        let ranked = apply_temporal_ranking(ordinal_window(), &analysis, 0.05, 60);
        assert_eq!(names(&ranked), ["Fd", "Eb", "Fc", "Fa"]);
    }

    #[test]
    fn t5_latest_orders_by_end_descending() {
        let analysis = analyze("When did Pellam last repaint his boat?", None);
        assert_eq!(analysis.temporal_ordinal, TemporalOrdinal::Latest);
        let ranked = apply_temporal_ranking(ordinal_window(), &analysis, 0.05, 60);
        // A `when` question: the quota needs 5 event-time facts, only 2 exist, so it does nothing.
        assert_eq!(names(&ranked), ["Fa", "Eb", "Fc", "Fd"]);
    }

    #[test]
    fn t6_t7_generic_first_and_last_do_not_reorder() {
        for question in [
            "What was the first car Dunstan owned?",
            "What is Rowena's last name?",
        ] {
            let analysis = analyze(question, None);
            assert_eq!(
                analysis.temporal_ordinal,
                TemporalOrdinal::None,
                "{question}"
            );
            let ranked = apply_temporal_ranking(ordinal_window(), &analysis, 0.05, 60);
            assert_eq!(names(&ranked), ["Fa", "Eb", "Fc", "Fd"], "{question}");
        }
    }

    fn timed(name: &str, score: f32) -> Candidate {
        fact(name, score, Some(event((2020, 5, 5), (2020, 5, 5))))
    }

    #[test]
    fn t8_when_quota_promotes_and_displaces_deterministically() {
        // Window of 6: T1, n1, T2, n2, n3, n4 | below: n5, T3, T4, T5, T6
        let list = vec![
            timed("T1", 0.99),
            fact("n1", 0.98, None),
            timed("T2", 0.97),
            episode("n2", 0.96, (2020, 5, 5)),
            fact("n3", 0.95, None),
            episode("n4", 0.94, (2020, 5, 5)),
            fact("n5", 0.93, None),
            timed("T3", 0.92),
            timed("T4", 0.91),
            timed("T5", 0.90),
            timed("T6", 0.89),
        ];
        let when = QueryAnalysis {
            question_type: QuestionType::When,
            ..QueryAnalysis::default()
        };
        let ranked = apply_temporal_ranking(list, &when, 0.05, 6);
        assert_eq!(
            names(&ranked),
            [
                "T1", "n1", "T2", "T3", "T4", "T5", "n2", "n3", "n4", "n5", "T6"
            ]
        );
    }

    #[test]
    fn t9_when_quota_never_manufactures() {
        let list = vec![
            timed("T1", 0.99),
            fact("n1", 0.98, None),
            timed("T2", 0.97),
            fact("n2", 0.96, None),
            timed("T3", 0.95),
            timed("T4", 0.94),
        ];
        let when = QueryAnalysis {
            question_type: QuestionType::When,
            ..QueryAnalysis::default()
        };
        let ranked = apply_temporal_ranking(list, &when, 0.05, 3);
        assert_eq!(names(&ranked), ["T1", "n1", "T2", "n2", "T3", "T4"]);
    }

    #[test]
    fn t10_episodes_are_never_promoted_by_the_quota() {
        let mut list = vec![fact("n1", 0.99, None), fact("n2", 0.98, None)];
        list.extend((0..6).map(|i| episode(&format!("E{i}"), 0.5 - i as f32 * 0.01, (2020, 1, 1))));
        let when = QueryAnalysis {
            question_type: QuestionType::When,
            ..QueryAnalysis::default()
        };
        let ranked = apply_temporal_ranking(list.clone(), &when, 0.05, 2);
        assert_eq!(names(&ranked), names(&list));
    }

    #[test]
    fn no_analysis_signal_means_no_change() {
        let list = ordinal_window();
        let ranked = apply_temporal_ranking(list.clone(), &QueryAnalysis::default(), 0.05, 60);
        assert_eq!(ranked, list);
    }

    #[test]
    fn episode_message_time_in_range_is_never_boosted() {
        let e = episode("E", 0.50, (2021, 3, 15));
        let f = fact("F", 0.51, None);
        let analysis = with_constraint((2021, 3, 1), (2021, 3, 31));
        let ranked = apply_temporal_ranking(vec![f, e], &analysis, 0.10, 60);
        assert_eq!(names(&ranked), ["F", "E"]);
    }
}
