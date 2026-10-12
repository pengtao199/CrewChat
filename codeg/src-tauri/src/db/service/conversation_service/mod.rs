use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::db::entities::conversation::ConversationKind;
use crate::db::entities::{conversation, folder};
use crate::db::error::DbError;
use crate::db::service::conversation_tag_service;
use crate::models::{AgentType, DbConversationSummary};

pub async fn create(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
) -> Result<conversation::Model, DbError> {
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        None,
        ConversationKind::Regular,
        None,
    )
    .await
}

/// Mirror of [`create`] for folderless chat-mode conversations: identical row
/// shape but `kind = 'chat'`, so the sidebar routes the row to its flat "Chat"
/// section. Callers must pair it with the hidden chat folder created in the
/// same flow (`create_chat_conversation_core`).
pub async fn create_chat(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
) -> Result<conversation::Model, DbError> {
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        None,
        ConversationKind::Chat,
        None,
    )
    .await
}

/// Mirror of [`create`] plus optional delegation linkage. Used by the
/// multi-agent broker when spawning a child sub-session — populates
/// `parent_id` / `parent_tool_use_id` / `delegation_call_id` so the lifecycle
/// subscriber and frontend can rebuild the parent ↔ child binding without
/// inspecting the live broker state. `kind` follows the invariant
/// `delegate ⟺ parent_id set`.
pub async fn create_with_delegation(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
) -> Result<conversation::Model, DbError> {
    let kind = if delegation.is_some() {
        ConversationKind::Delegate
    } else {
        ConversationKind::Regular
    };
    // [crewchat] Phase 2.2: a delegation child names the MEMBER ROLE it runs
    // as. It rides the `team` column with no members — `leader_role_id` is
    // exactly "the role this conversation runs as" (the team's leader for a
    // group conversation, the member for a child), and every team consumer
    // treats an empty member list as "nothing to gate, nothing to frame".
    let team_json = delegation
        .as_ref()
        .and_then(|link| link.member_role_id.as_ref())
        .map(|role_id| {
            serde_json::to_string(&crate::models::conversation::ConversationTeam {
                leader_role_id: role_id.clone(),
                member_role_ids: Vec::new(),
            })
            .map_err(|error| DbError::Migration(format!("serialize child role: {error}")))
        })
        .transpose()?;
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        delegation,
        kind,
        team_json,
    )
    .await
}

/// [crewchat] Mirror of [`create`] plus a team snapshot (Phase 1.1) — the
/// new-session dialog's group conversation. The team serializes to the JSON
/// blob stored on the row (`ConversationTeam`): leader + members as roster
/// role ids. Consumers: the delegation broker (member gating), the prompt
/// frame (in-room roster), and the conversation header (member chips).
pub async fn create_with_team(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    team: crate::models::conversation::ConversationTeam,
) -> Result<conversation::Model, DbError> {
    let team_json = serde_json::to_string(&team)
        .map_err(|error| DbError::Migration(format!("serialize team: {error}")))?;
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        None,
        ConversationKind::Regular,
        Some(team_json),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_inner(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
    kind: ConversationKind,
    team: Option<String>,
) -> Result<conversation::Model, DbError> {
    let at_str = serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let now = Utc::now();
    let (parent_id, parent_tool_use_id, delegation_call_id) = match delegation {
        Some(link) => (
            Some(link.parent_conversation_id),
            Some(link.parent_tool_use_id),
            Some(link.delegation_call_id),
        ),
        None => (None, None, None),
    };
    let model = conversation::ActiveModel {
        id: NotSet,
        folder_id: Set(folder_id),
        title: Set(title),
        title_locked: Set(false),
        agent_type: Set(at_str),
        status: Set(conversation::ConversationStatus::InProgress),
        kind: Set(kind),
        model: Set(None),
        git_branch: Set(git_branch),
        external_id: Set(None),
        parent_id: Set(parent_id),
        parent_tool_use_id: Set(parent_tool_use_id),
        delegation_call_id: Set(delegation_call_id),
        message_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        pinned_at: Set(None),
        origin_cwd: Set(None),
        team: Set(team),
    };
    Ok(model.insert(conn).await?)
}

pub async fn update_status(
    conn: &DatabaseConnection,
    conversation_id: i32,
    status: conversation::ConversationStatus,
) -> Result<(), DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let mut active: conversation::ActiveModel = conv.into();
    active.status = Set(status);
    active.updated_at = Set(Utc::now());
    active.update(conn).await?;
    Ok(())
}

/// Conditional status transition (CAS): write `new_status` only if the row's
/// current `status` equals `expected`. Returns `true` when the row was
/// updated. Used by the lifecycle subscriber on disconnect/error so a
/// concurrent user-driven `completed` (or a prior `pending_review` from
/// `TurnComplete`) cannot be silently overwritten.
pub async fn update_status_if(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected: conversation::ConversationStatus,
    new_status: conversation::ConversationStatus,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let result = conversation::Entity::update_many()
        .col_expr(conversation::Column::Status, Expr::value(new_status))
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::Status.eq(expected))
        .exec(conn)
        .await?;
    Ok(result.rows_affected > 0)
}

/// Manual rename: set the title AND lock it. Once locked, the per-turn
/// auto-title backfill ([`refresh_auto_title`]) leaves this row alone, so the
/// user's hand-picked name survives every subsequent session-file parse.
pub async fn update_title(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<(), DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let mut active: conversation::ActiveModel = conv.into();
    active.title = Set(Some(title));
    active.title_locked = Set(true);
    active.updated_at = Set(Utc::now());
    active.update(conn).await?;
    Ok(())
}

/// Auto-derive counterpart to [`update_title`]: write `title` ONLY when the row
/// is not user-locked and the value actually changed. Never sets `title_locked`
/// (the title stays eligible for future auto-refreshes, e.g. when an agent like
/// OpenCode regenerates its own session title) and deliberately does NOT bump
/// `updated_at` — a title backfill is metadata, not user activity, so it must
/// not float the row to the top of a recency-sorted sidebar. Returns `true`
/// when a row was written so the caller can broadcast a sidebar upsert.
///
/// Implemented as a single conditional UPDATE (`... WHERE id = ? AND
/// title_locked = false AND (title IS NULL OR title <> ?)`) so the lock/equality
/// checks and the write are atomic: a manual rename ([`update_title`], which
/// sets `title_locked = true`) that lands between a would-be read and the write
/// can never be clobbered, because the lock predicate is re-evaluated at write
/// time by the database. A non-existent row simply matches nothing (`false`).
pub async fn refresh_auto_title(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let title = title.trim();
    if title.is_empty() {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            sea_orm::Condition::any()
                .add(conversation::Column::Title.is_null())
                .add(conversation::Column::Title.ne(title)),
        )
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// First-prompt seed: write `title` ONLY when the row is unlocked AND still
/// empty. Unlike [`refresh_auto_title`], this will not replace an existing
/// name — a later user prompt must not overwrite the first one, and an
/// agent-generated ACP title that already landed must not be clobbered by
/// the next send. Returns `true` when a row was written so the caller can
/// broadcast a sidebar upsert. Does not bump `updated_at` or set the lock.
pub async fn seed_auto_title_if_empty(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let title = title.trim();
    if title.is_empty() {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            sea_orm::Condition::any()
                .add(conversation::Column::Title.is_null())
                .add(conversation::Column::Title.eq("")),
        )
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Write the session's `model` ONLY when the row still has none. The sibling
/// of [`seed_auto_title_if_empty`], and for the same reason: a conversation
/// row is inserted before the agent has named a model, so the column is NULL
/// for every session started in-app and only the transcript knows the answer.
/// Without this the sidebar — which reads the row, not the transcript — could
/// only show a model for imported sessions.
///
/// First value wins. The detail view re-reads the transcript on every open and
/// stays exact, so the stored value is a cheap projection for the list rather
/// than a second source of truth; re-writing it on every model switch would
/// buy a row write per switch for a chip nobody reads mid-turn.
///
/// Returns `true` when a row was written so the caller can broadcast a sidebar
/// upsert. Does not bump `updated_at` — the sidebar sorts on it, and merely
/// opening a conversation must not float it to the top of Recent (same
/// reasoning as [`update_pin`]).
pub async fn seed_model_if_empty(
    conn: &DatabaseConnection,
    conversation_id: i32,
    model: &str,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let model = model.trim();
    if model.is_empty() {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Model, Expr::value(model))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            sea_orm::Condition::any()
                .add(conversation::Column::Model.is_null())
                .add(conversation::Column::Model.eq("")),
        )
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Lock a row's title WITHOUT rewriting it. For a conversation whose name was
/// typed by the user somewhere else — a work task's title, an automation's name
/// — the seed passed to [`create`] already IS the name; all that's missing is
/// the promise that [`refresh_auto_title`] will keep its hands off it, exactly
/// as it does after a manual rename ([`update_title`]).
///
/// Without this, a task-launched session drifts to whatever the agent's session
/// file parses to — for agents with no title of their own, the first line of the
/// composed prompt — and the board's own name for the work is lost (issue #495).
///
/// Deliberately does NOT touch `title` or bump `updated_at`: like
/// [`refresh_auto_title`], flipping this flag is metadata, not user activity,
/// and must not float the row to the top of a recency-sorted sidebar. A
/// non-existent row simply matches nothing.
///
/// Callers must lock BEFORE broadcasting the row's first sidebar upsert: the
/// conversation id is not knowable to any client until that broadcast, so
/// locking first makes an auto-title backfill on this row impossible rather
/// than merely unlikely.
pub async fn lock_title(conn: &DatabaseConnection, conversation_id: i32) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    conversation::Entity::update_many()
        .col_expr(conversation::Column::TitleLocked, Expr::value(true))
        .filter(conversation::Column::Id.eq(conversation_id))
        .exec(conn)
        .await?;
    Ok(())
}

/// Rename a locked title when its OWNER was renamed: write `new_title` only if
/// the row still carries `expected`. Used when a work task is retitled — the
/// session it produced should follow the card it came from, but only while the
/// two are still in sync.
///
/// The equality guard is the whole point. Both a task-derived title and a hand
/// picked one leave `title_locked = true`, so the flag alone cannot tell them
/// apart; `expected` (the task's PREVIOUS title) can. If the user renamed the
/// conversation themselves, nothing matches and not a byte is written. Same
/// optimistic shape as [`refresh_auto_title`]: one conditional UPDATE, so the
/// comparison happens at write time in the database and a rename landing in
/// between can never be clobbered. Returns `true` when a row was written.
///
/// Leaves `title_locked` alone (already true for every row this can match) and,
/// like its siblings, does not bump `updated_at`.
pub async fn retitle_if_unchanged(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected: &str,
    new_title: &str,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let new_title = new_title.trim();
    if new_title.is_empty() || new_title == expected {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(new_title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::Title.eq(expected))
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Conditionally adopt one title discovered from Codex's session index.
///
/// Every field used to select the candidate is re-checked in the UPDATE, plus
/// the title observed by the candidate read. This prevents a delayed refresh
/// from writing an index title after the row was re-pointed, deleted, manually
/// renamed, moved into a deleted folder, or refreshed by another task.
///
/// `kind` is deliberately absent: it is written once at insert and never
/// updated, so the candidate query's filter cannot go stale between the two.
async fn refresh_codex_auto_title_candidate(
    conn: &DatabaseConnection,
    candidate: &conversation::Model,
    title: &str,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;

    let title = title.trim();
    let Some(external_id) = candidate.external_id.as_deref() else {
        return Ok(false);
    };
    if title.is_empty() || candidate.title.as_deref() == Some(title) {
        return Ok(false);
    }

    let old_title = match candidate.title.as_deref() {
        Some(old_title) => conversation::Column::Title.eq(old_title),
        None => conversation::Column::Title.is_null(),
    };
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(candidate.id))
        .filter(conversation::Column::AgentType.eq(AgentType::Codex.as_wire().into_owned()))
        .filter(conversation::Column::ExternalId.eq(external_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(
            conversation::Column::FolderId.in_subquery(
                sea_orm::sea_query::Query::select()
                    .column(folder::Column::Id)
                    .from(folder::Entity)
                    .and_where(folder::Column::DeletedAt.is_null())
                    .to_owned(),
            ),
        )
        .filter(old_title)
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Refresh every live, unlocked Codex conversation whose external session id
/// has a title in `titles`. Candidate selection is limited to indexed session
/// ids and chunked below SQLite's bound-variable limit. Each UPDATE atomically
/// re-checks the complete candidate identity via
/// [`refresh_codex_auto_title_candidate`]. Converged rows issue no UPDATE and
/// title refreshes never bump `updated_at`. This is a best-effort reconciliation:
/// a failed chunk or row is logged and skipped while successful row ids are
/// retained for downstream notifications.
///
/// Candidate scope mirrors [`list_all`]'s own visibility rules — a live folder
/// and `kind != 'loop'` — because every refreshed id is broadcast as a sidebar
/// upsert. Refreshing a row this query would never return would push a row the
/// list deliberately hides into every client's sidebar until its next refetch.
///
/// Each candidate keeps its OWN autocommit UPDATE rather than sharing one
/// transaction per chunk: the guarantee callers rely on is that a row that
/// failed (or lost its CAS) never holds back the rows that succeeded, and a
/// chunk-wide transaction would roll those back together.
pub(crate) async fn refresh_codex_auto_titles(
    conn: &DatabaseConnection,
    titles: &HashMap<String, String>,
) -> Vec<i32> {
    if titles.is_empty() {
        return Vec::new();
    }

    const SQLITE_TITLE_QUERY_CHUNK_SIZE: usize = 500;
    let external_ids: Vec<String> = titles
        .iter()
        .filter(|(_, title)| !title.trim().is_empty())
        .map(|(external_id, _)| external_id.clone())
        .collect();
    let mut refreshed = Vec::new();

    for external_id_chunk in external_ids.chunks(SQLITE_TITLE_QUERY_CHUNK_SIZE) {
        let candidates = match conversation::Entity::find()
            .filter(conversation::Column::AgentType.eq(AgentType::Codex.as_wire().into_owned()))
            .filter(conversation::Column::ExternalId.is_in(external_id_chunk.iter().cloned()))
            .filter(conversation::Column::DeletedAt.is_null())
            .filter(conversation::Column::TitleLocked.eq(false))
            .filter(conversation::Column::Kind.ne(ConversationKind::Loop))
            .filter(
                conversation::Column::FolderId.in_subquery(
                    sea_orm::sea_query::Query::select()
                        .column(folder::Column::Id)
                        .from(folder::Entity)
                        .and_where(folder::Column::DeletedAt.is_null())
                        .to_owned(),
                ),
            )
            .order_by_asc(conversation::Column::Id)
            .all(conn)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    session_count = external_id_chunk.len(),
                    "failed to select Codex title refresh candidates; skipping chunk"
                );
                continue;
            }
        };

        for candidate in candidates {
            let Some(external_id) = candidate.external_id.as_deref() else {
                continue;
            };
            let Some(title) = titles.get(external_id) else {
                continue;
            };
            match refresh_codex_auto_title_candidate(conn, &candidate, title).await {
                Ok(true) => refreshed.push(candidate.id),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    conversation_id = candidate.id,
                    external_id,
                    "failed to refresh Codex title candidate; skipping row"
                ),
            }
        }
    }

    refreshed
}

/// Adopt an imported conversation's newest activity from its agent-side
/// transcript: stamp `updated_at` with the session file's own last-activity
/// time (never `now()` — the scan is not the activity) and re-sync
/// `message_count`. Returns `true` when a row was written, so the caller can
/// broadcast a sidebar upsert.
///
/// This is the counterpart to [`refresh_auto_title`] for the OTHER half of a
/// re-import: a session the user kept working on in the agent's own CLI after
/// importing it into codeg. Its title may be unchanged while its activity is
/// hours newer, and `updated_at` is what the sidebar's "recently updated"
/// ordering (and the relative timestamp on each row) reads.
///
/// One conditional UPDATE, guarded so it can never do harm:
/// * `updated_at < activity_at` — strictly forward. A re-import can never move
///   a conversation backwards or re-order an unchanged one, re-running is a
///   no-op, and a turn running live in codeg (which stamps `updated_at =
///   now()`) wins over a transcript tail parsed moments earlier.
/// * `deleted_at IS NULL` — a soft-deleted conversation stays deleted; agent
///   activity must not half-resurrect an invisible row.
/// * `parent_id IS NULL` — delegation children are not sidebar rows and are
///   maintained by the delegation flow.
///
/// Everything else the user owns is left alone: `created_at`, `title` (and its
/// lock), `pinned_at`, `status`, and folder placement.
pub async fn refresh_external_activity(
    conn: &DatabaseConnection,
    conversation_id: i32,
    activity_at: chrono::DateTime<Utc>,
    message_count: u32,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::UpdatedAt, Expr::value(activity_at))
        .col_expr(
            conversation::Column::MessageCount,
            Expr::value(message_count as i32),
        )
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::ParentId.is_null())
        .filter(conversation::Column::UpdatedAt.lt(activity_at))
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Pin or unpin a conversation. Sets `pinned_at = now()` when pinning, `NULL`
/// when unpinning. Only the `pinned_at` column is written — `updated_at` is
/// deliberately left untouched (SeaORM updates only the `Set` field), because
/// pinning is a view preference, not conversation activity, and must not float
/// the row to the top of a recency-sorted sidebar (same reasoning as
/// [`refresh_auto_title`]). The sidebar's "Pinned" section orders by `pinned_at`
/// descending, so a freshly pinned conversation jumps to the top.
pub async fn update_pin(
    conn: &DatabaseConnection,
    conversation_id: i32,
    pinned: bool,
) -> Result<(), DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let mut active: conversation::ActiveModel = conv.into();
    active.pinned_at = Set(pinned.then(Utc::now));
    active.update(conn).await?;
    Ok(())
}

/// Bind an agent session id (`external_id`) to a conversation row WITHOUT ever
/// orphaning the session it was previously bound to.
///
/// Returns `Some(preserved_row_id)` when the previous session had to be moved
/// onto a freshly inserted row to keep it reachable, `None` when the write was
/// an ordinary (re)binding that created nothing.
///
/// Either `Ok` is a promise the caller builds on: **the row is now bound to
/// `external_id`**. `send_prompt_linked` flips the row to `InProgress` and
/// dispatches the prompt on exactly that basis. When the id cannot be taken
/// because another row holds it, this returns [`DbError::Conflict`] rather than
/// a quiet `Ok(None)` — see the conflict guard in the body for why reporting a
/// refusal as success would misfile the user's turn.
///
/// # The invariant
///
/// **After this commits, the previous session's history is still reachable.**
///
/// Stated as a post-condition on purpose. The tempting phrasing ("before
/// re-pointing, some other row must already hold S1") reads as an ordering
/// requirement and leads straight into the unique index below.
///
/// Note "history is reachable", not "some row holds the id". `continues` is
/// what separates the two: it lists the sessions the INCOMING id carries
/// forward (see [`crate::acp::continued_session_ids`]). When the outgoing id
/// is one of them, this is the same conversation moving to a new agent
/// session — its turns are still readable through the new id, so the row just
/// advances and nothing is split off. Passing an empty slice is always safe
/// in the sense that it never loses data; it only risks splitting a
/// continuation into a duplicate conversation.
///
/// Why it matters: the conversation list is built purely from DB rows
/// (`list_all`) — nothing scans the agent's own transcript store. So a session
/// id that no row references is invisible in the UI even though its transcript
/// is intact on disk, and the user reasonably reads that as "my conversation
/// was deleted". That is codeg#500: a connection spawned with `session_id =
/// None` mints a fresh session, then a prompt carrying an existing
/// `conversation_id` adopts that row and the fresh id lands on top of the id
/// the row's whole history hangs off.
///
/// # Why the writes are ordered the way they are
///
/// `idx_conversation_external_agent` is UNIQUE over
/// `(external_id, agent_type)` (see the init migration). So the preserving row
/// can only be inserted AFTER the current row has released S1 — insert-first
/// fails the constraint every time. Both statements share one transaction, so
/// the "S1 released but nobody holds it" window can never be observed by
/// another connection and never commits. (SQLite treats NULLs as distinct, so
/// any number of rows may sit at `external_id IS NULL`.)
///
/// The claim write that opens the transaction is a SELF-ASSIGNMENT
/// (`updated_at = updated_at`). It exists for the same reason as the one in
/// `persist_fork_outcome` — lead with a write so SQLite takes the writer lock
/// immediately instead of a deferred read snapshot it may fail to promote
/// (`SQLITE_BUSY_SNAPSHOT`, surfaced as a bogus "database is locked") — but it
/// must not CHANGE `updated_at`, because the row is read straight afterwards
/// and the preserving row copies that pre-transition timestamp to hold its
/// place in the sidebar's activity ordering. A self-assigning UPDATE still
/// matches the row and still claims the lock.
///
/// The `deleted_at IS NULL` guard is inherited from the previous
/// `update_external_id`: `SessionStarted` writes are not serialized against
/// deletes (deleting only soft-marks the row; the agent stays connected and
/// bound), so a late event must not half-resurrect an invisible row.
pub async fn bind_external_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
    external_id: &str,
    continues: &[String],
) -> Result<Option<i32>, DbError> {
    use sea_orm::sea_query::Expr;
    use sea_orm::TransactionTrait;

    let external_id = external_id.to_string();
    let continues: Vec<String> = continues.to_vec();
    // The closure below MOVES `external_id`; keep a copy for the conflict
    // message, which is built after the transaction has returned.
    let requested_id = external_id.clone();
    let outcome = conn
        .transaction::<_, BindTxOutcome, sea_orm::DbErr>(|txn| {
            Box::pin(async move {
                let now = Utc::now();

                // Claim the writer lock without disturbing any value.
                let claimed = conversation::Entity::update_many()
                    .col_expr(
                        conversation::Column::UpdatedAt,
                        Expr::col(conversation::Column::UpdatedAt).into(),
                    )
                    .filter(conversation::Column::Id.eq(conversation_id))
                    .filter(conversation::Column::DeletedAt.is_null())
                    .exec(txn)
                    .await?;
                if claimed.rows_affected == 0 {
                    // Gone or soft-deleted. Every caller treats "no live row" as
                    // nothing to do, so this stays Ok — same contract the old
                    // `update_external_id` had.
                    return Ok(BindTxOutcome::Bound(None));
                }

                // Read under the write lock: pristine values, and no other
                // writer can interpose before this transaction finishes.
                let current = conversation::Entity::find_by_id(conversation_id)
                    .one(txn)
                    .await?
                    .ok_or_else(|| {
                        sea_orm::DbErr::Custom(format!("conversation {conversation_id} not found"))
                    })?;

                let previous = current.external_id.clone();
                let repoints_away = matches!(
                    previous.as_deref(),
                    Some(prev) if prev != external_id && !continues.iter().any(|c| c == prev)
                );

                // Conflict guard, covering BOTH write branches below.
                //
                // `idx_conversation_external_agent` is UNIQUE over
                // `(external_id, agent_type)` and — unlike every read in this
                // module — carries no `deleted_at` predicate, so a soft-deleted
                // row still occupies its id. Claiming an id another row holds
                // therefore fails the constraint outright, rolls the whole
                // transaction back, and surfaces a raw "UNIQUE constraint
                // failed" out of `send_prompt_linked`: the user's prompt just
                // errors. Both branches are exposed — the plain bind (a fresh
                // row whose connection resumed a session codeg already has a
                // row for) as much as the split's release.
                //
                // Refuse instead, and change nothing. The invariant still
                // holds, from both ends: this row keeps the session it had, and
                // the incoming session is already reachable through the row
                // that holds it. Taking the id would orphan THAT row's history
                // — the very failure this function exists to prevent — so there
                // is no better move available here, only a louder one.
                //
                // A soft-deleted holder is refused on the same terms rather
                // than having its id taken, matching how the rest of this
                // module treats rows the sidebar never shows (`import_one` and
                // `refresh_existing` return `Skipped` for one instead of
                // rewriting it). Conversations have no restore path, so such a
                // row is inert — but it is inert with the id, and the index
                // counts it.
                //
                // Skipped when this row already holds the id: it is then the
                // holder, so by the same index no other row can be.
                //
                // Surfaced as `DbError::Conflict`, NOT as a quiet `Ok(None)`.
                // The distinction is load-bearing: `Ok` here means "this row is
                // bound to `external_id`", and `send_prompt_linked` goes on to
                // flip the row to `InProgress` and dispatch the prompt on that
                // basis. A refusal reported as success would send the turn into
                // a session the row does not own — it would land in the HOLDER
                // row's transcript while every event named the refused row, so
                // the user's message would move conversations on reload. An
                // `Err` returns before either step, exactly as the raw unique
                // violation used to, and the retry in
                // `lifecycle::handle_event_with_retry` gives up after three
                // attempts.
                if previous.as_deref() != Some(external_id.as_str()) {
                    let holder = conversation::Entity::find()
                        .filter(conversation::Column::ExternalId.eq(external_id.clone()))
                        .filter(conversation::Column::AgentType.eq(current.agent_type.clone()))
                        .filter(conversation::Column::Id.ne(conversation_id))
                        .one(txn)
                        .await?;
                    if let Some(holder) = holder {
                        // Logged here rather than left to the caller: these
                        // fields are what post-hoc diagnosis needs, and the
                        // error string that reaches the user must stay short.
                        tracing::warn!(
                            conversation_id,
                            holder_row_id = holder.id,
                            holder_deleted = holder.deleted_at.is_some(),
                            from_session = previous.as_deref().unwrap_or("<none>"),
                            to_session = %external_id,
                            agent_type = %current.agent_type,
                            "[conversation] refused to bind a session another row \
                             already holds; both histories left where they are"
                        );
                        return Ok(BindTxOutcome::Refused {
                            holder_row_id: holder.id,
                        });
                    }
                }

                // A row that already holds this id, or holds none yet, is an
                // ordinary bind: first bind, resume, or a duplicate
                // SessionStarted. A row whose id the INCOMING session carries
                // forward is one too — same conversation, new agent session,
                // with the earlier turns still readable through the new id (see
                // `continues`). Splitting that would clone the conversation in
                // the sidebar every time a memory-only custom agent restarts.
                // Nothing to preserve in any of these.
                if !repoints_away {
                    let mut active: conversation::ActiveModel = current.into();
                    active.external_id = Set(Some(external_id));
                    active.updated_at = Set(now);
                    active.update(txn).await?;
                    return Ok(BindTxOutcome::Bound(None));
                }

                let previous = previous.expect("repoints_away implies a previous id");

                // Does another live row already carry the outgoing session?
                //
                // Under today's schema it cannot: this row holds `previous`,
                // and the unique index admits exactly one holder per
                // `(external_id, agent_type)`. The cases that look like they
                // produce it do not. Fork writes its sibling INSERT and its
                // re-point in ONE transaction, so `previous` is observable only
                // on this row (before) or on the sibling with this row already
                // advanced (after — and then `repoints_away` is false and we
                // never reach here). A replayed `SessionStarted` is likewise
                // the idempotent branch above.
                //
                // Kept as a guard rather than an assert because the cost is one
                // indexed lookup and the failure it covers is silent data loss:
                // if the index is ever narrowed (a partial `WHERE deleted_at IS
                // NULL` would do it), two rows COULD hold `previous`, and
                // preserving it a second time would then insert a duplicate
                // conversation for history that already has a home.
                let already_preserved = conversation::Entity::find()
                    .filter(conversation::Column::ExternalId.eq(previous.clone()))
                    .filter(conversation::Column::AgentType.eq(current.agent_type.clone()))
                    .filter(conversation::Column::Id.ne(conversation_id))
                    .filter(conversation::Column::DeletedAt.is_null())
                    .one(txn)
                    .await?
                    .is_some();

                // Snapshot what the preserving row inherits BEFORE the re-point
                // consumes `current`.
                let carried = CarriedOverRow::from(&current);

                // Release S1 first — the unique index leaves no other order.
                let mut active: conversation::ActiveModel = current.into();
                active.external_id = Set(Some(external_id.clone()));
                // The model described the session being released, and `carried`
                // has already taken it for the row that keeps S1's history. Left
                // in place it would name S1's model on a row that is now S2 —
                // and `seed_model_if_empty` only fills an EMPTY column, so
                // nothing would ever correct it. Cleared, S2 seeds itself on its
                // next open.
                active.model = Set(None);
                active.updated_at = Set(now);
                active.update(txn).await?;

                if already_preserved {
                    tracing::info!(
                        conversation_id,
                        from_session = %previous,
                        to_session = %external_id,
                        "[conversation] re-pointed a conversation to a new session; the \
                         previous session is already held by another row"
                    );
                    return Ok(BindTxOutcome::Bound(None));
                }

                let agent_type = carried.agent_type.clone();
                let preserved = carried.into_active_model(previous.clone()).insert(txn).await?;
                // The preserving row IS the old conversation as far as the user
                // can tell, so it keeps the tags they put on it.
                conversation_tag_service::copy_conversation_tags(txn, conversation_id, preserved.id)
                    .await?;
                // The one signal that this happened at all. Deliberately WARN:
                // every occurrence means a connection bound to a row while
                // holding a session unrelated to that row's history, which is
                // never intentional — the split keeps it from destroying data,
                // but the cause is still worth finding. These five fields are
                // what post-hoc diagnosis actually needs (codeg#500 was
                // reconstructed by hand from WAL dumps for want of them).
                tracing::warn!(
                    conversation_id,
                    preserved_row_id = preserved.id,
                    from_session = %previous,
                    to_session = %external_id,
                    agent_type = %agent_type,
                    "[conversation] session changed under a bound conversation; \
                     preserved the previous session's history on a new row"
                );
                Ok(BindTxOutcome::Bound(Some(preserved.id)))
            })
        })
        .await
        .map_err(|e| match e {
            sea_orm::TransactionError::Connection(e)
            | sea_orm::TransactionError::Transaction(e) => DbError::Database(e),
        })?;
    match outcome {
        BindTxOutcome::Bound(preserved) => Ok(preserved),
        // Raised AFTER the transaction commits rather than by rolling it back:
        // the only statement it ran is the self-assigning claim, which changes
        // no value, so commit and rollback are indistinguishable on disk and
        // committing keeps the writer lock held for the shortest time.
        //
        // The message is what reaches the user through
        // `AcpError::protocol(e.to_string())`, so it names the conflict in
        // terms the sidebar can be read against; the WARN inside the
        // transaction carries the full diagnostic tuple.
        BindTxOutcome::Refused { holder_row_id } => Err(DbError::Conflict(format!(
            "agent session {requested_id} is already bound to conversation \
             {holder_row_id}; refusing to move it onto conversation \
             {conversation_id}"
        ))),
    }
}

/// What [`bind_external_id`]'s transaction concluded.
///
/// A refusal has to leave the closure as a distinct VALUE rather than an early
/// `Ok(None)`, because the two mean opposite things to a caller: `Bound` means
/// the row now holds the requested session and it is safe to build on that,
/// while `Refused` means nothing was written and the operation must be
/// abandoned. It is translated to [`DbError::Conflict`] outside the
/// transaction (a `sea_orm::DbErr` raised inside would be flattened into
/// `DbError::Database` by the `TransactionError` mapping and become
/// indistinguishable from transient contention).
enum BindTxOutcome {
    /// The row holds `external_id`. `Some(id)` when the outgoing session had to
    /// be split onto a new row to stay reachable.
    Bound(Option<i32>),
    /// Another row already holds `(external_id, agent_type)`. Nothing written.
    Refused { holder_row_id: i32 },
}

/// The fields a preserving row inherits from the row it is splitting off from.
///
/// Snapshotted before the re-point because turning the model into an
/// `ActiveModel` consumes it.
struct CarriedOverRow {
    folder_id: i32,
    title: Option<String>,
    title_locked: bool,
    agent_type: String,
    status: conversation::ConversationStatus,
    kind: ConversationKind,
    model: Option<String>,
    git_branch: Option<String>,
    parent_id: Option<i32>,
    message_count: i32,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    origin_cwd: Option<String>,
    /// [crewchat] Carried like the rest of the row: a fork of a group
    /// conversation is still a group conversation.
    team: Option<String>,
}

impl CarriedOverRow {
    fn from(row: &conversation::Model) -> Self {
        Self {
            folder_id: row.folder_id,
            title: row.title.clone(),
            title_locked: row.title_locked,
            agent_type: row.agent_type.clone(),
            // `InProgress` is deliberately NOT carried over: no agent is
            // attached to the outgoing session any more, so copying it would
            // leave a row spinning in the sidebar forever. This is the same
            // call `persist_fork_outcome` makes for its sibling. A row that
            // already reached a terminal state keeps it — that state is
            // user-visible and still true of the history being preserved.
            status: match row.status {
                conversation::ConversationStatus::InProgress => {
                    conversation::ConversationStatus::PendingReview
                }
                ref settled => settled.clone(),
            },
            kind: row.kind.clone(),
            model: row.model.clone(),
            git_branch: row.git_branch.clone(),
            // Carried so the `kind == Delegate ⟺ parent_id IS NOT NULL`
            // invariant survives, and so a preserved child stays inside its
            // parent's sub-session subtree instead of surfacing as a root.
            parent_id: row.parent_id,
            message_count: row.message_count,
            // Copied, not stamped `now`: the preserving row IS the old
            // conversation, so it must keep its place in the sidebar's
            // activity ordering rather than jumping to the top as if new.
            created_at: row.created_at,
            updated_at: row.updated_at,
            // The Gemini/Cline stale-external-id fallback matches on
            // `origin_cwd ?? folder.path`, so dropping this would break
            // history lookup for a re-parented conversation.
            origin_cwd: row.origin_cwd.clone(),
            team: row.team.clone(),
        }
    }

    fn into_active_model(self, external_id: String) -> conversation::ActiveModel {
        conversation::ActiveModel {
            id: NotSet,
            folder_id: Set(self.folder_id),
            title: Set(self.title),
            title_locked: Set(self.title_locked),
            agent_type: Set(self.agent_type),
            status: Set(self.status),
            kind: Set(self.kind),
            model: Set(self.model),
            git_branch: Set(self.git_branch),
            external_id: Set(Some(external_id)),
            parent_id: Set(self.parent_id),
            // Deliberately NOT carried: both are close to unique per delegation
            // call, and duplicating them would point the parent's tool-call view
            // at two children for one call.
            parent_tool_use_id: Set(None),
            delegation_call_id: Set(None),
            message_count: Set(self.message_count),
            created_at: Set(self.created_at),
            updated_at: Set(self.updated_at),
            deleted_at: Set(None),
            // Pinning is a view preference attached to the row the user pinned,
            // not to the history.
            pinned_at: Set(None),
            origin_cwd: Set(self.origin_cwd),
            team: Set(self.team),
        }
    }
}

/// Re-point a conversation at a DIFFERENT SPELLING of the session it is already
/// bound to — an alias normalization, not a session change.
///
/// The one caller is the detail load, which resolves an ACP-minted UUID to the
/// branch id the parser actually indexes by (Gemini / Cline) and writes it back
/// so later lookups go direct. Because both ids denote the same session, there
/// is no history to preserve and [`bind_external_id`]'s split would be wrong
/// here — it would manufacture a phantom conversation for the old spelling.
///
/// `expected_old` makes that narrow intent explicit and doubles as a CAS: the
/// caller passes the exact id it resolved FROM, so a concurrent rebind (a
/// `SessionStarted` landing mid-parse) leaves the write matching nothing
/// instead of clobbering the newer binding. A no-match is a silent no-op.
pub async fn renormalize_external_id_alias(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected_old: Option<&str>,
    external_id: String,
) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    let mut query = conversation::Entity::update_many()
        .col_expr(conversation::Column::ExternalId, Expr::value(external_id))
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null());
    query = match expected_old {
        Some(old) => query.filter(conversation::Column::ExternalId.eq(old)),
        None => query.filter(conversation::Column::ExternalId.is_null()),
    };
    query.exec(conn).await?;
    Ok(())
}

/// Re-parent every live conversation of `from_folder_id` (a task worktree
/// folder about to be removed) onto the project folder, stamping `origin_cwd`
/// with the worktree's original path. History loading is external_id-driven and
/// unaffected; the Gemini/Cline/OpenClaw stale-id fallback matches on
/// `origin_cwd ?? folder.path`, which this preserves.
pub async fn reparent_folder_conversations(
    conn: &DatabaseConnection,
    from_folder_id: i32,
    to_folder_id: i32,
    origin_cwd: &str,
) -> Result<u64, DbError> {
    use sea_orm::sea_query::Expr;
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::FolderId, Expr::value(to_folder_id))
        .col_expr(
            conversation::Column::OriginCwd,
            Expr::value(Some(origin_cwd.to_string())),
        )
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::FolderId.eq(from_folder_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected)
}

pub async fn soft_delete(conn: &DatabaseConnection, conversation_id: i32) -> Result<(), DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let mut active: conversation::ActiveModel = conv.into();
    active.deleted_at = Set(Some(Utc::now()));
    active.update(conn).await?;
    Ok(())
}

/// Undo [`soft_delete`] for a conversation the user re-selected in the
/// import picker. Returns `true` when this call is the one that brought the row
/// back, `false` when it was already live (or is a delegation child, which is
/// never a sidebar row) — so a caller can count restores without double-
/// counting a concurrent one.
///
/// Deleting a conversation in codeg never touches the agent's own session file
/// and never removes the row: it only stamps `deleted_at`. Everything needed to
/// bring it back is therefore still on both sides, which is what makes restore
/// a single conditional UPDATE rather than a re-insert — the conversation keeps
/// its id, so bound tabs, token-usage rows and delegation children all still
/// point at it.
///
/// Three columns move, and only these:
/// * `deleted_at → NULL` — the restore itself.
/// * `folder_id → folder_id` — the folder the import is landing this session's
///   group in, which `add_folder` has just made live AND open. The row's own
///   `folder_id` is deliberately NOT preserved: it may point at a folder the
///   user has since removed or closed, and restoring into an invisible folder
///   looks exactly like a restore that did not work. This is also the folder
///   header the user checked the row under in the picker. (Live rows are still
///   never moved — see `import_service::refresh_existing`; this applies only to
///   a row being brought back.)
/// * `status → pending_review` — the same status a fresh import lands on, and
///   for the same reason: the sidebar's "show completed" filter defaults OFF,
///   so a conversation restored as `completed` would come back invisible.
///
/// `updated_at` is deliberately left alone: a restore is not activity, and the
/// caller's `refresh_external_activity` pass adopts the transcript's real
/// last-activity time right after, so the row sorts where it belongs.
///
/// The `deleted_at IS NOT NULL` guard is re-evaluated by the database at write
/// time, so a row that was un-deleted between the caller's read and this write
/// is not clobbered back to `pending_review`.
pub async fn restore_soft_deleted(
    conn: &DatabaseConnection,
    conversation_id: i32,
    folder_id: i32,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let res = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::DeletedAt,
            Expr::value(None::<chrono::DateTime<Utc>>),
        )
        .col_expr(conversation::Column::FolderId, Expr::value(folder_id))
        .col_expr(
            conversation::Column::Status,
            Expr::value(conversation::ConversationStatus::PendingReview),
        )
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_not_null())
        .filter(conversation::Column::ParentId.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

fn parse_agent_type(s: &str) -> AgentType {
    match serde_json::from_value(serde_json::Value::String(s.to_string())) {
        Ok(at) => at,
        Err(_) => {
            // DB has a value the enum does not recognise (manual edit or removed variant).
            // Fall back to ClaudeCode so the row stays readable, but log so resume-as-wrong-agent
            // regressions are traceable.
            tracing::warn!(
                "[conversation_service] unknown agent_type {s:?} in DB, falling back to ClaudeCode"
            );
            AgentType::ClaudeCode
        }
    }
}

fn conv_to_summary(r: conversation::Model) -> DbConversationSummary {
    let status = serde_json::to_value(&r.status)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", r.status));
    DbConversationSummary {
        id: r.id,
        folder_id: r.folder_id,
        title: r.title,
        title_locked: r.title_locked,
        agent_type: parse_agent_type(&r.agent_type),
        status,
        kind: r.kind.clone(),
        model: r.model,
        git_branch: r.git_branch,
        external_id: r.external_id,
        message_count: r.message_count as u32,
        // Pure mapper: `child_count` is backfilled by `fill_child_counts` over
        // the returned set, never queried per-row here.
        child_count: 0,
        created_at: r.created_at,
        updated_at: r.updated_at,
        pinned_at: r.pinned_at,
        parent_id: r.parent_id,
        parent_tool_use_id: r.parent_tool_use_id,
        delegation_call_id: r.delegation_call_id,
        // [crewchat] Team snapshot (Phase 1.1). Corrupt JSON reads as "no
        // team" rather than failing the whole list — the row is still usable
        // as a plain session.
        team: r
            .team
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        origin_cwd: r.origin_cwd,
        // Backfilled by `fill_summary_extras`, like `child_count`.
        tag_ids: Vec::new(),
    }
}

/// Backfill each summary's `child_count` with its number of direct, non-deleted
/// delegation children using ONE `GROUP BY` aggregate over the whole set (never
/// per-row — no N+1). `child_count > 0` iff `list_children` would return rows
/// (same `parent_id == id AND deleted_at IS NULL` predicate), so the sidebar
/// chevron neither expands to nothing nor hides a real subtree. No-op on an
/// empty slice (avoids an `IN ()`).
async fn fill_child_counts(
    conn: &DatabaseConnection,
    summaries: &mut [DbConversationSummary],
) -> Result<(), DbError> {
    if summaries.is_empty() {
        return Ok(());
    }
    let ids: Vec<i32> = summaries.iter().map(|s| s.id).collect();
    let pairs: Vec<(Option<i32>, i64)> = conversation::Entity::find()
        .select_only()
        .column(conversation::Column::ParentId)
        .column_as(conversation::Column::Id.count(), "cnt")
        .filter(conversation::Column::ParentId.is_in(ids))
        .filter(conversation::Column::DeletedAt.is_null())
        .group_by(conversation::Column::ParentId)
        .into_tuple()
        .all(conn)
        .await?;
    let mut counts: std::collections::HashMap<i32, u32> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (parent_id, cnt) in pairs {
        if let Some(pid) = parent_id {
            counts.insert(pid, cnt.max(0) as u32);
        }
    }
    for s in summaries.iter_mut() {
        s.child_count = counts.get(&s.id).copied().unwrap_or(0);
    }
    Ok(())
}

/// Backfill what a summary carries but its row does not: `child_count` and
/// `tag_ids`, each with ONE query over the whole set. Every summary that leaves
/// this module for the UI goes through here — the sidebar replaces a row
/// wholesale on each `conversation://changed` upsert, so a path that skipped the
/// tags would wipe them from the sidebar until the next full refresh.
async fn fill_summary_extras(
    conn: &DatabaseConnection,
    summaries: &mut [DbConversationSummary],
) -> Result<(), DbError> {
    fill_child_counts(conn, summaries).await?;
    if summaries.is_empty() {
        return Ok(());
    }
    let ids: Vec<i32> = summaries.iter().map(|s| s.id).collect();
    let mut tags = conversation_tag_service::tag_ids_by_conversation(conn, &ids).await?;
    for s in summaries.iter_mut() {
        s.tag_ids = tags.remove(&s.id).unwrap_or_default();
    }
    Ok(())
}

pub async fn get_by_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<DbConversationSummary, DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;

    let mut summary = conv_to_summary(conv);
    fill_summary_extras(conn, std::slice::from_mut(&mut summary)).await?;
    Ok(summary)
}

/// Resolve a `codeg://session/<ref>` path to a live (non-deleted) conversation.
///
/// `session_ref` is either Codeg's numeric primary key (what MCP and the
/// `codeg://session/<id>` markdown mentions use) or the agent's own session
/// id stored as `external_id` (a Grok UUID, a Codex thread id, …). Several
/// rows can share an `external_id` across agents; the most recently updated
/// live row wins. Missing rows return `Ok(None)` — a stale deep link is not
/// an error.
///
/// A numeric ref is tried as a primary key *first* and as an `external_id`
/// only if no live row carries that id: nothing stops an agent from handing
/// out all-digit session ids, and silently resolving one to an unrelated
/// conversation that happens to own that PK is worse than a second query.
pub async fn find_live_by_session_ref(
    conn: &DatabaseConnection,
    session_ref: &str,
) -> Result<Option<DbConversationSummary>, DbError> {
    let session_ref = session_ref.trim();
    if session_ref.is_empty() {
        return Ok(None);
    }
    let by_pk = match session_ref.parse::<i32>() {
        Ok(id) if id > 0 => {
            conversation::Entity::find_by_id(id)
                .filter(conversation::Column::DeletedAt.is_null())
                .one(conn)
                .await?
        }
        _ => None,
    };
    let conv = match by_pk {
        Some(conv) => Some(conv),
        None => {
            conversation::Entity::find()
                .filter(conversation::Column::ExternalId.eq(session_ref))
                .filter(conversation::Column::DeletedAt.is_null())
                .order_by_desc(conversation::Column::UpdatedAt)
                .one(conn)
                .await?
        }
    };
    match conv {
        Some(conv) => {
            let mut summary = conv_to_summary(conv);
            fill_summary_extras(conn, std::slice::from_mut(&mut summary)).await?;
            Ok(Some(summary))
        }
        None => Ok(None),
    }
}

/// Look up a child conversation by its `delegation_call_id` (the broker's
/// `task_id`). Returns `Ok(None)` when no row matches — used by the broker's
/// `ChildStatusLookup` DB fallback to recover a delegation task's terminal
/// status after its in-memory result was evicted from the completed-cache.
/// Unlike [`get_by_id`] this never errors hard on "not found": a missing row
/// is a legitimate "unknown task" answer.
pub async fn get_by_delegation_call_id(
    conn: &DatabaseConnection,
    delegation_call_id: &str,
) -> Result<Option<DbConversationSummary>, DbError> {
    let conv = conversation::Entity::find()
        .filter(conversation::Column::DelegationCallId.eq(delegation_call_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?;
    Ok(conv.map(conv_to_summary))
}

pub async fn list_by_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    status: Option<String>,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let mut query = conversation::Entity::find()
        .filter(conversation::Column::FolderId.eq(folder_id))
        .filter(conversation::Column::DeletedAt.is_null());

    // Filter by agent_type
    if let Some(ref at) = agent_type {
        let at_str = serde_json::to_value(at)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        query = query.filter(conversation::Column::AgentType.eq(at_str));
    }

    // Search by title
    if let Some(ref s) = search {
        if !s.is_empty() {
            query = query.filter(conversation::Column::Title.contains(s));
        }
    }

    // Filter by status
    if let Some(ref st) = status {
        if let Ok(status_enum) = serde_json::from_value::<conversation::ConversationStatus>(
            serde_json::Value::String(st.clone()),
        ) {
            query = query.filter(conversation::Column::Status.eq(status_enum));
        }
    }

    // Sort
    query = match sort_by.as_deref() {
        Some("oldest") => query.order_by_asc(conversation::Column::CreatedAt),
        _ => query.order_by_desc(conversation::Column::CreatedAt),
    };

    let rows = query.all(conn).await?;

    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_summary_extras(conn, &mut summaries).await?;

    Ok(summaries)
}

/// List conversations across folders. When `folder_ids` is `None`, queries all
/// When `folder_ids` is provided, results are scoped to that set. Otherwise
/// returns conversations across every non-deleted folder (open or not).
///
/// `include_children` controls visibility of delegation sub-sessions. When
/// `false` (the default for the top-level list), rows whose `parent_id` is
/// non-null are filtered out — they belong to their parent's tool-call view,
/// not the workspace conversation list. Rows with `kind = 'loop'` are always
/// excluded — they belong to the loops workbench.
pub async fn list_all(
    conn: &DatabaseConnection,
    folder_ids: Option<Vec<i32>>,
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    status: Option<String>,
    include_children: bool,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let mut query = conversation::Entity::find().filter(conversation::Column::DeletedAt.is_null());

    // Loop-engineering runs never surface in the workspace conversation list —
    // their entry point is the loops workbench.
    query = query.filter(conversation::Column::Kind.ne(ConversationKind::Loop));

    if !include_children {
        query = query.filter(conversation::Column::ParentId.is_null());
    }

    match folder_ids {
        Some(ids) if !ids.is_empty() => {
            query = query.filter(conversation::Column::FolderId.is_in(ids));
        }
        _ => {
            // Exclude conversations whose folder was soft-deleted.
            let active_folder_ids: Vec<i32> = folder::Entity::find()
                .filter(folder::Column::DeletedAt.is_null())
                .all(conn)
                .await?
                .into_iter()
                .map(|m| m.id)
                .collect();
            if active_folder_ids.is_empty() {
                return Ok(Vec::new());
            }
            query = query.filter(conversation::Column::FolderId.is_in(active_folder_ids));
        }
    }

    if let Some(ref at) = agent_type {
        let at_str = serde_json::to_value(at)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        query = query.filter(conversation::Column::AgentType.eq(at_str));
    }

    if let Some(ref s) = search {
        if !s.is_empty() {
            query = query.filter(conversation::Column::Title.contains(s));
        }
    }

    if let Some(ref st) = status {
        if let Ok(status_enum) = serde_json::from_value::<conversation::ConversationStatus>(
            serde_json::Value::String(st.clone()),
        ) {
            query = query.filter(conversation::Column::Status.eq(status_enum));
        }
    }

    query = match sort_by.as_deref() {
        Some("oldest") => query.order_by_asc(conversation::Column::UpdatedAt),
        _ => query.order_by_desc(conversation::Column::UpdatedAt),
    };

    let rows = query.all(conn).await?;
    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_summary_extras(conn, &mut summaries).await?;
    Ok(summaries)
}

/// List delegation children of a single parent conversation, newest first
/// (`created_at` DESC), matching the sidebar's newest-on-top ordering so a
/// freshly-spawned sub-agent surfaces right under its parent. The only other
/// consumer (`inject_delegation_meta`) keys these by `parent_tool_use_id` and
/// is order-agnostic. Returns rows where `parent_id == parent_conversation_id`.
/// Soft-deleted children are filtered out so a removed sub-session stays hidden
/// in the parent's tool-call view too.
pub async fn list_children(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let rows = conversation::Entity::find()
        .filter(conversation::Column::ParentId.eq(parent_conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .order_by_desc(conversation::Column::CreatedAt)
        // Explicit id tie-break so same-timestamp siblings are deterministic and
        // match the frontend re-sort (which tie-breaks id DESC): the raw fetch
        // snapshot and a live-inserted child then land in the same order.
        .order_by_desc(conversation::Column::Id)
        .all(conn)
        .await?;
    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_summary_extras(conn, &mut summaries).await?;
    Ok(summaries)
}

#[cfg(test)]
mod tests;
