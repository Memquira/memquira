//! The `EpisodeStore` contract, run against every backend.

use chrono::{Duration, TimeZone, Utc};
use memcore_common::MemcoreError;
use memcore_core::ports::EpisodeStore;
use memcore_core::{Episode, MessageRole, TenantContext};
use serde_json::json;
use uuid::Uuid;

pub const ORG: &str = "org_episode_store";
pub const USER: &str = "user_episode_store";

pub fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

pub fn episode(
    tenant: &TenantContext,
    session: Option<&str>,
    message: Option<&str>,
    content: &str,
) -> Episode {
    Episode {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        session_id: session.map(str::to_string),
        message_id: message.map(str::to_string),
        speaker: Some("Priya".to_string()),
        role: MessageRole::Assistant,
        content: content.to_string(),
        observed_at: Some(Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap()),
        recorded_at: Utc.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap(),
        embedded: false,
        metadata: json!({ "session_id": "S1" }),
    }
}

/// Every field survives a round trip.
pub async fn round_trip(store: &dyn EpisodeStore) {
    let mut original = episode(
        &tenant(),
        Some("S1"),
        Some("D1:1"),
        "I adopted a greyhound.",
    );
    original.observed_at = None;
    let stored = store
        .insert_episodes(&tenant(), vec![original.clone()])
        .await
        .unwrap();
    assert_eq!(stored, vec![original.clone()]);
    let read = store.get_episodes(&tenant(), &[original.id]).await.unwrap();
    assert_eq!(read, vec![original]);
}

/// A client message ID is stored once per organization, user and session; other scopes and rows
/// without a message ID never collide.
pub async fn idempotency_and_scopes(store: &dyn EpisodeStore) {
    let me = tenant();
    let other_user = TenantContext::new(ORG, "someone_else").unwrap();
    let other_org = TenantContext::new("another_org", USER).unwrap();

    let first = episode(&me, Some("S1"), Some("D1:1"), "First copy.");
    let stored = store
        .insert_episodes(&me, vec![first.clone()])
        .await
        .unwrap();
    assert_eq!(stored[0].id, first.id);

    // The same message again (new random ID): the existing row comes back, nothing is added.
    let retry = episode(&me, Some("S1"), Some("D1:1"), "Second copy.");
    let stored = store.insert_episodes(&me, vec![retry]).await.unwrap();
    assert_eq!(stored[0].id, first.id);
    assert_eq!(stored[0].content, "First copy.");

    // No session counts as one session: repeated there too.
    let a = episode(&me, None, Some("D1:1"), "Sessionless.");
    let b = episode(&me, None, Some("D1:1"), "Sessionless again.");
    store.insert_episodes(&me, vec![a.clone()]).await.unwrap();
    let stored = store.insert_episodes(&me, vec![b]).await.unwrap();
    assert_eq!(stored[0].id, a.id);

    // Other session, user or organization: separate rows.
    store
        .insert_episodes(
            &me,
            vec![episode(&me, Some("S2"), Some("D1:1"), "Other session.")],
        )
        .await
        .unwrap();
    store
        .insert_episodes(
            &other_user,
            vec![episode(
                &other_user,
                Some("S1"),
                Some("D1:1"),
                "Other user.",
            )],
        )
        .await
        .unwrap();
    store
        .insert_episodes(
            &other_org,
            vec![episode(&other_org, Some("S1"), Some("D1:1"), "Other org.")],
        )
        .await
        .unwrap();

    // Without a message ID, every message is stored.
    store
        .insert_episodes(
            &me,
            vec![
                episode(&me, Some("S1"), None, "No id."),
                episode(&me, Some("S1"), None, "No id."),
            ],
        )
        .await
        .unwrap();

    assert_eq!(store.list_episodes(&me, 100).await.unwrap().len(), 5);
    assert_eq!(
        store.list_episodes(&other_user, 100).await.unwrap().len(),
        1
    );
    assert_eq!(store.list_episodes(&other_org, 100).await.unwrap().len(), 1);
    assert_eq!(store.count_episodes_by_org(ORG).await.unwrap(), 6);

    // A tenant cannot write rows for another tenant.
    assert!(
        store
            .insert_episodes(&me, vec![episode(&other_user, None, None, "Forged.")])
            .await
            .is_err()
    );
    // Nor read them.
    let theirs = store.list_episodes(&other_user, 10).await.unwrap();
    assert!(
        store
            .get_episodes(&me, &[theirs[0].id])
            .await
            .unwrap()
            .is_empty()
    );
}

/// `embedded`, listing order, deletion and retention.
pub async fn lifecycle(store: &dyn EpisodeStore) {
    let me = tenant();
    let other = TenantContext::new(ORG, "someone_else").unwrap();
    let mut old = episode(&me, Some("S0"), Some("D0:1"), "Old message.");
    old.recorded_at = Utc::now() - Duration::days(400);
    let mut new = episode(&me, Some("S1"), Some("D1:1"), "New message.");
    new.recorded_at = Utc::now();
    let mut theirs = episode(&other, Some("S0"), Some("D0:1"), "Their old message.");
    theirs.recorded_at = Utc::now() - Duration::days(400);
    store
        .insert_episodes(&me, vec![new.clone(), old.clone()])
        .await
        .unwrap();
    store.insert_episodes(&other, vec![theirs]).await.unwrap();

    store.mark_episodes_embedded(&me, &[new.id]).await.unwrap();
    let listed = store.list_episodes(&me, 10).await.unwrap();
    assert_eq!(
        listed.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![old.id, new.id],
        "oldest recorded_at first"
    );
    assert!(listed[1].embedded && !listed[0].embedded);
    assert_eq!(store.list_episodes(&me, 1).await.unwrap().len(), 1);

    let cutoff = Utc::now() - Duration::days(365);
    let dry = store
        .delete_episodes_recorded_before(&me, cutoff, true)
        .await
        .unwrap();
    assert_eq!(dry, vec![old.id]);
    assert_eq!(store.list_episodes(&me, 10).await.unwrap().len(), 2);
    let deleted = store
        .delete_episodes_recorded_before(&me, cutoff, false)
        .await
        .unwrap();
    assert_eq!(deleted, vec![old.id]);
    assert_eq!(store.list_episodes(&me, 10).await.unwrap().len(), 1);
    assert_eq!(store.list_episodes(&other, 10).await.unwrap().len(), 1);

    assert_eq!(store.delete_user_episodes(&me).await.unwrap(), 1);
    assert!(store.list_episodes(&me, 10).await.unwrap().is_empty());
    assert_eq!(store.list_episodes(&other, 10).await.unwrap().len(), 1);
}

// ------------------------------------------------------------------------------------------------
// The checked insert used by ingestion.

fn count_with(rows: &[Episode], message: &str) -> usize {
    rows.iter()
        .filter(|row| row.message_id.as_deref() == Some(message))
        .count()
}

/// The same occurrence resumes (existing row); a different message under a reused ID is a
/// conflict, judged on every identity-bearing field; the stored message is never changed.
pub async fn checked_conflicts(store: &dyn EpisodeStore) {
    let me = tenant();
    let first = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    let stored = store
        .insert_episodes_checked(&me, vec![first.clone()])
        .await
        .unwrap();
    assert_eq!(stored, vec![first.clone()]);

    // Same occurrence: new episode ID, new receipt time, other request metadata, a message time
    // that differs only below the microsecond. The existing row comes back.
    let mut retry = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    retry.recorded_at = Utc::now();
    retry.metadata = json!({ "attempt": 2 });
    retry.observed_at = first.observed_at.map(|t| t + Duration::nanoseconds(400));
    let stored = store
        .insert_episodes_checked(&me, vec![retry])
        .await
        .unwrap();
    assert_eq!(stored[0].id, first.id);

    let mut changed_role = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    changed_role.role = MessageRole::User;
    let mut changed_speaker = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    changed_speaker.speaker = Some("Rowena".to_string());
    let mut no_speaker = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    no_speaker.speaker = None;
    let mut changed_time = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    changed_time.observed_at = first.observed_at.map(|t| t + Duration::minutes(1));
    let mut no_time = episode(&me, Some("S9"), Some("C1:1"), "I keep three beehives.");
    no_time.observed_at = None;
    let cases = [
        (
            "content",
            episode(&me, Some("S9"), Some("C1:1"), "I sold my car."),
        ),
        ("role", changed_role),
        ("speaker", changed_speaker),
        ("speaker removed", no_speaker),
        ("message time", changed_time),
        ("message time removed", no_time),
    ];
    for (case, candidate) in cases {
        let error = store
            .insert_episodes_checked(&me, vec![candidate])
            .await
            .unwrap_err();
        assert_eq!(error, MemcoreError::message_id_conflict(0), "{case}");
        let message = error.to_string();
        assert!(
            !message.contains("C1:1") && !message.contains("beehives") && !message.contains("car")
        );
    }

    let rows = store.list_episodes(&me, 100).await.unwrap();
    assert_eq!(count_with(&rows, "C1:1"), 1);
    assert_eq!(
        store.get_episodes(&me, &[first.id]).await.unwrap(),
        vec![first]
    );
}

/// A batch with one conflicting message stores nothing at all, not even its valid new messages.
pub async fn checked_batch_is_atomic(store: &dyn EpisodeStore) {
    let me = tenant();
    let first = episode(&me, Some("S8"), Some("B1:1"), "Original message.");
    store
        .insert_episodes_checked(&me, vec![first.clone()])
        .await
        .unwrap();
    let before = store.list_episodes(&me, 100).await.unwrap().len();

    let new_one = episode(
        &me,
        Some("S8"),
        Some("B1:2"),
        "A new message in the same request.",
    );
    let no_id = episode(&me, Some("S8"), None, "A message without an ID.");
    let resumed = episode(&me, Some("S8"), Some("B1:1"), "Original message.");
    let conflicting = episode(
        &me,
        Some("S8"),
        Some("B1:1"),
        "Different content, reused ID.",
    );
    let error = store
        .insert_episodes_checked(
            &me,
            vec![new_one.clone(), no_id.clone(), resumed, conflicting],
        )
        .await
        .unwrap_err();
    assert_eq!(error, MemcoreError::message_id_conflict(3));
    assert_eq!(store.list_episodes(&me, 100).await.unwrap().len(), before);
    assert!(
        store
            .get_episodes(&me, &[new_one.id, no_id.id])
            .await
            .unwrap()
            .is_empty()
    );

    // The same batch without the conflicting message is stored as usual.
    let stored = store
        .insert_episodes_checked(&me, vec![new_one.clone(), no_id.clone()])
        .await
        .unwrap();
    assert_eq!(stored, vec![new_one, no_id]);
}

/// Scopes: no session is one scope; other sessions, users and organizations never conflict;
/// distinct IDs with identical text and messages without an ID are separate rows.
pub async fn checked_scopes(store: &dyn EpisodeStore) {
    let me = tenant();
    let other_user = TenantContext::new(ORG, "checked_other_user").unwrap();
    let other_org = TenantContext::new("checked_other_org", USER).unwrap();

    let a = episode(&me, None, Some("N1:1"), "Sessionless message.");
    store
        .insert_episodes_checked(&me, vec![a.clone()])
        .await
        .unwrap();
    let again = episode(&me, None, Some("N1:1"), "Sessionless message.");
    assert_eq!(
        store
            .insert_episodes_checked(&me, vec![again])
            .await
            .unwrap()[0]
            .id,
        a.id
    );
    let changed = episode(&me, None, Some("N1:1"), "Another sessionless message.");
    assert!(
        store
            .insert_episodes_checked(&me, vec![changed])
            .await
            .unwrap_err()
            .is_message_id_conflict()
    );

    for (scope, row) in [
        (
            "other session",
            episode(&me, Some("S2"), Some("N1:1"), "Other session."),
        ),
        (
            "other user",
            episode(&other_user, None, Some("N1:1"), "Other user."),
        ),
        (
            "other org",
            episode(&other_org, None, Some("N1:1"), "Other org."),
        ),
    ] {
        let tenant = TenantContext::new(&row.org_id, &row.user_id).unwrap();
        let stored = store
            .insert_episodes_checked(&tenant, vec![row.clone()])
            .await
            .unwrap();
        assert_eq!(stored[0].id, row.id, "{scope}");
    }

    let first = episode(&me, Some("S3"), Some("D1:1"), "ok");
    let second = episode(&me, Some("S3"), Some("D1:2"), "ok");
    let stored = store
        .insert_episodes_checked(&me, vec![first.clone(), second.clone()])
        .await
        .unwrap();
    assert_eq!(stored, vec![first, second]);
    let no_id = || episode(&me, Some("S3"), None, "ok");
    let stored = store
        .insert_episodes_checked(&me, vec![no_id(), no_id()])
        .await
        .unwrap();
    assert_ne!(stored[0].id, stored[1].id);

    // A forged row for another tenant is refused.
    assert!(
        store
            .insert_episodes_checked(&me, vec![episode(&other_user, None, None, "Forged.")])
            .await
            .is_err()
    );
}

/// Concurrent batches reusing the same NEW message ID for different messages: exactly one wins;
/// for the same message, every batch gets the one stored row.
pub async fn checked_concurrent_reuse(store: &dyn EpisodeStore) {
    let me = tenant();
    let make = |content: &str| episode(&me, Some("S7"), Some("R1:1"), content);
    let (a, b, c, d) = tokio::join!(
        store.insert_episodes_checked(&me, vec![make("first variant")]),
        store.insert_episodes_checked(&me, vec![make("second variant")]),
        store.insert_episodes_checked(&me, vec![make("third variant")]),
        store.insert_episodes_checked(&me, vec![make("fourth variant")]),
    );
    let results = [a, b, c, d];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(MemcoreError::is_message_id_conflict)
    );
    let rows = store.list_episodes(&me, 100).await.unwrap();
    assert_eq!(count_with(&rows, "R1:1"), 1);

    let same = || episode(&me, Some("S6"), Some("R2:1"), "the same message");
    let (a, b, c, d) = tokio::join!(
        store.insert_episodes_checked(&me, vec![same()]),
        store.insert_episodes_checked(&me, vec![same()]),
        store.insert_episodes_checked(&me, vec![same()]),
        store.insert_episodes_checked(&me, vec![same()]),
    );
    let ids: Vec<Uuid> = [a, b, c, d].into_iter().map(|r| r.unwrap()[0].id).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
    let rows = store.list_episodes(&me, 100).await.unwrap();
    assert_eq!(count_with(&rows, "R2:1"), 1);
}
