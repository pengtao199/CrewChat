//! Main-process side of the `codeg-mcp` round-trip: accept UDS / named-pipe
//! connections from companion processes, validate the per-launch token,
//! resolve the parent's current conversation, and hand off to the broker.
//!
//! The listener is intentionally tiny — most of the work (depth checking,
//! spawn lifecycle, timeout, cancellation) happens inside
//! [`DelegationBroker`]. The listener is the boundary between the wire and
//! the broker, plus the place where the per-launch token policy is enforced.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::acp::delegation::broker::{DelegationBroker, StatusWait};
use crate::acp::browser_tools::{
    BrowserActOutcome, BrowserCaptureOutcome, BrowserConsoleOutcome, BrowserEvalOutcome,
    BrowserSnapshotOutcome, BrowserTabOutcome, BrowserTabsOutcome, BrowserToolAccess,
    ERROR_NO_SUCH_TAB,
};
use crate::acp::computer_tools::{
    ComputerActOutcome, ComputerAppsOutcome, ComputerCaptureOutcome, ComputerSnapshotOutcome,
    ComputerToolAccess, ComputerVerifyOutcome, ComputerWindowsOutcome, ERROR_NO_SUCH_TARGET,
};
use crate::acp::delegation::transport::{
    read_frame, write_frame, BrokerAskRequest, BrokerBrowserActRequest,
    BrokerBrowserCaptureRequest, BrokerBrowserConsoleRequest, BrokerBrowserEvalRequest,
    BrokerBrowserSnapshotRequest, BrokerBrowserTabOpRequest, BrokerBrowserTabsRequest,
    BrokerCancelRequest, BrokerCancelTaskRequest, BrokerComputerActRequest,
    BrokerComputerAppsRequest, BrokerComputerCaptureRequest, BrokerComputerLaunchRequest,
    BrokerComputerSnapshotRequest,
    BrokerComputerVerifyRequest, BrokerComputerWindowsRequest, BrokerCommitFeedbackRequest,
    BrokerFeedbackRequest, BrokerMessage, BrokerRequest,
    BrokerCreateAutomationRequest, BrokerCreateWorkTaskRequest, BrokerResponse,
    BrokerResumeTaskRequest, BrokerSessionRequest, BrokerStatusRequest,
    BrokerTaskCompleteRequest, BrokerTaskProgressRequest,
};
use crate::acp::delegation::types::{
    DelegationRequest, DelegationTaskReport, ResumeDelegationRequest, TaskStatus,
};
use crate::acp::feedback::{PendingFeedback, SessionFeedbackAccess};
use crate::acp::question::{QuestionOutcome, SessionQuestionAccess};
#[cfg(unix)]
use crate::acp::scratch_dir::SUN_PATH_CAP;
use crate::acp::chat_authoring::{AuthoringContext, AuthoringOutcome, ChatAuthoringAccess};
use crate::acp::session_info::{SessionInfo, SessionInfoAccess};
use crate::acp::work_task_tools::{TaskReportAck, WorkTaskToolAccess};
use crate::models::AgentType;
use serde_json::Value;

/// Hard ceiling on a *positive* `get_delegation_status` long-poll, so a single
/// MCP tool call can't block the companion's round-trip unbounded. The child
/// keeps running past this; the LLM simply re-issues the wait. An explicit
/// `wait_ms = 0` opts out of the ceiling and blocks until the task is terminal.
const STATUS_WAIT_MAX_MS: u64 = 60_000;


/// The bound-but-not-yet-served socket handed from [`DelegationListener::bind`]
/// to [`DelegationListener::accept_loop`]. A UDS listener on unix; on Windows,
/// the first named-pipe server instance (the loop creates each subsequent one
/// itself).
#[cfg(unix)]
pub type BoundSocket = tokio::net::UnixListener;
#[cfg(windows)]
pub type BoundSocket = tokio::net::windows::named_pipe::NamedPipeServer;

/// Pluggable "what conversation is this parent currently in?" lookup. The
/// production impl wraps `ConnectionManager.get_state`; tests use an
/// in-memory map.
///
/// Kept as a trait so the listener can be unit-tested without spinning up a
/// real `ConnectionManager` or RwLock<SessionState>.
#[async_trait]
pub trait ParentSessionLookup: Send + Sync {
    async fn current_conversation_id(&self, parent_connection_id: &str) -> Option<i32>;
}

/// Per-launch token entry. Bound at MCP injection time and revoked on parent
/// connection teardown.
#[derive(Debug, Clone)]
pub struct TokenEntry {
    pub parent_connection_id: String,
    pub working_dir: PathBuf,
}

#[derive(Default)]
pub struct TokenRegistry {
    inner: RwLock<HashMap<String, TokenEntry>>,
}

impl TokenRegistry {
    pub async fn register(&self, token: String, entry: TokenEntry) {
        self.inner.write().await.insert(token, entry);
    }

    pub async fn revoke(&self, token: &str) {
        self.inner.write().await.remove(token);
    }

    pub async fn lookup(&self, token: &str) -> Option<TokenEntry> {
        self.inner.read().await.get(token).cloned()
    }

    /// Drop every token whose `parent_connection_id` matches. Used on parent
    /// connection teardown so a leaked token can't be reused.
    pub async fn revoke_by_parent(&self, parent_connection_id: &str) {
        let mut map = self.inner.write().await;
        map.retain(|_, entry| entry.parent_connection_id != parent_connection_id);
    }

    /// How many companions are currently reachable, and across how many
    /// distinct parent ACP connections. One token is minted per companion
    /// launch, so `companions` counts injected `codeg-mcp` processes; the two
    /// numbers differ when a connection was re-injected without its old token
    /// having been revoked yet. Read-only — used by the service-status
    /// indicator, never by the wire path.
    pub async fn stats(&self) -> TokenStats {
        let map = self.inner.read().await;
        let parents: std::collections::HashSet<&str> = map
            .values()
            .map(|entry| entry.parent_connection_id.as_str())
            .collect();
        TokenStats {
            companions: map.len(),
            parent_connections: parents.len(),
        }
    }
}

/// Snapshot of [`TokenRegistry`] occupancy. See [`TokenRegistry::stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenStats {
    pub companions: usize,
    pub parent_connections: usize,
}

pub struct DelegationListener {
    pub broker: Arc<DelegationBroker>,
    pub tokens: Arc<TokenRegistry>,
    pub parent_lookup: Arc<dyn ParentSessionLookup>,
    /// Pulls pending live-feedback notes for the `check_user_feedback` tool.
    /// Shares the same `tokens` registry and parent-connection scoping as the
    /// delegation arms — one companion, one socket, two features.
    pub feedback: Arc<dyn SessionFeedbackAccess>,
    /// Registers / cancels the blocking `ask_user_question` tool's pending
    /// questions. Same `tokens` registry and parent-connection scoping.
    pub questions: Arc<dyn SessionQuestionAccess>,
    /// Resolves a referenced session for the `get_session_info` tool. Unlike the
    /// other arms this is NOT parent-scoped — it looks any non-deleted session up
    /// by its codeg conversation id (still token-gated against an invalid caller).
    pub session_info: Arc<dyn SessionInfoAccess>,
    /// Records work-task reports (`task_progress` / `task_complete`) against the
    /// task the parent connection is executing. Same token → parent-connection
    /// scoping as the delegation arms.
    pub tasks: Arc<dyn WorkTaskToolAccess>,
    /// Creates automations / board tasks on behalf of the chat that asked
    /// (`create_automation` / `create_work_task`). The impl re-checks the
    /// feature flags at call time, so flipping the setting off stops writes
    /// from sessions that were launched while it was on.
    pub authoring: Arc<dyn ChatAuthoringAccess>,
    /// Lists the built-in browser's tabs, reads a shared page and acts on one
    /// (`browser_list_tabs` / `browser_snapshot` / the action tools). Like the
    /// authoring impl it
    /// re-checks its feature flag at call time; unlike every other arm here it
    /// exists only in the desktop build, because a browser tab is a native
    /// webview — server mode gets `NoBrowserTabs`.
    pub browser: Arc<dyn BrowserToolAccess>,
    /// Lists the desktop's applications and windows and reads a shared window
    /// (`computer_*`). Desktop-only like the browser — server mode gets
    /// `NoComputerDesktop` — and like it, re-checks its switch at call time.
    pub computer: Arc<dyn ComputerToolAccess>,
}

impl DelegationListener {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        parent_lookup: Arc<dyn ParentSessionLookup>,
        feedback: Arc<dyn SessionFeedbackAccess>,
        questions: Arc<dyn SessionQuestionAccess>,
        session_info: Arc<dyn SessionInfoAccess>,
        tasks: Arc<dyn WorkTaskToolAccess>,
        authoring: Arc<dyn ChatAuthoringAccess>,
        browser: Arc<dyn BrowserToolAccess>,
        computer: Arc<dyn ComputerToolAccess>,
    ) -> Arc<Self> {
        Arc::new(Self {
            broker,
            tokens,
            parent_lookup,
            feedback,
            questions,
            session_info,
            tasks,
            authoring,
            browser,
            computer,
        })
    }

    /// Bind the socket, then serve it forever. Kept as the one-call entry
    /// point for callers that don't need to observe the bind separately;
    /// [`DelegationService`](super::service::DelegationService) uses the two
    /// halves so it can report a bind failure to the caller instead of losing
    /// it inside a detached task.
    pub async fn run(self: Arc<Self>, socket_path: PathBuf) -> std::io::Result<()> {
        let bound = Self::bind(&socket_path).await?;
        self.accept_loop(bound, socket_path).await
    }

    /// Take ownership of the socket. Split out from [`Self::accept_loop`] so
    /// the failure every caller actually cares about — the address is taken,
    /// the directory is gone, permissions are wrong — surfaces synchronously.
    ///
    /// Binds a short-lived sibling and `rename`s it onto `socket_path` rather
    /// than unlinking that path and binding it directly. `rename(2)` replaces
    /// the destination atomically, so the path never stops naming a bound
    /// socket, and **a bind that fails leaves whatever was already serving
    /// there reachable and untouched**.
    ///
    /// That invariant is the point. This function is on the recovery path — a
    /// user pressing "start service" on an indicator that may simply have
    /// mis-probed — and unlink-then-bind cannot promise it: with the unlink
    /// done and the bind then failing (fd exhaustion, ENOSPC), a perfectly
    /// healthy socket would have been destroyed to no purpose, by the very
    /// action meant to repair it.
    ///
    /// No fallback to unlink-then-bind on failure, deliberately: BOTH paths are
    /// checked against [`SUN_PATH_CAP`] below (so a length limit cannot reject
    /// one and admit the other) and every remaining failure reason — EMFILE,
    /// ENOSPC, a read-only directory — applies to both, so a fallback would
    /// only reintroduce the destructive window in exactly the conditions that
    /// triggered it.
    ///
    /// # Why the lengths are checked here, before anything else
    ///
    /// `bind(2)` enforces [`SUN_PATH_CAP`] but `rename(2)` does not — it is a
    /// plain directory operation with only `PATH_MAX` to answer to. So an
    /// over-long `socket_path` used to sail through this function: the staged
    /// name bound fine, the rename published it at a path no `connect(2)` on
    /// the system could ever name, and this returned `Ok`. The caller then
    /// reported a healthy service — `task_alive`, no `last_error`, a status
    /// indicator lit green — while every companion process was unable to reach
    /// it. A silent total failure, produced by the safety mechanism.
    ///
    /// Refusing up front converts that into a loud error, and doing it BEFORE
    /// the first filesystem call is what keeps the paragraph above true: there
    /// is no staged entry, no `create_dir_all`, and above all no chance of
    /// disturbing an incumbent socket that is still serving this path.
    ///
    /// The staged path is measured too rather than assumed shorter. It usually
    /// is — `.stg-<pid>-<8 hex>` is 8 bytes under `codeg-delegation-<pid>.sock`
    /// — but that holds only for names at least as long as the staged one, and
    /// a caller passing a SHORT name in a deep directory would otherwise clear
    /// this check and then fail the real `bind` on the staged path instead.
    #[cfg(unix)]
    pub async fn bind(socket_path: &Path) -> std::io::Result<BoundSocket> {
        let staging = Self::staging_socket_path(socket_path);
        // Pure: `staging_socket_path` only reshapes the name, so both operands
        // exist before this function has touched the filesystem at all.
        for path in [socket_path, staging.as_path()] {
            if !fits_sun_path(path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "socket path is {} bytes; AF_UNIX addresses cap at {} here. \
                         A `rename` onto this path would succeed and publish a socket \
                         nothing could connect to, so it is refused instead. Point \
                         TMPDIR at a shorter directory: {}",
                        path.as_os_str().len(),
                        SUN_PATH_CAP - 1,
                        path.display(),
                    ),
                ));
            }
        }
        if let Some(parent) = socket_path.parent() {
            // Compared as WRITTEN, not resolved: `default_socket_path` builds
            // the fallback by joining onto `short_socket_dir()`, so the two
            // spellings are byte-identical. Anything that canonicalized the
            // path upstream would take the other branch — on macOS `/tmp` is a
            // symlink to `/private/tmp` — and silently lose the guard below, so
            // keep this function's input un-normalized.
            if parent == short_socket_dir() {
                // Our own fallback directory, and it lives in a `1777` /tmp: it
                // can be waiting for us as another account's directory or as a
                // symlink aimed elsewhere. `create_root` creates it `0700` and
                // refuses both, and unlike the branch below its error is
                // PROPAGATED — a squatted directory must fail the bind, not
                // quietly host our socket.
                //
                // Re-worded on the way out: `create_root` says "scratch root",
                // which is the wrong noun for a broker socket and would send
                // whoever reads it off the status indicator into the wrong
                // subsystem.
                crate::acp::scratch_dir::create_root(parent).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "cannot use {} for the delegation socket: {e}",
                            parent.display()
                        ),
                    )
                })?;
            } else {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }
        // Clear a leftover from a bind that died between these two steps.
        let _ = tokio::fs::remove_file(&staging).await;
        let listener = tokio::net::UnixListener::bind(&staging)?;
        if let Err(e) = tokio::fs::rename(&staging, socket_path).await {
            // Don't leave the staged entry for the next bind to trip over.
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(e);
        }
        tracing::info!("[delegation] listening on UDS {}", socket_path.display());
        Ok(listener)
    }

    /// Sibling path for [`Self::bind`]'s staged socket.
    ///
    /// PID-scoped like the socket itself, then salted. The PID is what makes it
    /// safe: the staging directory is `$TMPDIR`, shared with every other codeg
    /// process, and two binds that picked the same staged name could interleave
    /// into real corruption — one process's `remove_file` clearing the other's
    /// staged entry, then its own bind recreating it under that name, so the
    /// first process's `rename` publishes the *second* process's socket at its
    /// path. No two live processes share a PID, so that can't happen across
    /// processes; the salt covers the only within-process caller that isn't
    /// already serialized by `DelegationService`'s state lock.
    ///
    /// The whole name is deliberately SHORTER than a real socket name
    /// (`codeg-delegation-<pid>.sock`): `sun_path` caps a unix socket address
    /// at 104 bytes on macOS, and a staged path longer than the real one could
    /// fail to bind where the real path would have succeeded — turning a safety
    /// measure into a startup failure. Replacing the file name rather than
    /// appending to it keeps the staged path the cheaper of the two.
    #[cfg(unix)]
    fn staging_socket_path(socket_path: &Path) -> PathBuf {
        let salt = uuid::Uuid::new_v4().simple().to_string();
        socket_path.with_file_name(format!(
            ".stg-{}-{}",
            std::process::id(),
            &salt[..8]
        ))
    }

    #[cfg(windows)]
    pub async fn bind(socket_path: &Path) -> std::io::Result<BoundSocket> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let path_str = socket_path.to_string_lossy().to_string();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path_str)?;
        tracing::info!("[delegation] listening on named pipe {path_str}");
        Ok(server)
    }

    /// Run the accept loop until the socket is unbound. Errors on accept are
    /// logged and the loop continues — a single bad connection can't bring
    /// down the listener.
    #[cfg(unix)]
    pub async fn accept_loop(
        self: Arc<Self>,
        listener: BoundSocket,
        _socket_path: PathBuf,
    ) -> std::io::Result<()> {
        loop {
            match listener.accept().await {
                Ok((mut conn, _)) => {
                    let me = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = me.serve_one(&mut conn).await {
                            tracing::error!("[delegation] connection failed: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("[delegation] accept failed: {e}");
                    // Brief backoff so a persistent accept error doesn't pin a core.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Windows variant: follow Tokio's recommended accept pattern — wait for a
    /// connect, immediately create the *next* server instance, then hand the
    /// connected instance off to a worker. This keeps a pipe instance
    /// available at all times, so clients calling `ClientOptions::open()`
    /// between connections don't see `NotFound`.
    #[cfg(windows)]
    pub async fn accept_loop(
        self: Arc<Self>,
        bound: BoundSocket,
        socket_path: PathBuf,
    ) -> std::io::Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let path_str = socket_path.to_string_lossy().to_string();
        let mut server = bound;
        loop {
            if let Err(e) = server.connect().await {
                tracing::error!("[delegation] connect failed: {e}");
                // Re-create the instance so the next iteration has a fresh
                // listener; a failed connect leaves the current one unusable.
                server = ServerOptions::new().create(&path_str)?;
                continue;
            }
            let connected = server;
            // Re-bind BEFORE serving the current client, so a client that
            // opens during this turn finds a server instance to connect to.
            server = ServerOptions::new().create(&path_str)?;
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let mut conn = connected;
                if let Err(e) = me.serve_one(&mut conn).await {
                    tracing::error!("[delegation] connection failed: {e}");
                }
            });
        }
    }

    /// Stream-generic per-connection handler. Exposed so unit tests can drive
    /// it over `tokio::io::duplex` instead of a real socket.
    pub async fn serve_one<C>(&self, conn: &mut C) -> std::io::Result<()>
    where
        C: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        let msg: BrokerMessage = read_frame(conn).await?;
        let resp = match msg {
            // Untokened on purpose — see `BrokerMessage::Ping`. Answered before
            // anything else is touched so the probe measures the serve path and
            // nothing more.
            BrokerMessage::Ping => BrokerResponse {
                outcome: serde_json::json!({ "ok": true }),
            },
            BrokerMessage::Call(req) => report_response(self.process(req).await)?,
            BrokerMessage::Status(req) => {
                // A status long-poll — especially `wait_ms = 0` (block until
                // terminal) — can park for the whole lifetime of the child.
                // Race it against peer-close on this one-shot connection so a
                // companion that cancels and drops the request socket doesn't
                // leave this task parked until the task happens to finish. A
                // status query has no side effects (unlike a delegation), so
                // abandoning the wait is safe and there's nothing to cancel
                // broker-side. The companion never writes a second frame on
                // this socket, so the probe read only resolves on EOF/error.
                let status_fut = self.process_status(req);
                tokio::pin!(status_fut);
                let mut probe = [0u8; 1];
                let reports = tokio::select! {
                    biased;
                    reports = &mut status_fut => reports,
                    _ = conn.read(&mut probe) => return Ok(()),
                };
                reports_response(reports)?
            }
            BrokerMessage::CancelTask(req) => report_response(self.process_cancel_task(req).await)?,
            BrokerMessage::ResumeTask(req) => report_response(self.process_resume_task(req).await)?,
            BrokerMessage::Feedback(req) => {
                // at-least-once delivery: READ pending notes (no mutation),
                // WRITE the response, and COMMIT them delivered ONLY on a
                // successful write. A dropped/failed write skips the commit, so
                // the notes stay pending for the agent's next check.
                match self.feedback_target(&req).await {
                    None => {
                        // Invalid token: return an empty envelope (no leak of
                        // whether any feedback exists), nothing to commit.
                        write_frame(conn, &feedback_response(&[])?).await?;
                    }
                    Some(parent_conn_id) => {
                        let pending = self
                            .feedback
                            .read_pending_feedback(&parent_conn_id)
                            .await;
                        // Read-only: the response carries the note ids
                        // (`_commit_ids`); delivery is committed LATER, by the
                        // companion's `CommitFeedback` once it actually returns
                        // the result to the agent. So a cancel that suppresses
                        // the agent-facing response leaves the notes pending.
                        write_frame(conn, &feedback_response(&pending)?).await?;
                    }
                }
                return Ok(());
            }
            BrokerMessage::CommitFeedback(req) => {
                self.process_commit_feedback(req).await;
                // Empty ack so the companion can confirm the listener saw it.
                BrokerResponse {
                    outcome: Value::Null,
                }
            }
            BrokerMessage::Ask(req) => {
                // Register the question (broadcasting the card) and park until
                // the user answers — racing peer-close exactly like `Status`.
                // The companion holds this connection open for the whole wait
                // and never writes a second frame, so the probe read only
                // resolves on EOF/error; a canceled tool call drops the
                // companion's future, closing this socket, which we observe and
                // tear the pending question down. An invalid token, a gone
                // connection, or a connection that already has a pending ask
                // (one-at-a-time) yields a `declined` outcome (the LLM proceeds
                // with its own judgment) rather than hanging.
                let Some(parent_conn_id) = self.ask_target(&req).await else {
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                };
                let Some(reg) = self
                    .questions
                    .register_question(&parent_conn_id, req.questions)
                    .await
                else {
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                };
                let question_id = reg.question_id;
                let mut answer_rx = reg.answer_rx;
                // Close the teardown race: `ask_target` validated the token, but the
                // parent connection may have been revoked + swept
                // (`cancel_questions_by_parent`) in the window before the insert
                // above — the sweep would have missed this just-registered entry,
                // leaving it parked until peer-close. The token is revoked before
                // the sweep, so a re-check that now finds it gone means teardown is
                // underway: cancel immediately so the ask can't linger.
                if self.tokens.lookup(&req.token).await.is_none() {
                    self.questions
                        .cancel_question(&parent_conn_id, &question_id)
                        .await;
                    write_frame(conn, &ask_declined_response()?).await?;
                    return Ok(());
                }
                let mut probe = [0u8; 1];
                let outcome = tokio::select! {
                    biased;
                    ans = &mut answer_rx => ans.ok(),
                    _ = conn.read(&mut probe) => {
                        self.questions
                            .cancel_question(&parent_conn_id, &question_id)
                            .await;
                        return Ok(());
                    }
                };
                let resp = match outcome {
                    Some(o) => ask_response(&o)?,
                    // Sender dropped without sending (connection teardown drain):
                    // surface a declined outcome so the tool returns cleanly.
                    None => ask_declined_response()?,
                };
                write_frame(conn, &resp).await?;
                return Ok(());
            }
            BrokerMessage::SessionInfo(req) => {
                // Read-only resolution (DB + a bounded transcript parse). No
                // peer-close race needed: unlike Status/Ask this never blocks on
                // a long-poll or a human — the bounded parse always completes —
                // and there is nothing to tear down on cancel.
                session_response(self.process_session_info(req).await)?
            }
            BrokerMessage::TaskProgress(req) => {
                task_ack_response(self.process_task_progress(req).await)?
            }
            BrokerMessage::TaskComplete(req) => {
                task_ack_response(self.process_task_complete(req).await)?
            }
            BrokerMessage::CreateAutomation(req) => {
                // A bounded DB write. Like SessionInfo it never long-polls, so
                // there is no peer-close race to run — and unlike Ask there is
                // nothing to tear down if the caller cancels: either the row
                // landed or it didn't, and the response is simply dropped.
                authoring_response(self.process_create_automation(req).await)?
            }
            BrokerMessage::CreateWorkTask(req) => {
                authoring_response(self.process_create_work_task(req).await)?
            }
            BrokerMessage::BrowserTabs(req) => {
                // A registry read. No peer-close race for the same reason as
                // SessionInfo: it cannot block on anything.
                browser_tabs_response(self.process_browser_tabs(req).await)?
            }
            BrokerMessage::BrowserSnapshot(req) => {
                // This one CAN take a moment — it evaluates in the page's
                // isolated world and waits for the answer — but it is bounded
                // by the read's own 15 s engine timeout rather than by a human
                // or a long-poll, and abandoning it early would leave the
                // activity line unwritten while the page had already been
                // read. So no peer-close race here either: the read runs to its
                // own end, and a caller that walked away simply gets no answer.
                browser_snapshot_response(self.process_browser_snapshot(req).await)?
            }
            BrokerMessage::BrowserAct(req) => {
                // Same shape as the read: bounded by the engine's own
                // timeouts, and an action that has happened has to leave its
                // line on the strip whether or not the caller is still there.
                browser_act_response(self.process_browser_act(req).await)?
            }
            BrokerMessage::BrowserConsole(req) => {
                // A registry read, like the listing; nothing to block on.
                browser_console_response(self.process_browser_console(req).await)?
            }
            BrokerMessage::BrowserCapture(req) => {
                // Bounded by the capture's own engine timeout, as the read
                // is; the line it leaves on the strip is written on the
                // codeg side whether or not the caller waits.
                browser_capture_response(self.process_browser_capture(req).await)?
            }
            BrokerMessage::BrowserEval(req) => {
                // The one browser message that waits on a human, so it can sit
                // here for a couple of minutes. Still no peer-close race: the
                // question is in front of a person, and whipping it away
                // because the agent's socket went quiet would train them to
                // dismiss dialogs that vanish. It is bounded by the
                // confirmation's own timeout either way.
                browser_eval_response(self.process_browser_eval(req).await)?
            }
            BrokerMessage::BrowserTabOp(req) => {
                // Bounded by the settle timeout on the codeg side. No
                // peer-close race for the same reason the snapshot arm has
                // none: dropping the future mid-flight would leave a tab open
                // (or a page navigated) with nothing on the strip to say so.
                browser_tab_op_response(self.process_browser_tab_op(req).await)?
            }
            BrokerMessage::ComputerApps(req) => {
                // One driver listing behind the helper, bounded by the
                // helper's own timeout. No peer-close race, as for the
                // browser listing: nothing here waits on a person.
                computer_response(&self.process_computer_apps(req).await)?
            }
            BrokerMessage::ComputerWindows(req) => {
                computer_response(&self.process_computer_windows(req).await)?
            }
            BrokerMessage::ComputerCapture(req) => {
                // Like the browser capture: the read runs to its own end, so
                // the activity line it writes on the codeg side is never
                // skipped because the caller walked away.
                computer_capture_response(self.process_computer_capture(req).await)?
            }
            BrokerMessage::ComputerSnapshot(req) => {
                computer_response(&self.process_computer_snapshot(req).await)?
            }
            BrokerMessage::ComputerVerify(req) => {
                computer_response(&self.process_computer_verify(req).await)?
            }
            BrokerMessage::ComputerAct(req) => {
                // Runs to its end like a read: an action cannot be recalled
                // halfway, and the line it leaves on the panel's activity
                // list must not depend on whether the caller is still there.
                computer_response(&self.process_computer_act(req).await)?
            }
            BrokerMessage::ComputerLaunch(req) => {
                // To its end, as an action: a started application cannot be
                // recalled either.
                computer_response(&self.process_computer_launch(req).await)?
            }
            BrokerMessage::ComputerClipboard(req) => {
                // To its end: a write cannot be recalled.
                computer_response(&self.process_computer_clipboard(req).await)?
            }
            BrokerMessage::Cancel(cancel) => {
                self.process_cancel(cancel).await;
                // Empty ack — the companion only uses this to detect the
                // listener has at least seen the cancel before dropping.
                BrokerResponse {
                    outcome: Value::Null,
                }
            }
        };
        write_frame(conn, &resp).await?;
        Ok(())
    }

    /// Validate the token, resolve the caller's parent connection/conversation,
    /// and query the status of every requested task id (optionally blocking per
    /// the wire `wait_ms`: omitted → immediate snapshot, explicit `0` → block
    /// until a task is terminal, a positive value → bounded long-poll clamped to
    /// [`STATUS_WAIT_MAX_MS`]). Backs the `get_delegation_status` tool. Returns
    /// one report per requested id, in request order. An invalid token reports
    /// `Unknown` for each id — the caller can't usefully distinguish it from a
    /// genuinely unknown task, and we don't leak which.
    async fn process_status(&self, req: BrokerStatusRequest) -> Vec<DelegationTaskReport> {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return req.task_ids.iter().map(|id| unknown_report(id)).collect();
        };
        // Identity-less hosts (Cursor) announce this MCP call as a generic
        // "MCP: tool" and never upgrade it on the wire — the companion
        // round-trip landing here is the FIRST moment anything knows the call
        // is `get_delegation_status`. Restore the live card's identity before
        // running the (possibly long-poll-blocked) status query; on ordinary
        // hosts the broker's sticky gate makes this a no-op.
        let mut rename_input = serde_json::Map::new();
        rename_input.insert(
            "task_ids".into(),
            serde_json::Value::Array(
                req.task_ids
                    .iter()
                    .map(|id| serde_json::Value::String(id.clone()))
                    .collect(),
            ),
        );
        if let Some(ms) = req.wait_ms {
            rename_input.insert("wait_ms".into(), serde_json::Value::Number(ms.into()));
        }
        self.broker
            .rewrite_identityless_tool_call(
                &entry.parent_connection_id,
                crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE,
                serde_json::Value::Object(rename_input),
            )
            .await;
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        // Map the wire `wait_ms` to a wait mode: omitted → immediate poll, an
        // explicit `0` → block with no timeout (long-running children), any
        // positive value → bounded long-poll clamped to the hard ceiling.
        let wait = match req.wait_ms {
            None => StatusWait::Immediate,
            Some(0) => StatusWait::Infinite,
            Some(ms) => StatusWait::Bounded(ms.min(STATUS_WAIT_MAX_MS)),
        };
        self.broker
            .get_tasks_status(
                &entry.parent_connection_id,
                parent_conversation_id,
                &req.task_ids,
                wait,
            )
            .await
    }

    /// Validate the token, resolve the caller's parent, and cancel the task.
    /// Backs the `cancel_delegation` tool.
    async fn process_cancel_task(&self, req: BrokerCancelTaskRequest) -> DelegationTaskReport {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return unknown_report(&req.task_id);
        };
        // Same identity restoration as `process_status` — see the comment
        // there. `cancel_delegation` results are free-form text, so the
        // completion-time sniff can't cover this tool; the call-time rename
        // is its only identity source on identity-less hosts.
        let mut rename_input = serde_json::Map::new();
        rename_input.insert(
            "task_id".into(),
            serde_json::Value::String(req.task_id.clone()),
        );
        self.broker
            .rewrite_identityless_tool_call(
                &entry.parent_connection_id,
                crate::acp::delegation::CANCEL_TOOL_REWRITE_TITLE,
                serde_json::Value::Object(rename_input),
            )
            .await;
        let parent_conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        self.broker
            .cancel_task_by_id(
                &entry.parent_connection_id,
                parent_conversation_id,
                &req.task_id,
            )
            .await
    }

    /// Validate the token, resolve the caller's parent, and resume the task.
    /// Backs the `resume_delegation` tool. Unlike `delegate_to_agent`, the
    /// parent conversation is REQUIRED here even though the broker could
    /// technically look the child up without it: ownership of a resumable task
    /// is proven by the child row's `parent_id` matching the caller's current
    /// conversation, so a caller with no conversation has no claim to resume
    /// anything.
    async fn process_resume_task(&self, req: BrokerResumeTaskRequest) -> DelegationTaskReport {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return unknown_report(&req.task_id);
        };
        // Same call-time identity restoration as `process_status` /
        // `process_cancel_task`: like cancel, the resume result is free-form
        // report text with no stable prefix for the completion-time sniff, so
        // this rename is the only identity source on identity-less hosts.
        let mut rename_input = serde_json::Map::new();
        rename_input.insert(
            "task_id".into(),
            serde_json::Value::String(req.task_id.clone()),
        );
        if let Some(reason) = req.reason.as_deref() {
            rename_input.insert(
                "reason".into(),
                serde_json::Value::String(reason.to_string()),
            );
        }
        self.broker
            .rewrite_identityless_tool_call(
                &entry.parent_connection_id,
                crate::acp::delegation::RESUME_TOOL_REWRITE_TITLE,
                serde_json::Value::Object(rename_input),
            )
            .await;
        let Some(parent_conversation_id) = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await
        else {
            return cancel("parent has no active conversation");
        };
        self.broker
            .resume_delegation(ResumeDelegationRequest {
                parent_connection_id: entry.parent_connection_id,
                parent_conversation_id,
                task_id: req.task_id,
                reason: req.reason,
                external_handle: req.external_handle,
            })
            .await
    }

    /// Validate the token and resolve the `check_user_feedback` target: the
    /// caller's parent connection id. `None` on an invalid token — the LLM can't
    /// usefully distinguish "no notes" from "bad token", and we don't leak which.
    async fn feedback_target(&self, req: &BrokerFeedbackRequest) -> Option<String> {
        let entry = self.tokens.lookup(&req.token).await?;
        Some(entry.parent_connection_id)
    }

    /// Validate the token and resolve the `ask_user_question` target: the
    /// caller's parent connection id. `None` on an invalid token — the LLM gets
    /// a `declined` outcome (proceed with judgment), and we don't leak which.
    async fn ask_target(&self, req: &BrokerAskRequest) -> Option<String> {
        let entry = self.tokens.lookup(&req.token).await?;
        Some(entry.parent_connection_id)
    }

    /// Mark the named feedback notes delivered, after the companion confirms it
    /// returned them to the agent. Token-scoped to the parent connection. Unknown
    /// tokens are dropped (no LLM on the receiving end to react).
    async fn process_commit_feedback(&self, req: BrokerCommitFeedbackRequest) {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return;
        };
        self.feedback
            .commit_feedback_delivered(&entry.parent_connection_id, req.ids)
            .await;
    }

    /// Validate token + dispatch cancel to the broker. Unknown tokens and
    /// parent-mismatched cancels are silently dropped — there's no LLM on
    /// the receiving end of this method to react to errors.
    async fn process_cancel(&self, cancel: BrokerCancelRequest) {
        let Some(_entry) = self.tokens.lookup(&cancel.token).await else {
            return;
        };
        let reason = cancel
            .reason
            .unwrap_or_else(|| "mcp client canceled".into());
        self.broker
            .cancel_by_external_handle(&cancel.external_handle, reason)
            .await;
    }

    /// Validate the token and resolve the `get_session_info` target. An invalid
    /// token yields a `found:false` outcome (the LLM can't usefully distinguish it
    /// from a deleted session, and we don't leak which).
    ///
    /// SCOPE (deliberate, user-confirmed): the lookup is by codeg conversation id
    /// and is intentionally NOT scoped to the caller's parent connection or to the
    /// session ids actually referenced in the prompt — any non-deleted session
    /// resolves. This is sound in codeg's single-tenant trust model: there is no
    /// per-user isolation anywhere (desktop is one local user; server mode shares
    /// one `CODEG_TOKEN` + one data dir across an operator's devices), the user can
    /// already open every session in the UI, and the agent already has full
    /// filesystem access to every agent's raw session files via its own tools — so
    /// reading session metadata by id is strictly less capability than the agent
    /// already holds, not an escalation. The token gate above still prevents an
    /// unrelated process from reaching the broker at all.
    async fn process_session_info(&self, req: BrokerSessionRequest) -> SessionInfo {
        if self.tokens.lookup(&req.token).await.is_none() {
            return SessionInfo::not_found(req.session_id);
        }
        self.session_info
            .resolve(req.session_id, req.max_messages.unwrap_or(0))
            .await
    }

    /// Validate the token and list the browser tabs an agent may know about.
    ///
    /// An invalid token gets the same answer a runtime with no browser gets:
    /// an empty list. Consistent with every other arm here — a caller that
    /// cannot prove it is a companion learns nothing, not even whether the
    /// user has any tabs open.
    ///
    /// Not scoped to the caller's parent connection, for the reason spelled
    /// out on [`BrokerBrowserTabsRequest`]: tabs belong to the user, and what
    /// an agent may read of one is the per-tab grant.
    async fn process_browser_tabs(&self, req: BrokerBrowserTabsRequest) -> BrowserTabsOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserTabsOutcome::default();
        }
        self.browser.list_tabs().await
    }

    /// Validate the token and read one shared page.
    ///
    /// An invalid token is told the tab does not exist — the same answer a
    /// wrong id gets, so a caller off the street cannot use the refusal codes
    /// to probe which tabs are open or which of them are shared.
    async fn process_browser_snapshot(
        &self,
        req: BrokerBrowserSnapshotRequest,
    ) -> BrowserSnapshotOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserSnapshotOutcome::refused(
                &req.tab_id,
                ERROR_NO_SUCH_TAB,
                format!("No browser tab {} is open.", req.tab_id),
            );
        }
        self.browser.snapshot(&req.tab_id, req.max_chars).await
    }

    /// Validate the token and act on one shared page. An invalid token gets
    /// the same "no such tab" a wrong id gets, for the reason given on
    /// [`Self::process_browser_snapshot`].
    async fn process_browser_act(&self, req: BrokerBrowserActRequest) -> BrowserActOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserActOutcome::refused(
                &req.tab_id,
                ERROR_NO_SUCH_TAB,
                format!("No browser tab {} is open.", req.tab_id),
            );
        }
        self.browser.act(&req.tab_id, req.request).await
    }

    /// Validate the token and read one shared page's console; an invalid
    /// token gets "no such tab", as everywhere here.
    async fn process_browser_console(
        &self,
        req: BrokerBrowserConsoleRequest,
    ) -> BrowserConsoleOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserConsoleOutcome::refused(
                &req.tab_id,
                ERROR_NO_SUCH_TAB,
                format!("No browser tab {} is open.", req.tab_id),
            );
        }
        self.browser.console(&req.tab_id, req.query).await
    }

    /// Validate the token and capture one shared page; an invalid token gets
    /// "no such tab", as everywhere here.
    async fn process_browser_capture(
        &self,
        req: BrokerBrowserCaptureRequest,
    ) -> BrowserCaptureOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserCaptureOutcome::refused(
                &req.tab_id,
                ERROR_NO_SUCH_TAB,
                format!("No browser tab {} is open.", req.tab_id),
            );
        }
        self.browser.capture(&req.tab_id, req.request).await
    }

    /// Validate the token and run one snippet on one shared page; an invalid
    /// token gets "no such tab", as everywhere here.
    ///
    /// Checking the token BEFORE the access impl matters more here than
    /// anywhere else on this surface: the impl is what raises the dialog, and
    /// a caller with no standing must not be able to put a question in front
    /// of the user at all.
    async fn process_browser_eval(&self, req: BrokerBrowserEvalRequest) -> BrowserEvalOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return BrowserEvalOutcome::refused(
                &req.tab_id,
                ERROR_NO_SUCH_TAB,
                format!("No browser tab {} is open.", req.tab_id),
            );
        }
        self.browser.eval(&req.tab_id, req.request).await
    }

    /// Validate the token and open / navigate / close one tab.
    ///
    /// The token is checked before the access impl, as everywhere here: an
    /// invalid one must not be able to open a tab on the user's screen, and
    /// hears the same "no such tab" every other unauthenticated round trip
    /// gets. An `Open` has no tab to name, so it hears the note instead —
    /// which reveals nothing either, being what a session with the browser
    /// switched off also hears.
    async fn process_browser_tab_op(&self, req: BrokerBrowserTabOpRequest) -> BrowserTabOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            let tab_id = req.op.tab_id();
            return BrowserTabOutcome::refused(
                tab_id,
                ERROR_NO_SUCH_TAB,
                match tab_id {
                    Some(tab_id) => format!("No browser tab {tab_id} is open."),
                    None => crate::acp::browser_tools::NO_BROWSER_NOTE.to_string(),
                },
            );
        }
        self.browser.tab_op(req.op).await
    }

    /// Validate the token and list the running applications. An invalid token
    /// hears what a runtime with no desktop hears: nothing is listed, so a
    /// caller that cannot prove it is a companion learns nothing about what
    /// is running.
    async fn process_computer_apps(&self, req: BrokerComputerAppsRequest) -> ComputerAppsOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerAppsOutcome::default();
        }
        self.computer.list_apps().await
    }

    /// Validate the token and list the windows. Same unauthenticated answer as
    /// the application listing.
    async fn process_computer_windows(
        &self,
        req: BrokerComputerWindowsRequest,
    ) -> ComputerWindowsOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerWindowsOutcome::default();
        }
        self.computer.list_windows(req.pid).await
    }

    /// An invalid token is told the window does not exist — the answer a
    /// wrong id gets — so the refusal codes cannot be used from outside to
    /// probe which windows exist or which are shared.
    async fn process_computer_capture(
        &self,
        req: BrokerComputerCaptureRequest,
    ) -> ComputerCaptureOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerCaptureOutcome::refused(
                &req.target_id,
                ERROR_NO_SUCH_TARGET,
                crate::acp::computer_tools::no_such_target_note(&req.target_id),
            );
        }
        self.computer.capture(&req.target_id, req.max_dimension).await
    }

    async fn process_computer_snapshot(
        &self,
        req: BrokerComputerSnapshotRequest,
    ) -> ComputerSnapshotOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerSnapshotOutcome::refused(
                &req.target_id,
                ERROR_NO_SUCH_TARGET,
                crate::acp::computer_tools::no_such_target_note(&req.target_id),
            );
        }
        self.computer.snapshot(&req.target_id, req.request).await
    }

    async fn process_computer_verify(
        &self,
        req: BrokerComputerVerifyRequest,
    ) -> ComputerVerifyOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerVerifyOutcome::refused(
                &req.target_id,
                ERROR_NO_SUCH_TARGET,
                crate::acp::computer_tools::no_such_target_note(&req.target_id),
            );
        }
        self.computer.verify(&req.target_id, req.request).await
    }

    /// Same unauthenticated answer as a read: an invalid token is told the
    /// window does not exist, and nothing is done.
    async fn process_computer_act(&self, req: BrokerComputerActRequest) -> ComputerActOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return ComputerActOutcome::refused(
                &req.target_id,
                ERROR_NO_SUCH_TARGET,
                crate::acp::computer_tools::no_such_target_note(&req.target_id),
            );
        }
        self.computer
            .act(&req.target_id, req.request, req.delivery)
            .await
    }

    /// Validate the token and start the application. An invalid token hears
    /// what a runtime with no desktop hears, and nothing is started.
    async fn process_computer_launch(
        &self,
        req: BrokerComputerLaunchRequest,
    ) -> crate::acp::computer_tools::ComputerLaunchOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return crate::acp::computer_tools::ComputerLaunchOutcome::refused(
                crate::acp::computer_tools::ERROR_UNAVAILABLE,
                crate::acp::computer_tools::NO_DESKTOP_NOTE,
            );
        }
        self.computer.launch_app(req.name, req.key).await
    }

    /// Validate the token and read or write the clipboard. An invalid token
    /// hears what a runtime with no desktop hears, and nothing is done.
    async fn process_computer_clipboard(
        &self,
        req: crate::acp::delegation::transport::BrokerComputerClipboardRequest,
    ) -> crate::acp::computer_tools::ComputerClipboardOutcome {
        if self.tokens.lookup(&req.token).await.is_none() {
            return crate::acp::computer_tools::ComputerClipboardOutcome::refused(
                crate::acp::computer_tools::ERROR_UNAVAILABLE,
                crate::acp::computer_tools::NO_DESKTOP_NOTE,
            );
        }
        self.computer.clipboard(req.op).await
    }

    /// Validate the token and hand the progress report to the task engine,
    /// which resolves the parent connection to its owning task + generation.
    async fn process_task_progress(&self, req: BrokerTaskProgressRequest) -> TaskReportAck {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return TaskReportAck::rejected("invalid token");
        };
        self.tasks
            .report_progress(&entry.parent_connection_id, &req.message)
            .await
    }

    /// Validate the token and hand the final verdict to the task engine.
    async fn process_task_complete(&self, req: BrokerTaskCompleteRequest) -> TaskReportAck {
        let Some(entry) = self.tokens.lookup(&req.token).await else {
            return TaskReportAck::rejected("invalid token");
        };
        self.tasks
            .complete(
                &entry.parent_connection_id,
                &req.verdict,
                req.summary.as_deref(),
            )
            .await
    }

    /// Resolve the caller's [`AuthoringContext`] from its per-launch token: the
    /// conversation it is currently in (for defaulting the target project) plus
    /// the working directory recorded at injection. `None` when the token is
    /// invalid — the caller gets a soft refusal, not a leak of whether the token
    /// merely expired.
    async fn authoring_context(&self, token: &str) -> Option<AuthoringContext> {
        let entry = self.tokens.lookup(token).await?;
        let conversation_id = self
            .parent_lookup
            .current_conversation_id(&entry.parent_connection_id)
            .await;
        Some(AuthoringContext {
            conversation_id,
            working_dir: entry.working_dir,
        })
    }

    /// Validate the token and hand the automation spec to the authoring impl,
    /// which re-checks the feature flag before writing.
    async fn process_create_automation(
        &self,
        req: BrokerCreateAutomationRequest,
    ) -> AuthoringOutcome {
        let Some(ctx) = self.authoring_context(&req.token).await else {
            return AuthoringOutcome::rejected("automation", "invalid token");
        };
        self.authoring.create_automation(ctx, req.spec).await
    }

    /// Validate the token and hand the task spec to the authoring impl.
    async fn process_create_work_task(&self, req: BrokerCreateWorkTaskRequest) -> AuthoringOutcome {
        let Some(ctx) = self.authoring_context(&req.token).await else {
            return AuthoringOutcome::rejected("work_task", "invalid token");
        };
        self.authoring.create_work_task(ctx, req.spec).await
    }

    async fn process(&self, req: BrokerRequest) -> DelegationTaskReport {
        // 1. Token + parent_connection_id consistency check. Treat both as
        //    "canceled" since the LLM can't usefully react to either —
        //    the parent has either been torn down or is impersonating.
        let entry = match self.tokens.lookup(&req.token).await {
            Some(e) => e,
            None => return cancel("invalid token"),
        };
        if entry.parent_connection_id != req.parent_connection_id {
            return cancel("token does not match parent connection");
        }

        // 2. Resolve the parent's current conversation. Without one the
        //    broker can't link the child row to the parent.
        let parent_conversation_id = match self
            .parent_lookup
            .current_conversation_id(&req.parent_connection_id)
            .await
        {
            Some(id) => id,
            None => return cancel("parent has no active conversation"),
        };

        // 3. Parse the delegate_to_agent arguments. Schema validation lives
        //    on the LLM side; we only enforce what the broker can't.
        let agent_type = match req.input.get("agent_type").and_then(|v| v.as_str()) {
            Some(raw) => match parse_agent_type(raw) {
                Some(t) => t,
                None => return invalid_agent_type(raw),
            },
            None => return invalid_agent_type(""),
        };
        let task = match req.input.get("task").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => {
                return report_failed("invalid_working_dir", "missing or empty task");
            }
        };
        // The `working_dir` the LLM explicitly passed (before defaulting),
        // used by the broker's correlation key. `None` when omitted —
        // symmetric with the ACP `raw_input`, which also omits it then.
        let requested_working_dir = req
            .input
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let working_dir = requested_working_dir
            .clone()
            .or_else(|| Some(entry.working_dir.to_string_lossy().to_string()));

        let delegation_req = DelegationRequest {
            parent_connection_id: req.parent_connection_id,
            parent_conversation_id,
            parent_tool_use_id: req.parent_tool_use_id,
            agent_type,
            task,
            working_dir,
            requested_working_dir,
            external_handle: req.external_handle,
        };
        self.broker.start_delegation(delegation_req).await
    }
}

/// Serialize a [`DelegationTaskReport`] into a [`BrokerResponse`] for the wire.
/// Used by the `Call` / `CancelTask` arms, which each resolve to one report.
fn report_response(report: DelegationTaskReport) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&report).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a batch of [`DelegationTaskReport`]s into a `{ "tasks": [..] }`
/// envelope for the `Status` arm. The companion reads this back and renders it
/// uniformly as a `{ "tasks": [..] }` result — one entry per requested id,
/// whether the poll asked for a single id or a whole fan-out.
fn reports_response(reports: Vec<DelegationTaskReport>) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::json!({
            "tasks": serde_json::to_value(&reports).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
            })?,
        }),
    })
}

/// Serialize the pending feedback notes into a
/// `{ "count": N, "feedback": [..], "_commit_ids": [..] }` envelope for the
/// `Feedback` arm. Only the lean `text` + `created_at` reach the agent; the
/// `_commit_ids` are internal — the companion echoes them back in a
/// `CommitFeedback` once it delivers the result, and `render_feedback_result`
/// strips them from the agent-facing output. `count == 0` is "no new feedback".
fn feedback_response(items: &[PendingFeedback]) -> std::io::Result<BrokerResponse> {
    let notes: Vec<Value> = items
        .iter()
        .map(|p| serde_json::json!({ "text": p.text, "created_at": p.created_at }))
        .collect();
    let ids: Vec<&str> = items.iter().map(|p| p.id.as_str()).collect();
    Ok(BrokerResponse {
        outcome: serde_json::json!({
            "count": notes.len(),
            "feedback": notes,
            "_commit_ids": ids,
        }),
    })
}

/// Serialize a resolved [`QuestionOutcome`] into a [`BrokerResponse`] for the
/// `Ask` arm — the `{ answers, declined }` envelope the companion renders.
fn ask_response(outcome: &QuestionOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a resolved [`SessionInfo`] into a [`BrokerResponse`] for the
/// `SessionInfo` arm — the companion renders it into the `get_session_info`
/// tool result.
fn session_response(info: SessionInfo) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&info).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserTabsOutcome`] into a [`BrokerResponse`] for the
/// `BrowserTabs` arm — the companion renders it into the `browser_list_tabs`
/// tool result.
fn browser_tabs_response(outcome: BrowserTabsOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserSnapshotOutcome`] into a [`BrokerResponse`] for the
/// `BrowserSnapshot` arm — the companion renders it into the `browser_snapshot`
/// tool result.
fn browser_snapshot_response(
    outcome: BrowserSnapshotOutcome,
) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserActOutcome`] into a [`BrokerResponse`] for the
/// `BrowserAct` arm — the companion renders it into the action tool's result.
fn browser_act_response(outcome: BrowserActOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserConsoleOutcome`] for the `BrowserConsole` arm.
fn browser_console_response(outcome: BrowserConsoleOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserEvalOutcome`] for the `BrowserEval` arm. No size guard
/// like the capture's: what a snippet can send back is already bounded twice,
/// in the page's renderer and again in `EvalOutcome::from_answer`.
fn browser_eval_response(outcome: BrowserEvalOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserTabOutcome`] for the `BrowserTabOp` arm. Nothing to
/// guard for size: the answer is a tab summary at most.
fn browser_tab_op_response(outcome: BrowserTabOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`BrowserCaptureOutcome`] for the `BrowserCapture` arm. The
/// image rides inside as base64. The capture pipeline keeps it far under the
/// frame cap, but this is the last place before the frame is written, so it
/// is measured here too: a capture the companion would refuse to read is
/// answered with a refusal it can, rather than with a broken round trip.
fn browser_capture_response(outcome: BrowserCaptureOutcome) -> std::io::Result<BrokerResponse> {
    let encode = |outcome: &BrowserCaptureOutcome| {
        serde_json::to_vec(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })
    };
    let mut bytes = encode(&outcome)?;
    if bytes.len() > CAPTURE_RESPONSE_MAX_BYTES {
        let refused = BrowserCaptureOutcome::refused(
            &outcome.tab_id,
            crate::acp::browser_tools::ERROR_READ_FAILED,
            format!(
                "The screenshot came out too large to deliver ({} bytes). Ask for a smaller \
                 `maxWidth`, or `format: \"jpeg\"`.",
                bytes.len()
            ),
        );
        bytes = encode(&refused)?;
    }
    Ok(BrokerResponse {
        outcome: serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// What a serialized capture outcome may weigh: the frame cap less room for
/// the envelope around it.
const CAPTURE_RESPONSE_MAX_BYTES: usize = super::transport::MAX_FRAME_BYTES - 64 * 1024;

/// Serialize any computer-use outcome for its arm. Nothing but the capture
/// can come near the frame cap: a snapshot's tree is cut to `maxChars` on the
/// codeg side, and the listings and verdicts are small.
fn computer_response(outcome: &impl serde::Serialize) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`ComputerCaptureOutcome`], measured against the frame cap for
/// the same reason as the browser's: a capture the companion could not read
/// is answered with a refusal it can.
fn computer_capture_response(outcome: ComputerCaptureOutcome) -> std::io::Result<BrokerResponse> {
    let encode = |outcome: &ComputerCaptureOutcome| {
        serde_json::to_vec(outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })
    };
    let mut bytes = encode(&outcome)?;
    if bytes.len() > CAPTURE_RESPONSE_MAX_BYTES {
        let refused = ComputerCaptureOutcome::refused(
            &outcome.target_id,
            crate::acp::computer_tools::ERROR_READ_FAILED,
            format!(
                "The screenshot came out too large to deliver ({} bytes). Ask for a smaller \
                 `maxDimension`.",
                bytes.len()
            ),
        );
        bytes = encode(&refused)?;
    }
    Ok(BrokerResponse {
        outcome: serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize a [`TaskReportAck`] into a [`BrokerResponse`] for the
/// `TaskProgress` / `TaskComplete` arms — the companion renders it into the
/// tool result.
fn task_ack_response(ack: TaskReportAck) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&ack).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// Serialize an [`AuthoringOutcome`] into a [`BrokerResponse`] for the
/// `CreateAutomation` / `CreateWorkTask` arms — the companion renders it into
/// the tool result.
fn authoring_response(outcome: AuthoringOutcome) -> std::io::Result<BrokerResponse> {
    Ok(BrokerResponse {
        outcome: serde_json::to_value(&outcome).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
        })?,
    })
}

/// The `declined` outcome — used when the token is invalid, the connection is
/// gone, or the answer one-shot was dropped without a response. The LLM reads it
/// as "the user didn't answer; proceed with your own judgment".
fn ask_declined_response() -> std::io::Result<BrokerResponse> {
    ask_response(&QuestionOutcome {
        answers: Vec::new(),
        declined: true,
    })
}

/// A `Canceled` report for a setup-side rejection the LLM can't react to (bad
/// token, parent gone). Mirrors the old `cancel(..)` DelegationOutcome.
fn report_canceled(message: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: None,
        status: TaskStatus::Canceled,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: Some("canceled".into()),
        message: Some(message.into()),
        duration_ms: None,
        blocked_on: None,
    }
}

/// A `Failed` report carrying a wire-stable `error_code` for a bad argument.
fn report_failed(error_code: &str, message: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: None,
        status: TaskStatus::Failed,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: Some(error_code.into()),
        message: Some(message.into()),
        duration_ms: None,
        blocked_on: None,
    }
}

/// An `Unknown` report — used when a status/cancel request fails the token
/// check (we don't leak whether the task exists).
fn unknown_report(task_id: &str) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: TaskStatus::Unknown,
        child_conversation_id: None,
        agent_type: None,
        text: None,
        error_code: None,
        message: Some("unknown task id".into()),
        duration_ms: None,
        blocked_on: None,
    }
}

fn cancel(message: &str) -> DelegationTaskReport {
    report_canceled(message)
}

fn invalid_agent_type(raw: &str) -> DelegationTaskReport {
    if raw.is_empty() {
        report_failed("invalid_agent_type", "missing agent_type")
    } else {
        report_failed("invalid_agent_type", &format!("invalid agent_type: {raw}"))
    }
}

fn parse_agent_type(raw: &str) -> Option<AgentType> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// Whether `path` fits in `sockaddr_un::sun_path`.
///
/// STRICTLY less than the cap: the array has to hold the terminating NUL too,
/// so its last byte is never available to the path. Same rule, and the same
/// constant, as [`crate::acp::scratch_dir`] applies to the temp directory it
/// hands a child — this is codeg's own end of the identical budget.
#[cfg(unix)]
fn fits_sun_path(path: &Path) -> bool {
    path.as_os_str().len() < SUN_PATH_CAP
}

/// Short fallback directory for the broker socket, used when the ambient temp
/// directory is too long to hold one.
///
/// `/tmp` because it is the shortest directory POSIX guarantees exists, and
/// euid-scoped for the reason [`crate::acp::scratch_dir`] gives for its own
/// twin: `/tmp` is shared with every other account on the machine, so an
/// unscoped name would be owned by whichever user ran codeg first and
/// uncreatable by all the rest. [`DelegationListener::bind`] creates it `0700`,
/// keeping the socket as private as it was in the per-user `$TMPDIR` this
/// stands in for.
///
/// Deliberately NOT the scratch root. That directory is swept — entries are
/// enumerated and deleted by pid — and a long-lived socket has no business
/// sharing a namespace whose invariant is "everything here is disposable".
///
/// The cost of staying out of it: nothing reclaims this directory either, so a
/// process that dies without unlinking leaves one dead `.sock` inode behind
/// until the OS temp reaper gets to it. That is not a regression — the primary
/// `$TMPDIR/codeg-delegation-<pid>.sock` has always had exactly the same
/// property, and a stale entry is inert (pid-scoped, never consulted, replaced
/// by `rename` if the pid is ever recycled). Worth knowing before anyone adds a
/// sweep: it would need to cover BOTH locations or it would just move the leak.
#[cfg(unix)]
fn short_socket_dir() -> PathBuf {
    PathBuf::from(format!(
        "/tmp/codeg-{}",
        // Always succeeds; `geteuid` has no failure mode.
        unsafe { libc::geteuid() }
    ))
}

/// Default socket path for the running process, scoped to PID so multiple
/// codeg instances on the same machine don't collide.
///
/// Unix: a `.sock` file inside `temp_dir`, or — when that would not fit in
/// `sun_path` — inside [`short_socket_dir`]. The fallback is not hypothetical
/// housekeeping: codeg exports a ~72-byte per-session `TMPDIR` to the agents it
/// launches, so a codeg started from inside one is already within a few bytes
/// of the macOS cap, and a container or a hand-set `TMPDIR` clears it outright.
/// Without the fallback that produced a socket nobody could dial and no error
/// anywhere — see [`DelegationListener::bind`].
///
/// Windows: a named pipe address `\\.\pipe\codeg-delegation-<pid>`. Windows
/// named pipes live in their own kernel namespace and ignore `temp_dir`; the
/// argument is kept for signature parity across platforms.
#[cfg(unix)]
pub fn default_socket_path(temp_dir: &Path) -> PathBuf {
    let name = format!("codeg-delegation-{}.sock", std::process::id());
    let preferred = temp_dir.join(&name);
    if fits_sun_path(&preferred) {
        return preferred;
    }
    // No third candidate, because there is no third case to handle: this is
    // `/tmp/codeg-` + at most 10 digits of euid + `/codeg-delegation-` + at
    // most 10 digits of pid + `.sock` — 54 bytes at its absolute widest, or
    // half the smallest `sun_path` any of these platforms has.
    let short = short_socket_dir().join(&name);
    tracing::info!(
        "[delegation] {} is {} bytes, past the {}-byte AF_UNIX limit; \
         binding {} instead",
        preferred.display(),
        preferred.as_os_str().len(),
        SUN_PATH_CAP - 1,
        short.display(),
    );
    short
}

#[cfg(windows)]
pub fn default_socket_path(_temp_dir: &Path) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\codeg-delegation-{}", std::process::id()))
}

#[cfg(test)]
mod tests;

