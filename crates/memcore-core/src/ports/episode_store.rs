use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use uuid::Uuid;

use crate::{Episode, TenantContext};

/// Storage for episodes: source-message-level memory. Every method is scoped to
/// one tenant user except the organization count. Deletes are hard deletes.
#[async_trait]
pub trait EpisodeStore: Send + Sync {
    /// Stores `episodes` in order and returns, for each, the stored row. An episode whose client
    /// `message_id` is already stored for the same organization, user and session (a missing session
    /// counts as one session) is not stored again: the existing row is returned in its place.
    /// Episodes without a `message_id` are always stored.
    async fn insert_episodes(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>>;

    /// [`Self::insert_episodes`], but atomic for the whole batch
    /// and strict about client message IDs. An episode whose `message_id` is already stored for the
    /// same organization, user and session must be the same message occurrence
    /// ([`Episode::same_occurrence`]); then the existing row is returned (retry / resume).
    /// Otherwise NOTHING of the batch is stored and `MemcoreError::message_id_conflict(position)` is
    /// returned. The check and the insert are one transaction, so two concurrent batches reusing
    /// the same new ID for different messages cannot both succeed.
    async fn insert_episodes_checked(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>>;

    /// The tenant's episodes with these IDs, in no particular order; unknown IDs are skipped.
    async fn get_episodes(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<Vec<Episode>>;

    /// Marks the tenant's episodes with these IDs as embedded.
    async fn mark_episodes_embedded(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<()>;

    /// The tenant's episodes, oldest `recorded_at` first (ties by ID), at most `limit`.
    async fn list_episodes(
        &self,
        tenant: &TenantContext,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>>;

    /// The tenant's episodes strictly after `after` = (recorded_at, id) in
    /// (recorded_at, id) ascending order, at most `limit` (keyset pagination for complete exports).
    async fn list_episodes_after(
        &self,
        tenant: &TenantContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>>;

    /// Deletes all of the tenant's episodes; returns how many were deleted.
    async fn delete_user_episodes(&self, tenant: &TenantContext) -> MemcoreResult<usize>;

    /// IDs of the tenant's episodes with `recorded_at` before `cutoff`, deleted unless `dry_run`.
    async fn delete_episodes_recorded_before(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<Vec<Uuid>>;

    /// Number of episodes in the organization.
    async fn count_episodes_by_org(&self, org_id: &str) -> MemcoreResult<usize>;

    /// Maintenance only (never reachable from an API route): whether ANY
    /// tenant has an episode row with this id (episode ids are global keys). Existence only.
    async fn episode_id_exists_in_any_tenant(&self, episode_id: Uuid) -> MemcoreResult<bool>;
}
