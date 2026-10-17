//! Storage layer for the token-usage dashboard.
//!
//! Two responsibilities, kept apart:
//!   * **write** — [`replace_conversation_facts`] swaps one conversation's fact
//!     rows for a freshly parsed set inside a transaction, and stamps its sync
//!     row. Nothing else writes these tables.
//!   * **read** — [`fetch_facts`] returns the filtered fact rows the aggregator
//!     folds in memory (see `commands::token_usage`), and the small helpers
//!     below answer the facet / status questions with GROUP BY aggregates.
//!
//! Every read joins `conversation` rather than reading a denormalized copy, so
//! a folder move, an agent change or a soft delete shows up without a re-sync.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult, JoinType, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set, TransactionTrait,
};

use crate::db::entities::{conversation, folder, token_usage_sync, token_usage_turn};
use crate::db::error::DbError;

/// Rows per `INSERT` batch. Eleven columns each, so 200 rows is ~2 200 bound
/// parameters — comfortably under SQLite's per-statement variable limit on
/// every version we support.
const INSERT_CHUNK: usize = 200;

/// One turn's usage, ready to persist. The service assigns nothing itself —
/// the caller (which owns the parser) fills every field.
#[derive(Debug, Clone)]
pub struct UsageFact {
    pub turn_key: String,
    pub occurred_at: DateTime<Utc>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub duration_ms: i64,
}

impl UsageFact {
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// A fact joined to the conversation dimensions the dashboard filters on.
#[derive(Debug, Clone, FromQueryResult)]
pub struct UsageFactRow {
    pub conversation_id: i32,
    pub folder_id: i32,
    pub agent_type: String,
    pub model: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub total_tokens: i64,
    pub duration_ms: i64,
}

/// A conversation the sync pass may need to (re-)parse.
#[derive(Debug, Clone, FromQueryResult)]
pub struct SyncCandidate {
    pub id: i32,
    pub title: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// `conversation.updated_at` as of the last successful parse, or `None`
    /// when this conversation has never been parsed.
    pub synced_source_updated_at: Option<DateTime<Utc>>,
    /// Fact rows the last parse wrote. Lets the sync tell "this conversation
    /// genuinely has no usage" from "we used to read usage here and suddenly
    /// can't" — the second case must not overwrite real history with nothing.
    pub synced_turn_count: Option<i32>,
}

/// Written into `source_updated_at` by [`mark_stale_for_reparse`] to force the
/// next pass to re-read a conversation.
///
/// A sentinel rather than deleting the stamp, because the stamp carries
/// `turn_count` — the only thing that lets the sync tell "this conversation has
/// no usage" from "we recorded usage here and suddenly can't read it". Dropping
/// the row would take that knowledge with it and let a momentarily unreachable
/// transcript erase real history.
///
/// The epoch cannot collide with a real conversation in practice (rows are
/// stamped with `Utc::now()` or a parsed session timestamp), and if one somehow
/// did, that conversation would merely re-parse on every pass — a cost, never a
/// loss. [`SyncCandidate::is_stale`] checks for it explicitly rather than
/// relying on inequality alone.
pub fn force_reparse_marker() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

impl SyncCandidate {
    /// Stale ⇔ never parsed, explicitly marked for re-parse, or the row moved
    /// since the last parse. Exact inequality (not `>`): a parse that lands in
    /// the same millisecond as a turn must not be mistaken for up to date, and
    /// a row rewritten backwards by a restore must re-parse.
    pub fn is_stale(&self) -> bool {
        match self.synced_source_updated_at {
            None => true,
            Some(seen) => seen == force_reparse_marker() || seen != self.updated_at,
        }
    }
}

/// Which conversations a filtered read is allowed to see. Constructed by the
/// command layer (which expands folder ids to their worktree children);
/// `None` on a field means "no constraint on this dimension".
#[derive(Debug, Clone, Default)]
pub struct FactQuery {
    pub start: Option<DateTime<Utc>>,
    /// Exclusive.
    pub end: Option<DateTime<Utc>>,
    pub folder_ids: Option<Vec<i32>>,
    pub agent_types: Option<Vec<String>>,
    pub models: Option<Vec<String>>,
}

impl FactQuery {
    /// True when a dimension was narrowed to the empty set — which can only
    /// match nothing, so the caller can skip the query entirely instead of
    /// emitting an `IN ()`.
    pub fn is_empty_selection(&self) -> bool {
        self.folder_ids.as_ref().is_some_and(|v| v.is_empty())
            || self.agent_types.as_ref().is_some_and(|v| v.is_empty())
            || self.models.as_ref().is_some_and(|v| v.is_empty())
    }
}

fn base_select() -> sea_orm::Select<token_usage_turn::Entity> {
    token_usage_turn::Entity::find()
        .join(
            JoinType::InnerJoin,
            token_usage_turn::Relation::Conversation.def(),
        )
        .filter(conversation::Column::DeletedAt.is_null())
}

fn apply_query(
    select: sea_orm::Select<token_usage_turn::Entity>,
    q: &FactQuery,
) -> sea_orm::Select<token_usage_turn::Entity> {
    let mut select = select;
    if let Some(start) = q.start {
        select = select.filter(token_usage_turn::Column::OccurredAt.gte(start));
    }
    if let Some(end) = q.end {
        select = select.filter(token_usage_turn::Column::OccurredAt.lt(end));
    }
    if let Some(ref ids) = q.folder_ids {
        select = select.filter(conversation::Column::FolderId.is_in(ids.clone()));
    }
    if let Some(ref agents) = q.agent_types {
        select = select.filter(conversation::Column::AgentType.is_in(agents.clone()));
    }
    if let Some(ref models) = q.models {
        select = select.filter(token_usage_turn::Column::Model.is_in(models.clone()));
    }
    select
}

/// Fetch the filtered facts, newest first, capped at `limit`.
///
/// Newest-first matters: when a pathological history exceeds the cap, the rows
/// that survive are the recent ones the dashboard is mostly about, and the
/// caller reports the result as truncated rather than silently mixing a
/// half-scanned tail into the totals.
pub async fn fetch_facts(
    conn: &DatabaseConnection,
    q: &FactQuery,
    limit: u64,
) -> Result<Vec<UsageFactRow>, DbError> {
    if q.is_empty_selection() {
        return Ok(Vec::new());
    }
    let rows = apply_query(base_select(), q)
        .select_only()
        .column(token_usage_turn::Column::ConversationId)
        .column_as(conversation::Column::FolderId, "folder_id")
        .column_as(conversation::Column::AgentType, "agent_type")
        .column(token_usage_turn::Column::Model)
        .column(token_usage_turn::Column::OccurredAt)
        .column(token_usage_turn::Column::InputTokens)
        .column(token_usage_turn::Column::OutputTokens)
        .column(token_usage_turn::Column::CacheCreationTokens)
        .column(token_usage_turn::Column::CacheReadTokens)
        .column(token_usage_turn::Column::TotalTokens)
        .column(token_usage_turn::Column::DurationMs)
        .order_by_desc(token_usage_turn::Column::OccurredAt)
        .limit(limit)
        .into_model::<UsageFactRow>()
        .all(conn)
        .await?;
    Ok(rows)
}

/// Count the sessions the workspace list would show for this window — the
/// number the status bar's session counter also reports when the window is
/// unbounded.
///
/// The fold's own distinct-id count tells a different story: it includes
/// delegation children the sidebar hides, and it misses every session that
/// never recorded usage — empty sessions, agents that store no token counts,
/// transcripts the agent's own retention already deleted. Side by side those
/// two stories read as a bug ("1 050 sessions" in the status bar, "829" on the
/// dashboard), so the headline uses this count instead.
///
/// Two disjoint groups sum to the answer, both restricted to the workspace
/// list's visibility rules (live row, not a loop run, top level, live folder):
///
///  * sessions with at least one usage fact inside the window, and
///  * sessions with **no facts at all** whose own activity stamp falls inside
///    it. "No facts at all" (not "no facts in range") keeps the groups
///    disjoint — a session whose usage lies outside the window is genuinely
///    absent from that window, not half-present.
///
/// A model filter disables the factless group: a session with no usage rows
/// cannot name a model.
pub async fn workspace_conversation_count(
    conn: &DatabaseConnection,
    q: &FactQuery,
) -> Result<u64, DbError> {
    use sea_orm::sea_query::{Expr, Query};

    if q.is_empty_selection() {
        return Ok(0);
    }

    // Live folders up front, intersected with the filter — same shape as
    // `conversation_service::list_all`, and it spares both branches a join.
    let live_folders: Vec<i32> = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .select_only()
        .column(folder::Column::Id)
        .into_tuple()
        .all(conn)
        .await?;
    let allowed: Vec<i32> = match &q.folder_ids {
        Some(ids) => ids
            .iter()
            .copied()
            .filter(|id| live_folders.contains(id))
            .collect(),
        None => live_folders,
    };
    if allowed.is_empty() {
        return Ok(0);
    }

    let mut with_usage = token_usage_turn::Entity::find()
        .join(
            JoinType::InnerJoin,
            token_usage_turn::Relation::Conversation.def(),
        )
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::Kind.ne(conversation::ConversationKind::Loop))
        .filter(conversation::Column::ParentId.is_null())
        .filter(conversation::Column::FolderId.is_in(allowed.clone()));
    if let Some(start) = q.start {
        with_usage = with_usage.filter(token_usage_turn::Column::OccurredAt.gte(start));
    }
    if let Some(end) = q.end {
        with_usage = with_usage.filter(token_usage_turn::Column::OccurredAt.lt(end));
    }
    if let Some(ref agents) = q.agent_types {
        with_usage = with_usage.filter(conversation::Column::AgentType.is_in(agents.clone()));
    }
    if let Some(ref models) = q.models {
        with_usage = with_usage.filter(token_usage_turn::Column::Model.is_in(models.clone()));
    }
    let usage_count = with_usage
        .select_only()
        .column(token_usage_turn::Column::ConversationId)
        .distinct()
        .into_tuple::<i32>()
        .all(conn)
        .await?
        .len() as u64;

    if q.models.is_some() {
        return Ok(usage_count);
    }

    let mut factless = conversation::Entity::find()
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::Kind.ne(conversation::ConversationKind::Loop))
        .filter(conversation::Column::ParentId.is_null())
        .filter(conversation::Column::FolderId.is_in(allowed))
        .filter(
            Expr::col((conversation::Entity, conversation::Column::Id)).not_in_subquery(
                Query::select()
                    .column(token_usage_turn::Column::ConversationId)
                    .from(token_usage_turn::Entity)
                    .to_owned(),
            ),
        );
    // `updated_at` is the only activity signal a factless session has; for the
    // unbounded window (the case the status bar is compared against) the
    // predicate vanishes entirely.
    if let Some(start) = q.start {
        factless = factless.filter(conversation::Column::UpdatedAt.gte(start));
    }
    if let Some(end) = q.end {
        factless = factless.filter(conversation::Column::UpdatedAt.lt(end));
    }
    if let Some(ref agents) = q.agent_types {
        factless = factless.filter(conversation::Column::AgentType.is_in(agents.clone()));
    }
    let factless_count = factless.count(conn).await?;

    Ok(usage_count + factless_count)
}

/// Replace every fact row of one conversation and stamp its sync state.
///
/// Delete-then-insert (rather than a diff) is deliberate: a parser upgrade can
/// legitimately regroup turns, so the previous rows are not a reliable base to
/// patch. Both steps plus the sync stamp run in one transaction, so a crash
/// mid-write can never leave a conversation counted twice or counted as synced
/// while its rows are missing.
pub async fn replace_conversation_facts(
    conn: &DatabaseConnection,
    conversation_id: i32,
    source_updated_at: DateTime<Utc>,
    facts: &[UsageFact],
) -> Result<(u32, i64), DbError> {
    let now = Utc::now();
    let total_tokens: i64 = facts.iter().map(|f| f.total_tokens()).sum();
    let turn_count = facts.len() as u32;

    let txn = conn.begin().await?;

    token_usage_turn::Entity::delete_many()
        .filter(token_usage_turn::Column::ConversationId.eq(conversation_id))
        .exec(&txn)
        .await?;

    for chunk in facts.chunks(INSERT_CHUNK) {
        let models: Vec<token_usage_turn::ActiveModel> = chunk
            .iter()
            .map(|f| token_usage_turn::ActiveModel {
                id: sea_orm::ActiveValue::NotSet,
                conversation_id: Set(conversation_id),
                turn_key: Set(f.turn_key.clone()),
                occurred_at: Set(f.occurred_at),
                model: Set(f.model.clone()),
                input_tokens: Set(f.input_tokens),
                output_tokens: Set(f.output_tokens),
                cache_creation_tokens: Set(f.cache_creation_tokens),
                cache_read_tokens: Set(f.cache_read_tokens),
                total_tokens: Set(f.total_tokens()),
                duration_ms: Set(f.duration_ms),
            })
            .collect();
        token_usage_turn::Entity::insert_many(models)
            .exec(&txn)
            .await?;
    }

    let stamp = token_usage_sync::ActiveModel {
        conversation_id: Set(conversation_id),
        source_updated_at: Set(source_updated_at),
        turn_count: Set(turn_count as i32),
        total_tokens: Set(total_tokens),
        synced_at: Set(now),
    };
    token_usage_sync::Entity::insert(stamp)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(token_usage_sync::Column::ConversationId)
                .update_columns([
                    token_usage_sync::Column::SourceUpdatedAt,
                    token_usage_sync::Column::TurnCount,
                    token_usage_sync::Column::TotalTokens,
                    token_usage_sync::Column::SyncedAt,
                ])
                .to_owned(),
        )
        .exec(&txn)
        .await?;

    txn.commit().await?;
    Ok((turn_count, total_tokens))
}

/// Every live conversation, with its last-parse stamp attached.
///
/// Delegation children are included: their tokens are real spend, and folding
/// them into the parent would need a parser-level merge that does not exist.
/// They carry their own `folder_id`, so folder filtering still places them.
pub async fn list_sync_candidates(
    conn: &DatabaseConnection,
) -> Result<Vec<SyncCandidate>, DbError> {
    let rows = conversation::Entity::find()
        .select_only()
        .column(conversation::Column::Id)
        .column(conversation::Column::Title)
        .column(conversation::Column::UpdatedAt)
        .column_as(
            token_usage_sync::Column::SourceUpdatedAt,
            "synced_source_updated_at",
        )
        .column_as(token_usage_sync::Column::TurnCount, "synced_turn_count")
        .join_rev(
            JoinType::LeftJoin,
            token_usage_sync::Relation::Conversation.def(),
        )
        .filter(conversation::Column::DeletedAt.is_null())
        .order_by_desc(conversation::Column::UpdatedAt)
        .into_model::<SyncCandidate>()
        .all(conn)
        .await?;
    Ok(rows)
}

/// Drop facts and stamps whose conversation row is gone for good (hard delete,
/// or a restore that rolled the DB back past an import).
///
/// Soft-deleted conversations are left alone on purpose: they are already
/// invisible to every read (the join filters `deleted_at IS NULL`), and keeping
/// their rows means a future restore path would not silently lose history.
/// Returns how many conversations were pruned.
pub async fn prune_orphaned_facts(conn: &DatabaseConnection) -> Result<u32, DbError> {
    let live: Vec<i32> = conversation::Entity::find()
        .select_only()
        .column(conversation::Column::Id)
        .into_tuple()
        .all(conn)
        .await?;

    let recorded: Vec<i32> = token_usage_sync::Entity::find()
        .select_only()
        .column(token_usage_sync::Column::ConversationId)
        .into_tuple()
        .all(conn)
        .await?;

    let live_set: std::collections::HashSet<i32> = live.into_iter().collect();
    let orphans: Vec<i32> = recorded
        .into_iter()
        .filter(|id| !live_set.contains(id))
        .collect();
    if orphans.is_empty() {
        return Ok(0);
    }

    let pruned = orphans.len() as u32;
    // Chunked so a large orphan set cannot blow SQLite's variable limit.
    for chunk in orphans.chunks(INSERT_CHUNK) {
        token_usage_turn::Entity::delete_many()
            .filter(token_usage_turn::Column::ConversationId.is_in(chunk.to_vec()))
            .exec(conn)
            .await?;
        token_usage_sync::Entity::delete_many()
            .filter(token_usage_sync::Column::ConversationId.is_in(chunk.to_vec()))
            .exec(conn)
            .await?;
    }
    Ok(pruned)
}

/// Force the next sync to re-read a conversation, keeping both its facts and
/// everything the stamp knows about them.
///
/// This is the hook the import path uses: re-importing an already-imported
/// session is not guaranteed to bump `conversation.updated_at` — a title
/// refresh deliberately never does (so a re-import can't reorder the
/// recency-sorted sidebar), and an activity refresh only does when the parsed
/// transcript time is strictly newer than what the row already carries. A
/// transcript that grew in the agent's own CLI between imports would otherwise
/// stay invisible to the usage stamp.
///
/// Two things are deliberately preserved rather than deleted:
///   * the **facts**, so the dashboard keeps showing the old numbers until the
///     fresh parse lands instead of blanking in between;
///   * the **stamp row**, so `turn_count` survives — without it a re-import
///     followed by an unreachable transcript would slip past the
///     "an empty parse must not erase recorded usage" guard.
///
/// A conversation that was never parsed has no row and is already stale, so the
/// update simply matches nothing.
pub async fn mark_stale_for_reparse(
    conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    token_usage_sync::Entity::update_many()
        .col_expr(
            token_usage_sync::Column::SourceUpdatedAt,
            Expr::value(force_reparse_marker()),
        )
        .filter(token_usage_sync::Column::ConversationId.eq(conversation_id))
        .exec(conn)
        .await?;
    Ok(())
}

/// Accept a transcript that is gone for good: keep the recorded facts, advance
/// the stamp so no future pass revisits the conversation.
///
/// The counterpart to [`mark_stale_for_reparse`]. A source that reads as empty
/// is indistinguishable from one that will never read again, and this table
/// carries no attempt counter — a first miss and a thousandth look identical.
/// So "keep retrying" is not caution, it is an unbounded loop: every pass
/// re-walks the agent's whole transcript tree, fails the same way, and says so
/// again. Settling ends it.
///
/// Only `source_updated_at` moves. `turn_count`, `total_tokens` and `synced_at`
/// are left exactly as the last successful parse wrote them, because nothing
/// was re-derived — the numbers on screen are still that parse's numbers and
/// should not claim to be newer. The conversation comes back into scope only if
/// something moves its `updated_at` (a re-import, fresh activity) or a full
/// rebuild sweeps it up.
pub async fn settle_lost_source(
    conn: &DatabaseConnection,
    conversation_id: i32,
    source_updated_at: DateTime<Utc>,
) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    token_usage_sync::Entity::update_many()
        .col_expr(
            token_usage_sync::Column::SourceUpdatedAt,
            Expr::value(source_updated_at),
        )
        .filter(token_usage_sync::Column::ConversationId.eq(conversation_id))
        .exec(conn)
        .await?;
    Ok(())
}

// There is deliberately no `clear_all` here. A "rebuild everything" is a
// re-parse of every conversation (see `should_reparse` in
// `commands::token_usage`), not a wipe: swapping each conversation's rows
// inside its own transaction means the dashboard never reads a half-empty
// table, and a conversation whose transcript is momentarily unreachable keeps
// the numbers it already had instead of losing them for good.

#[derive(Debug, Clone, Default)]
pub struct SyncCounts {
    pub total_conversations: u64,
    pub synced_conversations: u64,
    pub stale_conversations: u64,
    pub fact_rows: u64,
    pub last_synced_at: Option<DateTime<Utc>>,
}

pub async fn sync_counts(conn: &DatabaseConnection) -> Result<SyncCounts, DbError> {
    let candidates = list_sync_candidates(conn).await?;
    let total = candidates.len() as u64;
    let stale = candidates.iter().filter(|c| c.is_stale()).count() as u64;
    let synced = candidates
        .iter()
        .filter(|c| c.synced_source_updated_at.is_some())
        .count() as u64;

    let fact_rows = token_usage_turn::Entity::find().count(conn).await?;

    let last_synced_at = token_usage_sync::Entity::find()
        .order_by_desc(token_usage_sync::Column::SyncedAt)
        .one(conn)
        .await?
        .map(|m| m.synced_at);

    Ok(SyncCounts {
        total_conversations: total,
        synced_conversations: synced,
        stale_conversations: stale,
        fact_rows,
        last_synced_at,
    })
}

#[derive(Debug, Clone, FromQueryResult)]
struct FacetFolderRow {
    folder_id: i32,
    tokens: Option<i64>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct FacetAgentRow {
    agent_type: String,
    tokens: Option<i64>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct FacetModelRow {
    model: Option<String>,
    tokens: Option<i64>,
}

/// Folder ids that carry recorded usage, paired with their folder rows.
/// Ordered by recorded tokens descending so the filter list leads with the
/// projects the user actually spends on.
pub async fn facet_folders(
    conn: &DatabaseConnection,
) -> Result<Vec<(folder::Model, i64)>, DbError> {
    let rows = base_select()
        .select_only()
        .column_as(conversation::Column::FolderId, "folder_id")
        .column_as(token_usage_turn::Column::TotalTokens.sum(), "tokens")
        .group_by(conversation::Column::FolderId)
        .into_model::<FacetFolderRow>()
        .all(conn)
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<i32> = rows.iter().map(|r| r.folder_id).collect();
    let folders = folder::Entity::find()
        .filter(folder::Column::Id.is_in(ids))
        .all(conn)
        .await?;
    let by_id: HashMap<i32, folder::Model> = folders.into_iter().map(|f| (f.id, f)).collect();

    let mut out: Vec<(folder::Model, i64)> = rows
        .into_iter()
        .filter_map(|r| by_id.get(&r.folder_id).cloned().map(|f| (f, r.tokens.unwrap_or(0))))
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.1));
    Ok(out)
}

pub async fn facet_agents(conn: &DatabaseConnection) -> Result<Vec<String>, DbError> {
    let rows = base_select()
        .select_only()
        .column_as(conversation::Column::AgentType, "agent_type")
        .column_as(token_usage_turn::Column::TotalTokens.sum(), "tokens")
        .group_by(conversation::Column::AgentType)
        .into_model::<FacetAgentRow>()
        .all(conn)
        .await?;
    let mut rows = rows;
    rows.sort_by_key(|b| std::cmp::Reverse(b.tokens.unwrap_or(0)));
    Ok(rows.into_iter().map(|r| r.agent_type).collect())
}

pub async fn facet_models(conn: &DatabaseConnection) -> Result<Vec<String>, DbError> {
    let rows = base_select()
        .select_only()
        .column(token_usage_turn::Column::Model)
        .column_as(token_usage_turn::Column::TotalTokens.sum(), "tokens")
        .group_by(token_usage_turn::Column::Model)
        .into_model::<FacetModelRow>()
        .all(conn)
        .await?;
    let mut rows = rows;
    rows.sort_by_key(|b| std::cmp::Reverse(b.tokens.unwrap_or(0)));
    Ok(rows.into_iter().filter_map(|r| r.model).collect())
}

/// Earliest and latest recorded turn across all live conversations — the outer
/// bound the "all time" range preset resolves to.
pub async fn facet_extent(
    conn: &DatabaseConnection,
) -> Result<(Option<DateTime<Utc>>, Option<DateTime<Utc>>), DbError> {
    let earliest = base_select()
        .order_by_asc(token_usage_turn::Column::OccurredAt)
        .one(conn)
        .await?
        .map(|m| m.occurred_at);
    let latest = base_select()
        .order_by_desc(token_usage_turn::Column::OccurredAt)
        .one(conn)
        .await?
        .map(|m| m.occurred_at);
    Ok((earliest, latest))
}

/// Resolve display labels for a set of conversation ids (title + folder), used
/// to decorate the "biggest sessions" list without dragging those strings
/// through the fact query.
pub async fn conversation_labels(
    conn: &DatabaseConnection,
    ids: &[i32],
) -> Result<HashMap<i32, (Option<String>, String, Option<String>)>, DbError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let convs = conversation::Entity::find()
        .filter(conversation::Column::Id.is_in(ids.to_vec()))
        .all(conn)
        .await?;
    let folder_ids: Vec<i32> = convs.iter().map(|c| c.folder_id).collect();
    let folders = folder::Entity::find()
        .filter(folder::Column::Id.is_in(folder_ids))
        .all(conn)
        .await?;
    let folder_labels: HashMap<i32, String> = folders
        .into_iter()
        .map(|f| (f.id, folder_display_label(&f)))
        .collect();

    Ok(convs
        .into_iter()
        .map(|c| {
            let folder_label = folder_labels.get(&c.folder_id).cloned();
            (c.id, (c.title, c.agent_type, folder_label))
        })
        .collect())
}

/// The name a folder shows under in the dashboard: its user alias when set,
/// otherwise its path-derived name. Matches the sidebar's precedence.
pub fn folder_display_label(f: &folder::Model) -> String {
    match f.alias.as_ref() {
        Some(alias) if !alias.trim().is_empty() => alias.clone(),
        _ => f.name.clone(),
    }
}

/// Every folder id whose `parent_id` is one of `ids` — the worktree children a
/// project-level filter should also count.
pub async fn expand_folder_ids(
    conn: &DatabaseConnection,
    ids: &[i32],
) -> Result<Vec<i32>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let children: Vec<i32> = folder::Entity::find()
        .select_only()
        .column(folder::Column::Id)
        .filter(folder::Column::ParentId.is_in(ids.to_vec()))
        .into_tuple()
        .all(conn)
        .await?;
    let mut out: Vec<i32> = ids.to_vec();
    out.extend(children);
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests;

