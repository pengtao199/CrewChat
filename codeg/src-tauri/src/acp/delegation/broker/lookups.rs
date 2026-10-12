// broker submodule: DB-backed lookups + traits (split from mod.rs)
use super::*;


/// Status-level facts the broker recovers from a child conversation row when a
/// task's in-memory completed-cache entry was evicted. Carries NO result text —
/// child output isn't stored in codeg's DB; the full result lives in the
/// child's own session (viewable via the frontend's child-session sheet).
#[derive(Debug, Clone)]
pub struct ChildStatusRecord {
    pub child_conversation_id: i32,
    pub status: TaskStatus,
    pub agent_type: AgentType,
    /// The parent conversation id this child was spawned under. Used to scope
    /// the DB fallback to the calling parent so one parent can't read another's
    /// task by guessing a UUID.
    pub parent_id: Option<i32>,
}


/// Everything `resume_delegation` needs to know about an interrupted child
/// before it can bring the session back: where it ran, which agent, which
/// agent-side session to load, and how it is currently recorded.
///
/// Superset of [`ChildStatusRecord`] — kept separate so the hot status path
/// keeps its lean shape while resume (rare) pays for the extra fields.
#[derive(Debug, Clone)]
pub struct ChildResumeContext {
    pub child_conversation_id: i32,
    pub status: TaskStatus,
    pub agent_type: AgentType,
    /// Parent conversation scope — same ownership rule as
    /// [`ChildStatusRecord::parent_id`].
    pub parent_id: Option<i32>,
    /// The ORIGINAL delegation's parent-side tool_use_id persisted on the row.
    /// Reused for the resumed run's meta writes / started event so the parent's
    /// original delegation card re-binds to the resumed child. `None` maps to a
    /// synthetic id (meta/events safely skipped).
    pub parent_tool_use_id: Option<String>,
    /// The agent-assigned session id to resume (`conversation.external_id`).
    /// `None` means the child died before its session id was ever recorded —
    /// nothing to load, so the task is not resumable.
    pub external_id: Option<String>,
    /// The child row's folder — the resumed prompt is sent back into the same
    /// row, which requires its folder id (send-adopt contract).
    pub folder_id: i32,
    /// Absolute path of `folder_id`, used as the resumed connection's
    /// working_dir. `None` when the folder row is gone (refuse, don't guess).
    pub working_dir: Option<String>,
    /// The child row's title (originally seeded from the delegated task text) —
    /// bounded and reused as the resumed task's `task_preview` label.
    pub title: Option<String>,
}


/// DB fallback for `get_delegation_status` / `cancel_delegation` once a task's
/// result has aged out of the broker's in-memory completed-cache. Abstracted
/// so broker unit tests can run without SeaORM; production wires
/// [`DbChildStatusLookup`] via [`DelegationBroker::with_status_lookup`].
#[async_trait]
pub trait ChildStatusLookup: Send + Sync {
    async fn find_by_call_id(&self, call_id: &str) -> Option<ChildStatusRecord>;

    /// Resolve the full resume context for `resume_delegation`. Default `None`
    /// ("cannot resume") so status-only lookups and existing test doubles stay
    /// unchanged; production and resume-focused tests override it.
    async fn find_resume_context_by_call_id(&self, _call_id: &str) -> Option<ChildResumeContext> {
        None
    }
}


/// Default lookup — always "unknown". Used by `DelegationBroker::new` /
/// `with_writers` (tests that don't exercise the DB-fallback path); production
/// replaces it via `with_status_lookup`.
#[derive(Default, Clone)]
pub struct NoopChildStatusLookup;


#[async_trait]
impl ChildStatusLookup for NoopChildStatusLookup {
    async fn find_by_call_id(&self, _call_id: &str) -> Option<ChildStatusRecord> {
        None
    }
}


/// `ConversationDepthLookup` over the live `AppDatabase`. Used by the
/// production wiring; tests use the in-module `MockDepth`.
pub struct DbDepthLookup {
    pub db: Arc<crate::db::AppDatabase>,
}


#[async_trait]
impl ConversationDepthLookup for DbDepthLookup {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError> {
        use sea_orm::EntityTrait;
        let row = crate::db::entities::conversation::Entity::find_by_id(conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(|e| DelegationError::SubagentRuntimeError(format!("db: {e}")))?;
        Ok(row.and_then(|r| r.parent_id))
    }
}


/// `ChildStatusLookup` over the live `AppDatabase`. Recovers a delegation
/// task's terminal status (NOT its text — child output isn't in codeg's DB)
/// from the child conversation row once its in-memory result was evicted.
pub struct DbChildStatusLookup {
    pub db: Arc<crate::db::AppDatabase>,
}


/// Map the serialized `ConversationStatus` string on a child conversation row
/// onto the delegation [`TaskStatus`] vocabulary. Shared by the status fallback
/// and the resume-context lookup so the two can't drift.
pub(crate) fn task_status_from_row(status: &str) -> TaskStatus {
    match status {
        "in_progress" => TaskStatus::Running,
        "pending_review" | "completed" => TaskStatus::Completed,
        "cancelled" => TaskStatus::Canceled,
        _ => TaskStatus::Unknown,
    }
}


#[async_trait]
impl ChildStatusLookup for DbChildStatusLookup {
    async fn find_by_call_id(&self, call_id: &str) -> Option<ChildStatusRecord> {
        let summary = crate::db::service::conversation_service::get_by_delegation_call_id(
            &self.db.conn,
            call_id,
        )
        .await
        .ok()
        .flatten()?;
        Some(ChildStatusRecord {
            child_conversation_id: summary.id,
            status: task_status_from_row(&summary.status),
            agent_type: summary.agent_type,
            parent_id: summary.parent_id,
        })
    }

    async fn find_resume_context_by_call_id(&self, call_id: &str) -> Option<ChildResumeContext> {
        let summary = crate::db::service::conversation_service::get_by_delegation_call_id(
            &self.db.conn,
            call_id,
        )
        .await
        .ok()
        .flatten()?;
        // The row's folder supplies the resumed connection's working_dir. A
        // deleted folder yields `working_dir: None`, which the broker turns
        // into an explicit "folder no longer exists" refusal.
        let working_dir =
            crate::db::service::folder_service::get_folder_by_id(&self.db.conn, summary.folder_id)
                .await
                .ok()
                .flatten()
                .map(|f| f.path);
        Some(ChildResumeContext {
            child_conversation_id: summary.id,
            status: task_status_from_row(&summary.status),
            agent_type: summary.agent_type,
            parent_id: summary.parent_id,
            parent_tool_use_id: summary.parent_tool_use_id,
            external_id: summary.external_id,
            folder_id: summary.folder_id,
            working_dir,
            title: summary.title,
        })
    }
}
