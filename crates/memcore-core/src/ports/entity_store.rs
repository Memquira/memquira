use async_trait::async_trait;
use memcore_common::MemcoreResult;
use uuid::Uuid;

use crate::pagination::PageCursor;
use crate::{Entity, EntityRelation, FactEntityLink, TenantContext};

/// Storage for the entity layer (migration 0014). Every method is scoped to one
/// tenant user: entities, links and relations never cross users or organizations. Deletes are hard
/// deletes.
#[async_trait]
pub trait EntityStore: Send + Sync {
    /// Inserts the entity, or replaces the stored one with the same ID for this tenant.
    async fn upsert_entity(&self, tenant: &TenantContext, entity: Entity) -> MemcoreResult<Entity>;

    async fn get_entity(&self, tenant: &TenantContext, id: Uuid) -> MemcoreResult<Option<Entity>>;

    /// Every entity of the tenant (resolution works over one user's entities), oldest first
    /// (`created_at`, then ID).
    async fn list_all_entities(&self, tenant: &TenantContext) -> MemcoreResult<Vec<Entity>>;

    /// A page of the tenant's entities, newest first (`created_at` DESC, then ID DESC), strictly
    /// after `cursor`, at most `limit`.
    async fn list_entities_page(
        &self,
        tenant: &TenantContext,
        limit: usize,
        cursor: Option<&PageCursor>,
    ) -> MemcoreResult<Vec<Entity>>;

    /// Stores the link unless the same (fact, entity, role) is already linked.
    async fn link_fact(&self, tenant: &TenantContext, link: FactEntityLink) -> MemcoreResult<()>;

    /// Removes one (fact, entity, role) link; returns whether it existed.
    async fn unlink_fact(
        &self,
        tenant: &TenantContext,
        link: &FactEntityLink,
    ) -> MemcoreResult<bool>;

    /// The tenant's links to this entity, oldest first.
    async fn links_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<FactEntityLink>>;

    /// The tenant's links from these facts, oldest first.
    async fn links_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<FactEntityLink>>;

    async fn insert_relation(
        &self,
        tenant: &TenantContext,
        relation: EntityRelation,
    ) -> MemcoreResult<EntityRelation>;

    /// The tenant's relations with this entity as source or target, oldest first.
    async fn relations_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<EntityRelation>>;

    /// Deletes all of the tenant's entities, links and relations; returns how many entities were
    /// deleted.
    async fn delete_user_entities(&self, tenant: &TenantContext) -> MemcoreResult<usize>;

    /// Removes the links and relations that come from these facts, then every entity of the tenant
    /// left without any link (fact deletion, retention). Returns the IDs of the deleted entities.
    async fn delete_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<Uuid>>;

    /// Number of entities in the organization.
    async fn count_entities_by_org(&self, org_id: &str) -> MemcoreResult<usize>;
}
