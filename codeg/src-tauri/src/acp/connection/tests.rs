use super::*;
use agent_client_protocol::schema::v1::{Diff, SessionConfigId};

/// Unwrap a select selector. The Grok synthesizers below only ever build
/// selects, so any other kind is a test failure rather than a branch to
/// handle — this keeps the assertions as terse as the irrefutable `let`
/// they replaced (which stopped compiling once `SessionConfigKindInfo`
/// gained its `Boolean` variant).
fn expect_select(kind: &SessionConfigKindInfo) -> &SessionConfigSelectInfo {
    match kind {
        SessionConfigKindInfo::Select(sel) => sel,
        other => panic!("expected a select config option, got {other:?}"),
    }
}

// ── PermissionQueue (#442) ──────────────────────────────────────────────
//
// The queue is what stops N concurrent `session/request_permission`s from
// collapsing into the single card slot and stranding the losers' responders
// forever. Driven here through a stub responder because the runtime's `Responder`
// has private fields and no public constructor.

/// How a stubbed responder was settled. `Ord` so the drain assertions can
/// sort — `HashMap::drain` yields an arbitrary order.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum StubSettled {
    Selected,
    Cancelled,
}

/// Records `(request_id, how)` into a shared log when settled, so a test can
/// assert that EVERY parked responder was answered exactly once — the whole
/// point of the queue is that none is silently dropped.
struct StubResponder {
    request_id: String,
    log: Arc<std::sync::Mutex<Vec<(String, StubSettled)>>>,
    /// Stands in for the request's `$/cancel_request` marker.
    withdrawn: Arc<std::sync::atomic::AtomicBool>,
}

impl PermissionResponder for StubResponder {
    fn respond_selected(self, _option_id: String) {
        self.log
            .lock()
            .unwrap()
            .push((self.request_id, StubSettled::Selected));
    }

    fn respond_cancelled(self) {
        self.log
            .lock()
            .unwrap()
            .push((self.request_id, StubSettled::Cancelled));
    }

    fn is_withdrawn(&self) -> bool {
        self.withdrawn.load(std::sync::atomic::Ordering::SeqCst)
    }
}

type StubLog = Arc<std::sync::Mutex<Vec<(String, StubSettled)>>>;

/// Admit `id` into `queue`; returns the card the queue says to publish now.
fn admit_stub(
    queue: &mut PermissionQueue<StubResponder>,
    log: &StubLog,
    id: &str,
) -> Option<QueuedPermission> {
    admit_stub_with_marker(queue, log, id, Default::default())
}

/// [`admit_stub`] with the handle that marks `id` withdrawn — what a
/// `$/cancel_request` does to a real responder, ahead of its watcher.
fn admit_stub_with_marker(
    queue: &mut PermissionQueue<StubResponder>,
    log: &StubLog,
    id: &str,
    withdrawn: Arc<std::sync::atomic::AtomicBool>,
) -> Option<QueuedPermission> {
    queue.admit(
        StubResponder {
            request_id: id.to_string(),
            log: Arc::clone(log),
            withdrawn,
        },
        QueuedPermission {
            request_id: id.to_string(),
            tool_call: serde_json::json!({ "toolCallId": id }),
            options: vec![],
        },
    )
}

fn stub_queue() -> (PermissionQueue<StubResponder>, StubLog) {
    (
        PermissionQueue::default(),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    )
}

#[test]
fn permission_queue_shows_only_the_first_of_concurrent_requests() {
    let (mut q, log) = stub_queue();
    // The regression itself: three approvals arrive back-to-back (codex
    // running parallel shell commands). Before the queue, each emit
    // overwrote the last and the first two became unanswerable.
    let first = admit_stub(&mut q, &log, "a").expect("first request must be shown");
    assert_eq!(first.request_id, "a");
    assert!(
        admit_stub(&mut q, &log, "b").is_none(),
        "a second concurrent request must queue, not replace the visible card"
    );
    assert!(admit_stub(&mut q, &log, "c").is_none());
    assert_eq!(q.waiting_len(), 2);
    assert_eq!(q.showing.as_deref(), Some("a"));
    assert!(log.lock().unwrap().is_empty(), "nothing settled yet");
}

#[test]
fn permission_queue_promotes_in_fifo_order_and_settles_every_responder() {
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");
    admit_stub(&mut q, &log, "c");

    let after_a = q.resolve("a", "allow".into());
    assert!(after_a.answered);
    assert_eq!(
        after_a.next.map(|c| c.request_id).as_deref(),
        Some("b"),
        "answering the visible card must promote the FIFO head"
    );
    let after_b = q.resolve("b", "allow".into());
    assert_eq!(after_b.next.map(|c| c.request_id).as_deref(), Some("c"));
    let after_c = q.resolve("c", "allow".into());
    assert!(after_c.answered);
    assert!(
        after_c.next.is_none(),
        "queue drained, nothing left to show"
    );
    assert_eq!(q.showing, None);
    assert_eq!(q.waiting_len(), 0);

    // Every one of the three agent tool calls got an answer — the bug was
    // that two of them never did.
    let settled = log.lock().unwrap().clone();
    assert_eq!(
        settled,
        vec![
            ("a".to_string(), StubSettled::Selected),
            ("b".to_string(), StubSettled::Selected),
            ("c".to_string(), StubSettled::Selected),
        ]
    );
}

#[test]
fn permission_queue_ignores_unknown_and_duplicate_answers() {
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");

    let unknown = q.resolve("nope", "allow".into());
    assert!(
        !unknown.answered,
        "an unknown id must not advance the queue (the caller emits nothing)"
    );
    assert_eq!(q.showing.as_deref(), Some("a"));

    assert!(q.resolve("a", "allow".into()).answered);
    // Two clients racing the same card: the second answer must not consume
    // an already-settled responder or promote a second time.
    let again = q.resolve("a", "reject".into());
    assert!(!again.answered);
    assert_eq!(q.showing.as_deref(), Some("b"));
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn permission_queue_answering_a_queued_card_removes_it_from_the_queue() {
    // Defensive path: a stale client answers a card that never reached the
    // screen. Its queue entry must go too, or promoting it later would show
    // a card whose responder is already consumed.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");
    admit_stub(&mut q, &log, "c");

    let out = q.resolve("b", "allow".into());
    assert!(out.answered);
    assert!(
        out.next.is_none(),
        "answering a non-visible card must not change what is on screen"
    );
    assert_eq!(q.showing.as_deref(), Some("a"));
    assert_eq!(q.waiting_len(), 1);

    assert_eq!(
        q.resolve("a", "allow".into()).next.map(|c| c.request_id),
        Some("c".to_string()),
        "the dead entry must be skipped by removal, not by a promote-time check"
    );
}

#[test]
fn permission_queue_drain_cancels_all_and_reports_the_visible_card() {
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");
    admit_stub(&mut q, &log, "c");

    assert_eq!(
        q.drain().as_deref(),
        Some("a"),
        "the visible card must be reported so the caller can emit a \
             compensating PermissionResolved — without it the card lingers on \
             every client with no live responder (the idle-Cancel ghost)"
    );
    assert_eq!(q.showing, None);
    assert_eq!(q.waiting_len(), 0);

    let mut settled = log.lock().unwrap().clone();
    settled.sort();
    assert_eq!(
        settled,
        vec![
            ("a".to_string(), StubSettled::Cancelled),
            ("b".to_string(), StubSettled::Cancelled),
            ("c".to_string(), StubSettled::Cancelled),
        ],
        "queued responders must be cancelled too, not leaked"
    );
}

#[test]
fn permission_queue_drain_aborts_detached_requests_and_leaves_no_dead_handles() {
    // pi's extension-UI select is answered on the question card, so it parks
    // only an abort handle here (#644). A drain MUST fire it: pi dispatches
    // the dialog detached and resolves a timed one locally, so an unreclaimed
    // bridge leaves pi blocked and wedges the connection's one-ask slot,
    // sending every later select back to the raw approval card.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    let (tx, mut rx) = oneshot::channel::<()>();
    q.park_detached(tx);
    assert!(
        rx.try_recv().is_err() && !rx.is_terminated(),
        "still parked"
    );

    assert_eq!(q.drain().as_deref(), Some("a"));
    assert!(
        matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed)),
        "the drain must drop the abort handle so the bridged task wakes"
    );

    // Parking sweeps handles whose task already finished, so a long-lived
    // connection doesn't accumulate them.
    let (dead_tx, dead_rx) = oneshot::channel::<()>();
    q.park_detached(dead_tx);
    drop(dead_rx);
    let (live_tx, _live_rx) = oneshot::channel::<()>();
    q.park_detached(live_tx);
    assert_eq!(q.detached.len(), 1, "the closed handle must be swept");
}

#[tokio::test]
async fn a_detached_request_drained_before_its_task_starts_still_aborts() {
    // Why `try_bridge_pi_select_ask` parks its abort handle BEFORE
    // registering the question: registration and parking are separate await
    // points, so a drain can land between them. Parking first makes that
    // window safe rather than merely narrow — dropping the sender is sticky,
    // so the bridged task, which only starts afterwards, still observes it on
    // its first poll and reclaims the ask instead of parking forever.
    let (mut q, _log) = stub_queue();
    let (abort_tx, abort_rx) = oneshot::channel::<()>();
    q.park_detached(abort_tx);
    q.drain();

    // The answer channel is deliberately left OPEN and unresolved, so the
    // only thing that can settle this select is the drained abort handle.
    let (_answer_tx, answer_rx) = oneshot::channel::<u8>();
    let answered = tokio::select! {
        biased;
        answered = answer_rx => answered.ok(),
        _ = abort_rx => None,
    };
    assert!(
        answered.is_none(),
        "a drain that lands before the task starts must still abort it — \
             otherwise the ask survives the drain and wedges the connection's \
             one-ask slot, sending every later select back to the raw card"
    );
}

#[test]
fn permission_queue_drain_with_nothing_shown_needs_no_compensation() {
    let (mut q, _log) = stub_queue();
    assert!(
        q.drain().is_none(),
        "an empty queue must not emit a spurious PermissionResolved"
    );
}

#[test]
fn permission_queue_admits_again_after_drain() {
    // The TurnComplete wedge guard: if a drain left `showing` set, every
    // later permission on this connection would queue behind a card that can
    // never be answered — reproducing the very symptom being fixed.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");
    q.drain();

    let fresh = admit_stub(&mut q, &log, "c");
    assert_eq!(
        fresh.map(|c| c.request_id).as_deref(),
        Some("c"),
        "a post-drain request must be shown immediately"
    );
    assert_eq!(q.showing.as_deref(), Some("c"));
}

#[test]
fn permission_queue_admit_then_drain_never_leaves_an_inert_card() {
    // The interleaving that motivated putting the responder map and the
    // queue under ONE lock: admit publishes a card, a Cancel drains, and the
    // card must be reported for compensation rather than left up.
    let (mut q, log) = stub_queue();
    let shown = admit_stub(&mut q, &log, "a").expect("shown");
    assert_eq!(q.drain().as_deref(), Some(shown.request_id.as_str()));
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![("a".to_string(), StubSettled::Cancelled)]
    );
}

#[test]
fn permission_queue_withdrawing_the_visible_card_promotes_the_next() {
    // The agent took back the approval on screen (claude aborts a tool
    // call's permission when the call is stopped) while another waits: the
    // dead card must give way now, not hold the screen until the turn ends.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");

    let Some(Departure::Shown { next }) = q.withdraw("a") else {
        panic!("`a` was the card on screen");
    };
    assert_eq!(next.map(|c| c.request_id).as_deref(), Some("b"));
    assert_eq!(q.showing.as_deref(), Some("b"));
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![("a".to_string(), StubSettled::Cancelled)],
        "only the withdrawn request is answered, and as cancelled"
    );
}

#[test]
fn permission_queue_never_shows_a_card_withdrawn_while_queued() {
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    admit_stub(&mut q, &log, "b");
    admit_stub(&mut q, &log, "c");

    assert!(matches!(q.withdraw("b"), Some(Departure::Queued)));
    assert_eq!(q.showing.as_deref(), Some("a"), "the screen is untouched");
    assert_eq!(q.waiting_len(), 1);
    assert_eq!(
        q.resolve("a", "allow".into())
            .next
            .map(|c| c.request_id)
            .as_deref(),
        Some("c"),
        "answering the visible card skips straight past the withdrawn one"
    );
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![
            ("b".to_string(), StubSettled::Cancelled),
            ("a".to_string(), StubSettled::Selected),
        ]
    );
}

/// A burst of withdrawals: `b` is already taken back when `a` leaves the
/// screen, but its own watcher has not run yet. Promoting it would flash a
/// dead card on every client and push it to chat channels, so the queue
/// answers it `Cancelled` on the spot and shows `c` — whichever way `a`
/// left.
#[test]
fn permission_queue_never_promotes_a_card_already_withdrawn() {
    for withdraw_a in [false, true] {
        let (mut q, log) = stub_queue();
        admit_stub(&mut q, &log, "a");
        let b_withdrawn = Arc::new(std::sync::atomic::AtomicBool::new(false));
        admit_stub_with_marker(&mut q, &log, "b", Arc::clone(&b_withdrawn));
        admit_stub(&mut q, &log, "c");
        b_withdrawn.store(true, std::sync::atomic::Ordering::SeqCst);

        let next = if withdraw_a {
            let Some(Departure::Shown { next }) = q.withdraw("a") else {
                panic!("`a` was the card on screen");
            };
            next
        } else {
            q.resolve("a", "allow".into()).next
        };
        assert_eq!(next.map(|c| c.request_id).as_deref(), Some("c"));
        assert_eq!(q.showing.as_deref(), Some("c"));
        assert_eq!(q.waiting_len(), 0);
        let a_answer = if withdraw_a {
            StubSettled::Cancelled
        } else {
            StubSettled::Selected
        };
        assert_eq!(
            log.lock().unwrap().clone(),
            vec![
                ("b".to_string(), StubSettled::Cancelled),
                ("a".to_string(), a_answer),
            ]
        );
        // `b`'s watcher arriving afterwards finds nothing left to do.
        assert!(q.withdraw("b").is_none());
    }

    // When every card behind the departing one is withdrawn, nothing is
    // promoted and the screen clears.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    let b_withdrawn = Arc::new(std::sync::atomic::AtomicBool::new(true));
    admit_stub_with_marker(&mut q, &log, "b", b_withdrawn);
    assert!(q.resolve("a", "allow".into()).next.is_none());
    assert_eq!(q.showing, None);
    assert_eq!(q.waiting_len(), 0);
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![
            ("b".to_string(), StubSettled::Cancelled),
            ("a".to_string(), StubSettled::Selected),
        ]
    );
}

#[test]
fn permission_queue_withdrawal_after_an_answer_is_a_no_op() {
    // The race a withdrawal loses whenever the user clicked first.
    let (mut q, log) = stub_queue();
    admit_stub(&mut q, &log, "a");
    assert!(q.resolve("a", "allow".into()).answered);
    assert!(q.withdraw("a").is_none());
    assert!(q.withdraw("never-admitted").is_none());
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![("a".to_string(), StubSettled::Selected)]
    );
}

/// A card's watcher never retires anything once the card has left the
/// queue, even when the withdrawal is ready at the same moment — a plain
/// `tokio::select!` would pick either arm at random, hence the repeats.
#[tokio::test]
async fn a_settled_permission_is_never_treated_as_withdrawn() {
    for _ in 0..64 {
        let (settled_tx, settled_rx) = oneshot::channel::<()>();
        drop(settled_tx);
        assert!(!withdrawn_while_parked(settled_rx, std::future::ready(())).await);
    }
    let (_parked, settled_rx) = oneshot::channel::<()>();
    assert!(withdrawn_while_parked(settled_rx, std::future::ready(())).await);
}

/// The withdrawal path end to end, over the real runtime: the agent raises
/// `session/request_permission` and takes it back with `$/cancel_request`,
/// exactly as claude-agent-acp does when the tool call's signal aborts. It
/// must get its answer (`Cancelled`) without anyone touching the card, and
/// the card the client showed must be retracted.
#[tokio::test]
async fn an_agent_withdrawing_a_permission_request_retires_its_card() {
    use agent_client_protocol::schema::v1::{
        PermissionOption, ToolCallUpdate, ToolCallUpdateFields,
    };
    use agent_client_protocol::Channel;

    let (client_end, agent_end) = Channel::duplex();
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-test".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    )));
    let perms = PendingPermissions::default();

    let agent = tokio::spawn(Agent.builder().connect_with(
        agent_end,
        async |cx: ConnectionTo<Client>| {
            let request = cx.send_request_to(
                Client,
                RequestPermissionRequest::new(
                    SessionId::new("s1"),
                    ToolCallUpdate::new("call-1", ToolCallUpdateFields::new().title("Bash")),
                    vec![PermissionOption::new(
                        "allow",
                        "Allow",
                        PermissionOptionKind::AllowOnce,
                    )],
                ),
            );
            request.cancel()?;
            request.block_task().await
        },
    ));

    let client = tokio::spawn({
        let state = Arc::clone(&state);
        let perms = perms.clone();
        Client
            .builder()
            .on_receive_request(
                async move |req: RequestPermissionRequest,
                            responder: Responder<RequestPermissionResponse>,
                            cx: ConnectionTo<Agent>| {
                    handle_permission_request(
                        &cx,
                        &state,
                        &EventEmitter::Noop,
                        &perms,
                        "/tmp",
                        AgentType::ClaudeCode,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                },
                on_receive_request!(),
            )
            .connect_with(client_end, async |_cx: ConnectionTo<Agent>| {
                std::future::pending::<Result<(), agent_client_protocol::Error>>().await
            })
    });

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), agent)
        .await
        .expect("a withdrawn request is answered without the user")
        .expect("agent task")
        .expect("a response, not an error");
    client.abort();
    assert!(
        matches!(response.outcome, RequestPermissionOutcome::Cancelled),
        "{:?}",
        response.outcome
    );

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");
    let shown = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::PermissionRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        })
        .expect("the card was shown");
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            AcpEvent::PermissionResolved { request_id } if *request_id == shown
        )),
        "the card must be retracted, not left up with no one behind it"
    );
    assert!(perms.lock().await.showing.is_none());
}

/// An agent request codeg has no handler for still gets an answer. One
/// that names the session is claimed by the session router — ahead of the
/// runtime's own `method_not_found` fallback — so without an explicit reply
/// the agent would block on it forever.
#[tokio::test]
async fn an_unhandled_session_request_is_answered_method_not_found() {
    use agent_client_protocol::Channel;

    let (client_end, agent_end) = Channel::duplex();
    let agent = tokio::spawn(Agent.builder().connect_with(
        agent_end,
        async |cx: ConnectionTo<Client>| {
            let request =
                UntypedMessage::new("_vendor/ask", serde_json::json!({"sessionId": "s1"}))?;
            Ok(cx.send_request_to(Client, request).block_task().await)
        },
    ));
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-test".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    )));
    let client = tokio::spawn(Client.builder().connect_with(
        client_end,
        async move |cx: ConnectionTo<Agent>| {
            let mut session =
                AgentSession::attach(&cx, NewSessionResponse::new(SessionId::new("s1")))?;
            let dispatch = session.read_update().await?;
            maybe_emit_ext_notification(
                &state,
                &EventEmitter::Noop,
                AgentType::ClaudeCode,
                dispatch,
                &mut CodeBuddyLiveState::default(),
            )
            .await;
            std::future::pending::<Result<(), agent_client_protocol::Error>>().await
        },
    ));

    let answer = tokio::time::timeout(std::time::Duration::from_secs(5), agent)
        .await
        .expect("the agent must not be left waiting")
        .expect("agent task")
        .expect("agent connection");
    client.abort();
    let error = answer.expect_err("an error reply");
    assert_eq!(
        error.code,
        agent_client_protocol::schema::v1::ErrorCode::MethodNotFound
    );
}

/// Records the questions an elicitation registers and cancels, and never
/// answers any — only a withdrawal can settle them.
#[derive(Default)]
struct UnansweredQuestions {
    pending: tokio::sync::Mutex<Vec<oneshot::Sender<crate::acp::question::QuestionOutcome>>>,
    canceled: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::acp::question::SessionQuestionAccess for UnansweredQuestions {
    async fn register_question(
        &self,
        _parent_connection_id: &str,
        _questions: Vec<crate::acp::question::QuestionSpec>,
    ) -> Option<crate::acp::question::RegisteredQuestion> {
        let (tx, rx) = oneshot::channel();
        // Kept alive, so the answer channel stays open and unresolved.
        self.pending.lock().await.push(tx);
        Some(crate::acp::question::RegisteredQuestion {
            question_id: "q-1".to_string(),
            answer_rx: rx,
        })
    }

    async fn cancel_question(&self, _parent_connection_id: &str, question_id: &str) {
        self.canceled.lock().unwrap().push(question_id.to_string());
    }

    async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
}

/// codex abandons a `request_user_input` it stops waiting for — its
/// `autoResolutionMs` expired, or the turn was interrupted — with
/// `$/cancel_request`. The question card must go at once (it used to wait
/// on a timer guessed from `autoResolutionMs`), and codex still gets a
/// well-formed `cancel` reply.
#[tokio::test]
async fn codex_withdrawing_a_question_elicitation_retires_the_card() {
    use agent_client_protocol::Channel;

    let (client_end, agent_end) = Channel::duplex();
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-test".to_string(),
        AgentType::Codex,
        None,
        "win".to_string(),
        None,
    )));
    let questions = Arc::new(UnansweredQuestions::default());
    let ask_config = crate::acp::question::QuestionRuntimeConfig::new();
    ask_config
        .set(crate::acp::question::QuestionConfig { enabled: true })
        .await;
    let access: Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )> = Some((questions.clone(), ask_config));

    let agent = tokio::spawn(Agent.builder().connect_with(
        agent_end,
        async |cx: ConnectionTo<Client>| {
            let request = cx.send_request_to(
                Client,
                CodexElicitationRequest(serde_json::json!({
                    "mode": "form",
                    "sessionId": "s1",
                    "toolCallId": "item-1",
                    "message": "Codex needs your input to continue.",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {"q1": {
                            "type": "string",
                            "title": "Which approach?",
                            "description": "Approach",
                            "oneOf": [
                                {"const": "Incremental", "title": "Incremental"},
                                {"const": "Rewrite", "title": "Rewrite"},
                            ],
                        }},
                        "required": ["q1"],
                    },
                    "_meta": {"codex": {"autoResolutionMs": 30_000}},
                })),
            );
            request.cancel()?;
            request.block_task().await
        },
    ));

    let client = tokio::spawn({
        let state = Arc::clone(&state);
        let perms = PendingPermissions::default();
        Client
            .builder()
            .on_receive_request(
                async move |req: CodexElicitationRequest,
                            responder: Responder<serde_json::Value>,
                            cx: ConnectionTo<Agent>| {
                    handle_elicitation_request(
                        &cx,
                        &access,
                        &perms,
                        &state,
                        &EventEmitter::Noop,
                        "conn-test",
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                },
                on_receive_request!(),
            )
            .connect_with(client_end, async |_cx: ConnectionTo<Agent>| {
                std::future::pending::<Result<(), agent_client_protocol::Error>>().await
            })
    });

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), agent)
        .await
        .expect("a withdrawn elicitation is answered without the user")
        .expect("agent task")
        .expect("a response, not an error");
    client.abort();
    assert_eq!(response["action"], "cancel", "{response}");
    assert_eq!(
        questions.canceled.lock().unwrap().clone(),
        vec!["q-1".to_string()],
        "the question card must be retracted"
    );
}

#[test]
fn grok_ask_ext_request_routes_and_parses_captured_wire_shape() {
    use agent_client_protocol::JsonRpcMessage;
    // Routing: the derive matches ONLY the underscore-prefixed ext method
    // (the runtime routes typed handlers on the raw wire method — verified against
    // grok 0.2.101, where the missing underscore made codeg answer "unhandled"
    // and grok fall back to inert rendering).
    assert!(GrokAskUserQuestionRequest::matches_method(
        "_x.ai/ask_user_question"
    ));
    assert!(!GrokAskUserQuestionRequest::matches_method(
        "x.ai/ask_user_question"
    ));
    assert!(!GrokAskUserQuestionRequest::matches_method(
        "session/prompt"
    ));

    // The exact params grok sends (captured from a real 0.2.101 run): the
    // transparent newtype must deserialize them and the raw object must parse
    // into register-valid specs.
    let params = serde_json::json!({
        "sessionId": "019f70eb-32e5-7692-ae92-86fb6cb916a5",
        "toolCallId": "call-1af86ae7-ed54-440e-a983-2c5d22aa6682-0",
        "questions": [{
            "question": "What is your favorite color?",
            "options": [
                { "label": "Red", "description": "Red" },
                { "label": "Green", "description": "Green" },
                { "label": "Blue", "description": "Blue" }
            ],
            "multiSelect": false
        }],
        "mode": "default"
    });
    let req: GrokAskUserQuestionRequest = serde_json::from_value(params).unwrap();
    let specs = crate::acp::question::parse_grok_ext_questions(&req.0).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].question, "What is your favorite color?");
    assert_eq!(specs[0].options.len(), 3);
    assert!(!specs[0].multi_select);
    crate::acp::question::validate_specs(&specs).unwrap();
}

fn diff_content(path: &str, old: Option<&str>, new: &str) -> ToolCallContent {
    let mut d = Diff::new(path, new);
    if let Some(o) = old {
        d = d.old_text(o.to_string());
    }
    ToolCallContent::Diff(d)
}

/// Clone a `_meta` map out of a JSON object literal, mirroring how codex-acp
/// ships tool-call / session-info `_meta`.
fn meta_map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().expect("object").clone()
}

fn subagent_launch_input(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Value> {
    match classify_codex_subagent_activity(agent_type, meta) {
        CodexSubagentActivity::Started { input, .. } => {
            Some(serde_json::from_str(&input).expect("valid JSON"))
        }
        _ => None,
    }
}

#[test]
fn codex_subagent_launch_becomes_an_agent_capsule_input() {
    // codex 0.147 forwards nothing but this for a native sub-agent, so it is
    // the whole live signal: name from the path's last segment, thread id as
    // the card's badge, and no prompt (the task text is encrypted).
    let sub = meta_map(serde_json::json!({
        "codex": {
            "subagent": {
                "threadId": "01a0098a-7e8a",
                "path": "/root/pnpm_build",
                "activity": "started",
            }
        }
    }));
    assert_eq!(
        subagent_launch_input(AgentType::Codex, Some(&sub)),
        Some(serde_json::json!({
            "subagent_type": "pnpm_build",
            "agent_id": "01a0098a-7e8a",
            // Says the card stands for the LAUNCH, so its "completed" is
            // not read as "the sub-agent finished" — the parser writes the
            // same key on reload.
            crate::parsers::codex::CODEX_SUBAGENT_LAUNCH_KEY: true,
        }))
    );
    // A trailing slash / empty path must not produce a nameless capsule, and
    // a missing thread id just drops the badge rather than the whole card.
    let odd = meta_map(serde_json::json!({
        "codex": { "subagent": { "path": "/", "activity": "started" } }
    }));
    assert_eq!(
        subagent_launch_input(AgentType::Codex, Some(&odd)),
        Some(serde_json::json!({
            "subagent_type": "subagent",
            crate::parsers::codex::CODEX_SUBAGENT_LAUNCH_KEY: true,
        }))
    );
}

#[test]
fn codex_subagent_activity_classified_only_for_codex_subagent_meta() {
    let started = meta_map(serde_json::json!({
        "codex": { "subagent": { "threadId": "t1", "path": "/root/x", "activity": "started" } }
    }));
    // Only Codex is gated — the same meta never reshapes another agent's call.
    assert!(matches!(
        classify_codex_subagent_activity(AgentType::ClaudeCode, Some(&started)),
        CodexSubagentActivity::None
    ));
    // A mid-life marker stays dropped: it carries no content and would open
    // a second, indistinguishable capsule for the same sub-agent.
    let interacted = meta_map(serde_json::json!({
        "codex": { "subagent": { "threadId": "t1", "path": "/root/x", "activity": "interacted" } }
    }));
    assert!(matches!(
        classify_codex_subagent_activity(AgentType::Codex, Some(&interacted)),
        CodexSubagentActivity::Other
    ));
    // A terminal marker is not dropped — it is the only live signal that the
    // child stopped working, and it is routed onto the LAUNCH capsule (its
    // own `toolCallId` is a synthetic `subagent-completed-<uuid>` codeg has
    // never seen), keyed by the thread id.
    for kind in ["completed", "interrupted"] {
        let terminal = meta_map(serde_json::json!({
            "codex": { "subagent": { "threadId": "t1", "path": "/root/x", "activity": kind } }
        }));
        match classify_codex_subagent_activity(AgentType::Codex, Some(&terminal)) {
            CodexSubagentActivity::Terminal {
                thread_id,
                kind: got,
            } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(got, kind);
            }
            other => panic!("{kind} should be terminal, got {other:?}"),
        }
        // Without a thread id there is no capsule to attribute it to, so it
        // degrades to the old drop rather than opening a stray card.
        let anonymous = meta_map(serde_json::json!({
            "codex": { "subagent": { "path": "/root/x", "activity": kind } }
        }));
        assert!(matches!(
            classify_codex_subagent_activity(AgentType::Codex, Some(&anonymous)),
            CodexSubagentActivity::Other
        ));
    }
    // Absent meta and sibling codex meta keys (goal / collaboration) are not
    // subagent activity and must render normally.
    assert!(matches!(
        classify_codex_subagent_activity(AgentType::Codex, None),
        CodexSubagentActivity::None
    ));
    let goal = meta_map(serde_json::json!({ "codex": { "goal": { "objective": "x" } } }));
    assert!(matches!(
        classify_codex_subagent_activity(AgentType::Codex, Some(&goal)),
        CodexSubagentActivity::None
    ));
    let collab = meta_map(serde_json::json!({
        "codex": { "collaboration": { "tool": "spawnAgent" } }
    }));
    assert!(matches!(
        classify_codex_subagent_activity(AgentType::Codex, Some(&collab)),
        CodexSubagentActivity::None
    ));
}

#[test]
fn codex_subagent_terminal_state_is_added_to_the_launch_input() {
    // The outcome rides `rawInput`, not `_meta`: `upsert_tool_call` replaces
    // meta wholesale but parses and swaps in a fresh raw_input, so re-sending
    // the launch's own input plus one key is the only patch that cannot drop
    // what codex-acp already put on the card.
    let launch = serde_json::json!({
        "subagent_type": "history_limits",
        "agent_id": "01a07fc2-db62",
        crate::parsers::codex::CODEX_SUBAGENT_LAUNCH_KEY: true,
    })
    .to_string();
    let settled: serde_json::Value =
        serde_json::from_str(&codex_subagent_terminal_input(&launch, "completed"))
            .expect("valid JSON");
    assert_eq!(
        settled,
        serde_json::json!({
            "subagent_type": "history_limits",
            "agent_id": "01a07fc2-db62",
            crate::parsers::codex::CODEX_SUBAGENT_LAUNCH_KEY: true,
            // The parser stamps the same key on reload, so the card reads
            // identically live and after a refresh.
            crate::parsers::codex::CODEX_SUBAGENT_STATE_KEY: "completed",
        })
    );
    // A launch whose input never parsed still yields a usable card rather
    // than propagating the damage.
    let recovered: serde_json::Value =
        serde_json::from_str(&codex_subagent_terminal_input("not json", "interrupted"))
            .expect("valid JSON");
    assert_eq!(
        recovered,
        serde_json::json!({
            crate::parsers::codex::CODEX_SUBAGENT_STATE_KEY: "interrupted",
        })
    );
}

#[test]
fn codex_plan_review_detected_only_for_codex_plan_review_meta() {
    // codex-acp #351: `_meta.codex.kind = "plan_review"` on the permission
    // REQUEST marks the Plan-mode review gate whose tool call was never
    // announced.
    let review = meta_map(serde_json::json!({
        "codex": { "kind": "plan_review", "planItemId": "item-7" }
    }));
    let is_review = |agent, id: &str, meta: Option<&serde_json::Map<String, serde_json::Value>>| {
        codex_plan_review_meta(agent, id, true, meta).is_some()
    };
    assert!(is_review(
        AgentType::Codex,
        "plan-review:item-7",
        Some(&review)
    ));
    // The legacy marker alone is enough — it is the 1.x identity.
    assert!(is_review(AgentType::Codex, "anything", Some(&review)));
    // Gated on Codex: an identical meta from another agent seeds nothing.
    assert!(!is_review(
        AgentType::ClaudeCode,
        "plan-review:item-7",
        Some(&review)
    ));
    // Ordinary permission requests carry no meta at all.
    assert!(!is_review(AgentType::Codex, "cmd-1", None));
    // Sibling `codex` keys and other `kind` values must not seed a card.
    let other_kind = meta_map(serde_json::json!({ "codex": { "kind": "mcp_tool_call" } }));
    assert!(!is_review(AgentType::Codex, "cmd-1", Some(&other_kind)));
    let sub = meta_map(serde_json::json!({ "codex": { "subagent": { "threadId": "t1" } } }));
    assert!(!is_review(AgentType::Codex, "cmd-1", Some(&sub)));
    // A non-string `kind` must not be coerced into a match.
    let numeric = meta_map(serde_json::json!({ "codex": { "kind": 1 } }));
    assert!(!is_review(AgentType::Codex, "cmd-1", Some(&numeric)));
}

#[test]
fn codex_plan_review_is_recognised_by_id_once_the_marker_is_gone() {
    // codex-acp 2.0.0, recorded: the request carries NO `_meta` at all;
    // only `toolCallId: "plan-review:<planItemId>"` identifies the gate.
    let seeded = codex_plan_review_meta(AgentType::Codex, "plan-review:plan-2", true, None)
        .expect("the id identifies the gate");
    // The legacy marker is written onto the seeded card, which is what
    // the frontend names it by (`codexMarksPlanReview`).
    assert_eq!(
        serde_json::Value::Object(seeded),
        serde_json::json!({"codex": {"kind": "plan_review", "planItemId": "plan-2"}})
    );
    // A request `_meta` that says something else is kept beside it.
    let other = meta_map(serde_json::json!({ "jetbrains": { "air": { "version": 1 } } }));
    let seeded =
        codex_plan_review_meta(AgentType::Codex, "plan-review:p", true, Some(&other)).unwrap();
    assert_eq!(seeded["jetbrains"]["air"]["version"], 1);
    assert_eq!(seeded["codex"]["planItemId"], "p");
    assert!(codex_plan_review_meta(AgentType::Codex, "plan-review:", true, None).is_none());
    assert!(codex_plan_review_meta(AgentType::ClaudeCode, "plan-review:p", true, None).is_none());
    // The id shape alone, on a request that is not the mode switch, is not
    // the review gate.
    assert!(codex_plan_review_meta(AgentType::Codex, "plan-review:p", false, None).is_none());
}

#[test]
fn config_option_state_command_suppressed_only_for_codex_set_config_action() {
    // codex-acp #293: `/plan` is a config-option state toggle (rendered as the
    // `collaboration_mode` selector), not an invokable slash command.
    let plan = meta_map(serde_json::json!({
        "commandAction": {
            "kind": "setConfigOption",
            "configId": "collaboration_mode",
            "value": "plan",
            "resetValue": "default",
            "presentation": "state"
        }
    }));
    assert!(is_config_option_state_command(
        AgentType::Codex,
        Some(&plan)
    ));
    // Gated on Codex — the same meta never suppresses another agent's command.
    assert!(!is_config_option_state_command(
        AgentType::ClaudeCode,
        Some(&plan)
    ));
    // `/goal` uses a `prefixPrompt` action (takes an objective argument) → a
    // real command, kept.
    let goal = meta_map(serde_json::json!({
        "commandAction": { "kind": "prefixPrompt", "presentation": "state" }
    }));
    assert!(!is_config_option_state_command(
        AgentType::Codex,
        Some(&goal)
    ));
    // codex-acp 2.0.0 sends the action under `_meta.jetbrains.air` only
    // (recorded) — the same toggle, suppressed the same way.
    let air_plan = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "commandAction": {
            "kind": "setConfigOption",
            "configId": "collaboration_mode",
            "value": "plan",
            "resetValue": "default",
            "presentation": "state"
        }
    }}}));
    assert!(is_config_option_state_command(
        AgentType::Codex,
        Some(&air_plan)
    ));
    let air_goal = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "commandAction": { "kind": "prefixPrompt", "presentation": "state" }
    }}}));
    assert!(!is_config_option_state_command(
        AgentType::Codex,
        Some(&air_goal)
    ));
    // Ordinary commands (no `commandAction`) and absent meta are kept.
    assert!(!is_config_option_state_command(AgentType::Codex, None));
    let plain = meta_map(serde_json::json!({ "somethingElse": true }));
    assert!(!is_config_option_state_command(
        AgentType::Codex,
        Some(&plain)
    ));
}

#[test]
fn goal_control_action_roundtrips_codex_wire_values() {
    // codex-acp #293: `_codex/session/goal_control` expects lowercase
    // "pause" / "clear" on the wire, and the same strings arrive from the
    // tauri command / HTTP endpoint — both directions must match exactly.
    assert_eq!(
        serde_json::to_value(GoalControlAction::Pause).unwrap(),
        serde_json::json!("pause")
    );
    assert_eq!(
        serde_json::to_value(GoalControlAction::Clear).unwrap(),
        serde_json::json!("clear")
    );
    assert_eq!(
        serde_json::from_value::<GoalControlAction>(serde_json::json!("pause")).unwrap(),
        GoalControlAction::Pause
    );
    assert_eq!(
        serde_json::from_value::<GoalControlAction>(serde_json::json!("clear")).unwrap(),
        GoalControlAction::Clear
    );
}

// --- native steering: capability synthesis + wire helpers -------------
// (reuses the shared `meta_map` test helper defined above)

#[test]
fn init_advertises_steering_reads_the_top_level_meta_flag() {
    let on = meta_map(serde_json::json!({"steering": {"supported": true}}));
    assert!(init_advertises_steering(Some(&on)));

    let off = meta_map(serde_json::json!({"steering": {"supported": false}}));
    assert!(!init_advertises_steering(Some(&off)));

    // Wrong nesting (e.g. another convention's namespace) must not count.
    let nested = meta_map(serde_json::json!({"symposium": {"steering": {"supported": true}}}));
    assert!(!init_advertises_steering(Some(&nested)));

    // Non-bool / absent → false.
    let stringly = meta_map(serde_json::json!({"steering": {"supported": "true"}}));
    assert!(!init_advertises_steering(Some(&stringly)));
    assert!(!init_advertises_steering(None));
}

#[test]
fn init_advertises_goal_requires_integer_version_at_least_1() {
    // The real advertisements: codex-acp 1.2.0+ / claude-agent-acp 0.66.0+.
    let codex = meta_map(serde_json::json!({"goal": {
        "version": 1,
        "controlMethod": "_session/goal",
        "actions": ["set", "pause", "resume", "clear"],
    }}));
    assert!(init_advertises_goal(Some(&codex)));
    // Future versions must keep selecting the neutral channel.
    let v2 = meta_map(serde_json::json!({"goal": {"version": 2}}));
    assert!(init_advertises_goal(Some(&v2)));
    // claude-agent-acp 0.82.0 / codex-acp 2.0.0 advertise it under AIR
    // only (recorded `initialize` responses).
    let air = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "capabilities": ["sessionFailure"],
        "goal": {"version": 1, "controlMethod": "_session/goal", "actions": ["set", "clear"]},
    }}}));
    assert!(init_advertises_goal(Some(&air)));
    assert_eq!(
        goal_advertised_control(Some(&air)),
        Some((
            "_session/goal".to_string(),
            vec!["set".to_string(), "clear".to_string()]
        ))
    );

    // Fail closed onto the legacy channel: sub-1, non-integer, stringly,
    // absent, or wrongly-shaped advertisements.
    for bad in [
        serde_json::json!({"goal": {"version": 0}}),
        serde_json::json!({"goal": {"version": 1.5}}),
        serde_json::json!({"goal": {"version": "1"}}),
        serde_json::json!({"goal": {}}),
        serde_json::json!({"goal": true}),
        serde_json::json!({"steering": {"supported": true}}),
    ] {
        let meta = meta_map(bad);
        assert!(!init_advertises_goal(Some(&meta)));
    }
    assert!(!init_advertises_goal(None));
}

#[test]
fn session_info_goal_value_reads_exactly_one_channel() {
    // A transitional adapter double-publishing the same transition through
    // both namespaces (even across separate updates) must yield the goal
    // from exactly ONE channel — the one pinned at initialize.
    let both = meta_map(serde_json::json!({
        "goal": {"objective": "neutral", "status": "active"},
        "codex": {"goal": {"objective": "legacy", "status": "active"}},
    }));
    let neutral = session_info_goal_value(true, Some(&both)).expect("neutral value");
    assert_eq!(
        neutral.get("objective").and_then(|v| v.as_str()),
        Some("neutral")
    );
    let legacy = session_info_goal_value(false, Some(&both)).expect("legacy value");
    assert_eq!(
        legacy.get("objective").and_then(|v| v.as_str()),
        Some("legacy")
    );

    // Neutral-pinned connections ignore a legacy-only update (and vice
    // versa) — the two updates of a double-publish collapse to one marker.
    let legacy_only = meta_map(
        serde_json::json!({"codex": {"goal": {"objective": "legacy", "status": "active"}}}),
    );
    assert!(session_info_goal_value(true, Some(&legacy_only)).is_none());
    let neutral_only =
        meta_map(serde_json::json!({"goal": {"objective": "neutral", "status": "active"}}));
    assert!(session_info_goal_value(false, Some(&neutral_only)).is_none());

    // `goal: null` IS a value (the clear signal), not an absent key.
    let cleared = meta_map(serde_json::json!({"goal": null}));
    assert!(matches!(
        session_info_goal_value(true, Some(&cleared)),
        Some(v) if v.is_null()
    ));
    assert!(session_info_goal_value(false, Some(&cleared)).is_none());
    assert!(session_info_goal_value(true, None).is_none());

    // claude-agent-acp 0.82.0 / codex-acp 2.0.0 publish the snapshot —
    // and its clear — under `_meta.jetbrains.air.goal` (recorded).
    let air = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "goal": {"objective": "Ship it", "status": "active"},
    }}}));
    assert_eq!(
        session_info_goal_value(true, Some(&air))
            .and_then(|g| g.get("objective"))
            .and_then(|v| v.as_str()),
        Some("Ship it")
    );
    assert!(session_info_goal_value(false, Some(&air)).is_none());
    let air_cleared =
        meta_map(serde_json::json!({"jetbrains": {"air": {"version": 1, "goal": null}}}));
    assert!(matches!(
        session_info_goal_value(true, Some(&air_cleared)),
        Some(v) if v.is_null()
    ));
    // An AIR envelope that carries something else is no goal update.
    let failure = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1, "sessionFailure": {"id": "x", "revision": 1}
    }}}));
    assert!(session_info_goal_value(true, Some(&failure)).is_none());
}

// --- live ACP session title (`session_info_update.title`) --------------

/// Drive one `session_info_update` carrying `title` through
/// `emit_conversation_update`.
async fn drive_session_info_title(state: &Arc<RwLock<SessionState>>, title: &str) {
    drive_session_info_title_from(state, AgentType::CodeBuddy, title).await;
}

/// [`drive_session_info_title`] for `agent_type`.
async fn drive_session_info_title_from(
    state: &Arc<RwLock<SessionState>>,
    agent_type: AgentType,
    title: &str,
) {
    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "session_info_update",
        "title": title,
    }))
    .expect("valid session_info_update wire shape");
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    emit_conversation_update(
        state,
        &EventEmitter::Noop,
        agent_type,
        update,
        None,
        &mut cache,
        &mut cb,
    )
    .await;
}

/// Titles emitted on this connection so far, oldest first.
async fn emitted_native_titles(state: &Arc<RwLock<SessionState>>) -> Vec<String> {
    state
        .read()
        .await
        .recent_events_after(0)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| match &e.payload {
            AcpEvent::NativeSessionTitle { title } => Some(title.clone()),
            _ => None,
        })
        .collect()
}

fn title_test_state(conversation_id: Option<i32>) -> Arc<RwLock<SessionState>> {
    let mut st = SessionState::new(
        "conn-title".to_string(),
        AgentType::CodeBuddy,
        None,
        "win".to_string(),
        None,
    );
    st.conversation_id = conversation_id;
    Arc::new(RwLock::new(st))
}

/// A changed title emits; the SAME title arriving again does not. CodeBuddy
/// (`sendPendingTitleUpdate`) resends its 80-code-unit fallback after every
/// completed prompt with no last-sent guard of its own, and that string
/// differs from the 100-char title our own parser derives from the same
/// first message — so without this skip the sidebar name would flip on
/// every turn (live write, then session-file parse, repeat).
#[tokio::test]
async fn session_info_title_emits_once_and_skips_an_identical_repeat() {
    let state = title_test_state(Some(7));

    drive_session_info_title(&state, "Fix the login flow").await;
    drive_session_info_title(&state, "Fix the login flow").await;
    drive_session_info_title(&state, "  Fix the login flow  ").await; // same after trim
    drive_session_info_title(&state, "Fix the signup flow").await;

    assert_eq!(
        emitted_native_titles(&state).await,
        vec![
            "Fix the login flow".to_string(),
            "Fix the signup flow".to_string()
        ],
        "only a CHANGED title may reach the lifecycle worker"
    );
}

/// A title published before the first prompt binds the row has nowhere to
/// land, so it is dropped — and deliberately NOT remembered, so the resend
/// that follows `ConversationLinked` is still accepted. Guards the
/// `conversation_id.is_some()` half of the skip: a stale cache here would
/// leave the row "Untitled" for the rest of the connection.
#[tokio::test]
async fn session_info_title_dropped_while_unbound_is_accepted_after_link() {
    let state = title_test_state(None);

    drive_session_info_title(&state, "Fix the login flow").await;
    assert!(
        emitted_native_titles(&state).await.is_empty(),
        "no row to write to yet"
    );
    assert!(
        state.read().await.last_native_title.is_none(),
        "a dropped title must not poison the skip-cache"
    );

    state
        .write()
        .await
        .apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 7,
            folder_id: 1,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });

    drive_session_info_title(&state, "Fix the login flow").await;
    assert_eq!(
        emitted_native_titles(&state).await,
        vec!["Fix the login flow".to_string()],
        "the same title must be accepted once the row exists"
    );
}

/// Claude Code's live title is not written from the wire: when the CLI
/// cannot generate a title, claude-agent-acp publishes the session's
/// newest `last-prompt` record instead (measured on 0.89.1, registry
/// (qqq)), which the history parser never produces, so writing it would
/// flip the row on every detail fetch. It is held, normalized, for the
/// transcript watcher (`session_title::release_held_wire_title`).
#[tokio::test]
async fn a_claude_session_info_title_is_held_for_the_transcript_watcher() {
    let state = title_test_state(Some(7));

    drive_session_info_title_from(
        &state,
        AgentType::ClaudeCode,
        "  Fix the login flow please look at [auth.ts](file:///tmp/auth.ts) first ",
    )
    .await;

    assert!(emitted_native_titles(&state).await.is_empty());
    let s = state.read().await;
    assert_eq!(
        s.held_wire_title.as_deref(),
        Some("Fix the login flow please look at [auth.ts](file:///tmp/auth.ts) first")
    );
    assert!(s.last_native_title.is_none());
}

/// Goal-only / metadata-only `session_info_update`s (the common case for
/// codex `/goal` transitions) carry no title and must not queue the
/// lifecycle worker.
#[tokio::test]
async fn session_info_without_a_title_emits_no_native_title() {
    let state = title_test_state(Some(7));
    for wire in [
        serde_json::json!({"sessionUpdate": "session_info_update"}),
        serde_json::json!({"sessionUpdate": "session_info_update", "title": null}),
        serde_json::json!({"sessionUpdate": "session_info_update", "title": "   "}),
    ] {
        let update: SessionUpdate =
            serde_json::from_value(wire).expect("valid session_info_update wire shape");
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        emit_conversation_update(
            &state,
            &EventEmitter::Noop,
            AgentType::Codex,
            update,
            None,
            &mut cache,
            &mut cb,
        )
        .await;
    }
    assert!(emitted_native_titles(&state).await.is_empty());
}

#[test]
fn goal_advertised_control_reads_method_and_actions_or_falls_back() {
    // claude 0.66+ / codex 1.2+ advertisements.
    let claude = meta_map(serde_json::json!({"goal": {
        "version": 1,
        "controlMethod": "_session/goal",
        "actions": ["set", "clear"],
    }}));
    assert_eq!(
        goal_advertised_control(Some(&claude)),
        Some((
            "_session/goal".to_string(),
            vec!["set".to_string(), "clear".to_string()]
        ))
    );
    // Advertised-but-empty actions are honored as "no controls" — the
    // card must not offer affordances the adapter never implemented.
    let none_offered = meta_map(serde_json::json!({"goal": {
        "version": 1,
        "controlMethod": "_session/goal",
        "actions": [],
    }}));
    assert_eq!(
        goal_advertised_control(Some(&none_offered)),
        Some(("_session/goal".to_string(), Vec::new()))
    );
    // No neutral advertisement / no usable method ⇒ None (legacy
    // defaults stay in force).
    let no_goal = meta_map(serde_json::json!({"steering": {"supported": true}}));
    assert_eq!(goal_advertised_control(Some(&no_goal)), None);
    let blank_method = meta_map(serde_json::json!({"goal": {
        "version": 1,
        "controlMethod": "   ",
        "actions": ["clear"],
    }}));
    assert_eq!(goal_advertised_control(Some(&blank_method)), None);
    assert_eq!(goal_advertised_control(None), None);
}

#[test]
fn the_legacy_vocabulary_is_resolved_at_initialize_not_at_construction() {
    // A fresh session knows NOTHING: a snapshot read during the handshake
    // (which happens — `spawn_agent` returns before `initialize` lands)
    // must not hand the card a legacy pair it latches forever.
    let fresh = SessionState::new(
        "c-goal".to_string(),
        AgentType::ClaudeCode,
        None,
        "w".to_string(),
        None,
    );
    assert_eq!(fresh.goal_actions, None);
    assert_eq!(fresh.to_snapshot().goal_actions, None);
    // ... and `None` reaches the client as an explicit null, not as an
    // absent field — absent is reserved for a server too old to have it,
    // which the client still maps to the legacy pair.
    let wire = serde_json::to_value(fresh.to_snapshot()).unwrap();
    assert_eq!(wire.get("goal_actions"), Some(&serde_json::Value::Null));

    // Initialize is where both cases are decided. Advertised wins…
    assert_eq!(
        resolve_goal_control(Some((
            "_session/goal".to_string(),
            vec!["set".to_string(), "clear".to_string()],
        ))),
        (
            Some("_session/goal".to_string()),
            vec!["set".to_string(), "clear".to_string()]
        )
    );
    // …and a non-advertising adapter resolves to the legacy pair, keeping
    // the method the state was built with.
    assert_eq!(
        resolve_goal_control(None),
        (None, vec!["pause".to_string(), "clear".to_string()])
    );
}

#[test]
fn air_session_failure_requires_wellformed_versioned_envelope() {
    let ok = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "sessionFailure": {"id": "t1:error", "revision": 1},
    }}}));
    assert!(air_session_failure(Some(&ok)).is_some());

    // Missing/zero/stringly version, or a failure outside the air
    // envelope, must yield nothing.
    for bad in [
        serde_json::json!({"jetbrains": {"air": {"sessionFailure": {"id": "x", "revision": 1}}}}),
        serde_json::json!({"jetbrains": {"air": {"version": 0, "sessionFailure": {}}}}),
        serde_json::json!({"jetbrains": {"air": {"version": "1", "sessionFailure": {}}}}),
        serde_json::json!({"jetbrains": {"sessionFailure": {"id": "x", "revision": 1}}}),
        serde_json::json!({"air": {"version": 1, "sessionFailure": {}}}),
    ] {
        let meta = meta_map(bad);
        assert!(air_session_failure(Some(&meta)).is_none());
    }
    assert!(air_session_failure(None).is_none());
}

#[test]
fn response_session_failure_reads_the_prompt_response_carrier() {
    // claude `failActiveWithSessionFailure` / codex
    // `terminalFailurePromptResponse` both attach a turn's terminal record
    // to the prompt response `_meta` under the SAME jetbrains.air envelope
    // the update channel uses, with a disguised `end_turn` stop reason —
    // this carrier is the only wire delivery of claude terminal failures.
    let meta = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "sessionFailure": {
            "id": "prompt-1:error",
            "revision": 6,
            "category": "connection",
            "severity": "error",
            "title": "The connection to Claude was lost.",
            "actions": ["new_session"],
        },
    }}}));
    let record = response_session_failure(Some(&meta)).expect("record");
    assert_eq!(record.id, "prompt-1:error");
    assert_eq!(record.revision, 6);
    assert_eq!(record.severity, "error");
    assert_eq!(record.actions, vec!["new_session".to_string()]);

    // Same gates as the update channel: malformed envelope or missing
    // identity ⇒ no record (and no synthetic-empty suppression).
    let unversioned = meta_map(serde_json::json!({"jetbrains": {"air": {
        "sessionFailure": {"id": "x", "revision": 1},
    }}}));
    assert!(response_session_failure(Some(&unversioned)).is_none());
    let no_identity = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "sessionFailure": {"title": "no id"},
    }}}));
    assert!(response_session_failure(Some(&no_identity)).is_none());
    assert!(response_session_failure(None).is_none());
}

#[test]
fn parse_session_failure_record_requires_identity_and_stays_lenient() {
    // The real codex shape (SESSION_FAILURE_POLICY: auth_required →
    // access/[login]).
    let full = serde_json::json!({
        "id": "turn-9:error",
        "revision": 2,
        "category": "access",
        "severity": "error",
        "title": "Authentication required.",
        "details": "Token expired",
        "actions": ["login"],
    });
    let record = parse_session_failure_record(&full).expect("record");
    assert_eq!(record.id, "turn-9:error");
    assert_eq!(record.revision, 2);
    assert_eq!(record.category, "access");
    assert_eq!(record.severity, "error");
    assert_eq!(record.title, "Authentication required.");
    assert_eq!(record.details.as_deref(), Some("Token expired"));
    assert_eq!(record.actions, vec!["login".to_string()]);
    assert!(!record.resolved);

    // id + revision are HARD requirements — no identity, no merge.
    for bad in [
        serde_json::json!({"revision": 1, "title": "x"}),
        serde_json::json!({"id": "", "revision": 1}),
        serde_json::json!({"id": "   ", "revision": 1}),
        serde_json::json!({"id": "x", "title": "no revision"}),
        serde_json::json!({"id": "x", "revision": 0}),
        serde_json::json!({"id": "x", "revision": -1}),
        serde_json::json!({"id": "x", "revision": "1"}),
    ] {
        assert!(parse_session_failure_record(&bad).is_none(), "{bad:?}");
    }

    // Everything else is lenient: unknown vocabulary passes through as
    // strings, blanks default, non-string action entries are skipped.
    let sparse = serde_json::json!({
        "id": "notice-1",
        "revision": 1,
        "category": "quantum",
        "actions": ["retry", 42, "sing"],
    });
    let record = parse_session_failure_record(&sparse).expect("record");
    assert_eq!(record.category, "quantum");
    assert_eq!(record.severity, "error");
    assert_eq!(record.title, "");
    assert_eq!(record.details, None);
    assert_eq!(
        record.actions,
        vec!["retry".to_string(), "sing".to_string()]
    );
}

/// claude-agent-acp 0.74.0's mid-session sign-out record, verbatim off the
/// `session_info_update` channel (`sessionFailureMeta`: id, revision,
/// category, severity, title, details, actions — `kind`/`recoveryPolicy`
/// stay agent-internal). Two things in it changed with that release and
/// are worth pinning:
///
/// * the CLI's own "… Please run /login" prose moved OUT of `title` into
///   `details`, leaving the policy's client-neutral fallback as the title —
///   so the strip reads as a heading with the TUI advice behind its
///   expander, not the other way round;
/// * the record may carry a `reason` refinement. codeg never produces one
///   (it is `--hide-claude-auth`-only; see the registry entry), and the
///   parser reads fields individually, so an unknown key must simply ride
///   through instead of failing the record.
#[test]
fn parse_session_failure_record_reads_claude_074_sign_out() {
    let signed_out = serde_json::json!({
        "id": "sess-7:session-error:epoch-1:1",
        "revision": 1,
        "category": "access",
        "severity": "error",
        "title": "Sign in to continue using Claude.",
        "details": "Invalid API key · Please run /login",
        "actions": ["login"],
    });
    let record = parse_session_failure_record(&signed_out).expect("record");
    assert_eq!(record.category, "access");
    assert_eq!(record.severity, "error");
    assert_eq!(record.title, "Sign in to continue using Claude.");
    assert_eq!(
        record.details.as_deref(),
        Some("Invalid API key · Please run /login")
    );
    // The banner renders a Login button off exactly this.
    assert_eq!(record.actions, vec!["login".to_string()]);

    // Same record plus the release's new refinement: parsed identically.
    let mut with_reason = signed_out.clone();
    with_reason["reason"] = serde_json::json!("claude_subscription_not_supported");
    assert_eq!(
        parse_session_failure_record(&with_reason).expect("record"),
        record
    );
}

#[test]
fn client_capabilities_advertise_air_for_claude_and_codex_only() {
    // Both AIR speakers must send EXACTLY the shape the adapters gate on:
    // integer version >= 1 plus "sessionFailure" in the capabilities
    // array (`clientSupportsTypedSessionFailures` in codex,
    // `supportsAirSessionFailures` in claude).
    for agent in [AgentType::ClaudeCode, AgentType::Codex] {
        let caps = serde_json::to_value(build_client_capabilities(agent, HostToolsPolicy::Default))
            .unwrap();
        let air = caps
            .get("_meta")
            .and_then(|m| m.get("jetbrains"))
            .and_then(|j| j.get("air"))
            .unwrap_or_else(|| panic!("{agent:?} must advertise jetbrains.air"));
        assert_eq!(air.get("version").and_then(|v| v.as_i64()), Some(1));
        let capabilities = air
            .get("capabilities")
            .and_then(|c| c.as_array())
            .unwrap_or_else(|| panic!("{agent:?} must advertise an AIR capabilities array"));
        assert!(capabilities
            .iter()
            .any(|v| v.as_str() == Some("sessionFailure")));
        // And exactly this much. Adding a capability here is not free — it
        // is what turns the corresponding behavior on.
        //
        // "asyncTasks" IS wanted, from BOTH (claude-agent-acp 0.73.0,
        // codex-acp 1.10.0): it is the only channel that reports whether an
        // agent's background work is still alive, and the only one that can
        // stop it.
        //
        // "recommendedValue" IS wanted too, and from BOTH
        // (claude-agent-acp 0.76.0, codex-acp 1.11.0): it names each
        // selector's recommended row, and on claude it additionally
        // retires the ambiguous `default` row so the value codeg journals
        // per turn is a real model id. Advertising a capability an agent
        // has NOT implemented is how a future meaning gets claimed by
        // accident — this one is safe only because both adapters now ship
        // it, so it goes back to being per-agent the moment either pin
        // moves backwards.
        //
        // The other two stay out. "agentFileChangeReport"
        // (claude-agent-acp 0.69.0 / codex-acp 1.4.0) buys a clamped,
        // truncating subset of what the `workspace_state` watcher already
        // sees — on claude still for an extra model round-trip per turn, on
        // codex since 1.12.0 for free, by parsing the turn diff, which is
        // explicitly narrower still (it omits anything not done through
        // `apply_patch`). And
        // "nativeSubagentSessions" would make both adapters SUPPRESS the
        // `Agent`/`Task` tool call that codeg builds its whole subagent
        // rendering around, replacing it with an announcement that carries
        // no parent tool-use id to rebuild it from. See the reasoning at
        // the advertisement site before relaxing this.
        //
        // "diffPatch" (codex-acp 2.0.0) is codex's alone: it trades 2.0.0's
        // per-hunk text fragments for one exact Git patch per file. claude
        // would send its patch in the permission request, whose placeholder
        // text the permission card cannot tell from an emptied file. Also
        // left out: "rawInputRendering" (it removes the question text a
        // message-only MCP elicitation's permission card shows) and
        // "planFile" (the plan card renders the plan TEXT).
        let mut expected: Vec<serde_json::Value> = vec![
            "sessionFailure".into(),
            "asyncTasks".into(),
            "recommendedValue".into(),
        ];
        if agent == AgentType::Codex {
            expected.push("diffPatch".into());
        }
        assert_eq!(
            capabilities, &expected,
            "{agent:?} advertises an unexpected AIR capability set"
        );
    }
    // Claude keeps its subagent-transcript flag alongside.
    let claude = serde_json::to_value(build_client_capabilities(
        AgentType::ClaudeCode,
        HostToolsPolicy::Default,
    ))
    .unwrap();
    assert_eq!(
        claude
            .get("_meta")
            .and_then(|m| m.get("subagent-transcript"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    // Non-AIR agents advertise nothing under jetbrains.
    for agent in [AgentType::Gemini, AgentType::Grok, AgentType::OpenCode] {
        let caps = serde_json::to_value(build_client_capabilities(agent, HostToolsPolicy::Default))
            .unwrap();
        assert!(caps.get("_meta").and_then(|m| m.get("jetbrains")).is_none());
    }
}

/// codex gets `terminal_output_delta` (its completion frames stop repeating
/// the output the bridge already streamed); claude must NOT — there the
/// same key moves shell output onto a `_meta` channel codeg does not
/// bridge. Neither may get the other spelling: it would move codex's shell
/// output onto `terminal_output`, a key its bridge does not read.
#[test]
fn client_capabilities_advertise_terminal_output_delta_to_codex_only() {
    let meta_of = |agent| {
        serde_json::to_value(build_client_capabilities(agent, HostToolsPolicy::Default))
            .unwrap()
            .get("_meta")
            .cloned()
            .unwrap_or_default()
    };
    let codex = meta_of(AgentType::Codex);
    assert_eq!(
        codex.get("terminal_output_delta").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(codex.get("terminal_output").is_none());
    for agent in [
        AgentType::ClaudeCode,
        AgentType::Pi,
        AgentType::Gemini,
        AgentType::Grok,
    ] {
        let meta = meta_of(agent);
        assert!(meta.get("terminal_output_delta").is_none(), "{agent:?}");
        assert!(meta.get("terminal_output").is_none(), "{agent:?}");
    }
}

#[test]
fn client_capabilities_advertise_parameterized_model_picker_for_cursor() {
    let caps = serde_json::to_value(build_client_capabilities(
        AgentType::Cursor,
        HostToolsPolicy::Default,
    ))
    .unwrap();
    assert_eq!(
        caps.get("_meta")
            .and_then(|m| m.get("parameterizedModelPicker"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "Cursor initialize must advertise parameterizedModelPicker"
    );
    // Cursor must not pick up Claude/Codex-only extensions.
    assert!(caps.get("_meta").and_then(|m| m.get("jetbrains")).is_none());
    assert!(caps
        .get("_meta")
        .and_then(|m| m.get("subagent-transcript"))
        .is_none());
}

#[test]
fn client_capabilities_advertise_parameterized_model_picker_for_custom_cursor_agent() {
    use std::collections::BTreeMap;

    use crate::acp::custom_registry::{
        hydrate, hydrate_test_guard, BinaryPlatformSpec, CustomAgentDef, CustomAgentSpec,
        CustomDistributionKind,
    };

    let _guard = hydrate_test_guard();
    // Every platform, because `build_meta` REJECTS a binary def with no
    // entry for the machine it runs on — a windows-only spec would leave
    // the id unregistered on every other host and this test would then be
    // asserting against `unregistered_meta`, not a cursor launch recipe.
    let mut binary = BTreeMap::new();
    for platform in [
        "darwin-aarch64",
        "darwin-x86_64",
        "linux-aarch64",
        "linux-x86_64",
        "windows-aarch64",
        "windows-x86_64",
    ] {
        binary.insert(
                platform.to_string(),
                BinaryPlatformSpec {
                    archive: format!(
                        "https://downloads.cursor.com/lab/2026.08.11-e8db854/{platform}/agent-cli-package.tar.gz"
                    ),
                    cmd: if platform.starts_with("windows") {
                        "./dist-package/cursor-agent.cmd".into()
                    } else {
                        "./dist-package/cursor-agent".into()
                    },
                    args: vec!["acp".into()],
                    ..Default::default()
                },
            );
    }
    let def = CustomAgentDef {
        registry_id: "test-cursor-acp".into(),
        name: "Test Cursor ACP".into(),
        description: String::new(),
        version: "1.0.0".into(),
        distribution_kind: CustomDistributionKind::Binary,
        spec: CustomAgentSpec {
            binary,
            ..Default::default()
        },
        icon_url: None,
        skills_shared_store: false,
        skills_dir: None,
        source: Default::default(),
        version_probe: None,
        supports_mcp: true,
    };
    assert!(
        hydrate(&[def]).is_empty(),
        "the def must actually register — an unregistered id falls back to \
             unregistered_meta, which advertises nothing"
    );
    let caps = serde_json::to_value(build_client_capabilities(
        AgentType::Custom("test-cursor-acp"),
        HostToolsPolicy::Default,
    ))
    .unwrap();
    assert_eq!(
        caps.get("_meta")
            .and_then(|m| m.get("parameterizedModelPicker"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "custom cursor-agent acp must advertise parameterizedModelPicker"
    );
    hydrate(&[]);
}

#[test]
fn client_capabilities_skip_parameterized_model_picker_for_non_cursor_custom_agent() {
    use crate::acp::custom_registry::{
        hydrate, hydrate_test_guard, CustomAgentDef, CustomAgentSpec, CustomDistributionKind,
        NpxSpec,
    };

    let _guard = hydrate_test_guard();
    let def = CustomAgentDef {
        registry_id: "test-codex-acp".into(),
        name: "Test Codex ACP".into(),
        description: String::new(),
        version: "1.7.0".into(),
        distribution_kind: CustomDistributionKind::Npx,
        spec: CustomAgentSpec {
            npx: Some(NpxSpec {
                package: "@agentclientprotocol/codex-acp@1.7.0".into(),
                args: vec![],
                env: Default::default(),
                cmd: Some("codex-acp".into()),
                node_required: None,
            }),
            ..Default::default()
        },
        icon_url: None,
        skills_shared_store: false,
        skills_dir: None,
        source: Default::default(),
        version_probe: None,
        supports_mcp: true,
    };
    // Same reason as the cursor case: an unregistered id would satisfy the
    // negative assertion for the wrong reason.
    assert!(hydrate(&[def]).is_empty());
    let caps = serde_json::to_value(build_client_capabilities(
        AgentType::Custom("test-codex-acp"),
        HostToolsPolicy::Default,
    ))
    .unwrap();
    assert!(
        caps.get("_meta")
            .and_then(|m| m.get("parameterizedModelPicker"))
            .is_none(),
        "non-cursor custom agents must not advertise parameterizedModelPicker"
    );
    hydrate(&[]);
}

#[test]
fn version_at_least_is_strict_semver_and_fails_closed() {
    assert!(version_at_least("0.64.0", "0.64.0"));
    assert!(version_at_least("0.64.1", "0.64.0"));
    assert!(version_at_least("0.65.0", "0.64.0"));
    assert!(version_at_least("1.0.0", "0.64.0"));
    // SemVer precedence: a prerelease of the FLOOR release precedes it —
    // it may predate the commit that shipped the promptRequired
    // guarantee, so it must not open the native channel…
    assert!(!version_at_least("0.64.0-rc1", "0.64.0"));
    // …while a prerelease whose numeric core is above the floor is fine.
    assert!(version_at_least("0.64.1-beta.2", "0.64.0"));
    // Build metadata is precedence-ignored per spec.
    assert!(version_at_least("0.64.0+sha.deadbeef", "0.64.0"));
    // Below the floor.
    assert!(!version_at_least("0.63.9", "0.64.0"));
    assert!(!version_at_least("0.9.9", "0.64.0"));
    // Fail closed on anything semver can't parse.
    assert!(!version_at_least("", "0.64.0"));
    assert!(!version_at_least("beta", "0.64.0"));
    assert!(!version_at_least("v0.64.0", "0.64.0"));
    assert!(!version_at_least("0.64", "0.64.0"));
    assert!(!version_at_least("0..1", "0.64.0"));
    assert!(!version_at_least("0.64.1abc", "0.64.0"));
}

#[test]
fn codex_user_input_shape_reads_the_running_adapter_version() {
    use crate::acp::question::CodexUserInputShape::{QuestionInDescription, QuestionInTitle};
    use agent_client_protocol::schema::v1::Implementation;
    let at = |v: &str| {
        codex_user_input_shape(AgentType::Codex, Some(&Implementation::new("codex-acp", v)))
    };

    // The 1.12.0 floor, and SemVer precedence around it — a `1.11.1`
    // prerelease is still the old shape, and 1.12.0 itself is the new one.
    assert_eq!(at("1.12.0"), Some(QuestionInTitle));
    assert_eq!(at("1.13.2"), Some(QuestionInTitle));
    assert_eq!(at("1.11.0"), Some(QuestionInDescription));
    assert_eq!(at("1.11.1-preview.6"), Some(QuestionInDescription));
    assert_eq!(at("1.12.0-preview.1"), Some(QuestionInDescription));

    // No `agentInfo`, or one codeg cannot parse: report nothing rather than
    // guess a generation. The elicitation parser then dates the form from
    // its own markers.
    assert_eq!(codex_user_input_shape(AgentType::Codex, None), None);
    assert_eq!(at("v1.12.0"), None);
    assert_eq!(at(""), None);

    // Nothing but codex speaks this form; DeepSeek gets the same
    // `elicitation.form` capability and must stay on the generic reading.
    assert_eq!(
        codex_user_input_shape(
            AgentType::DeepSeek,
            Some(&Implementation::new("deepseek-acp", "1.12.0"))
        ),
        None
    );
}

#[test]
fn synthesize_native_steering_requires_all_three_gates() {
    use agent_client_protocol::schema::v1::Implementation;
    let advertised = meta_map(serde_json::json!({"steering": {"supported": true}}));
    let proven = Implementation::new("claude-agent-acp", "0.65.0");
    let stale = Implementation::new("claude-agent-acp", "0.64.1");

    // All three gates → native.
    assert!(synthesize_native_steering(
        AgentType::ClaudeCode,
        Some(&advertised),
        Some(&proven)
    ));
    // Registry policy gate: codex advertises steering but has no
    // promptRequired minimum — never native, whatever it reports.
    assert!(!synthesize_native_steering(
        AgentType::Codex,
        Some(&advertised),
        Some(&proven)
    ));
    // Runtime proof gate: 0.64.1 advertises steering identically but still
    // settles the owning prompt on a mid-generation `injected` (#934, fixed
    // in 0.65.0 by #958). Launch prefers a PATH-resolved install over the
    // pinned package, so this arm is what keeps such a user on the pull
    // channel — as does a missing `agent_info`.
    assert!(!synthesize_native_steering(
        AgentType::ClaudeCode,
        Some(&advertised),
        Some(&stale)
    ));
    assert!(!synthesize_native_steering(
        AgentType::ClaudeCode,
        Some(&advertised),
        None
    ));
    // Advertisement gate.
    assert!(!synthesize_native_steering(
        AgentType::ClaudeCode,
        None,
        Some(&proven)
    ));
}

/// #892: a custom entry running claude-agent-acp (a second Claude account,
/// say) follows the Claude Code policy, picked by the name its running
/// adapter reports; the advertisement and the version floor gate it as
/// they gate the built-in agent.
#[test]
fn a_custom_agent_running_claude_agent_acp_steers_natively() {
    use agent_client_protocol::schema::v1::Implementation;
    let advertised = meta_map(serde_json::json!({"steering": {"supported": true}}));
    let claude = Implementation::new(registry::CLAUDE_AGENT_ACP_PACKAGE, "0.85.1");
    let claude_stale = Implementation::new(registry::CLAUDE_AGENT_ACP_PACKAGE, "0.64.1");
    let custom = AgentType::Custom("claude-code-2");

    assert!(synthesize_native_steering(
        custom,
        Some(&advertised),
        Some(&claude)
    ));
    assert_eq!(
        steering_policy_agent(custom, Some(&claude)),
        AgentType::ClaudeCode
    );
    // The floor still holds, and so do the advertisement and the proof.
    assert!(!synthesize_native_steering(
        custom,
        Some(&advertised),
        Some(&claude_stale)
    ));
    assert!(!synthesize_native_steering(custom, None, Some(&claude)));
    assert!(!synthesize_native_steering(custom, Some(&advertised), None));
    // Only the exact name: another adapter, or a look-alike without the
    // scope, keeps the pull channel.
    for name in [
        "acme-acp",
        "claude-agent-acp",
        "@agentclientprotocol/claude-agent-acp-fork",
    ] {
        let other = Implementation::new(name, "0.85.1");
        assert!(
            !synthesize_native_steering(custom, Some(&advertised), Some(&other)),
            "{name}"
        );
        assert_eq!(
            steering_policy_agent(custom, Some(&other)),
            custom,
            "{name}"
        );
    }
    // A built-in type is never remapped by what its adapter reports.
    assert!(!synthesize_native_steering(
        AgentType::Codex,
        Some(&advertised),
        Some(&claude)
    ));
    assert_eq!(
        steering_policy_agent(AgentType::Codex, Some(&claude)),
        AgentType::Codex
    );
}

#[test]
fn build_steer_params_shape_carries_the_prompt_required_opt_in() {
    let params = build_steer_params(
        "sess-1",
        &[crate::acp::types::PromptInputBlock::Text {
            text: "use the staging db".into(),
        }],
    );
    assert_eq!(params["sessionId"], "sess-1");
    // EXACT equality, not field probes: routing a text-only note through
    // `map_prompt_blocks` must stay byte-identical to the hand-built
    // `[{type,text}]` this used to emit. A future schema bump that starts
    // serializing `annotations`/`_meta` as null would change the wire for
    // every existing steer, and a field probe would not notice.
    assert_eq!(
        params["prompt"],
        serde_json::json!([{ "type": "text", "text": "use the staging db" }])
    );
    // The opt-in is what keeps the idle race host-owned — its absence
    // would regress to detached `startedNewTurn` turns.
    assert_eq!(
        params["_meta"]["steering"]["idleBehavior"],
        "promptRequired"
    );
}

#[test]
fn build_steer_params_maps_image_blocks_like_a_prompt() {
    // A steered draft with an attachment must hit the wire in the SAME
    // encoding `session/prompt` uses (`map_prompt_blocks`): the adapter's
    // steering handler feeds the array through its normal prompt
    // conversion, so ACP camelCase (`mimeType`) is what it reads.
    let params = build_steer_params(
        "sess-1",
        &[
            crate::acp::types::PromptInputBlock::Text {
                text: "match this mock".into(),
            },
            crate::acp::types::PromptInputBlock::Image {
                data: "aGk=".into(),
                mime_type: "image/png".into(),
                uri: None,
            },
        ],
    );
    assert_eq!(params["prompt"][0]["type"], "text");
    assert_eq!(params["prompt"][0]["text"], "match this mock");
    assert_eq!(params["prompt"][1]["type"], "image");
    assert_eq!(params["prompt"][1]["data"], "aGk=");
    assert_eq!(params["prompt"][1]["mimeType"], "image/png");
    assert_eq!(
        params["_meta"]["steering"]["idleBehavior"],
        "promptRequired"
    );
}

#[test]
fn parse_steer_outcome_maps_the_wire_strings_and_rejects_unknowns() {
    assert_eq!(
        parse_steer_outcome(&serde_json::json!({"outcome": "injected"})).unwrap(),
        SteerOutcome::Injected
    );
    assert_eq!(
        parse_steer_outcome(
            &serde_json::json!({"outcome": "promptRequired", "reason": "noRunningTurn"})
        )
        .unwrap(),
        SteerOutcome::PromptRequired
    );
    assert_eq!(
        parse_steer_outcome(&serde_json::json!({"outcome": "startedNewTurn"})).unwrap(),
        SteerOutcome::StartedNewTurn
    );
    // Unknown or missing outcome is a protocol error, not a silent
    // success — the caller must know whether the content was consumed.
    assert!(parse_steer_outcome(&serde_json::json!({"outcome": "queued"})).is_err());
    assert!(parse_steer_outcome(&serde_json::json!({})).is_err());
    assert!(parse_steer_outcome(&serde_json::json!({"outcome": 1})).is_err());
}

#[test]
fn hoist_request_permission_meta_carries_the_codex_reason_onto_the_card() {
    // codex-acp 1.7.0: the title is a fixed string and the REASON only
    // exists at request level, so the card is built from a tool call that
    // does not explain itself until this hoist runs.
    let mut tool_call = serde_json::json!({
        "toolCallId": "command-7",
        "kind": "execute",
        "status": "pending",
        "title": "Run command",
        "rawInput": { "command": "npm test", "cwd": "/workspace" }
    });
    let request_meta = meta_map(serde_json::json!({
        "permission": {
            "version": 1,
            "title": "Run command?",
            "description": "The test suite needs to run outside the current sandbox."
        }
    }));
    hoist_request_permission_meta(&mut tool_call, Some(&request_meta));
    assert_eq!(
        tool_call["_meta"]["permission"]["description"],
        serde_json::json!("The test suite needs to run outside the current sandbox.")
    );
    // Untouched otherwise — the standard fields stay the authority.
    assert_eq!(tool_call["title"], serde_json::json!("Run command"));
}

#[test]
fn hoist_request_permission_meta_preserves_existing_tool_call_meta() {
    // An agent-supplied `_meta` must survive: claude puts `claudeCode.title`
    // there and the dialog prefers it over the raw title. And a tool call
    // that already carried `permission` wins over the request level — it is
    // the more specific of the two.
    let mut tool_call = serde_json::json!({
        "toolCallId": "t1",
        "_meta": { "claudeCode": { "title": "Run the test suite" },
                   "permission": { "version": 1, "description": "from the tool call" } }
    });
    let request_meta = meta_map(serde_json::json!({
        "permission": { "version": 1, "description": "from the request" }
    }));
    hoist_request_permission_meta(&mut tool_call, Some(&request_meta));
    assert_eq!(
        tool_call["_meta"]["claudeCode"]["title"],
        serde_json::json!("Run the test suite")
    );
    assert_eq!(
        tool_call["_meta"]["permission"]["description"],
        serde_json::json!("from the tool call")
    );
}

#[test]
fn hoist_request_permission_meta_is_a_noop_without_a_permission_block() {
    // Every agent but codex ≥1.7.0 sends no request-level `permission`, and
    // codex's own plan-review request sends `codex` instead. Neither may
    // grow a stray `_meta` key.
    for request_meta in [
        None,
        Some(meta_map(serde_json::json!({
            "codex": { "kind": "plan_review", "planItemId": "p1" }
        }))),
    ] {
        let mut tool_call = serde_json::json!({ "toolCallId": "t1" });
        hoist_request_permission_meta(&mut tool_call, request_meta.as_ref());
        assert_eq!(tool_call, serde_json::json!({ "toolCallId": "t1" }));
    }
}

#[test]
fn hoist_request_permission_meta_reads_the_air_record() {
    // claude-agent-acp 0.82.0 / codex-acp 2.0.0 (recorded): the record
    // moved to `_meta.jetbrains.air.permission` and the old key goes to no
    // client. The card keeps reading `_meta.permission`.
    let mut tool_call = serde_json::json!({
        "toolCallId": "command-7",
        "title": "npm install",
        "rawInput": { "command": "npm install", "cwd": "/workspace" }
    });
    let request_meta = meta_map(serde_json::json!({"jetbrains": {"air": {
        "version": 1,
        "permission": {
            "version": 1,
            "title": "Run command?",
            "description": "Install the dependencies."
        }
    }}}));
    hoist_request_permission_meta(&mut tool_call, Some(&request_meta));
    assert_eq!(
        tool_call["_meta"]["permission"],
        serde_json::json!({
            "version": 1,
            "title": "Run command?",
            "description": "Install the dependencies."
        })
    );
}

/// claude-agent-acp 0.78.0's permission request for an MCP tool, verified
/// against `buildClaudePermissionPresentation` + `toolInfoFromToolUse`: an
/// MCP tool falls through the tool switch, so BOTH the card title and the
/// request-level `_meta.permission.title` are the raw `mcp__<server>__<tool>`
/// name, and the options are allow-once / reject.
fn claude_mcp_permission_request(
    tool_name: &str,
    options: Vec<agent_client_protocol::schema::v1::PermissionOption>,
) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::new("sess-1"),
        agent_client_protocol::schema::v1::ToolCallUpdate::new(
            "toolu_01",
            agent_client_protocol::schema::v1::ToolCallUpdateFields::new()
                .title(tool_name.to_string())
                .kind(ToolKind::Other)
                .raw_input(serde_json::json!({ "questions": [] })),
        ),
        options,
    )
    .meta(meta_map(serde_json::json!({
        "permission": { "version": 1, "title": tool_name }
    })))
}

fn claude_permission_options() -> Vec<agent_client_protocol::schema::v1::PermissionOption> {
    vec![
        agent_client_protocol::schema::v1::PermissionOption::new(
            "allow-once",
            "Yes",
            PermissionOptionKind::AllowOnce,
        ),
        agent_client_protocol::schema::v1::PermissionOption::new(
            "reject",
            "No",
            PermissionOptionKind::RejectOnce,
        ),
    ]
}

#[test]
fn codeg_ask_auto_allow_option_picks_allow_once_for_codegs_own_ask_tool() {
    let req = claude_mcp_permission_request(
        "mcp__codeg-mcp__ask_user_question",
        claude_permission_options(),
    );
    assert_eq!(
        codeg_ask_auto_allow_option(&req).as_deref(),
        Some("allow-once")
    );
    // The title alone is enough: an agent that sends no request-level
    // `_meta.permission` block still gets the shortcut.
    let mut bare = claude_mcp_permission_request(
        "mcp__codeg-mcp__ask_user_question",
        claude_permission_options(),
    );
    bare.meta = None;
    assert_eq!(
        codeg_ask_auto_allow_option(&bare).as_deref(),
        Some("allow-once")
    );
}

#[test]
fn codeg_ask_auto_allow_option_leaves_every_other_approval_on_the_card() {
    // Another server's ask tool: approving it is the user's call, not
    // codeg's, even though the tool half of the name matches.
    for foreign in [
        "mcp__other-server__ask_user_question",
        "mcp__codeg-mcp__delegate_to_agent",
        "Bash",
    ] {
        let req = claude_mcp_permission_request(foreign, claude_permission_options());
        assert!(
            codeg_ask_auto_allow_option(&req).is_none(),
            "{foreign} must keep its approval card"
        );
    }
    // An agent offering only a DURABLE allow writes a rule into the user's
    // own settings — that outlives this turn, so it stays their decision.
    let always_only = claude_mcp_permission_request(
        "mcp__codeg-mcp__ask_user_question",
        vec![
            agent_client_protocol::schema::v1::PermissionOption::new(
                "allow-with-updates",
                "Yes, and don't ask again",
                PermissionOptionKind::AllowAlways,
            ),
            agent_client_protocol::schema::v1::PermissionOption::new(
                "reject",
                "No",
                PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    assert!(codeg_ask_auto_allow_option(&always_only).is_none());
}

#[test]
fn codex_retry_indicator_extracts_message_and_object_http_status() {
    // codex-acp #289: object-variant `codexErrorInfo` carries an inner
    // `httpStatusCode`; the message + status are surfaced.
    let m = meta_map(serde_json::json!({
        "codex": { "error": {
            "message": "Reconnecting after provider returned 401",
            "codexErrorInfo": { "responseStreamDisconnected": { "httpStatusCode": 401 } },
            "additionalDetails": "HTTP status 401",
            "turnId": "turn-id",
            "willRetry": true
        } }
    }));
    assert_eq!(
        codex_retry_indicator(Some(&m)),
        Some((
            "Reconnecting after provider returned 401".to_string(),
            Some(401)
        ))
    );
}

#[test]
fn codex_retry_indicator_string_enum_yields_no_status() {
    // A bare string `codexErrorInfo` yields the message but no http status.
    let m = meta_map(serde_json::json!({
        "codex": { "error": {
            "message": "Server overloaded",
            "codexErrorInfo": "serverOverloaded",
            "willRetry": true
        } }
    }));
    assert_eq!(
        codex_retry_indicator(Some(&m)),
        Some(("Server overloaded".to_string(), None))
    );
}

#[test]
fn codex_retry_indicator_refuses_terminal_empty_and_absent() {
    // `willRetry: false` (e.g. 401 auth) must never render a retry banner.
    let terminal = meta_map(serde_json::json!({
        "codex": { "error": { "message": "unauthorized", "willRetry": false } }
    }));
    assert_eq!(codex_retry_indicator(Some(&terminal)), None);
    // Blank/whitespace message → nothing to show.
    let blank = meta_map(serde_json::json!({
        "codex": { "error": { "message": "   ", "willRetry": true } }
    }));
    assert_eq!(codex_retry_indicator(Some(&blank)), None);
    // No `codex.error` at all (a goal-only or empty session_info_update).
    let goal_only = meta_map(serde_json::json!({ "codex": { "goal": null } }));
    assert_eq!(codex_retry_indicator(Some(&goal_only)), None);
    assert_eq!(codex_retry_indicator(None), None);
}

#[test]
fn classify_load_failure_resource_not_found_maps_to_code() {
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound,
            "session abc not found",
        ),
        Some("resource_not_found"),
    );
    // The structured -32002 code takes precedence even when the message
    // would otherwise match the crash/ended family.
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound,
            "process exited with code 1",
        ),
        Some("resource_not_found"),
    );
}

#[test]
fn classify_load_failure_crash_and_ended_map_to_unavailable() {
    // The reported Claude 0.58.1 case: native CLI exits 1, wrapped as -32603.
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            "Internal error: { \"details\": \"Claude Code process exited with code 1\" }",
        ),
        Some("session_unavailable"),
    );
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            "The Claude Agent session has ended. Please start a new session.",
        ),
        Some("session_unavailable"),
    );
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            "Session not found",
        ),
        Some("session_unavailable"),
    );
}

#[test]
fn classify_load_failure_names_an_archived_session() {
    // The reported case: `codex archive <id>`, then reopen the conversation.
    // codex-acp answers session/load with a generic -32603 whose data names
    // the session and the command that restores it.
    let archived = "Internal error: {\n  \"details\": \"session \
             019bf0c4-4d1a-7c3e-9f21-6a0e5b8d2c47 is archived. Run `codex \
             unarchive 019bf0c4-4d1a-7c3e-9f21-6a0e5b8d2c47` to restore it.\"\n}";
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            archived
        ),
        Some("session_archived"),
    );

    // Archived is the more specific verdict: a body carrying both signals
    // must not degrade into the generic "unavailable" family, which offers
    // the user no way back.
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            "Session not found: session abc is archived.",
        ),
        Some("session_archived"),
    );

    // Codex reads history back out of its own rollout store, so an archived
    // session must stop with the banner — silently opening a new session
    // would orphan history that one command restores.
    assert!(!recovers_load_failure_locally(
        AgentType::Codex,
        Some("session_archived")
    ));
    // A custom agent's history is codeg's own transcript, so it keeps the
    // silent local recovery it has for the other classified failures.
    let custom = AgentType::custom("glm-acp-agent").expect("valid id");
    assert!(recovers_load_failure_locally(
        custom,
        Some("session_archived")
    ));
}

/// After a codex fork, the sibling row codeg creates to keep the pre-fork
/// history points at the PARENT thread — whose writer the forking process
/// holds until codex unloads the thread, about a minute after codeg closes
/// it, because `session/fork` never unsubscribes the parent. Opening it in
/// that window must stop with a banner, never fall through to
/// `session/new`: that rebinds the row to a fresh empty session and
/// destroys the only pointer to the history the row exists for.
#[test]
fn classify_load_failure_names_a_session_another_client_holds() {
    let busy = "Internal error: {\n  \"details\": \"thread \
             01a0626c-c601-78f1-a13d-2b26dd168501 already has an active \
             writer\"\n}";
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            busy
        ),
        Some("session_busy"),
    );

    assert!(!recovers_load_failure_locally(
        AgentType::Codex,
        Some("session_busy")
    ));
    // Unlike every other verdict, this one is refused for a custom agent
    // too: the others mean the session is GONE, so opening a fresh one and
    // linking the transcript forward loses nothing. A busy session is still
    // there, and starting over would trade a live history for an empty
    // session to work around a lock that clears on its own.
    let custom = AgentType::custom("glm-acp-agent").expect("valid id");
    assert!(!recovers_load_failure_locally(custom, Some("session_busy")));
}

/// The same lock as above, as each codex-acp version reports it. Both bodies
/// were captured live: two adapter processes on one `CODEX_HOME`, the
/// second `session/load`ing (and `session/resume`ing — the identical error)
/// a thread the first still held. 2.1.0 (#564) types it; the verdict must
/// come from that type, because the raw text it still carries in `details`
/// is codex's wording and carries no stability promise.
#[test]
fn classify_load_error_reads_codex_thread_active_writer_reason() {
    let error = |body: serde_json::Value| -> agent_client_protocol::Error {
        serde_json::from_value(body).expect("an ACP error body")
    };

    let codex_2_1_0 = error(serde_json::json!({
        "code": -32600,
        "message": "Invalid request: This Codex session is in use by another Codex \
            client (the Codex app, the CLI or an IDE extension). Close the session \
            there or quit that client, then try again.",
        "data": {
            "reason": "thread_active_writer",
            "threadId": "01a0f64d-42fe-7242-a005-bddc86d65de5",
            "details": "thread 01a0f64d-42fe-7242-a005-bddc86d65de5 already has an active writer"
        }
    }));
    assert_eq!(
        classify_session_load_error(&codex_2_1_0),
        Some("session_busy")
    );

    // The reason alone decides. Without it this body matches nothing: the
    // code is a plain InvalidRequest and the reworded text names no marker.
    let reworded = error(serde_json::json!({
        "code": -32600,
        "message": "Invalid request: session in use",
        "data": {
            "reason": "thread_active_writer",
            "threadId": "t-1",
            "details": "thread t-1 is locked by another process"
        }
    }));
    assert_eq!(classify_session_load_error(&reworded), Some("session_busy"));

    // ≤2.0.x: no reason, codex's raw text under -32603. The message
    // fallback keeps naming it, so an older adapter resolved off PATH (or
    // a custom pin) still gets the banner.
    let codex_2_0_1 = error(serde_json::json!({
        "code": -32603,
        "message": "Internal error",
        "data": {
            "details": "thread 01a0f64b-57af-7252-9bcd-6dd5b5bb04c0 already has an active writer"
        }
    }));
    assert_eq!(
        classify_session_load_error(&codex_2_0_1),
        Some("session_busy")
    );

    // Any other reason is not this one, and the rest of the ladder is
    // untouched: the code still outranks the message, and an unclassified
    // failure stays a `session/new` fallback.
    let other_reason = error(serde_json::json!({
        "code": -32600,
        "message": "Invalid request",
        "data": { "reason": "something_else" }
    }));
    assert_eq!(classify_session_load_error(&other_reason), None);
    let not_found = error(serde_json::json!({
        "code": -32002,
        "message": "Resource not found",
        "data": { "details": "process exited with code 1" }
    }));
    assert_eq!(
        classify_session_load_error(&not_found),
        Some("resource_not_found")
    );
    let archived = error(serde_json::json!({
        "code": -32603,
        "message": "Internal error",
        "data": { "details": "session abc is archived. Run `codex unarchive abc` to restore it." }
    }));
    assert_eq!(
        classify_session_load_error(&archived),
        Some("session_archived")
    );
    let unclassified = error(serde_json::json!({
        "code": -32603,
        "message": "Internal error",
        "data": { "details": "boom" }
    }));
    assert_eq!(classify_session_load_error(&unclassified), None);
}

/// codex-acp 2.2 answers an open the app-server died under with code 1001
/// (bodies captured live off 2.2.1, the app-server SIGKILLed). Neither form
/// may fall through to `session/new`, which 2.2 now lets succeed on the
/// restarted app-server: that would rebind the row to an empty session.
#[test]
fn classify_load_error_keeps_a_codex_app_server_loss_off_session_new() {
    let error = |body: serde_json::Value| -> agent_client_protocol::Error {
        serde_json::from_value(body).expect("an ACP error body")
    };

    let restarting = error(serde_json::json!({
        "code": 1001,
        "message": "The Codex app-server was killed by SIGKILL, which usually means it \
            ran out of memory. The agent starts it again on the next request.",
        "data": { "exitCode": null, "signal": "SIGKILL", "restartable": true }
    }));
    assert_eq!(codex_app_server_lost(&restarting), Some(true));
    assert_eq!(
        classify_session_load_error(&restarting),
        Some("session_unavailable")
    );

    let given_up = error(serde_json::json!({
        "code": 1001,
        "message": "The Codex app-server crashed 5 times in the last 5 min (last: it was \
            killed by SIGKILL, which usually means it ran out of memory), so the agent \
            stopped restarting it. Restart the agent, or try again in 5 min.",
        "data": {
            "exitCode": null,
            "signal": "SIGKILL",
            "restartable": false,
            "retryAfterMs": 282346
        }
    }));
    assert_eq!(codex_app_server_lost(&given_up), Some(false));
    assert_eq!(
        classify_session_load_error(&given_up),
        Some("session_unavailable")
    );
    // codex's own history is codex's store, so no local recovery: the
    // banner is the only path that keeps it.
    assert!(!recovers_load_failure_locally(
        AgentType::Codex,
        Some("session_unavailable")
    ));

    // The shape is the code AND a boolean `restartable`; either alone is
    // someone else's error and keeps the old ladder.
    let other_1001 = error(serde_json::json!({
        "code": 1001,
        "message": "something else",
        "data": { "details": "boom" }
    }));
    assert_eq!(codex_app_server_lost(&other_1001), None);
    assert_eq!(classify_session_load_error(&other_1001), None);
    let not_1001 = error(serde_json::json!({
        "code": -32603,
        "message": "Internal error",
        "data": { "restartable": true }
    }));
    assert_eq!(codex_app_server_lost(&not_1001), None);
    assert_eq!(classify_session_load_error(&not_1001), None);
}

#[test]
fn classify_load_failure_keeps_existing_behavior_for_recoverable_errors() {
    // "Method not found" (agent lacks resume) and "Authentication required"
    // must fall through to the existing session/new + silent-stop paths.
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::MethodNotFound,
            "Method not found",
        ),
        None,
    );
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::AuthRequired,
            "Authentication required",
        ),
        None,
    );
    // Any other internal error without a crash/ended signature stays a
    // session/new fallback.
    assert_eq!(
        classify_session_load_failure(
            agent_client_protocol::schema::v1::ErrorCode::InternalError,
            "some unrelated transient failure",
        ),
        None,
    );
}

#[test]
fn agents_codeg_records_itself_absorb_a_forgotten_session() {
    // The reported case: a custom agent keeps sessions in memory, so every
    // restart makes session/load fail with "Session not found". codeg has
    // the turns in its own transcript, so it must recover silently instead
    // of blanking the conversation behind a load-failed banner.
    let custom = AgentType::custom("glm-acp-agent").expect("valid id");
    assert!(recovers_load_failure_locally(
        custom,
        Some("session_unavailable")
    ));
    assert!(recovers_load_failure_locally(
        custom,
        Some("resource_not_found")
    ));
    // An unexpected failure is not a "forgotten session" — keep the
    // existing emit-then-fall-back behaviour even for custom agents.
    assert!(!recovers_load_failure_locally(custom, None));

    // Built-ins read history back out of the agent's own store, so a
    // forgotten session really is gone and must still stop with the banner.
    for builtin in [
        AgentType::ClaudeCode,
        AgentType::Codex,
        AgentType::Gemini,
        AgentType::Cursor,
    ] {
        assert!(
            !recovers_load_failure_locally(builtin, Some("session_unavailable")),
            "{builtin:?} has no codeg-side transcript to fall back on"
        );
    }
}

#[test]
fn cursor_env_policy_clears_inherited_creds_only_in_subscription() {
    let sub: BTreeMap<String, String> =
        [("CURSOR_AUTH_MODE".to_string(), "subscription".to_string())].into();

    // No configured creds → both injected empty (⇒ spawn strips inherited).
    let mut merged = vec![("PATH".to_string(), "/usr/bin".to_string())];
    apply_cursor_env_policy(&mut merged, &sub);
    assert!(merged
        .iter()
        .any(|(k, v)| k == "CURSOR_API_KEY" && v.is_empty()));
    assert!(merged
        .iter()
        .any(|(k, v)| k == "CURSOR_API_BASE_URL" && v.is_empty()));

    // A configured key is preserved; only the absent base URL is cleared.
    let mut with_key = vec![("CURSOR_API_KEY".to_string(), "sk-x".to_string())];
    apply_cursor_env_policy(&mut with_key, &sub);
    assert!(with_key
        .iter()
        .any(|(k, v)| k == "CURSOR_API_KEY" && v == "sk-x"));
    assert!(with_key
        .iter()
        .any(|(k, v)| k == "CURSOR_API_BASE_URL" && v.is_empty()));

    // Custom mode and legacy/no-mode rows are left untouched.
    for mode in [Some("custom"), None] {
        let rt: BTreeMap<String, String> = mode
            .map(|m| [("CURSOR_AUTH_MODE".to_string(), m.to_string())].into())
            .unwrap_or_default();
        let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_cursor_env_policy(&mut env, &rt);
        assert!(!env.iter().any(|(k, _)| k == "CURSOR_API_KEY"));
        assert!(!env.iter().any(|(k, _)| k == "CURSOR_API_BASE_URL"));
    }
}

#[test]
fn cursor_force_knob_is_tri_state() {
    // On.
    for on in ["1", "true", "TRUE", " 1 "] {
        assert!(cursor_force_enabled(Some(on)), "{on:?} must enable --force");
    }
    // Explicitly off — the value the panel now writes for "Ask before
    // running", which has to be distinguishable from the unset case.
    for off in ["0", "false", "", "  "] {
        assert!(!cursor_force_enabled(Some(off)), "{off:?} must not force");
    }
    // Never configured. Ask, matching what Cursor sessions have always
    // actually done, and matching what the panel now shows.
    assert!(!cursor_force_enabled(None));
}

#[test]
fn grok_env_policy_clears_inherited_key_only_in_subscription() {
    let sub: BTreeMap<String, String> =
        [("GROK_AUTH_MODE".to_string(), "subscription".to_string())].into();

    // Subscription with no configured key → inject empty (⇒ spawn strips the
    // inherited XAI_API_KEY so `grok login` is used).
    let mut merged = vec![("PATH".to_string(), "/usr/bin".to_string())];
    apply_grok_env_policy(&mut merged, &sub);
    assert!(merged
        .iter()
        .any(|(k, v)| k == "XAI_API_KEY" && v.is_empty()));

    // A configured key is preserved even in subscription mode (explicit wins).
    let mut with_key = vec![("XAI_API_KEY".to_string(), "xai-abc".to_string())];
    apply_grok_env_policy(&mut with_key, &sub);
    assert!(with_key
        .iter()
        .any(|(k, v)| k == "XAI_API_KEY" && v == "xai-abc"));

    // api_key mode and legacy/no-mode rows are left untouched.
    for mode in [Some("api_key"), None] {
        let rt: BTreeMap<String, String> = mode
            .map(|m| [("GROK_AUTH_MODE".to_string(), m.to_string())].into())
            .unwrap_or_default();
        let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_grok_env_policy(&mut env, &rt);
        assert!(!env.iter().any(|(k, _)| k == "XAI_API_KEY"));
    }
}

fn antigravity_runtime(method: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(ANTIGRAVITY_AUTH_METHOD_ENV.to_string(), method.to_string())])
}

#[test]
fn antigravity_env_policy_scrubs_credentials_the_chosen_method_does_not_use() {
    // Browser login: a GEMINI_API_KEY (or the Agent Platform trio)
    // inherited from the developer's shell must be cleared, or the server
    // silently authenticates as something the user did not pick.
    let mut env = vec![
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("GEMINI_API_KEY".to_string(), "leaked".to_string()),
        ("GOOGLE_CLOUD_PROJECT".to_string(), "stale".to_string()),
    ];
    apply_antigravity_env_policy(&mut env, &antigravity_runtime("oauth-personal"));
    for key in ["GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_CLOUD_PROJECT"] {
        let hits: Vec<_> = env.iter().filter(|(k, _)| k == key).collect();
        assert_eq!(hits.len(), 1, "{key} must appear exactly once");
        assert!(hits[0].1.is_empty(), "{key} must be cleared for env_remove");
    }
    assert!(env.iter().any(|(k, v)| k == "PATH" && v == "/usr/bin"));

    // gemini-api-key keeps its own credential and clears the rest.
    let mut env = vec![
        ("GEMINI_API_KEY".to_string(), "real-key".to_string()),
        ("GOOGLE_API_KEY".to_string(), "leaked".to_string()),
    ];
    apply_antigravity_env_policy(&mut env, &antigravity_runtime("gemini-api-key"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GEMINI_API_KEY" && v == "real-key"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GOOGLE_API_KEY" && v.is_empty()));

    // Agent Platform keeps the GOOGLE_* trio, drops GEMINI_API_KEY.
    let mut env = vec![
        ("GOOGLE_CLOUD_PROJECT".to_string(), "p".to_string()),
        ("GOOGLE_CLOUD_LOCATION".to_string(), "global".to_string()),
        ("GEMINI_API_KEY".to_string(), "leaked".to_string()),
    ];
    apply_antigravity_env_policy(&mut env, &antigravity_runtime("agent-platform"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GOOGLE_CLOUD_PROJECT" && v == "p"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GOOGLE_CLOUD_LOCATION" && v == "global"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GEMINI_API_KEY" && v.is_empty()));
}

/// A var the method READS but the panel did not store must still be cleared.
///
/// This is the whole Agent Platform choice: the method takes either a
/// `GOOGLE_API_KEY` or a project + location, and the server suppresses the
/// pair whenever the key is set — so the panel deletes `GOOGLE_API_KEY`
/// from the row when the field is left empty. A policy that only asked
/// "does this method read the var" left the slot open, and an inherited key
/// walked into it and outranked the project the user typed.
///
/// Note the shape this needs: the earlier cases all put the var IN `merged`
/// first, which is the one arrangement that cannot catch this — the bug was
/// about the var being absent.
#[test]
fn antigravity_env_policy_clears_a_kept_var_the_panel_left_empty() {
    // Exactly what the panel persists for "Agent Platform, no API key".
    let mut env = vec![
        ("GOOGLE_CLOUD_PROJECT".to_string(), "mine".to_string()),
        ("GOOGLE_CLOUD_LOCATION".to_string(), "global".to_string()),
    ];
    apply_antigravity_env_policy(&mut env, &antigravity_runtime("agent-platform"));
    let google_api_key: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == "GOOGLE_API_KEY")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        google_api_key,
        vec![""],
        "an inherited GOOGLE_API_KEY would suppress the project the user filled in"
    );
    // The credentials the panel DID store are untouched.
    assert!(env
        .iter()
        .any(|(k, v)| k == "GOOGLE_CLOUD_PROJECT" && v == "mine"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GOOGLE_CLOUD_LOCATION" && v == "global"));

    // Same rule for the API-key method with an empty key field, and for a
    // whitespace-only value — the panel trims before storing, so a blank
    // here is a leftover rather than a credential.
    let mut env = vec![("GEMINI_API_KEY".to_string(), "   ".to_string())];
    apply_antigravity_env_policy(&mut env, &antigravity_runtime("gemini-api-key"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "GEMINI_API_KEY" && v.is_empty()));
}

#[test]
fn antigravity_env_policy_leaves_unrecorded_and_unknown_methods_alone() {
    // Legacy rows (no recorded method) and a garbage value must not have
    // an operator-provided container env scrubbed out from under them.
    for runtime in [BTreeMap::new(), antigravity_runtime("not-a-method")] {
        let mut env = vec![("GEMINI_API_KEY".to_string(), "operator".to_string())];
        apply_antigravity_env_policy(&mut env, &runtime);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].1, "operator");
    }
}

#[test]
fn antigravity_settings_read_fails_closed_on_anything_it_cannot_rewrite() {
    // The vendor's own writer records the rule this mirrors: "a file that
    // cannot be parsed is left alone, since rewriting it would delete
    // content we could not read." Only a MISSING file is safe to create.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    assert!(matches!(read_antigravity_settings(&path), Ok(None)));

    // Hjson: the server accepts comments and trailing commas, codeg's
    // parser does not — so this lands in the give-up branch instead of
    // being flattened into strict JSON with the comments (and anything
    // else codeg misread) gone.
    std::fs::write(
        &path,
        "{\n  // my key\n  \"auth\": { \"type\": \"gemini-api-key\" },\n}\n",
    )
    .unwrap();
    assert!(read_antigravity_settings(&path).is_err());

    // A JSON array/scalar root is not editable either.
    std::fs::write(&path, "[1, 2]").unwrap();
    assert!(read_antigravity_settings(&path).is_err());

    std::fs::write(&path, r#"{"auth":{"type":"oauth-business"},"keep":1}"#).unwrap();
    let parsed = read_antigravity_settings(&path).unwrap().unwrap();
    assert_eq!(parsed["keep"], 1);
}

/// The sign-out asks this instead of reading the stored row, because the
/// row is not what the server infers from. Getting it wrong means aiming
/// `logout` at a flavor that has nothing to clear — which it answers `{}`
/// to, so the mistake would be reported to the user as a sign-out.
#[test]
fn antigravity_effective_auth_type_reads_the_file_the_server_reads() {
    let dir = tempfile::tempdir().unwrap();
    let acp_dir = dir.path().join("antigravity-acp");
    std::fs::create_dir_all(&acp_dir).unwrap();
    let path = acp_dir.join("settings.json");
    let home = || {
        BTreeMap::from([(
            "GEMINI_HOME".to_string(),
            dir.path().to_string_lossy().to_string(),
        )])
    };

    let declared = |method: &str| AntigravityAuthType::Declared(method.to_string());

    // No file at all: positive knowledge that there is no method to find,
    // so the server has nothing to infer from and clears both flavors.
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        AntigravityAuthType::Absent
    );

    std::fs::write(&path, r#"{"auth":{"type":"oauth-business"}}"#).unwrap();
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        declared("oauth-business")
    );

    // The FILE wins over the row, which is the whole reason this exists:
    // the two can disagree (a hand edit, a sync codeg was refused) and only
    // one of them is what the agent authenticates with.
    let mut disagreeing = antigravity_runtime("oauth-personal");
    disagreeing.extend(home());
    assert_eq!(
        antigravity_effective_auth_type(&disagreeing),
        declared("oauth-business")
    );

    // The legacy spelling resolves, as it does server-side before the
    // membership test — otherwise a caller matching canonical ids would
    // read `vertex-ai` as "some OAuth method" and sign out of nothing.
    std::fs::write(&path, r#"{"auth":{"type":"vertex-ai"}}"#).unwrap();
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        declared("agent-platform")
    );

    // An `auth` block with no type, and a blank one, are both "no method" —
    // still positive knowledge, because codeg read the file.
    std::fs::write(&path, r#"{"auth":{"scopes":[]}}"#).unwrap();
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        AntigravityAuthType::Absent
    );
    std::fs::write(&path, r#"{"auth":{"type":"   "}}"#).unwrap();
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        AntigravityAuthType::Absent
    );

    // Hjson: the server reads it and codeg does not, so the method is
    // whatever that file says. NOT `Absent` — this is the distinction the
    // whole enum exists for. A caller that treated it as "nothing there"
    // would sign out of a `gemini-api-key` connection, clear nothing, and
    // be told `{}`.
    std::fs::write(
        &path,
        "{\n  // mine\n  \"auth\": {\"type\": \"oauth-personal\"},\n}\n",
    )
    .unwrap();
    assert_eq!(
        antigravity_effective_auth_type(&home()),
        AntigravityAuthType::Unreadable
    );

    // And a home that cannot be named at all is unknown for the same
    // reason: there is a file somewhere, codeg just cannot say where.
    // Platform-native key, as in the path tests below: `child_home_dir`
    // reads `USERPROFILE` on Windows (`expanduser` never consults `HOME`
    // there), so blanking `HOME` removes nothing, the fallback lands on the
    // runner's real profile, and the answer flips to `Absent`.
    #[cfg(windows)]
    let home_key = "USERPROFILE";
    #[cfg(not(windows))]
    let home_key = "HOME";
    let unnameable = BTreeMap::from([
        (home_key.to_string(), String::new()),
        ("GEMINI_HOME".to_string(), String::new()),
    ]);
    assert_eq!(
        antigravity_effective_auth_type(&unnameable),
        AntigravityAuthType::Unreadable
    );
}

#[test]
fn antigravity_settings_sync_leaves_an_unparseable_file_untouched() {
    // End to end: a hand-commented settings.json must survive a launch
    // byte for byte, warning instead of clobbering.
    let dir = tempfile::tempdir().unwrap();
    let acp_dir = dir.path().join("antigravity-acp");
    std::fs::create_dir_all(&acp_dir).unwrap();
    let path = acp_dir.join("settings.json");
    let original = "{\n  // hand written\n  \"gcp\": { \"project\": \"mine\" },\n}\n";
    std::fs::write(&path, original).unwrap();

    let mut runtime = antigravity_runtime("oauth-business");
    runtime.insert(
        "GEMINI_HOME".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    let report = sync_antigravity_settings_file(&runtime);

    // The panel must be able to SAY this: the row now claims
    // `oauth-business` while the file still says nothing at all.
    assert_eq!(report.status, AntigravitySyncStatus::Skipped);
    assert!(report.reason.is_some_and(|r| r.contains("strict JSON")));

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn antigravity_settings_sync_writes_through_gemini_home_and_defaults_the_method() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("antigravity-acp").join("settings.json");
    let home = || {
        BTreeMap::from([(
            "GEMINI_HOME".to_string(),
            dir.path().to_string_lossy().to_string(),
        )])
    };

    // No recorded method and no file: fall back to the method the panel
    // DISPLAYS as selected, so a user who never opened it still gets a
    // session instead of `Authentication required`.
    sync_antigravity_settings_file(&home());
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["auth"]["type"], "oauth-personal");

    // An `auth.type` already on disk is NEVER overridden by that fallback.
    std::fs::write(&path, r#"{"auth":{"type":"gemini-api-key"},"keep":7}"#).unwrap();
    sync_antigravity_settings_file(&home());
    let held: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(held["auth"]["type"], "gemini-api-key");
    assert_eq!(held["keep"], 7);

    // An explicit panel choice does override it, and keeps foreign keys.
    let mut runtime = antigravity_runtime("oauth-business");
    runtime.extend(home());
    runtime.insert("GOOGLE_CLOUD_PROJECT".to_string(), "acme".to_string());
    runtime.insert("GOOGLE_CLOUD_LOCATION".to_string(), "eu".to_string());
    sync_antigravity_settings_file(&runtime);
    let updated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(updated["auth"]["type"], "oauth-business");
    assert_eq!(updated["gcp"]["project"], "acme");
    assert_eq!(updated["gcp"]["location"], "eu");
    assert_eq!(updated["keep"], 7);
}

#[test]
fn antigravity_settings_merge_preserves_foreign_keys_and_skips_no_op_writes() {
    // The file is the USER's: the server parses it as Hjson and documents
    // it as user-provided, so a codeg write may only touch `auth.type` and
    // the `gcp` block.
    let existing = serde_json::json!({
        "auth": { "type": "gemini-api-key" },
        "gcp": { "project": "hand-written", "location": "eu" },
        "someFutureKey": { "nested": [1, 2, 3] }
    });
    let merged = merge_antigravity_settings(
        Some(existing.clone()),
        "oauth-business",
        GcpField::Keep,
        GcpField::Keep,
    )
    .expect("editable")
    .expect("auth.type changed, so this is a real write");
    assert_eq!(merged["auth"]["type"], "oauth-business");
    // No panel values supplied ⇒ the hand-written gcp block is untouched.
    assert_eq!(merged["gcp"]["project"], "hand-written");
    assert_eq!(merged["gcp"]["location"], "eu");
    assert_eq!(merged["someFutureKey"]["nested"][2], 3);

    // Panel values overwrite only the fields they carry.
    let merged = merge_antigravity_settings(
        Some(existing.clone()),
        "oauth-business",
        GcpField::Set("proj"),
        GcpField::Keep,
    )
    .expect("editable")
    .expect("changed");
    assert_eq!(merged["gcp"]["project"], "proj");
    assert_eq!(merged["gcp"]["location"], "eu", "location was not supplied");

    // Already says exactly this ⇒ no write.
    assert!(merge_antigravity_settings(
        Some(existing),
        "gemini-api-key",
        GcpField::Keep,
        GcpField::Keep
    )
    .expect("editable")
    .is_none());

    // No file at all: created from scratch. (A non-object ROOT never gets
    // here — the read side already refused it.)
    let created = merge_antigravity_settings(
        None,
        "oauth-personal",
        GcpField::Set("p"),
        GcpField::Set("global"),
    )
    .expect("editable")
    .expect("created");
    assert_eq!(created["auth"]["type"], "oauth-personal");
    assert_eq!(created["gcp"]["project"], "p");
    assert_eq!(created["gcp"]["location"], "global");
}

#[test]
fn antigravity_settings_merge_refuses_blocks_that_are_not_objects() {
    // The vendor's `_auth_block` logs "not editing %r because `auth` is not
    // an object" and gives up. Replacing that value with an object would
    // delete whatever the user meant by it, so codeg refuses too.
    let odd_auth = serde_json::json!({ "auth": "managed-elsewhere", "keep": 1 });
    assert!(merge_antigravity_settings(
        Some(odd_auth),
        "oauth-personal",
        GcpField::Keep,
        GcpField::Keep
    )
    .is_err());

    // Same for `gcp` — but ONLY when there is actually something to write
    // into it. With no project or location supplied, a strange `gcp` is
    // none of codeg's business and must not block the `auth.type` update.
    let odd_gcp = serde_json::json!({ "gcp": ["not", "an", "object"] });
    assert!(merge_antigravity_settings(
        Some(odd_gcp.clone()),
        "oauth-personal",
        GcpField::Set("p"),
        GcpField::Keep,
    )
    .is_err());
    let untouched = merge_antigravity_settings(
        Some(odd_gcp),
        "oauth-personal",
        GcpField::Keep,
        GcpField::Keep,
    )
    .expect("editable")
    .expect("auth.type still written");
    assert_eq!(untouched["auth"]["type"], "oauth-personal");
    assert_eq!(untouched["gcp"], serde_json::json!(["not", "an", "object"]));

    // An explicit JSON null reads as absent, not as a foreign shape.
    let null_auth = serde_json::json!({ "auth": null, "keep": 2 });
    let filled = merge_antigravity_settings(
        Some(null_auth),
        "gemini-api-key",
        GcpField::Keep,
        GcpField::Keep,
    )
    .expect("editable")
    .expect("changed");
    assert_eq!(filled["auth"]["type"], "gemini-api-key");
    assert_eq!(filled["keep"], 2);
}

/// Clearing the project and location in the panel has to REACH the file.
///
/// The old signature could not say it: "the panel does not manage this
/// field" and "the panel manages it and the user emptied it" both arrived
/// as `None`, and the merge left the block alone for both. So the values
/// written by an earlier save stayed in force forever — and for
/// `oauth-business` that file is the ONLY place the project comes from, so
/// the agent kept authenticating against a project the UI showed nowhere.
#[test]
fn antigravity_settings_gcp_fields_can_be_cleared_by_the_panel() {
    let existing = || {
        serde_json::json!({
            "auth": { "type": "oauth-business" },
            "gcp": { "project": "stale", "location": "eu" },
            "keep": 1
        })
    };

    // The panel owns both fields for this method and both are now empty.
    let cleared = merge_antigravity_settings(
        Some(existing()),
        "oauth-business",
        GcpField::Clear,
        GcpField::Clear,
    )
    .expect("editable")
    .expect("the gcp block changed, so this is a real write");
    assert!(
        cleared.get("gcp").is_none(),
        "an emptied block should go rather than linger as {{}}: {cleared}"
    );
    assert_eq!(cleared["auth"]["type"], "oauth-business");
    assert_eq!(cleared["keep"], 1, "foreign keys still survive a clear");

    // One cleared, one set.
    let partial = merge_antigravity_settings(
        Some(existing()),
        "oauth-business",
        GcpField::Set("new"),
        GcpField::Clear,
    )
    .expect("editable")
    .expect("changed");
    assert_eq!(partial["gcp"]["project"], "new");
    assert!(partial["gcp"].get("location").is_none());

    // A clear with nothing on disk to clear is not a write — otherwise
    // every launch would rewrite a running server's file for nothing.
    assert!(merge_antigravity_settings(
        Some(serde_json::json!({ "auth": { "type": "oauth-business" } })),
        "oauth-business",
        GcpField::Clear,
        GcpField::Clear,
    )
    .expect("editable")
    .is_none());

    // And a clear against a `gcp` that is not an object must not REFUSE the
    // edit: there is nothing there to remove, so it is the same "none of
    // codeg's business" case as having nothing to say, and blocking would
    // take the `auth.type` update down with it — the one part of this file
    // the agent cannot start without.
    let odd = serde_json::json!({ "gcp": ["not", "an", "object"] });
    let still_written = merge_antigravity_settings(
        Some(odd),
        "oauth-business",
        GcpField::Clear,
        GcpField::Clear,
    )
    .expect("a clear must not refuse a block it cannot edit")
    .expect("auth.type still written");
    assert_eq!(still_written["auth"]["type"], "oauth-business");
    assert_eq!(
        still_written["gcp"],
        serde_json::json!(["not", "an", "object"])
    );
}

/// Which fields count as the panel's is decided by the METHOD, so a
/// hand-written block under a method that never renders those inputs is
/// still none of codeg's business.
#[test]
fn antigravity_gcp_ownership_follows_the_recorded_method() {
    let empty = BTreeMap::new();
    for method in ["oauth-business", "agent-platform"] {
        assert!(matches!(
            antigravity_gcp_field(&empty, Some(method), "GOOGLE_CLOUD_PROJECT"),
            GcpField::Clear
        ));
    }
    // The two methods with no project/location inputs, and a legacy row
    // with no recorded method at all.
    for method in [Some("oauth-personal"), Some("gemini-api-key"), None] {
        assert!(matches!(
            antigravity_gcp_field(&empty, method, "GOOGLE_CLOUD_PROJECT"),
            GcpField::Keep
        ));
    }
    // A value present is always a write, whoever recorded it.
    let filled = BTreeMap::from([("GOOGLE_CLOUD_PROJECT".to_string(), " p ".to_string())]);
    assert!(matches!(
        antigravity_gcp_field(&filled, Some("oauth-business"), "GOOGLE_CLOUD_PROJECT"),
        GcpField::Set("p")
    ));
}

/// A `GEMINI_HOME` that only codeg's OWN environment carries still names
/// the directory the agent uses.
///
/// `merge_agent_env` lists the variables a launch SETS; anything absent is
/// inherited, and relocating the tree from a container's environment
/// (`GEMINI_HOME=/data/gemini` in the image, nothing in the per-agent row)
/// is exactly that shape. Treating "absent" as "unset" sent codeg to
/// `~/.gemini` — so on a Docker deployment it wrote `auth.type` into
/// `/root/.gemini` while the agent read the relocated file, and the panel
/// named a token path that was never written. The same three-state
/// distinction `child_home_dir` makes for `HOME`, for the same reason.
#[test]
fn antigravity_settings_path_follows_a_gemini_home_codeg_only_inherits() {
    // Platform-native, and HOME is pinned in the row rather than read from
    // the process: other tests relocate the real one through `temp_env`,
    // and a read here would race them. codeg's own `GEMINI_HOME` is
    // injected for the same reason — see
    // [`antigravity_acp_dir_with_inherited`].
    #[cfg(windows)]
    let (home_key, child_home, inherited, from_row) = (
        "USERPROFILE",
        "C:\\srv\\agy",
        "C:\\data\\gemini",
        "C:\\srv\\row",
    );
    #[cfg(not(windows))]
    let (home_key, child_home, inherited, from_row) =
        ("HOME", "/srv/agy", "/data/gemini", "/srv/row");
    let base = || BTreeMap::from([(home_key.to_string(), child_home.to_string())]);

    let codegs_own = || Some(std::ffi::OsString::from(inherited));

    // ABSENT from the row: the child inherits codeg's, so codeg's own value
    // is the exact answer.
    assert_eq!(
        antigravity_acp_dir_with_inherited(&base(), codegs_own()).expect("nameable"),
        PathBuf::from(inherited).join(ANTIGRAVITY_ACP_SUBDIR)
    );

    // Present in the row: that is what the child is launched with, so it
    // outranks the inherited one.
    let mut overridden = base();
    overridden.insert("GEMINI_HOME".to_string(), from_row.to_string());
    assert_eq!(
        antigravity_acp_dir_with_inherited(&overridden, codegs_own()).expect("nameable"),
        PathBuf::from(from_row).join(ANTIGRAVITY_ACP_SUBDIR)
    );

    // Present but EMPTY is a removal (the spawn layer reads a blank as
    // `env_remove`), and a removal is NOT the same as absent: the child then
    // sees no `GEMINI_HOME` at all and falls back to `~/.gemini` under its
    // own home. Collapsing the two would send codeg to the inherited value
    // for a launch that deliberately took it away.
    let mut removed = base();
    removed.insert("GEMINI_HOME".to_string(), String::new());
    assert_eq!(
        antigravity_acp_dir_with_inherited(&removed, codegs_own()).expect("nameable"),
        PathBuf::from(child_home)
            .join(".gemini")
            .join(ANTIGRAVITY_ACP_SUBDIR)
    );

    // And codeg having none either is the plain default.
    assert_eq!(
        antigravity_acp_dir_with_inherited(&base(), None).expect("nameable"),
        PathBuf::from(child_home)
            .join(".gemini")
            .join(ANTIGRAVITY_ACP_SUBDIR)
    );
}

/// `GEMINI_HOME=~/x` names `$HOME/x` to the server, so it has to name the
/// same thing here.
///
/// Antigravity runs `os.path.expanduser` on the value
/// (`acp_server/paths.py`). codeg built the path with a bare
/// `PathBuf::from`, so it created a directory literally named `~` under its
/// own working directory and wrote `auth.type` into THAT — leaving
/// `session/new` failing with `Authentication required` no matter how many
/// times the panel was saved.
#[test]
fn antigravity_settings_path_expands_a_tilde_home_the_way_the_server_does() {
    let home = dirs::home_dir().expect("home dir");
    let runtime = BTreeMap::from([("GEMINI_HOME".to_string(), "~/agy-test".to_string())]);
    assert_eq!(
        antigravity_acp_dir_for_env(&runtime).expect("nameable"),
        home.join("agy-test").join("antigravity-acp")
    );

    // …and against the CHILD's home when the launch relocates it, since the
    // server runs its `expanduser` in that environment. Resolving against
    // codeg's home wrote the auth file into a tree the agent never opens.
    // Platform-native fixture: `child_home_dir` refuses a home that is not
    // absolute, and a unix-style `/srv/agy` has no drive prefix so Windows
    // does not consider it absolute. A shared literal would pass on unix
    // and fail in the Windows server CI cell, which runs these for real.
    #[cfg(windows)]
    let (home_key, child_home) = ("USERPROFILE", "C:\\srv\\agy");
    #[cfg(not(windows))]
    let (home_key, child_home) = ("HOME", "/srv/agy");

    let relocated = BTreeMap::from([
        (home_key.to_string(), child_home.to_string()),
        ("GEMINI_HOME".to_string(), "~/profile".to_string()),
    ]);
    assert_eq!(
        antigravity_acp_dir_for_env(&relocated).expect("nameable"),
        PathBuf::from(child_home)
            .join("profile")
            .join("antigravity-acp")
    );
    // The `~/.gemini` default follows it too.
    let default_under_child = BTreeMap::from([(home_key.to_string(), child_home.to_string())]);
    assert_eq!(
        antigravity_acp_dir_for_env(&default_under_child).expect("nameable"),
        PathBuf::from(child_home)
            .join(".gemini")
            .join("antigravity-acp")
    );
    // With the home REMOVED for the child there is no honest answer, and a
    // guess would both strand a tree and leave auth.type unwritten.
    let no_home = BTreeMap::from([
        (home_key.to_string(), String::new()),
        ("GEMINI_HOME".to_string(), "~/profile".to_string()),
    ]);
    assert!(antigravity_acp_dir_for_env(&no_home).is_err());
    // …unless the value is absolute, which does not depend on a home at all.
    let no_home_absolute = BTreeMap::from([
        (home_key.to_string(), String::new()),
        ("GEMINI_HOME".to_string(), "/srv/gemini".to_string()),
    ]);
    assert_eq!(
        antigravity_acp_dir_for_env(&no_home_absolute).expect("nameable"),
        PathBuf::from("/srv/gemini").join("antigravity-acp")
    );

    // An absolute value is still taken verbatim, and an EXACTLY empty one
    // means the spawn layer removed the var, so the child falls back to its
    // own default rather than to codeg's cwd.
    let absolute = BTreeMap::from([("GEMINI_HOME".to_string(), "/srv/gemini".to_string())]);
    assert_eq!(
        antigravity_acp_dir_for_env(&absolute).expect("nameable"),
        PathBuf::from("/srv/gemini").join("antigravity-acp")
    );
    let removed = BTreeMap::from([("GEMINI_HOME".to_string(), String::new())]);
    assert_eq!(
        antigravity_acp_dir_for_env(&removed).expect("nameable"),
        crate::parsers::antigravity::resolve_antigravity_acp_dir()
    );
}

#[test]
fn antigravity_settings_sync_leaves_a_foreign_auth_block_untouched() {
    // End to end: the refusal must reach the file, not just the merge.
    let dir = tempfile::tempdir().unwrap();
    let acp_dir = dir.path().join("antigravity-acp");
    std::fs::create_dir_all(&acp_dir).unwrap();
    let path = acp_dir.join("settings.json");
    let original = r#"{"auth":"managed-elsewhere","keep":1}"#;
    std::fs::write(&path, original).unwrap();

    let mut runtime = antigravity_runtime("oauth-personal");
    runtime.insert(
        "GEMINI_HOME".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    let report = sync_antigravity_settings_file(&runtime);

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(report.status, AntigravitySyncStatus::Skipped);
}

#[test]
fn codex_env_policy_forces_mcp_filter_off_and_overrides_user_twin() {
    // Codex gets the flag injected so codex-acp never drops the injected
    // `codeg-mcp` server on a config.toml name collision.
    let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
    apply_codex_env_policy(AgentType::Codex, &mut env, None);
    assert!(env
        .iter()
        .any(|(k, v)| k == "DISABLE_MCP_CONFIG_FILTERING" && v == "true"));

    // A user-supplied twin is replaced (not duplicated) so the override wins.
    let mut with_twin = vec![(
        "DISABLE_MCP_CONFIG_FILTERING".to_string(),
        "false".to_string(),
    )];
    apply_codex_env_policy(AgentType::Codex, &mut with_twin, None);
    let hits: Vec<_> = with_twin
        .iter()
        .filter(|(k, _)| k == "DISABLE_MCP_CONFIG_FILTERING")
        .collect();
    assert_eq!(hits.len(), 1, "no duplicate key");
    assert_eq!(hits[0].1, "true", "codeg override wins over user twin");
}

#[test]
fn codex_env_policy_is_noop_for_other_agents() {
    for agent in [AgentType::Grok, AgentType::ClaudeCode, AgentType::Gemini] {
        let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_codex_env_policy(agent, &mut env, Some("read-only"));
        assert!(
            !env.iter().any(|(k, _)| k == "DISABLE_MCP_CONFIG_FILTERING"),
            "{agent:?} must not receive the codex-only flag"
        );
        assert!(
            !env.iter().any(|(k, _)| k == "INITIAL_AGENT_MODE"),
            "{agent:?} must not receive codex's approval preset"
        );
    }
}

#[test]
fn codex_env_policy_injects_the_config_derived_approval_preset() {
    // Without this, codex-acp seeds its default `agent` preset and re-sends
    // that approvalPolicy every turn, so the user's ~/.codex/config.toml
    // sandbox/approval choice is dead (#442).
    let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
    apply_codex_env_policy(AgentType::Codex, &mut env, Some("read-only"));
    assert!(env
        .iter()
        .any(|(k, v)| k == "INITIAL_AGENT_MODE" && v == "read-only"));

    // Nothing mappable → stay out of the way entirely, leaving codex-acp's
    // own default rather than pinning a preset the user never chose.
    let mut none = vec![("PATH".to_string(), "/usr/bin".to_string())];
    apply_codex_env_policy(AgentType::Codex, &mut none, None);
    assert!(!none.iter().any(|(k, _)| k == "INITIAL_AGENT_MODE"));
}

#[test]
fn codex_env_policy_lets_an_explicit_env_preset_win_over_the_config() {
    // An explicit runtime-env key is a stronger signal than a config-file
    // inference, so it must NOT be clobbered (and must not be duplicated).
    let mut env = vec![(
        "INITIAL_AGENT_MODE".to_string(),
        "agent-full-access".to_string(),
    )];
    apply_codex_env_policy(AgentType::Codex, &mut env, Some("read-only"));
    let hits: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == "INITIAL_AGENT_MODE")
        .collect();
    assert_eq!(hits.len(), 1, "no duplicate key");
    assert_eq!(hits[0].1, "agent-full-access");

    // A blank twin carries no intent, so the config-derived value fills it.
    let mut blank = vec![("INITIAL_AGENT_MODE".to_string(), "  ".to_string())];
    apply_codex_env_policy(AgentType::Codex, &mut blank, Some("read-only"));
    let hits: Vec<_> = blank
        .iter()
        .filter(|(k, _)| k == "INITIAL_AGENT_MODE")
        .collect();
    assert_eq!(hits.len(), 1, "blank twin replaced, not duplicated");
    assert_eq!(hits[0].1, "read-only");
}

#[test]
fn synthesize_edit_single_diff_makes_canonical_edit() {
    let content = vec![diff_content("/a.rs", Some("old line\n"), "new line\n")];
    let json = synthesize_edit_input_from_diffs(&content).expect("one diff -> canonical edit");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["file_path"], "/a.rs");
    assert_eq!(v["old_string"], "old line\n");
    assert_eq!(v["new_string"], "new line\n");
    // Classifies as "edit" on the frontend via old_string/new_string.
    assert!(v.get("changes").is_none());
}

#[test]
fn synthesize_edit_new_file_uses_write_shape() {
    // codex-acp sends old_text=None for new files. Encode that as a write-
    // shaped input (`{file_path, content}`) so the frontend classifies it as
    // a creation (`inferFromInput` → "write" → `--- /dev/null` diff), not a
    // modification. Edit-shaped keys must be absent, or `inferFromInput`
    // would route it back to "edit".
    let content = vec![diff_content("/new.rs", None, "fn main() {}\n")];
    let json = synthesize_edit_input_from_diffs(&content).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["file_path"], "/new.rs");
    assert_eq!(v["content"], "fn main() {}\n");
    assert!(v.get("old_string").is_none());
    assert!(v.get("new_string").is_none());
}

#[test]
fn build_new_file_diff_matches_frontend_write_builder() {
    // Format parity with session-files.ts's `write` diff builder: a
    // `--- /dev/null` header (so `isAddedFileDiff` fires) then every
    // `split("\n")` segment — including the trailing empty one — as a `+`
    // line, with `+1,N` counting those segments.
    assert_eq!(
        build_new_file_diff("src/x.rs", "a\nb\n"),
        "--- /dev/null\n+++ b/src/x.rs\n@@ -0,0 +1,3 @@\n+a\n+b\n+"
    );
}

#[test]
fn synthesize_edit_multi_diff_makes_changes_map() {
    let content = vec![
        diff_content("/a.rs", Some("a-old"), "a-new"),
        diff_content("/b.rs", None, "b-new"),
    ];
    let json = synthesize_edit_input_from_diffs(&content).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    // Object map keyed by path — the shape extractEditChangesPayload reads.
    // /a.rs is an edit → old/new text for the frontend's generateUnifiedDiff.
    assert_eq!(v["changes"]["/a.rs"]["old_text"], "a-old");
    assert_eq!(v["changes"]["/a.rs"]["new_text"], "a-new");
    // /b.rs is a new file (old_text=None) → a ready-made creation diff whose
    // `--- /dev/null` header makes `isAddedFileDiff` classify it as new;
    // it must NOT carry old_text/new_text (that path builds a `--- a/…`
    // modification diff instead).
    let b_diff = v["changes"]["/b.rs"]["diff"]
        .as_str()
        .expect("new-file entry carries a prebuilt diff");
    assert!(b_diff.contains("--- /dev/null"));
    assert!(b_diff.contains("+b-new"));
    assert!(v["changes"]["/b.rs"].get("old_text").is_none());
    assert!(v["changes"]["/b.rs"].get("new_text").is_none());
}

#[test]
fn synthesize_edit_returns_none_without_diff() {
    // No Diff block -> None, so callers keep the agent's own raw_input.
    assert!(synthesize_edit_input_from_diffs(&[]).is_none());
}

#[test]
fn synthesize_edit_combines_the_hunks_of_one_file() {
    // codex-acp 2.0.0 without a patch sends ONE block per hunk; claude
    // sends one per `structuredPatch` hunk after a multi-site Edit. Keyed
    // by path, every hunk but the last used to vanish.
    let content = vec![
        diff_content(
            "/a.rs",
            Some("fn a() {\n    1\n}\n"),
            "fn a() {\n    2\n}\n",
        ),
        diff_content("/a.rs", Some("fn z() {\n    1\n}"), "fn z() {\n    3\n}"),
    ];
    let v: serde_json::Value =
        serde_json::from_str(&synthesize_edit_input_from_diffs(&content).unwrap()).unwrap();
    assert_eq!(v["file_path"], "/a.rs");
    assert_eq!(
        v["old_string"],
        format!("fn a() {{\n    1\n}}\n{HUNK_SEPARATOR}\nfn z() {{\n    1\n}}")
    );
    assert_eq!(
        v["new_string"],
        format!("fn a() {{\n    2\n}}\n{HUNK_SEPARATOR}\nfn z() {{\n    3\n}}")
    );

    // Across several files, each file's hunks combine in its own entry.
    let content = vec![
        diff_content("/a.rs", Some("a1"), "A1"),
        diff_content("/b.rs", Some("b1"), "B1"),
        diff_content("/a.rs", Some("a2"), "A2"),
    ];
    let v: serde_json::Value =
        serde_json::from_str(&synthesize_edit_input_from_diffs(&content).unwrap()).unwrap();
    assert_eq!(
        v["changes"]["/a.rs"]["old_text"],
        format!("a1\n{HUNK_SEPARATOR}\na2")
    );
    assert_eq!(
        v["changes"]["/a.rs"]["new_text"],
        format!("A1\n{HUNK_SEPARATOR}\nA2")
    );
    assert_eq!(v["changes"]["/b.rs"]["old_text"], "b1");
}

/// A codex-acp 2.0.0 `diffPatch` block, as recorded off its scenario
/// harness with the capability declared.
fn patch_block(path: &str, patch: serde_json::Value) -> ToolCallContent {
    let mut d = Diff::new(path, "");
    d.meta = Some(meta_map(serde_json::json!({
        "kind": "update",
        "jetbrains": {"air": {"version": 1, "diffPatch": patch}}
    })));
    ToolCallContent::Diff(d)
}

#[test]
fn synthesize_edit_takes_a_codex_git_patch_with_absolute_headers() {
    let content = vec![patch_block(
        "/workspace/src/app.ts",
        serde_json::json!({
            "version": 1,
            "format": "git_patch",
            "text": "diff --git a/workspace/src/app.ts b/workspace/src/app.ts\n--- a/workspace/src/app.ts\n+++ b/workspace/src/app.ts\n@@ -1,2 +1,2 @@\n-const a = 1;\n+const a = 2;\n export {a};\n"
        }),
    )];
    let v: serde_json::Value =
        serde_json::from_str(&synthesize_edit_input_from_diffs(&content).unwrap()).unwrap();
    // The only file: its path titles the card, the patch renders verbatim.
    assert_eq!(v["file_path"], "/workspace/src/app.ts");
    assert_eq!(
            v["changes"]["/workspace/src/app.ts"]["diff"],
            "--- a//workspace/src/app.ts\n+++ b//workspace/src/app.ts\n@@ -1,2 +1,2 @@\n-const a = 1;\n+const a = 2;\n export {a};\n"
        );
    // The placeholders never reach the content text.
    assert!(serialize_tool_call_content(&content, true).is_none());
}

#[test]
fn a_git_patch_keeps_added_deleted_and_renamed_headers() {
    let added = normalize_git_patch(
            "diff --git a/workspace/new.txt b/workspace/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/workspace/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n",
            "/workspace/new.txt",
        )
        .unwrap();
    assert_eq!(
            added,
            "new file mode 100644\n--- /dev/null\n+++ b//workspace/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n"
        );
    let deleted = normalize_git_patch(
            "diff --git a/workspace/old.txt b/workspace/old.txt\ndeleted file mode 100644\n--- a/workspace/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
            "/workspace/old.txt",
        )
        .unwrap();
    assert!(
        deleted.starts_with("deleted file mode 100644\n--- a//workspace/old.txt\n+++ /dev/null\n")
    );
    let renamed = normalize_git_patch(
            "diff --git a/workspace/before.ts b/workspace/after.ts\nrename from workspace/before.ts\nrename to workspace/after.ts\n--- a/workspace/before.ts\n+++ b/workspace/after.ts\n@@ -1 +1 @@\n-x\n+y\n",
            "/workspace/after.ts",
        )
        .unwrap();
    assert_eq!(
            renamed,
            "rename from /workspace/before.ts\nrename to /workspace/after.ts\n--- a//workspace/before.ts\n+++ b//workspace/after.ts\n@@ -1 +1 @@\n-x\n+y\n"
        );
    // A removed line that happens to start with `-- ` is content, not a
    // header, once the first hunk has begun.
    let tricky = normalize_git_patch(
        "--- a/w/f\n+++ b/w/f\n@@ -1 +1 @@\n--- not a header\n+++ nor this\n",
        "/w/f",
    )
    .unwrap();
    assert!(tricky.ends_with("@@ -1 +1 @@\n--- not a header\n+++ nor this\n"));
    // Windows: codex writes the drive path without a leading slash.
    assert_eq!(patch_header_path("a/C:/work/App.ts"), "C:/work/App.ts");
    assert_eq!(
        patch_header_path("\"a/w/with \\\"q\\\".ts\""),
        "/w/with \"q\".ts"
    );
    assert_eq!(patch_header_path("b/w/has space.ts\t"), "/w/has space.ts");
    // Git's C quoting of a control character, and of a raw byte in octal.
    // A backslash is `\\`, a tab `\t`, a raw byte `\NNN` octal.
    assert_eq!(
        patch_header_path("\"a/w/back\\\\slash.ts\""),
        "/w/back\\slash.ts"
    );
    assert_eq!(
        patch_header_path("\"a/w/tab\\there.ts\""),
        "/w/tab\there.ts"
    );
    assert_eq!(patch_header_path("\"a/w/bell\\007.ts\""), "/w/bell\u{7}.ts");
    // No hunk, no patch.
    assert!(normalize_git_patch("--- a/w/f\n+++ b/w/f\n", "/w/f").is_none());
}

#[test]
fn a_git_patch_keeps_the_carriage_returns_of_its_hunks() {
    // "The patch keeps the provider bytes, including a carriage return."
    let normalized = normalize_git_patch(
            "diff --git a/w/f.txt b/w/f.txt\r\n--- a/w/f.txt\r\n+++ b/w/f.txt\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n",
            "/w/f.txt",
        )
        .unwrap();
    assert_eq!(
        normalized,
        "--- a//w/f.txt\n+++ b//w/f.txt\n@@ -1 +1 @@\r\n-old\r\n+new\r\n"
    );
    // Without a final newline, none is invented.
    assert_eq!(
        normalize_git_patch("--- a/w/f\n+++ b/w/f\n@@ -1 +1 @@\n-a\n+b", "/w/f").unwrap(),
        "--- a//w/f\n+++ b//w/f\n@@ -1 +1 @@\n-a\n+b"
    );
}

#[test]
fn an_unusable_patch_block_is_shown_as_nothing_not_as_an_empty_file() {
    // The contract: a receiver that rejects the patch "must show the
    // change as unavailable" — the `oldText: null` / `newText: ""`
    // placeholders must never read as a freshly created empty file.
    for bad in [
        serde_json::json!({"version": 2, "format": "git_patch", "text": "@@ -1 +1 @@\n-a\n+b\n"}),
        serde_json::json!({"version": 1, "format": "unified", "text": "@@ -1 +1 @@\n-a\n+b\n"}),
        serde_json::json!({"version": 1, "format": "git_patch", "text": "no hunk"}),
        serde_json::json!({"version": 1, "format": "git_patch"}),
    ] {
        let content = vec![patch_block("/w/f", bad.clone())];
        assert!(
            synthesize_edit_input_from_diffs(&content).is_none(),
            "{bad} must not synthesize an edit"
        );
        assert!(serialize_tool_call_content(&content, true).is_none());
    }
}

fn claude_meta(tool: &str) -> serde_json::Map<String, serde_json::Value> {
    meta_map(serde_json::json!({"claudeCode": {"toolName": tool}}))
}

#[test]
fn claude_edit_input_gets_its_text_back_from_the_diff() {
    // claude-agent-acp 0.82.0 (AIR), recorded: `rawInput` is `{file_path}`
    // alone and the model's strings ride only in the diff block.
    let content = vec![diff_content(
        "/w/app.ts",
        Some("const value = 1;"),
        "const value = 2;",
    )];
    let merged = claude_complete_file_edit_input(
        AgentType::ClaudeCode,
        Some(&claude_meta("Edit")),
        Some(r#"{"file_path":"/w/app.ts","replace_all":false}"#),
        &content,
    )
    .expect("a stripped Edit input is completed");
    let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(v["file_path"], "/w/app.ts");
    assert_eq!(v["old_string"], "const value = 1;");
    assert_eq!(v["new_string"], "const value = 2;");
    assert_eq!(v["replace_all"], false, "the input's own keys survive");

    // A Write: the new text is the file.
    let content = vec![diff_content("/w/new.ts", None, "export const x = 1;\n")];
    let merged = claude_complete_file_edit_input(
        AgentType::ClaudeCode,
        Some(&claude_meta("Write")),
        Some(r#"{"file_path":"/w/new.ts"}"#),
        &content,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(v["content"], "export const x = 1;\n");

    // An input that still carries its text (≤0.81) is authoritative.
    assert!(claude_complete_file_edit_input(
        AgentType::ClaudeCode,
        Some(&claude_meta("Edit")),
        Some(r#"{"file_path":"/w/app.ts","old_string":"a","new_string":"b"}"#),
        &[diff_content("/w/app.ts", Some("x"), "y")],
    )
    .is_none());
    // Other tools, other agents, and frames without a diff: nothing to do.
    assert!(claude_complete_file_edit_input(
        AgentType::ClaudeCode,
        Some(&claude_meta("Bash")),
        Some(r#"{"command":"ls"}"#),
        &[diff_content("/w/app.ts", Some("x"), "y")],
    )
    .is_none());
    assert!(claude_complete_file_edit_input(
        AgentType::Codex,
        Some(&claude_meta("Edit")),
        Some(r#"{"file_path":"/w/app.ts"}"#),
        &[diff_content("/w/app.ts", Some("x"), "y")],
    )
    .is_none());
    assert!(claude_complete_file_edit_input(
        AgentType::ClaudeCode,
        Some(&claude_meta("Edit")),
        Some(r#"{"file_path":"/w/app.ts"}"#),
        &[],
    )
    .is_none());
}

#[test]
fn codex_stdin_is_bridged_the_way_older_adapters_sent_it() {
    // 1.13.x: an output delta of `\ny\n`. 2.0.0 (AIR): `_meta.terminal_input`.
    let stdin = meta_map(serde_json::json!({
        "terminal_input": {"data": "y", "terminal_id": "cmd-1"}
    }));
    assert_eq!(
        hosted_terminal_output_delta(AgentType::Codex, Some(&stdin)),
        Some("\ny\n".to_string())
    );
    let both = meta_map(serde_json::json!({
        "terminal_output_delta": {"data": "Continue? ", "terminal_id": "cmd-1"},
        "terminal_input": {"data": "y", "terminal_id": "cmd-1"}
    }));
    assert_eq!(
        hosted_terminal_output_delta(AgentType::Codex, Some(&both)),
        Some("Continue? \ny\n".to_string())
    );
    // pi has no stdin channel; its unnamespaced key is not read.
    assert_eq!(
        hosted_terminal_output_delta(AgentType::Pi, Some(&stdin)),
        None
    );
}

#[test]
fn a_viewed_claude_result_fills_only_a_bare_completion() {
    use agent_client_protocol::schema::v1::ToolCallStatus;
    let mut stash: HashMap<String, String> =
        HashMap::from([("toolu_read".to_string(), "1\tconst a = 1;".to_string())]);
    // Not terminal yet: keep waiting.
    assert!(claude_viewed_result_for_completion(
        AgentType::ClaudeCode,
        &mut stash,
        "toolu_read",
        Some(&ToolCallStatus::InProgress),
        false,
    )
    .is_none());
    assert!(stash.contains_key("toolu_read"));
    // The AIR completion: a bare status. The stashed text fills it.
    assert_eq!(
        claude_viewed_result_for_completion(
            AgentType::ClaudeCode,
            &mut stash,
            "toolu_read",
            Some(&ToolCallStatus::Completed),
            false,
        )
        .as_deref(),
        Some("1\tconst a = 1;")
    );
    assert!(stash.is_empty(), "spent");

    // A completion that carries its own result (any pre-AIR adapter) is
    // never overridden — but the entry is still spent.
    stash.insert("toolu_grep".into(), "stale".into());
    assert!(claude_viewed_result_for_completion(
        AgentType::ClaudeCode,
        &mut stash,
        "toolu_grep",
        Some(&ToolCallStatus::Completed),
        true,
    )
    .is_none());
    assert!(stash.is_empty());
    // A failure carries its own text.
    stash.insert("toolu_x".into(), "stale".into());
    assert!(claude_viewed_result_for_completion(
        AgentType::ClaudeCode,
        &mut stash,
        "toolu_x",
        Some(&ToolCallStatus::Failed),
        false,
    )
    .is_none());
    assert!(stash.is_empty());
}

#[test]
fn claude_viewed_results_are_stashed_only_for_announced_viewed_file_tools() {
    let mut cb = CodeBuddyLiveState::default();
    cb.air_tool_call_meta.open(
        AgentType::ClaudeCode,
        "toolu_read",
        Some(claude_meta("Read")),
    );
    cb.air_tool_call_meta.open(
        AgentType::ClaudeCode,
        "toolu_bash",
        Some(claude_meta("Bash")),
    );
    let params = serde_json::json!({
        "sessionId": "s",
        "message": {"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_read", "content": "1\tconst a = 1;"},
            {"type": "tool_result", "tool_use_id": "toolu_bash", "content": "a.ts"},
            {"type": "tool_result", "tool_use_id": "toolu_unseen", "content": "?"}
        ]}}
    });
    stash_claude_viewed_results(&params, &mut cb);
    assert_eq!(
        cb.claude_viewed_results
            .get("toolu_read")
            .map(String::as_str),
        Some("1\tconst a = 1;")
    );
    assert!(!cb.claude_viewed_results.contains_key("toolu_bash"));
    assert!(!cb.claude_viewed_results.contains_key("toolu_unseen"));
}

#[test]
fn codex_activity_is_classified_off_the_raw_input_once_the_marker_is_gone() {
    // codex-acp 2.0.0, recorded: no `_meta.codex.subagent`; the activity's
    // three facts ride `rawInput`, and the AIR flag says only "subagent".
    let meta =
        meta_map(serde_json::json!({"jetbrains": {"air": {"subagent": true, "version": 1}}}));
    let raw = serde_json::json!({
        "activityKind": "started", "agentPath": "/root/weather", "agentThreadId": "child-thread"
    });
    let derived = codex_activity_classification_meta(AgentType::Codex, Some(&meta), Some(&raw))
        .expect("an activity input classifies");
    match classify_codex_subagent_activity(AgentType::Codex, Some(&derived)) {
        CodexSubagentActivity::Started { thread_id, .. } => {
            assert_eq!(thread_id.as_deref(), Some("child-thread"));
        }
        other => panic!("expected a launch, got {other:?}"),
    }
    // A legacy marker is read as it is.
    let legacy = meta_map(serde_json::json!({
        "codex": {"subagent": {"threadId": "t", "path": "/root/x", "activity": "started"}}
    }));
    assert!(
        codex_activity_classification_meta(AgentType::Codex, Some(&legacy), Some(&raw)).is_none()
    );
    // A collaboration call (same AIR flag, different input) is no activity.
    let collab = serde_json::json!({
        "senderThreadId": "s", "receiverThreadIds": ["c"], "agentsStates": {}, "prompt": "p"
    });
    assert!(
        codex_activity_classification_meta(AgentType::Codex, Some(&meta), Some(&collab)).is_none()
    );
    assert!(codex_activity_classification_meta(AgentType::ClaudeCode, None, Some(&raw)).is_none());
}

#[test]
fn serialize_excludes_diffs_when_hoisted_to_raw_input() {
    let content = vec![diff_content("/a.rs", Some("old"), "new")];
    // Default keeps the diff (unchanged behavior for non-hoisted content).
    assert!(serialize_tool_call_content(&content, true)
        .unwrap()
        .contains("--- /a.rs"));
    // When the edit is hoisted into raw_input, the diff is dropped so it
    // isn't shipped twice and the header stats don't read the full-file blob.
    assert!(serialize_tool_call_content(&content, false).is_none());
}

#[test]
fn pi_preflight_flags_missing_custom_command() {
    let mut env = BTreeMap::new();
    env.insert(
        "PI_ACP_PI_COMMAND".to_string(),
        "/nonexistent/definitely-not-pi-xyz".to_string(),
    );
    let msg = pi_launch_preflight(&env, &std::env::temp_dir())
        .expect("an unresolvable custom pi command must be flagged");
    // Frontend invariant: routes to the localized SDK-missing install prompt.
    assert!(msg.contains("is not installed"), "got: {msg}");
    assert!(msg.contains("definitely-not-pi-xyz"), "got: {msg}");
}

#[test]
fn pi_preflight_accepts_resolvable_custom_command() {
    // A binary we know exists and is executable on this platform — proves the
    // preflight clears (returns None) for a resolvable PI_ACP_PI_COMMAND.
    let existing = if cfg!(windows) {
        "C:\\Windows\\System32\\cmd.exe"
    } else {
        "/bin/sh"
    };
    let mut env = BTreeMap::new();
    env.insert("PI_ACP_PI_COMMAND".to_string(), existing.to_string());
    assert!(pi_launch_preflight(&env, &std::env::temp_dir()).is_none());
}

/// A bare `pi` is looked up on the PATH the launch hands the child, which
/// can differ from codeg's own (a per-agent override, the OfficeCLI prepend).
#[cfg(unix)]
#[test]
fn pi_preflight_searches_the_launch_path() {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::tempdir().unwrap();
    let pi = bin.path().join("pi");
    std::fs::write(&pi, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".to_string(),
        bin.path().to_string_lossy().into_owned(),
    );
    assert!(pi_launch_preflight(&env, cwd.path()).is_none());

    let empty = tempfile::tempdir().unwrap();
    env.insert(
        "PATH".to_string(),
        empty.path().to_string_lossy().into_owned(),
    );
    let msg = pi_launch_preflight(&env, cwd.path()).expect("no pi on the launch PATH");
    assert!(msg.contains("is not installed"), "got: {msg}");
}

/// A relative `PATH` entry is searched inside the workspace, where pi-acp's
/// spawn runs — never in codeg's own cwd.
#[cfg(unix)]
#[test]
fn pi_preflight_anchors_a_relative_path_entry_in_the_workspace() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("tools")).unwrap();
    let pi = workspace.path().join("tools").join("pi");
    std::fs::write(&pi, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = BTreeMap::from([("PATH".to_string(), "tools".to_string())]);
    assert!(pi_launch_preflight(&env, workspace.path()).is_none());
}

/// pi-acp spawns a relative command with the session's workspace as cwd, so
/// that is where it must exist — not in codeg's own cwd.
#[cfg(unix)]
#[test]
fn pi_preflight_resolves_a_relative_command_in_the_workspace() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let script = workspace.path().join("pi-test.sh");
    std::fs::write(&script, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = BTreeMap::new();
    env.insert("PI_ACP_PI_COMMAND".to_string(), "./pi-test.sh".to_string());
    assert!(pi_launch_preflight(&env, workspace.path()).is_none());

    let elsewhere = tempfile::tempdir().unwrap();
    assert!(pi_launch_preflight(&env, elsewhere.path()).is_some());
}

/// The two answers pi-acp 0.0.34 gives when pi predates
/// `get_available_thinking_levels`, as captured from a live pi-acp 0.0.34
/// driving pi 0.80.10: `RequestError.internalError({}, msg)` from
/// `session/new`, and a plain `Error` from `session/load`, which its ACP SDK
/// files under `data.details`.
fn pi_outdated_open_failures() -> [agent_client_protocol::Error; 2] {
    let pi_said = "pi get_available_thinking_levels failed: \
                       Unknown command: get_available_thinking_levels";
    [
        agent_client_protocol::Error::new(-32603, format!("Internal error: {pi_said}"))
            .data(serde_json::json!({})),
        agent_client_protocol::Error::new(-32603, "Internal error")
            .data(serde_json::json!({ "details": pi_said })),
    ]
}

#[test]
fn an_outdated_pi_is_recognised_on_both_session_opens() {
    for failure in pi_outdated_open_failures() {
        let tagged = tag_pi_runtime_outdated(&failure, AgentType::Pi)
            .unwrap_or_else(|| panic!("not recognised: {failure}"));
        assert!(tagged.to_string().contains(PI_RUNTIME_OUTDATED_SENTINEL));
        // Only pi-acp speaks for pi; another agent's identical text is not
        // evidence about pi.
        assert!(tag_pi_runtime_outdated(&failure, AgentType::Codex).is_none());
    }
}

/// Other failures of the same RPC — pi answered, the answer was wrong — are
/// not an old pi and must keep their own reading.
#[test]
fn other_pi_open_failures_are_not_called_outdated() {
    for message in [
        "Internal error: pi returned a thinking level absent from available levels",
        "Internal error: pi get_available_thinking_levels returned invalid levels",
        "Internal error: pi get_available_models failed: Unknown command: get_available_models",
    ] {
        let failure = agent_client_protocol::Error::new(-32603, message);
        assert!(
            tag_pi_runtime_outdated(&failure, AgentType::Pi).is_none(),
            "{message}"
        );
    }
}

#[test]
fn an_outdated_pi_outranks_the_other_session_new_readings() {
    let [new_failure, load_failure] = pi_outdated_open_failures();
    let tagged = tag_new_session_failure(new_failure, AgentType::Pi, &[]);
    let raw = tagged.to_string();
    assert!(raw.contains(PI_RUNTIME_OUTDATED_SENTINEL), "{raw}");
    assert!(!raw.contains(MCP_SUSPECT_SENTINEL), "{raw}");
    assert!(!raw.contains(AUTH_REQUIRED_SENTINEL), "{raw}");

    // Both opens end as the same coded error, with codeg's own
    // instructions in place of the wire text.
    let load_tagged = tag_pi_runtime_outdated(&load_failure, AgentType::Pi).unwrap();
    for err in [connection_failure(tagged), connection_failure(load_tagged)] {
        assert_eq!(err.code(), Some("agent_runtime_outdated"));
        let shown = err.to_string();
        assert!(shown.contains(registry::PI_MIN_RUNTIME_VERSION), "{shown}");
        assert!(!shown.contains(PI_RUNTIME_OUTDATED_SENTINEL), "{shown}");
    }
}

#[test]
fn prepend_path_unix_prepends_and_keeps_single_key() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    prepend_dir_to_path_env(&mut env, "/home/u/.local/bin", "/fallback", false);
    assert_eq!(env.get("PATH").unwrap(), "/home/u/.local/bin:/usr/bin:/bin");
    assert_eq!(env.keys().filter(|k| k.as_str() == "PATH").count(), 1);
}

#[test]
fn prepend_path_unix_seeds_from_fallback_when_absent() {
    let mut env = BTreeMap::new();
    prepend_dir_to_path_env(&mut env, "/x/bin", "/usr/bin:/bin", false);
    assert_eq!(env.get("PATH").unwrap(), "/x/bin:/usr/bin:/bin");
}

#[test]
fn prepend_path_windows_is_case_insensitive_and_no_clobber() {
    // Regression for the `Path` vs `PATH` clobber: a pre-existing `Path`
    // must be reused (not joined by a second `PATH` key that a later
    // case-insensitive `Command::env` could overwrite).
    let mut env = BTreeMap::new();
    env.insert("Path".to_string(), r"C:\Windows".to_string());
    prepend_dir_to_path_env(
        &mut env,
        r"C:\Users\u\AppData\Local\OfficeCLI",
        "ignored-fallback",
        true,
    );
    // Exactly one PATH-ish key, the original casing preserved, value prepended.
    let path_keys: Vec<&String> = env
        .keys()
        .filter(|k| k.eq_ignore_ascii_case("PATH"))
        .collect();
    assert_eq!(path_keys.len(), 1, "{env:?}");
    assert_eq!(
        env.get("Path").unwrap(),
        r"C:\Users\u\AppData\Local\OfficeCLI;C:\Windows"
    );
}

#[test]
fn prepend_path_windows_seeds_from_fallback_with_semicolon() {
    let mut env = BTreeMap::new();
    prepend_dir_to_path_env(
        &mut env,
        r"C:\OfficeCLI",
        r"C:\Windows;C:\Windows\System32",
        true,
    );
    // No prior key → default `Path` casing on Windows.
    assert_eq!(
        env.get("Path").unwrap(),
        r"C:\OfficeCLI;C:\Windows;C:\Windows\System32"
    );
}

#[test]
fn prepend_path_windows_collapses_duplicate_casings() {
    // Pathological but possible: both `PATH` and `Path` present. All
    // PATH-ish keys must collapse to exactly one, prepended onto the
    // effective (last-applied → `Path`) value, so no stale duplicate can
    // overwrite the injected dir when the child Command applies env.
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), r"C:\a".to_string());
    env.insert("Path".to_string(), r"C:\b".to_string());
    prepend_dir_to_path_env(&mut env, r"C:\OfficeCLI", "ignored-fallback", true);
    let path_keys: Vec<&String> = env
        .keys()
        .filter(|k| k.eq_ignore_ascii_case("PATH"))
        .collect();
    assert_eq!(
        path_keys.len(),
        1,
        "exactly one PATH-ish key must remain: {env:?}"
    );
    assert_eq!(env.get("Path").unwrap(), r"C:\OfficeCLI;C:\b");
}

#[test]
fn client_capabilities_gate_per_agent() {
    // Serialize to inspect the wire shape — `_meta` is the serde rename
    // and the exact key path the adapters read.
    let caps_of = |agent: AgentType| {
        serde_json::to_value(build_client_capabilities(agent, HostToolsPolicy::Default))
            .expect("caps serialize")
    };

    // Claude Code: subagent-transcript opt-in (strict boolean true), and
    // no elicitation (which would un-gate AskUserQuestion duplication).
    let claude = caps_of(AgentType::ClaudeCode);
    assert_eq!(
        claude["_meta"]["subagent-transcript"],
        serde_json::Value::Bool(true)
    );
    assert!(claude.get("elicitation").is_none());

    // Codex: form elicitation; its `_meta` carries ONLY the AIR
    // advertisement (no subagent-transcript, which is claude's opt-in).
    let codex = caps_of(AgentType::Codex);
    assert!(codex.get("elicitation").is_some());
    assert!(codex["_meta"].get("subagent-transcript").is_none());
    assert!(codex["_meta"].get("jetbrains").is_some());

    // DeepSeek: form elicitation too — deepseek-acp routes its
    // ask_user_question + plan review through `elicitation/create` forms
    // when the bit is advertised (button fallback otherwise).
    let deepseek = caps_of(AgentType::DeepSeek);
    assert!(deepseek.get("elicitation").is_some());
    assert!(deepseek.get("_meta").is_none());

    // Cursor: parameterized model picker only (no elicitation / AIR).
    let cursor = caps_of(AgentType::Cursor);
    assert_eq!(
        cursor["_meta"]["parameterizedModelPicker"],
        serde_json::Value::Bool(true)
    );
    assert!(cursor.get("elicitation").is_none());
    assert!(cursor["_meta"].get("jetbrains").is_none());

    // Everyone else: neither gate; fs + terminal always advertised.
    let other = caps_of(AgentType::Gemini);
    assert!(other.get("_meta").is_none());
    assert!(other.get("elicitation").is_none());
    assert_eq!(other["terminal"], serde_json::Value::Bool(true));
    assert_eq!(other["fs"]["readTextFile"], serde_json::Value::Bool(true));
}

#[test]
fn host_tools_agent_withholds_both_execution_channels() {
    let caps_of = |agent: AgentType, host_tools: HostToolsPolicy| {
        serde_json::to_value(build_client_capabilities(agent, host_tools)).expect("caps serialize")
    };

    // #436: the whole point. An agent told codeg hosts neither channel
    // does its own reads and runs its own shell, so its OS sandbox — the
    // only control still working under `grok agent stdio` — covers them.
    // BOTH must go: leaving either advertised hands the agent a way back
    // into codeg's unsandboxed process for the same file.
    //
    // `ClientCapabilities` serializes its unset fields as explicit `false`
    // rather than omitting them, so assert THAT shape — not absence. The
    // two are equivalent on the wire, verified against grok 1.0.0 under a
    // kernel `deny`: sending `{fs:{readTextFile:false,…},terminal:false}`
    // produced the same outcome as omitting the keys entirely (local read →
    // `EPERM`, every shell fallback blocked, `FsViolation` audited).
    let withheld = caps_of(AgentType::Grok, HostToolsPolicy::Agent);
    assert_eq!(withheld["terminal"], serde_json::Value::Bool(false));
    assert_eq!(
        withheld["fs"]["readTextFile"],
        serde_json::Value::Bool(false)
    );
    assert_eq!(
        withheld["fs"]["writeTextFile"],
        serde_json::Value::Bool(false)
    );

    // Default is untouched — this is opt-in, and a regression here would
    // silently break every agent's terminal.
    let hosted = caps_of(AgentType::Grok, HostToolsPolicy::Default);
    assert_eq!(hosted["terminal"], serde_json::Value::Bool(true));
    assert_eq!(hosted["fs"]["readTextFile"], serde_json::Value::Bool(true));
    assert_eq!(hosted["fs"]["writeTextFile"], serde_json::Value::Bool(true));

    // The per-agent gates are about a DIFFERENT axis (which optional
    // protocol surfaces each adapter understands) and must survive the
    // withholding — dropping codex's elicitation would strand its Plan-mode
    // `request_user_input`, and dropping claude's `_meta` would silently
    // turn subagent transcripts back off.
    let codex = caps_of(AgentType::Codex, HostToolsPolicy::Agent);
    assert!(codex.get("elicitation").is_some());
    assert_eq!(codex["terminal"], serde_json::Value::Bool(false));
    let claude = caps_of(AgentType::ClaudeCode, HostToolsPolicy::Agent);
    assert_eq!(
        claude["_meta"]["subagent-transcript"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(claude["fs"]["readTextFile"], serde_json::Value::Bool(false));
}

#[test]
fn a_withheld_channel_is_refused_as_method_not_found() {
    // Every channel codeg stops advertising must also stop being SERVED.
    // Advertisement is a declaration; an agent that calls the method anyway
    // (or a future adapter that ignores client capabilities) would otherwise
    // land the operation back in codeg's unsandboxed process — the bug.
    for method in [
        "fs/read_text_file",
        "fs/write_text_file",
        "terminal/create",
        "terminal/output",
        "terminal/wait_for_exit",
        "terminal/kill",
        "terminal/release",
    ] {
        let error = unadvertised_channel_error(method);
        assert_eq!(
            error.code,
            agent_client_protocol::Error::method_not_found().code
        );
        let text = error.to_string();
        // The knob has to be named: a bare "Method not found" on a channel
        // that worked yesterday reads as a codeg bug, not as a setting.
        assert!(text.contains(method), "{text}");
        assert!(text.contains(HOST_TOOLS_ENV), "{text}");
    }
}

#[test]
fn strict_fs_policy_is_only_a_boundary_once_the_terminal_is_withheld() {
    // Pins the condition behind the connect-time warning: `strict` gates
    // reads, but that gate is walkable through a shell for as long as codeg
    // serves one. The two knobs are orthogonal — this asserts the predicate
    // pair the warning keys off, so the warning can't silently stop firing.
    let strict = FsAccessPolicy::strict(Path::new("/workspace"));
    assert!(strict.confines_reads());
    assert!(HostToolsPolicy::Default.hosts_channels());
    assert!(!HostToolsPolicy::Agent.hosts_channels());

    // The default policy leaves reads open, so there is no false promise to
    // warn about — only writes are rooted.
    let permissive =
        FsAccessPolicy::permissive(Path::new("/workspace"), AgentType::Grok, &BTreeMap::new());
    assert!(!permissive.confines_reads());
}

#[test]
fn claude_raw_sdk_meta_enabled_only_for_claude() {
    let claude_meta =
        claude_raw_sdk_session_meta(AgentType::ClaudeCode).expect("Claude must have raw SDK meta");
    assert_eq!(
        claude_meta
            .get("claudeCode")
            .and_then(|v| v.get("emitRawSDKMessages"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    assert!(claude_raw_sdk_session_meta(AgentType::Codex).is_none());
}

#[test]
fn map_claude_sdk_ext_notification_maps_valid_payload() {
    let raw = UntypedMessage::new(
        "_claude/sdkMessage",
        serde_json::json!({
            "sessionId": "session-123",
            "message": {
                "type": "system",
                "subtype": "api_retry",
                "attempt": 3,
                "max_retries": 10
            }
        }),
    )
    .unwrap();

    let event = map_claude_sdk_ext_notification(&raw).expect("valid sdk payload should map");

    match event {
        AcpEvent::ClaudeSdkMessage {
            session_id,
            message,
        } => {
            // connection_id 不再属于 AcpEvent，envelope 上提到顶层
            assert_eq!(session_id, "session-123");
            assert_eq!(message.get("type").and_then(|v| v.as_str()), Some("system"));
        }
        _ => panic!("expected ClaudeSdkMessage"),
    }
}

#[test]
fn is_known_ext_method_covers_every_mapped_method() {
    // The anti-log-storm invariant: `_claude/sdkMessage` arrives for EVERY
    // SDK message but only maps when it is an API retry, so it must be
    // recognized here — otherwise each unmapped one logs a line on a
    // per-message hot path.
    assert!(is_known_ext_method(CLAUDE_SDK_EXT_METHOD));
    for method in GROK_EXT_UPDATE_METHODS {
        assert!(is_known_ext_method(method), "{method} must be known");
    }
    // A method no mapper claims is exactly what the log is for.
    assert!(!is_known_ext_method("_vendor/somethingNew"));
    assert!(!is_known_ext_method("session/update"));
}

#[test]
fn map_claude_sdk_ext_notification_rejects_non_api_retry() {
    let non_retry = UntypedMessage::new(
        "_claude/sdkMessage",
        serde_json::json!({
            "sessionId": "session-123",
            "message": {"type": "system", "subtype": "status"}
        }),
    )
    .unwrap();
    assert!(map_claude_sdk_ext_notification(&non_retry).is_none());
}

#[test]
fn map_claude_sdk_ext_notification_rejects_invalid_payload() {
    let wrong_method = UntypedMessage::new(
        "_other/method",
        serde_json::json!({"sessionId": "s", "message": {}}),
    )
    .unwrap();
    assert!(map_claude_sdk_ext_notification(&wrong_method).is_none());

    let missing_fields =
        UntypedMessage::new("_claude/sdkMessage", serde_json::json!({"sessionId": 1})).unwrap();
    assert!(map_claude_sdk_ext_notification(&missing_fields).is_none());
}

/// `_claude/sdkMessage` params carrying the `system/init` Claude Code
/// 2.1.284 sent through claude-agent-acp 0.84.0 for three deliberately
/// broken directory plugins — captured live, trimmed to the fields read
/// here, paths shortened.
fn claude_init_with_plugin_errors() -> serde_json::Value {
    serde_json::json!({
        "sessionId": "s1",
        "message": {
            "type": "system",
            "subtype": "init",
            "model": "claude-fable-5-1[1m]",
            "plugins": [{
                "name": "context7",
                "path": "/p/cache/claude-plugins-official/context7/unknown",
                "source": "context7@claude-plugins-official"
            }],
            "plugin_errors": [
                {
                    "plugin": "inline[0]",
                    "type": "path-not-found",
                    "message": "Path not found: /tmp/p/does-not-exist (commands)",
                    "path": "/tmp/p/does-not-exist"
                },
                {
                    "plugin": "inline[1]",
                    "type": "generic-error",
                    "message": "Failed to load plugin: Plugin broken-plugin has a corrupt manifest file at /tmp/p/broken-plugin/.claude-plugin/plugin.json. JSON parse error: JSON Parse error: Unexpected token '}'",
                    "path": "/tmp/p/broken-plugin"
                },
                {
                    "plugin": "inline[2]",
                    "type": "generic-error",
                    "message": "Failed to load plugin: hooks: must be an object mapping event names to matcher arrays",
                    "path": "/tmp/p/hookfail-plugin"
                }
            ]
        }
    })
}

#[test]
fn plugin_failures_are_read_off_a_claude_init_frame() {
    let failures =
        claude_plugin_load_failures(&claude_init_with_plugin_errors()).expect("three failures");
    assert_eq!(failures.len(), 3);
    assert_eq!(
        failures[0],
        PluginLoadFailure {
            plugin: "inline[0]".to_string(),
            kind: "path-not-found".to_string(),
            message: "Path not found: /tmp/p/does-not-exist (commands)".to_string(),
            path: Some("/tmp/p/does-not-exist".to_string()),
        }
    );
    assert_eq!(failures[2].kind, "generic-error");
}

#[test]
fn frames_that_list_no_plugin_failures_report_none() {
    let init = |extra: serde_json::Value| {
        let mut message = serde_json::json!({"type": "system", "subtype": "init"});
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::json!({"sessionId": "s1", "message": message})
    };
    // The CLI omits the key when nothing failed, and so does every CLI
    // before 2.1.283.
    assert_eq!(
        claude_plugin_load_failures(&init(serde_json::json!({}))),
        None
    );
    assert_eq!(
        claude_plugin_load_failures(&init(serde_json::json!({"plugin_errors": []}))),
        None
    );
    // The same field on any other frame is not an init's list.
    let mut retry = claude_init_with_plugin_errors();
    retry["message"]["subtype"] = serde_json::json!("api_retry");
    assert_eq!(claude_plugin_load_failures(&retry), None);
    let mut assistant = claude_init_with_plugin_errors();
    assistant["message"]["type"] = serde_json::json!("assistant");
    assert_eq!(claude_plugin_load_failures(&assistant), None);
    assert_eq!(
        claude_plugin_load_failures(&serde_json::json!({"sessionId": "s1"})),
        None
    );
}

#[test]
fn an_unnamed_plugin_failure_is_dropped_and_an_untyped_one_reads_generic() {
    let params = serde_json::json!({
        "sessionId": "s1",
        "message": {
            "type": "system",
            "subtype": "init",
            "plugin_errors": [
                {"type": "generic-error", "message": "no name"},
                {"plugin": "  ", "message": "blank name"},
                {"plugin": "guard@acme", "message": "hooks: bad"}
            ]
        }
    });
    assert_eq!(
        claude_plugin_load_failures(&params),
        Some(vec![PluginLoadFailure {
            plugin: "guard@acme".to_string(),
            kind: "generic-error".to_string(),
            message: "hooks: bad".to_string(),
            path: None,
        }])
    );
}

#[test]
fn plugin_failures_are_forwarded_once_per_distinct_list() {
    let mut cb = CodeBuddyLiveState::default();
    let broken = claude_init_with_plugin_errors();
    assert_eq!(
        take_new_claude_plugin_failures(&broken, &mut cb).map(|f| f.len()),
        Some(3)
    );
    // The CLI repeats the list on every turn's init: not news.
    assert_eq!(take_new_claude_plugin_failures(&broken, &mut cb), None);
    // A changed list is.
    let mut fewer = broken.clone();
    fewer["message"]["plugin_errors"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    assert_eq!(
        take_new_claude_plugin_failures(&fewer, &mut cb).map(|f| f.len()),
        Some(1)
    );
    assert_eq!(take_new_claude_plugin_failures(&fewer, &mut cb), None);
    // A frame that is not an init neither reports nor forgets anything.
    let mut assistant = broken.clone();
    assistant["message"]["type"] = serde_json::json!("assistant");
    assert_eq!(take_new_claude_plugin_failures(&assistant, &mut cb), None);
    assert_eq!(take_new_claude_plugin_failures(&fewer, &mut cb), None);
}

#[test]
fn a_plugin_failure_that_went_away_and_came_back_is_reported_again() {
    let mut cb = CodeBuddyLiveState::default();
    let broken = claude_init_with_plugin_errors();
    let mut clean = broken.clone();
    clean["message"]
        .as_object_mut()
        .unwrap()
        .remove("plugin_errors");
    assert!(take_new_claude_plugin_failures(&broken, &mut cb).is_some());
    // A clean init reports nothing…
    assert_eq!(take_new_claude_plugin_failures(&clean, &mut cb), None);
    // …and the same failure coming back is news again.
    assert_eq!(
        take_new_claude_plugin_failures(&broken, &mut cb).map(|f| f.len()),
        Some(3)
    );
    // An empty list is a clean init too.
    let mut empty = broken.clone();
    empty["message"]["plugin_errors"] = serde_json::json!([]);
    assert_eq!(take_new_claude_plugin_failures(&empty, &mut cb), None);
    assert!(take_new_claude_plugin_failures(&broken, &mut cb).is_some());
}

#[test]
fn the_session_load_replay_drops_only_frames_reporting_plugin_failures() {
    let frame = |method: &str, params: serde_json::Value| {
        Dispatch::Notification(UntypedMessage::new(method, params).unwrap())
    };
    let broken = claude_init_with_plugin_errors();
    assert!(claude_plugin_failures_skipped_on_replay(&frame(
        CLAUDE_SDK_EXT_METHOD,
        broken.clone()
    )));
    // A clean init and an API retry still reach the replay's usual path.
    let mut clean = broken.clone();
    clean["message"]
        .as_object_mut()
        .unwrap()
        .remove("plugin_errors");
    assert!(!claude_plugin_failures_skipped_on_replay(&frame(
        CLAUDE_SDK_EXT_METHOD,
        clean
    )));
    assert!(!claude_plugin_failures_skipped_on_replay(&frame(
        CLAUDE_SDK_EXT_METHOD,
        serde_json::json!({
            "sessionId": "s1",
            "message": {"type": "system", "subtype": "api_retry", "attempt": 1}
        })
    )));
    // The same payload under any other method is not claude's raw stream.
    assert!(!claude_plugin_failures_skipped_on_replay(&frame(
        "_vendor/other",
        broken
    )));
}

#[test]
fn plugin_load_failures_reach_the_frontend_in_its_shape() {
    let event = AcpEvent::PluginLoadFailures {
        failures: vec![
            PluginLoadFailure {
                plugin: "inline[0]".to_string(),
                kind: "path-not-found".to_string(),
                message: "Path not found: /x".to_string(),
                path: Some("/x".to_string()),
            },
            PluginLoadFailure {
                plugin: "guard@acme".to_string(),
                kind: "hook-load-failed".to_string(),
                message: "hooks: bad".to_string(),
                path: None,
            },
        ],
    };
    // `lib/types.ts` reads `type: "plugin_load_failures"` and each entry's
    // `plugin` / `kind` / `message` / optional `path`.
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        serde_json::json!({
            "type": "plugin_load_failures",
            "failures": [
                {
                    "plugin": "inline[0]",
                    "kind": "path-not-found",
                    "message": "Path not found: /x",
                    "path": "/x"
                },
                {"plugin": "guard@acme", "kind": "hook-load-failed", "message": "hooks: bad"}
            ]
        })
    );
}

#[tokio::test]
async fn a_claude_init_frame_raises_its_plugin_failures_once() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-plugins".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    )));
    let mut cb = CodeBuddyLiveState::default();
    let init = || {
        Dispatch::Notification(
            UntypedMessage::new(CLAUDE_SDK_EXT_METHOD, claude_init_with_plugin_errors()).unwrap(),
        )
    };
    for _ in 0..3 {
        maybe_emit_ext_notification(
            &state,
            &EventEmitter::Noop,
            AgentType::ClaudeCode,
            init(),
            &mut cb,
        )
        .await;
    }
    // Another agent's raw frames are never read for this: only claude asks
    // for the raw SDK stream.
    maybe_emit_ext_notification(
        &state,
        &EventEmitter::Noop,
        AgentType::Codex,
        init(),
        &mut CodeBuddyLiveState::default(),
    )
    .await;
    let raised: Vec<usize> = state
        .read()
        .await
        .recent_events_after(0)
        .expect("buffered")
        .iter()
        .filter_map(|envelope| match &envelope.payload {
            AcpEvent::PluginLoadFailures { failures } => Some(failures.len()),
            _ => None,
        })
        .collect();
    assert_eq!(raised, vec![3], "one event for three identical inits");
}

/// The five `_x.ai/session/setup` frames grok 1.0.40 emits BEFORE the
/// session id exists, captured verbatim off `grok agent stdio` during
/// `session/new`. Each carries `"sessionId": null`, which is what killed the
/// connection in #794 — the runtime treats the present-but-null field as
/// session-bound, and `sacp` 11 then failed to parse it into a `SessionId`.
#[test]
fn null_session_id_is_recognized_on_grok_setup_frames() {
    for phase in [
        "auth",
        "resolve_workspace",
        "folder_trust",
        "plugin_registry",
        "mcp_merge",
    ] {
        let raw = UntypedMessage::new(
            "_x.ai/session/setup",
            serde_json::json!({
                "method": "session/new",
                "phase": phase,
                "sessionId": serde_json::Value::Null
            }),
        )
        .unwrap();
        assert!(
            has_null_session_id(&raw),
            "phase {phase} should be recognized as a null sessionId"
        );
    }
}

/// The two shapes that must keep flowing: the LATER `_x.ai/session/setup`
/// phases, which carry a real session id and belong to the session channel,
/// and connection-level pushes that have no `sessionId` field at all
/// (`_auth/status_update`, grok's `_x.ai/settings/update`).
#[test]
fn null_session_id_leaves_real_and_absent_session_ids_alone() {
    let with_id = UntypedMessage::new(
        "_x.ai/session/setup",
        serde_json::json!({
            "method": "session/new",
            "phase": "persistence_init",
            "sessionId": "01a0c4fa-c78f-7bf1-9cc3-5c9f4e3e919a"
        }),
    )
    .unwrap();
    assert!(!has_null_session_id(&with_id));

    let no_id = UntypedMessage::new(
        "_auth/status_update",
        serde_json::json!({"authStatus": {"kind": "authenticated"}}),
    )
    .unwrap();
    assert!(!has_null_session_id(&no_id));

    let settings = UntypedMessage::new(
        "_x.ai/settings/update",
        serde_json::json!({"sharing_enabled": false, "session_picker_grouped": null}),
    )
    .unwrap();
    assert!(!has_null_session_id(&settings));
}

/// A REQUEST with a null `sessionId` is parked by the runtime like any
/// session-bound message, and nothing would ever answer it: the agent would
/// block for the life of the connection. Over the real runtime, the guard
/// answers it `invalid_params` instead.
#[tokio::test]
async fn a_request_with_a_null_session_id_is_answered_not_parked() {
    let (client_end, agent_end) = agent_client_protocol::Channel::duplex();
    let client = tokio::spawn(
        Client
            .builder()
            .with_handler(ClaimNullSessionIds)
            .connect_with(client_end, async |_cx: ConnectionTo<Agent>| {
                std::future::pending::<Result<(), agent_client_protocol::Error>>().await
            }),
    );

    let answer = Agent
        .builder()
        .connect_with(agent_end, async |cx: ConnectionTo<Client>| {
            let request = UntypedMessage::new(
                "_test/before_the_session",
                serde_json::json!({"sessionId": null}),
            )?;
            Ok(tokio::time::timeout(
                std::time::Duration::from_secs(5),
                cx.send_request_to(Client, request).block_task(),
            )
            .await)
        })
        .await
        .expect("agent connection");

    let error = answer
        .expect("the request is answered, not parked")
        .expect_err("a null sessionId is rejected");
    assert!(
        matches!(
            error.code,
            agent_client_protocol::schema::v1::ErrorCode::InvalidParams
        ),
        "{error}"
    );
    client.abort();
}

/// The exact `_x.ai/session_notification` envelope captured from grok 0.2.111
/// running `/compact` — `auto_compact_completed` under `params.update`, with
/// the token delta and an `_meta.eventId`.
#[test]
fn map_grok_ext_notification_maps_auto_compact_completed() {
    let raw = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "019f9475-c67f-7390-9ee5-a09d29986a6c",
            "update": {
                "sessionUpdate": "auto_compact_completed",
                "tokens_before": 45389,
                "tokens_after": 16486,
                "summary_preview": null
            },
            "_meta": {
                "eventId": "019f9475-c67f-7390-9ee5-a09d29986a6c-4",
                "agentTimestampMs": 1784902203750u64
            }
        }),
    )
    .unwrap();

    let event = map_grok_ext_notification(&raw, AgentType::Grok)
        .expect("auto_compact_completed should map to a compaction card");
    match event {
        AcpEvent::ToolCall {
            tool_call_id,
            status,
            meta,
            ..
        } => {
            assert_eq!(tool_call_id, "019f9475-c67f-7390-9ee5-a09d29986a6c-4");
            assert_eq!(status, "completed");
            let meta = meta.expect("compaction card needs meta");
            assert_eq!(
                meta.get("contextCompaction").and_then(|v| v.as_bool()),
                Some(true)
            );
            assert_eq!(
                meta.get("tokensBefore").and_then(|v| v.as_u64()),
                Some(45389)
            );
            assert_eq!(
                meta.get("tokensAfter").and_then(|v| v.as_u64()),
                Some(16486)
            );
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

/// The same variant may arrive on the sibling `_x.ai/session/update` method.
#[test]
fn map_grok_ext_notification_handles_session_update_method() {
    let raw = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "auto_compact_completed", "tokens_before": 100, "tokens_after": 100 }
            }),
        )
        .unwrap();
    // No `_meta.eventId` → a generated id, but it must still map.
    assert!(matches!(
        map_grok_ext_notification(&raw, AgentType::Grok),
        Some(AcpEvent::ToolCall { .. })
    ));
}

#[test]
fn map_grok_ext_notification_image_dropped_surfaces_error() {
    let raw = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": {
                    "sessionUpdate": "image_dropped",
                    "notes": [
                        "Image 1 was dropped before send: too small (1×1); images must be at least 8×8 pixels."
                    ]
                }
            }),
        )
        .unwrap();
    match map_grok_ext_notification(&raw, AgentType::Grok) {
        Some(AcpEvent::Error {
            message,
            terminal,
            code,
            ..
        }) => {
            // The stable code is what lets the frontend treat this as the
            // answer to a click (the user just attached this image) rather
            // than as the session's standing error.
            assert_eq!(code.as_deref(), Some(IMAGE_DROPPED_ERROR_CODE));
            assert!(
                message.contains("too small"),
                "error should carry grok's drop reason; got: {message}"
            );
            // grok's note already opens with "Image 1 was dropped before
            // send"; a prefix here would stutter it back at the user.
            assert!(
                message.starts_with("Image 1 was dropped"),
                "grok's own sentence must be shown verbatim; got: {message}"
            );
            assert!(!terminal, "a dropped image must not kill the connection");
        }
        other => panic!("expected non-terminal Error, got {other:?}"),
    }
}

/// Without `notes` there is no sentence to show, so the fallback has to
/// supply the subject itself.
#[test]
fn map_grok_ext_notification_image_dropped_without_notes_still_names_the_subject() {
    let raw = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "image_dropped", "reason": "decode failed" }
        }),
    )
    .unwrap();
    match map_grok_ext_notification(&raw, AgentType::Grok) {
        Some(AcpEvent::Error { message, .. }) => {
            assert_eq!(message, "Image dropped: decode failed");
        }
        other => panic!("expected Error, got {other:?}"),
    }

    let bare = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "image_dropped", "notes": [] }
        }),
    )
    .unwrap();
    match map_grok_ext_notification(&bare, AgentType::Grok) {
        Some(AcpEvent::Error { message, .. }) => {
            assert!(
                message.to_lowercase().contains("image"),
                "a note-less drop must still say what happened; got: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn final_agent_boundary_appends_routes_for_every_agent_holding_a_snapshot() {
    let visible = "ask [@Antigravity](codeg://agent/antigravity) to build";
    let blocks = vec![PromptInputBlock::Text {
        text: visible.into(),
    }];
    // Not Codex-specific: any parent whose companion carried the delegation
    // group routes, including custom agents.
    for parent in [
        AgentType::Codex,
        AgentType::ClaudeCode,
        AgentType::Gemini,
        AgentType::custom("delegating-custom").expect("valid custom id"),
    ] {
        let prompt = prepare_agent_bound_prompt(parent, blocks.clone(), true, None, &[]);
        assert_eq!(prompt.len(), 2, "{parent} must receive the routing block");
        assert!(matches!(
            &prompt[0],
            ContentBlock::Text(text) if text.text == visible
        ));
        assert!(matches!(
            &prompt[1],
            ContentBlock::Text(text)
                if text.text.contains("Codeg composer routing metadata (authoritative)")
                    && text.text.contains(r#""agentType":"antigravity""#)
        ));
    }

    // An agent that never received the companion (OpenClaw's
    // supports_mcp=false, pi's wire exclusion) reaches here with delegation
    // off and keeps a pristine prompt.
    let unrouted = prepare_agent_bound_prompt(AgentType::OpenClaw, blocks, false, None, &[]);
    assert_eq!(unrouted.len(), 1);
    assert!(matches!(
        &unrouted[0],
        ContentBlock::Text(text) if text.text == visible
    ));
}

#[test]
fn final_agent_boundary_appends_the_team_frame_after_the_routing_frame() {
    let visible = "ask [@Codex](codeg://agent/codex) then plan";
    let blocks = vec![PromptInputBlock::Text {
        text: visible.into(),
    }];
    let members = vec![crate::acp::delegation::team::TeamMember {
        name: "前端工程师".into(),
        agent_type: "codex".into(),
        duty: Some("专注于界面与交互实现".into()),
        role_id: "r-frontend".into(),

        brain_model: String::new(),
        brain_provider: String::new(),
        mcp: Vec::new(),
    }];
    let prompt = prepare_agent_bound_prompt(AgentType::ClaudeCode, blocks, true, None, &members);
    assert_eq!(prompt.len(), 3, "user text + routing frame + team frame");
    assert!(matches!(
        &prompt[1],
        ContentBlock::Text(text) if text.text.contains("Codeg composer routing metadata")
    ));
    assert!(matches!(
        &prompt[2],
        ContentBlock::Text(text)
            if text.text.contains("Codeg session team (authoritative): ")
                && text.text.contains(r#""name":"前端工程师""#)
                && text.text.contains(r#""agentType":"codex""#)
    ));

    // Closed gate: a team frame would name a channel this connection does
    // not have, so members append nothing.
    let blocks = vec![PromptInputBlock::Text {
        text: "hello".into(),
    }];
    let prompt = prepare_agent_bound_prompt(AgentType::OpenClaw, blocks, false, None, &members);
    assert_eq!(prompt.len(), 1);
}

#[test]
fn final_agent_boundary_appends_the_role_frame_first_and_undelegated() {
    use crate::models::agent_role::AgentRole;
    let role = AgentRole {
        id: "r-lead".into(),
        name: "管理员".into(),
        soul: "先规划、再委派、最后汇总。".into(),
        rules: "不要独自写完整个任务。".into(),
        agent_type: "claude_code".into(),
        model: String::new(),
        model_provider: String::new(),
        skills: vec![],
        mcp: vec![],
        permission_mode: String::new(),
        auto_approve: false,
        created_at: None,
        updated_at: None,
    };
    let members = vec![crate::acp::delegation::team::TeamMember {
        name: "前端工程师".into(),
        agent_type: "codex".into(),
        duty: None,
        role_id: "r-frontend".into(),
        mcp: Vec::new(),
        brain_model: String::new(),
        brain_provider: String::new(),
    }];

    let blocks = vec![PromptInputBlock::Text {
        text: "ask [@Codex](codeg://agent/codex) then plan".into(),
    }];
    let prompt =
        prepare_agent_bound_prompt(AgentType::ClaudeCode, blocks, true, Some(&role), &members);
    assert_eq!(prompt.len(), 4, "user + role + routing + team");
    assert!(matches!(
        &prompt[1],
        ContentBlock::Text(text) if text.text.contains("codeg_internal_role")
    ));
    assert!(matches!(
        &prompt[2],
        ContentBlock::Text(text) if text.text.contains("Codeg composer routing metadata")
    ));
    assert!(matches!(
        &prompt[3],
        ContentBlock::Text(text) if text.text.contains("Codeg session team (authoritative): ")
    ));

    // The persona is NOT delegation-gated: the role describes how this
    // session works even when codeg-mcp never made it in.
    let blocks = vec![PromptInputBlock::Text {
        text: "hello".into(),
    }];
    let prompt =
        prepare_agent_bound_prompt(AgentType::OpenClaw, blocks, false, Some(&role), &members);
    assert_eq!(prompt.len(), 2, "user + role only");
    assert!(matches!(
        &prompt[1],
        ContentBlock::Text(text) if text.text.contains("codeg_internal_role")
    ));
}

#[test]
fn normalize_grok_image_blocks_lifts_decodable_image_blobs_only() {
    let blocks = vec![
        PromptInputBlock::Text {
            text: "see this".into(),
        },
        PromptInputBlock::Resource {
            uri: "clipboard://shot.png-abc".into(),
            mime_type: Some("image/png".into()),
            text: None,
            blob: Some("aGk=".into()),
        },
        PromptInputBlock::Resource {
            uri: "clipboard://notes.md".into(),
            mime_type: Some("text/markdown".into()),
            text: Some("hi".into()),
            blob: None,
        },
        PromptInputBlock::Image {
            data: "already".into(),
            mime_type: "image/jpeg".into(),
            uri: None,
        },
    ];
    let out = normalize_grok_image_blocks(blocks);
    assert!(matches!(&out[0], PromptInputBlock::Text { text } if text == "see this"));
    assert!(
        matches!(
            &out[1],
            PromptInputBlock::Image { data, mime_type, uri: Some(u) }
                if data == "aGk="
                    && mime_type == "image/png"
                    && u == "clipboard://shot.png-abc"
        ),
        "{:?}",
        out[1]
    );
    assert!(matches!(
        &out[2],
        PromptInputBlock::Resource {
            mime_type: Some(m),
            ..
        } if m == "text/markdown"
    ));
    assert!(matches!(
        &out[3],
        PromptInputBlock::Image { data, .. } if data == "already"
    ));
}

/// grok's validator rejects `image/svg+xml` outright ("unsupported or
/// unrecognised image format") and the model then sees nothing at all. As a
/// resource blob the same file lands in the session's assets, where the
/// model can read the source — so an undecodable mime must NOT ride the
/// native carriage, whichever producer built the block.
#[test]
fn normalize_grok_image_blocks_demotes_mimes_grok_cannot_decode() {
    let blocks = vec![
        PromptInputBlock::Image {
            data: "PHN2Zy8+".into(),
            mime_type: "image/svg+xml".into(),
            uri: Some("file:///tmp/diagram.svg".into()),
        },
        // Path-less (pasted): the demotion has to invent a stable uri.
        // Upper-cased on purpose — the "is it an image at all" guard and the
        // allow-list must read the same string the same way, or an
        // undecodable format sails through as native.
        PromptInputBlock::Image {
            data: "PHN2Zy8+".into(),
            mime_type: "IMAGE/SVG+XML".into(),
            uri: None,
        },
        // Already on the resource carriage and undecodable — left alone,
        // never promoted.
        PromptInputBlock::Resource {
            uri: "clipboard://icon.svg".into(),
            mime_type: Some("image/svg+xml".into()),
            text: None,
            blob: Some("PHN2Zy8+".into()),
        },
    ];
    let out = normalize_grok_image_blocks(blocks);
    assert!(
        matches!(
            &out[0],
            PromptInputBlock::Resource { uri, mime_type: Some(m), text: None, blob: Some(b) }
                if uri == "file:///tmp/diagram.svg"
                    && m == "image/svg+xml"
                    && b == "PHN2Zy8+"
        ),
        "{:?}",
        out[0]
    );
    assert!(
        matches!(
            &out[1],
            PromptInputBlock::Resource { uri, blob: Some(b), .. }
                if uri == "clipboard://grok-image-1" && b == "PHN2Zy8+"
        ),
        "{:?}",
        out[1]
    );
    assert!(
        matches!(
            &out[2],
            PromptInputBlock::Resource { mime_type: Some(m), .. } if m == "image/svg+xml"
        ),
        "{:?}",
        out[2]
    );
}

/// The allow-list is the boundary between the two carriages, so pin it:
/// grok's own raster set in, everything else out.
#[test]
fn grok_decodes_image_mime_covers_groks_raster_set_only() {
    for mime in [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/tiff",
        "IMAGE/PNG",
    ] {
        assert!(grok_decodes_image_mime(mime), "{mime} should be decodable");
    }
    for mime in [
        "image/svg+xml",
        "image/avif",
        "image/heic",
        "image/x-icon",
        "text/markdown",
        "",
    ] {
        assert!(
            !grok_decodes_image_mime(mime),
            "{mime} must keep the resource carriage"
        );
    }
}

/// A failed auto-compaction renders the shared compaction card in its
/// failed state — where every other compaction outcome shows — not a
/// session error under the composer.
#[test]
fn map_grok_ext_notification_auto_compact_failed_renders_failed_compaction_card() {
    let raw = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "auto_compact_failed", "reason": " API error (status 503) " },
                "_meta": { "eventId": "ev-compact-failed" }
            }),
        )
        .unwrap();
    match map_grok_ext_notification(&raw, AgentType::Grok) {
        Some(AcpEvent::ToolCall {
            tool_call_id,
            status,
            meta,
            ..
        }) => {
            assert_eq!(tool_call_id, "ev-compact-failed");
            assert_eq!(status, "failed");
            assert_eq!(
                meta,
                Some(serde_json::json!({
                    "contextCompaction": { "version": 1, "error": "API error (status 503)" }
                }))
            );
        }
        other => panic!("expected a failed compaction card, got {other:?}"),
    }

    // No reason: still a failed card, just without hover text.
    let bare = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "auto_compact_failed", "reason": "  " }
        }),
    )
    .unwrap();
    match map_grok_ext_notification(&bare, AgentType::Grok) {
        Some(AcpEvent::ToolCall { status, meta, .. }) => {
            assert_eq!(status, "failed");
            assert_eq!(
                meta,
                Some(serde_json::json!({ "contextCompaction": { "version": 1 } }))
            );
        }
        other => panic!("expected a failed compaction card, got {other:?}"),
    }
}

#[test]
fn map_grok_ext_notification_is_grok_gated_and_scoped() {
    let compact = serde_json::json!({
        "sessionId": "s",
        "update": { "sessionUpdate": "auto_compact_completed", "tokens_before": 1, "tokens_after": 1 }
    });
    // Non-grok agent: ignored even for the same payload.
    let raw = UntypedMessage::new("_x.ai/session_notification", compact.clone()).unwrap();
    assert!(map_grok_ext_notification(&raw, AgentType::Codex).is_none());

    // Turn-level state is intentionally left to the prompt-response path.
    let turn = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "turn_completed", "stop_reason": "error", "agent_result": "boom" }
            }),
        )
        .unwrap();
    assert!(map_grok_ext_notification(&turn, AgentType::Grok).is_none());

    // Unrelated method: ignored.
    let other = UntypedMessage::new("session/update", compact).unwrap();
    assert!(map_grok_ext_notification(&other, AgentType::Grok).is_none());
}

#[test]
fn track_grok_spawn_call_pairs_dedupes_and_drops_failed() {
    let mut cb = CodeBuddyLiveState::default();
    // Announce (pending) — re-announce on a later frame must not re-queue.
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-1", &None);
    track_grok_spawn_call(&mut cb, true, None, "call-1", &None);
    assert_eq!(cb.grok_pending_spawn_ids.len(), 1);
    // A status-only update WITHOUT the meta marker still tracks a seen id.
    track_grok_spawn_call(&mut cb, false, Some("completed"), "call-1", &None);
    assert!(cb.grok_settled_spawn_ids.contains("call-1"));
    // A failed spawn leaves the pairing queue so later pairs can't shift.
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-2", &None);
    track_grok_spawn_call(&mut cb, false, Some("failed"), "call-2", &None);
    assert!(!cb
        .grok_pending_spawn_ids
        .iter()
        .any(|p| p.call_id == "call-2"));
    // A never-seen id (another tool) is entirely ignored.
    track_grok_spawn_call(&mut cb, false, Some("completed"), "call-x", &None);
    assert!(!cb.grok_settled_spawn_ids.contains("call-x"));
}

/// A DELAYED prior-turn `subagent_spawned` (its launch turn ended, its
/// pending entry was cleared) must NOT steal the pairing slot a NEW turn's
/// spawn queued: the `(description, subagent_type)` captured from the
/// launch input has to match the notification's own fields.
#[test]
fn delayed_subagent_spawned_does_not_steal_a_new_turns_slot() {
    let mut cb = CodeBuddyLiveState::default();
    let input_b = Some(
        serde_json::json!({
            "description": "B task", "prompt": "PB", "subagent_type": "plan"
        })
        .to_string(),
    );
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-b", &input_b);

    // The prior turn's child announces late, with ITS OWN description.
    let stale = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-a",
        "description": "A task",
        "subagent_type": "explore"
    }));
    assert!(
        map_grok_subagent_notification(&stale, AgentType::Grok, true, &mut cb).is_empty(),
        "a mismatched spawned must pair nothing"
    );
    assert_eq!(cb.grok_pending_spawn_ids.len(), 1, "B keeps its slot");

    // A delayed notification that OMITS the description (same type or
    // none at all) must not wildcard past B's captured description either.
    let stale_bare = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-a2",
        "subagent_type": "plan"
    }));
    assert!(
        map_grok_subagent_notification(&stale_bare, AgentType::Grok, true, &mut cb).is_empty(),
        "an event missing a captured field must not match"
    );
    assert_eq!(cb.grok_pending_spawn_ids.len(), 1, "B still keeps its slot");

    // B's own notification pairs it.
    let real = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-b",
        "description": "B task",
        "subagent_type": "plan"
    }));
    map_grok_subagent_notification(&real, AgentType::Grok, true, &mut cb);
    assert_eq!(
        cb.grok_subagent_to_call.get("sub-b").map(String::as_str),
        Some("call-b")
    );
    assert!(cb.grok_pending_spawn_ids.is_empty());
}

fn grok_subagent_notif(update: serde_json::Value) -> UntypedMessage {
    UntypedMessage::new(
        "_x.ai/session/update",
        serde_json::json!({ "sessionId": "sess-1", "update": update }),
    )
    .unwrap()
}

/// Full background-subagent lifecycle over the stateful mapper: spawned
/// pairs FIFO, progress lands as a meta-only ToolCallUpdate on the paired
/// call, and finished settles over the BackgroundActivity channel with the
/// launch call's id (so the frontend flips its marker card in-memory).
#[test]
fn map_grok_subagent_notification_background_lifecycle() {
    let mut cb = CodeBuddyLiveState::default();
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-1", &None);
    // Background: the launch call settles before the pairing arrives.
    track_grok_spawn_call(&mut cb, true, Some("completed"), "call-1", &None);

    let spawned = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-1",
        "child_session_id": "sub-1",
        "subagent_type": "explore"
    }));
    // Pairing consumed the pending slot; the already-settled call means a
    // background child is now outstanding (idle-sweep exemption). Passed
    // with turn_active=false: the BackgroundActivity channel is
    // out-of-turn-safe by design and must not be gated.
    match map_grok_subagent_notification(&spawned, AgentType::Grok, false, &mut cb).as_slice() {
        // Out of turn there is no session stamp (it would resurrect a ghost
        // live_message) — only the background report.
        [AcpEvent::BackgroundActivity {
            outstanding,
            settled,
            ..
        }] => {
            assert_eq!(*outstanding, 1);
            assert!(settled.is_empty());
        }
        other => panic!("expected one BackgroundActivity, got {other:?}"),
    }
    assert!(cb.grok_pending_spawn_ids.is_empty());
    // The child's session id is remembered for the card, whether or not it
    // was emitted this time.
    assert_eq!(
        cb.grok_call_child_session.get("call-1").map(String::as_str),
        Some("sub-1")
    );

    let progress = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_progress",
        "subagent_id": "sub-1",
        "duration_ms": 4200,
        "turn_count": 1,
        "tool_call_count": 7,
        "context_usage_pct": 12.5,
        "tools_used": ["read_file"]
    }));
    // Out-of-turn tick: dropped whole. A post-TurnComplete ToolCallUpdate
    // would resurrect a ghost `live_message` in the SessionState snapshot
    // (the apply arm lazily creates it), while the frontend diverts it out
    // of the transcript anyway.
    assert!(
        map_grok_subagent_notification(&progress, AgentType::Grok, false, &mut cb).is_empty(),
        "progress must not emit outside an active turn"
    );
    // Cross-turn tick: the launch turn ended and a LATER turn is Prompting
    // (turn_active=true) — the per-turn eligibility clear must still drop
    // it, or the old tool id would be appended into the NEW turn's live
    // message as a ghost card.
    let eligibility_backup = cb.grok_progress_eligible.clone();
    cb.grok_progress_eligible.clear();
    assert!(
        map_grok_subagent_notification(&progress, AgentType::Grok, true, &mut cb).is_empty(),
        "a prior turn's background child must not tick into a later turn"
    );
    cb.grok_progress_eligible = eligibility_backup;
    match map_grok_subagent_notification(&progress, AgentType::Grok, true, &mut cb).as_slice() {
        [AcpEvent::ToolCallUpdate {
            tool_call_id,
            status,
            meta,
            ..
        }] => {
            assert_eq!(tool_call_id, "call-1");
            assert_eq!(*status, None, "progress must not touch the status");
            let progress = meta
                .as_ref()
                .and_then(|m| m.get("grokSubagentProgress"))
                .expect("progress meta");
            assert_eq!(
                progress.get("toolCallCount").and_then(|v| v.as_u64()),
                Some(7)
            );
            assert_eq!(
                progress.get("durationMs").and_then(|v| v.as_u64()),
                Some(4200)
            );
            assert_eq!(
                progress.get("contextUsagePct").and_then(|v| v.as_f64()),
                Some(12.5)
            );
            // Meta is REPLACED on apply, so every tick must re-send the
            // session ids or the "open child session" affordance vanishes
            // the moment the first progress tick lands.
            assert_eq!(
                meta.as_ref()
                    .and_then(|m| m.get("grokSubagentSession"))
                    .and_then(|s| s.get("childSessionId"))
                    .and_then(|v| v.as_str()),
                Some("sub-1")
            );
        }
        other => panic!("expected one ToolCallUpdate, got {other:?}"),
    }

    let finished = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_finished",
        "subagent_id": "sub-1",
        "child_session_id": "sub-1",
        "status": "completed",
        "tool_calls": 7,
        "duration_ms": 63775,
        "output": "## Findings"
    }));
    // The settle is likewise out-of-turn-safe (a background child usually
    // finishes after its launch turn ended).
    match map_grok_subagent_notification(&finished, AgentType::Grok, false, &mut cb).as_slice() {
        [AcpEvent::BackgroundActivity {
            session_id,
            outstanding,
            settled,
            ..
        }] => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(*outstanding, 0, "the settled child leaves the count");
            assert_eq!(settled.len(), 1);
            let s = &settled[0];
            assert_eq!(s.task_id, "sub-1");
            assert_eq!(s.status, "completed");
            assert_eq!(s.tool_use_id.as_deref(), Some("call-1"));
            assert_eq!(s.result.as_deref(), Some("## Findings"));
        }
        other => panic!("expected BackgroundActivity, got {other:?}"),
    }
    // Lifecycle over: a duplicate finished no longer routes anywhere.
    assert!(map_grok_subagent_notification(&finished, AgentType::Grok, false, &mut cb).is_empty());
}

/// A BLOCKING spawn (call not yet settled when the child finishes) must NOT
/// emit a settle — its own completion frame carries the output; a duplicate
/// marker would double-render it. Non-grok agents never route at all.
#[test]
fn map_grok_subagent_notification_skips_blocking_and_other_agents() {
    let mut cb = CodeBuddyLiveState::default();
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-1", &None);
    let spawned = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-1"
    }));
    // Blocking: the call is still in_progress at pairing time, so there is
    // no background report — only the session stamp, which is exactly what
    // lets the user open a blocking child's transcript while it runs (its
    // launch call stays output-less until the very end).
    match map_grok_subagent_notification(&spawned, AgentType::Grok, true, &mut cb).as_slice() {
        [AcpEvent::ToolCallUpdate {
            tool_call_id, meta, ..
        }] => {
            assert_eq!(tool_call_id, "call-1");
            // No `child_session_id` on this wire shape → fall back to the
            // subagent id, which is the same value in every capture.
            assert_eq!(
                meta.as_ref()
                    .and_then(|m| m.get("grokSubagentSession"))
                    .and_then(|s| s.get("childSessionId"))
                    .and_then(|v| v.as_str()),
                Some("sub-1")
            );
        }
        other => panic!("expected one ToolCallUpdate, got {other:?}"),
    }
    let finished = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_finished",
        "subagent_id": "sub-1",
        "status": "completed",
        "output": "done"
    }));
    assert!(
        map_grok_subagent_notification(&finished, AgentType::Grok, true, &mut cb).is_empty(),
        "blocking spawn settles via its own completion frame"
    );

    // Gating: same payload from a non-grok agent is untouched (and state
    // untouched — the pending queue keeps its entry).
    let mut cb2 = CodeBuddyLiveState::default();
    track_grok_spawn_call(&mut cb2, true, Some("pending"), "call-9", &None);
    let spawned2 = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-9"
    }));
    assert!(map_grok_subagent_notification(&spawned2, AgentType::Codex, true, &mut cb2).is_empty());
    assert_eq!(cb2.grok_pending_spawn_ids.len(), 1);
}

/// A traversal-shaped `child_session_id` must not reach the card: the
/// frontend hands that value to `get_conversation`, which resolves a session
/// directory by it, so the live path applies the same `is_safe_subagent_id`
/// gate the history parser (`grok::subagent_stats`) does. The stamp still
/// carries the subagent id — the run is identified, there is just no session
/// offered to open.
#[test]
fn map_grok_subagent_notification_rejects_unsafe_child_session_id() {
    let mut cb = CodeBuddyLiveState::default();
    track_grok_spawn_call(&mut cb, true, Some("pending"), "call-1", &None);
    let spawned = grok_subagent_notif(serde_json::json!({
        "sessionUpdate": "subagent_spawned",
        "subagent_id": "sub-1",
        "child_session_id": "../../../etc/passwd"
    }));
    match map_grok_subagent_notification(&spawned, AgentType::Grok, true, &mut cb).as_slice() {
        [AcpEvent::ToolCallUpdate { meta, .. }] => {
            let session = meta
                .as_ref()
                .and_then(|m| m.get("grokSubagentSession"))
                .expect("session meta");
            assert_eq!(
                session.get("subagentId").and_then(|v| v.as_str()),
                Some("sub-1")
            );
            assert!(
                session.get("childSessionId").is_none(),
                "a traversal-shaped child id must not reach the viewer"
            );
        }
        other => panic!("expected one ToolCallUpdate, got {other:?}"),
    }
    // Nothing memorized either — a later progress tick has nothing to
    // re-send, so the rejection holds for the whole run.
    assert!(cb.grok_call_child_session.is_empty());
}

/// The turn-loop consults this to keep a compaction-only `/compact` turn
/// from being reclassified as `"empty"` (which re-surfaces a spurious error).
/// It must count exactly the compaction outcomes that emit a card/error.
#[test]
fn grok_ext_notification_is_turn_output_marks_compaction_outcomes() {
    let notif = |variant: &str| {
        Dispatch::Notification(
            UntypedMessage::new(
                "_x.ai/session_notification",
                serde_json::json!({
                    "sessionId": "s",
                    "update": {
                        "sessionUpdate": variant,
                        "tokens_before": 9, "tokens_after": 8, "reason": "x"
                    }
                }),
            )
            .unwrap(),
        )
    };
    // Both compaction outcomes are visible turn output.
    assert!(grok_ext_notification_is_turn_output(
        &notif("auto_compact_completed"),
        AgentType::Grok
    ));
    assert!(grok_ext_notification_is_turn_output(
        &notif("auto_compact_failed"),
        AgentType::Grok
    ));
    // turn_completed is deliberately left to the prompt-response path — it is
    // NOT counted here (otherwise a genuinely empty turn would be masked).
    assert!(!grok_ext_notification_is_turn_output(
        &notif("turn_completed"),
        AgentType::Grok
    ));
    // Never fires for a non-grok agent.
    assert!(!grok_ext_notification_is_turn_output(
        &notif("auto_compact_completed"),
        AgentType::Codex
    ));
}

/// Helper mirroring what claude-agent-acp puts on the wire: a plain
/// `session/update` notification carrying one AIR async-task frame.
fn async_task_notif(update: serde_json::Value) -> Dispatch {
    Dispatch::Notification(
        UntypedMessage::new(
            "session/update",
            serde_json::json!({ "sessionId": "s", "update": update }),
        )
        .unwrap(),
    )
}

/// Every severity the RFD defines has to survive the raw read, and an
/// unknown one has to survive it TOO — grading happens in the consumer,
/// which degrades an unrecognized level to `info` rather than dropping it.
#[test]
fn a_notice_is_read_at_every_severity() {
    for severity in ["info", "warning", "error", "_vendor_specific"] {
        let notice = session_notice(&async_task_notif(serde_json::json!({
            "sessionUpdate": "notice",
            "severity": severity,
            "title": "Model fallback",
            "description": "Switched to Sonnet.",
        })))
        .expect("notice");
        assert_eq!(notice.severity, severity);
        assert_eq!(notice.title, "Model fallback");
        assert_eq!(notice.description.as_deref(), Some("Switched to Sonnet."));
    }
}

/// Frames captured byte-for-byte off the live adapters over stdio, with the
/// same `clientCapabilities.session` block `build_client_capabilities` sends.
///
/// * claude-agent-acp 0.81.0: a project `UserPromptSubmit` hook exiting 2
///   blocks the prompt before any model call (the turn reported 0 tokens),
///   and the SDK's `informational` frame arrives as this notice.
/// * codex-acp 1.13.0 (codex 0.155.1): a throwaway `CODEX_HOME` whose
///   `model` is not in the catalog, logged in with a dummy API key — the
///   fallback-metadata warning fires at turn start, then the 401 produces
///   the transport-fallback warning (a multi-line title, kept verbatim).
#[test]
fn notices_captured_live_from_both_adapters_are_read_verbatim() {
    let from_wire = |line: &str| {
        let frame: serde_json::Value = serde_json::from_str(line).expect("captured frame");
        Dispatch::Notification(
            UntypedMessage::new(
                frame["method"].as_str().expect("method"),
                frame["params"].clone(),
            )
            .unwrap(),
        )
    };

    let claude = session_notice(&from_wire(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"f11ac967-067f-480a-8acc-a3c689b40bb3","update":{"sessionUpdate":"notice","severity":"warning","title":"UserPromptSubmit operation blocked by hook:","description":"[echo 'Blocked by the codeg probe hook: prompts are not allowed in this folder.' >&2; exit 2]: Blocked by the codeg probe hook: prompts are not allowed in this folder.\n\n\nOriginal prompt: say hi"}}}"#,
        ))
        .expect("claude notice");
    assert_eq!(claude.severity, "warning");
    assert_eq!(claude.title, "UserPromptSubmit operation blocked by hook:");
    assert!(claude
        .description
        .as_deref()
        .is_some_and(|d| d.ends_with("\n\n\nOriginal prompt: say hi")));

    let codex = session_notice(&from_wire(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"01a0cc1b-7aa2-78e3-8e55-c9ab3b7f506e","update":{"sessionUpdate":"notice","severity":"warning","title":"Model metadata for `codeg-probe-unknown-model` not found. Defaulting to fallback metadata; this can degrade performance and cause issues."}}}"#,
        ))
        .expect("codex notice");
    assert_eq!(codex.severity, "warning");
    assert!(codex
        .title
        .starts_with("Model metadata for `codeg-probe-unknown-model`"));
    assert_eq!(codex.description, None);

    let codex_multiline = session_notice(&from_wire(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"01a0cc1b-7aa2-78e3-8e55-c9ab3b7f506e","update":{"sessionUpdate":"notice","severity":"warning","title":"Falling back from WebSockets to HTTPS transport. unexpected status 401 Unauthorized: {\n  \"error\": {\n    \"me, url: wss://api.openai.com/v1/responses, cf-ray: a3f623f1398b2eaf-LAX, request id: req_350561679b9e4cf6895346dd25919bd9, auth error: 401, auth error code: invalid_api_key"}}}"#,
        ))
        .expect("codex transport notice");
    assert!(codex_multiline
        .title
        .starts_with("Falling back from WebSockets"));
    assert!(codex_multiline.title.contains('\n'));
}

/// `title` is required and non-empty per the RFD. Without it there is
/// nothing to show, and a toast with an empty body is worse than silence.
#[test]
fn a_notice_without_a_usable_title_is_dropped() {
    for title in [
        serde_json::json!(""),
        serde_json::json!("   "),
        serde_json::Value::Null,
    ] {
        assert!(
            session_notice(&async_task_notif(serde_json::json!({
                "sessionUpdate": "notice",
                "severity": "warning",
                "title": title,
            })))
            .is_none(),
            "blank title must not produce a notice"
        );
    }
    // A blank description is absence, not an empty second line.
    let notice = session_notice(&async_task_notif(serde_json::json!({
        "sessionUpdate": "notice",
        "severity": "warning",
        "title": "Fast mode turned off",
        "description": "  ",
    })))
    .expect("notice");
    assert_eq!(notice.description, None);
    // Severity is the one field with a sane default: a notice that reached
    // us without one is still worth showing.
    let notice = session_notice(&async_task_notif(serde_json::json!({
        "sessionUpdate": "notice",
        "title": "Context compacted",
    })))
    .expect("notice");
    assert_eq!(notice.severity, "info");
}

/// Claiming a dispatch before the typed pipeline is destructive, so both
/// readers have to be exact about what they claim.
#[test]
fn the_session_readers_claim_only_their_own_frames() {
    assert!(session_notice(&async_task_notif(serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "title": "not a notice",
    })))
    .is_none());
    assert!(
        session_compaction_event(&async_task_notif(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "compactionId": "cmp_1",
        })))
        .is_none()
    );
    // Neither rides anything but `session/update` — an extension method
    // that happens to nest the same shape must not be swallowed.
    let other_method = Dispatch::Notification(
        UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "s",
                "update": {"sessionUpdate": "notice", "severity": "error", "title": "nope"},
            }),
        )
        .unwrap(),
    );
    assert!(session_notice(&other_method).is_none());
    assert!(session_compaction_event(&other_method).is_none());
}

/// The whole point of the translation: claude's reserved `_meta` block is
/// what the card's full label (counts, duration, trigger) is built from, so
/// it has to arrive on the synthetic call untouched.
#[test]
fn a_claude_compaction_update_keeps_its_reserved_meta() {
    let event = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_1",
        "status": "completed",
        "_meta": {"contextCompaction": {
            "version": 1, "trigger": "automatic",
            "preTokens": 180000, "postTokens": 42000, "durationMs": 3200,
        }},
    })))
    .expect("compaction event");
    match event {
        AcpEvent::ToolCallUpdate {
            tool_call_id,
            status,
            title,
            meta,
            ..
        } => {
            assert_eq!(tool_call_id, "cmp_1");
            assert_eq!(status.as_deref(), Some("completed"));
            assert_eq!(title.as_deref(), Some(CONTEXT_COMPACTION_TITLE));
            let payload = meta
                .as_ref()
                .and_then(|m| m.get("contextCompaction"))
                .expect("card payload");
            assert_eq!(
                payload.get("trigger").and_then(|v| v.as_str()),
                Some("automatic")
            );
            assert_eq!(
                payload.get("preTokens").and_then(|v| v.as_u64()),
                Some(180000)
            );
            assert_eq!(
                payload.get("postTokens").and_then(|v| v.as_u64()),
                Some(42000)
            );
            assert_eq!(
                payload.get("durationMs").and_then(|v| v.as_u64()),
                Some(3200)
            );
        }
        other => panic!("expected a ToolCallUpdate, got {other:?}"),
    }
}

/// claude-agent-acp 0.82.0 (recorded): the same block, moved under AIR —
/// and on a failure the error stays a sibling of `status`, no longer
/// repeated in the block.
#[test]
fn a_claude_compaction_update_takes_its_meta_from_the_air_namespace() {
    let event = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_1",
        "status": "completed",
        "_meta": {"jetbrains": {"air": {"version": 1, "contextCompaction": {
            "version": 1, "trigger": "automatic",
            "preTokens": 1000, "postTokens": 100, "durationMs": 7,
        }}}},
    })))
    .expect("compaction event");
    let AcpEvent::ToolCallUpdate {
        meta, raw_output, ..
    } = event
    else {
        panic!("expected a ToolCallUpdate");
    };
    let payload = meta
        .as_ref()
        .and_then(|m| m.get("contextCompaction"))
        .expect("card payload");
    assert_eq!(
        payload.get("preTokens").and_then(|v| v.as_u64()),
        Some(1000)
    );
    assert_eq!(
        payload.get("postTokens").and_then(|v| v.as_u64()),
        Some(100)
    );
    assert_eq!(
        payload.get("trigger").and_then(|v| v.as_str()),
        Some("automatic")
    );
    // 0.82.0 no longer repeats the streamed summary on the completion, and
    // an absent summary must not blank what the chunks already delivered.
    assert!(raw_output.is_none());

    let failed = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_2",
        "status": "failed",
        "error": "Context too small",
        "_meta": {"jetbrains": {"air": {"version": 1, "contextCompaction": {"version": 1}}}},
    })))
    .expect("compaction event");
    let AcpEvent::ToolCallUpdate { meta, .. } = failed else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(
        meta.as_ref()
            .and_then(|m| m.get("contextCompaction"))
            .and_then(|p| p.get("error"))
            .and_then(|v| v.as_str()),
        Some("Context too small")
    );
}

/// codex sends no `_meta` at all (its legacy call carried none either), so
/// the card must still be recognizable — recognition is by the
/// `contextCompaction` key, not by anything inside it.
#[test]
fn a_codex_compaction_update_is_still_recognizable_without_meta() {
    for status in ["in_progress", "completed", "failed", "cancelled"] {
        let event = session_compaction_event(&async_task_notif(serde_json::json!({
            "sessionUpdate": "compaction_update",
            "compactionId": "item_7",
            "status": status,
        })))
        .expect("compaction event");
        match event {
            AcpEvent::ToolCallUpdate {
                status: got, meta, ..
            } => {
                assert_eq!(got.as_deref(), Some(status));
                assert!(
                    meta.as_ref()
                        .and_then(|m| m.get("contextCompaction"))
                        .is_some(),
                    "the card keys off this marker"
                );
            }
            other => panic!("expected a ToolCallUpdate, got {other:?}"),
        }
    }
}

/// A failed compaction's reason is a SIBLING of `status` on the wire but a
/// member of the card's payload, so it has to be folded in where the card
/// looks — otherwise the divider says "failed" with no reason.
#[test]
fn a_failed_compaction_carries_its_error_into_the_card_payload() {
    let event = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_2",
        "status": "failed",
        "error": "Codex ended the turn before compaction completed.",
    })))
    .expect("compaction event");
    let AcpEvent::ToolCallUpdate { meta, .. } = event else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(
        meta.as_ref()
            .and_then(|m| m.get("contextCompaction"))
            .and_then(|p| p.get("error"))
            .and_then(|v| v.as_str()),
        Some("Codex ended the turn before compaction completed.")
    );
}

/// The frame codex-acp 2.1.1 sent live (its id aside) when the
/// compaction's model request got an HTTP 400 whose body is a service
/// error envelope: the same turn's failure banner already read the
/// message, and the card reads it too.
#[test]
fn a_failed_compaction_reads_a_service_error_envelope_down_to_its_message() {
    let event = session_compaction_event(&async_task_notif(serde_json::json!({
            "sessionUpdate": "compaction_update",
            "compactionId": "019d3c1e-2f0a-7a52-9b1e-6f2d1c9e8a10",
            "status": "failed",
            "error": "{\"type\": \"error\", \"status\": 400, \"error\": {\"type\": \"invalid_request_error\", \"message\": \"The 'fake-model' model is not supported when using Codex with a ChatGPT account.\"}}",
        })))
        .expect("compaction event");
    let AcpEvent::ToolCallUpdate { meta, .. } = event else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(
        meta.as_ref()
            .and_then(|m| m.get("contextCompaction"))
            .and_then(|p| p.get("error"))
            .and_then(|v| v.as_str()),
        Some("The 'fake-model' model is not supported when using Codex with a ChatGPT account.")
    );
}

/// The retained summary is the one thing the legacy presentation could
/// never carry. It rides `raw_output` — the streamed chunks APPEND — and
/// every translated update claims that channel with the codeg marker the
/// card expands on, because a bare `raw_output` is exactly what a legacy
/// call fills with its metadata object.
#[test]
fn a_compaction_summary_is_carried_on_raw_output_and_claimed_by_the_marker() {
    let event = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_3",
        "status": "completed",
        "summary": [{"type": "text", "text": "We refactored the parser."}],
    })))
    .expect("compaction event");
    let AcpEvent::ToolCallUpdate {
        raw_output,
        raw_output_append,
        meta,
        ..
    } = event
    else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(raw_output.as_deref(), Some("We refactored the parser."));
    assert_eq!(raw_output_append, None, "the settled summary REPLACES");
    assert_eq!(
        meta.as_ref()
            .and_then(|m| m.get(COMPACTION_SUMMARY_META_KEY)),
        Some(&serde_json::Value::Bool(true))
    );

    // The opening frame carries no summary yet, but it is the frame that
    // opens the card, so it has to make the claim too.
    let opening = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "cmp_3",
        "status": "in_progress",
    })))
    .expect("opening frame");
    let AcpEvent::ToolCallUpdate {
        raw_output, meta, ..
    } = opening
    else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(raw_output, None);
    assert_eq!(
        meta.as_ref()
            .and_then(|m| m.get(COMPACTION_SUMMARY_META_KEY)),
        Some(&serde_json::Value::Bool(true))
    );

    let chunk = session_compaction_event(&async_task_notif(serde_json::json!({
        "sessionUpdate": "compaction_summary_chunk",
        "compactionId": "cmp_3",
        "content": {"type": "text", "text": " Then the tests."},
    })))
    .expect("summary chunk");
    let AcpEvent::ToolCallUpdate {
        tool_call_id,
        status,
        raw_output,
        raw_output_append,
        meta,
        ..
    } = chunk
    else {
        panic!("expected a ToolCallUpdate");
    };
    assert_eq!(tool_call_id, "cmp_3");
    assert_eq!(status, None, "a summary chunk must not touch the status");
    assert_eq!(raw_output.as_deref(), Some(" Then the tests."));
    assert_eq!(raw_output_append, Some(true), "chunks append");
    // `_meta` is replace-on-update, and the lifecycle frames own it.
    assert_eq!(meta, None, "a summary chunk must not touch the meta");
}

/// `session` reaches the two agents that built these, and NOBODY else.
/// Both members must serialize as OBJECTS: claude tests `typeof notices ===
/// "object" && !== null && !Array.isArray`, and codex's compaction gate is
/// `!= null` — so an empty struct serializing to `null`, or being skipped,
/// would silently withhold the capability.
#[test]
fn the_session_capability_block_reaches_only_its_two_agents() {
    let session_of = |agent| {
        serde_json::to_value(build_client_capabilities(agent, HostToolsPolicy::Default))
            .unwrap()
            .get("session")
            .cloned()
    };
    for agent in [AgentType::ClaudeCode, AgentType::Codex] {
        assert_eq!(
            session_of(agent),
            Some(serde_json::json!({"compaction": {}, "notices": {}})),
            "{agent:?}"
        );
    }
    for agent in [
        AgentType::Gemini,
        AgentType::Pi,
        AgentType::Grok,
        AgentType::DeepSeek,
        AgentType::OpenCode,
        // A custom agent wrapping either adapter is deliberately NOT
        // advertised to: the id it resolves through is the user's, so
        // codeg cannot know what binary is behind it.
        AgentType::Custom("my-agent"),
    ] {
        assert_eq!(
            session_of(agent),
            None,
            "{agent:?} never implemented these; advertising would be a lie"
        );
    }
}

/// The spawn frame is the only one carrying the task's identity, so every
/// field of it has to survive the raw read — the merge downstream can only
/// revise what this captured.
#[test]
fn async_task_spawn_frame_is_read_whole() {
    let delta = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_spawned",
        "asyncTaskId": "t1",
        "name": "pnpm test",
        "taskType": "shell",
        "description": "pnpm test --watch",
        "showInTranscript": false,
        "canStop": true,
        "outputFilePath": "/tmp/tasks/t1.output",
        "toolCallId": "tool-9",
    })))
    .expect("spawn frame");
    assert!(delta.spawned);
    assert_eq!(delta.task_id, "t1");
    assert_eq!(delta.name.as_deref(), Some("pnpm test"));
    assert_eq!(delta.task_type.as_deref(), Some("shell"));
    assert_eq!(delta.description.as_deref(), Some("pnpm test --watch"));
    assert_eq!(delta.show_in_transcript, Some(false));
    assert_eq!(delta.can_stop, Some(true));
    assert_eq!(
        delta.output_file_path.as_deref(),
        Some("/tmp/tasks/t1.output")
    );
    assert_eq!(delta.tool_call_id.as_deref(), Some("tool-9"));
}

/// Progress and state frames revise an announced task. They must NOT read as
/// creates — `spawned` is what lets `apply_event` refuse to invent a row for
/// an announcement codeg failed to read.
#[test]
fn async_task_progress_and_state_frames_are_not_creates() {
    let progress = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_progress",
        "asyncTaskId": "t1",
        "lastToolName": "Bash",
        "usage": { "totalTokens": 1200, "toolUses": 3, "durationMs": 4500 },
    })))
    .expect("progress frame");
    assert!(!progress.spawned);
    assert_eq!(progress.last_tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        progress.usage,
        Some(AsyncTaskUsage {
            total_tokens: 1200,
            tool_uses: 3,
            duration_ms: 4500,
        })
    );
    // Absent fields stay absent — the merge must not blank a stored value.
    assert!(progress.name.is_none());
    assert!(progress.state.is_none());

    let state = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_state_update",
        "asyncTaskId": "t1",
        "state": "completed",
        "summary": "3 files changed",
    })))
    .expect("state frame");
    assert!(!state.spawned);
    assert_eq!(state.state.as_deref(), Some("completed"));
    assert_eq!(state.summary.as_deref(), Some("3 files changed"));
}

/// codex-acp 1.10.0's frames, verbatim off a live stdio session (`sleep 400`
/// left running in a persistent shell, then stopped). codex fills in far
/// less than claude — no `description`, `usage` or `outputFilePath`, and the
/// task id EQUALS the tool call id — so the reader has to survive on the
/// spawn frame's four fields alone, and `to_record`'s defaults must not
/// invent anything the strip would then render.
#[test]
fn async_task_reader_handles_the_codex_spawn_and_stop_frames() {
    let spawn = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_spawned",
        "asyncTaskId": "exec-74096479-1a0d-4c6d-bbfa-ae10fce94da2",
        "name": "sleep 400",
        "taskType": "shell",
        "showInTranscript": false,
        "canStop": true,
        "toolCallId": "exec-74096479-1a0d-4c6d-bbfa-ae10fce94da2",
    })))
    .expect("codex spawn frame");
    assert!(spawn.spawned);
    assert_eq!(spawn.name.as_deref(), Some("sleep 400"));
    assert_eq!(spawn.task_type.as_deref(), Some("shell"));
    assert_eq!(spawn.can_stop, Some(true));
    // Absent upstream, and they must stay absent rather than become empty
    // strings the strip would render as a blank meta line.
    assert!(spawn.description.is_none());
    assert!(spawn.usage.is_none());
    assert!(spawn.output_file_path.is_none());
    assert_eq!(spawn.tool_call_id.as_deref(), Some(spawn.task_id.as_str()));

    let record = spawn.to_record();
    assert_eq!(record.name, "sleep 400");
    assert_eq!(record.task_type, "shell");
    // No `state` on the wire: the row must start LIVE, or the strip would
    // never show a task that codex only ever revises at its terminal edge.
    assert_eq!(record.state, "running");
    assert!(record.can_stop);
    assert!(!crate::acp::types::async_task_state_is_terminal(
        &record.state
    ));

    let stopped = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_state_update",
        "asyncTaskId": "exec-74096479-1a0d-4c6d-bbfa-ae10fce94da2",
        "state": "stopped",
        "toolCallId": "exec-74096479-1a0d-4c6d-bbfa-ae10fce94da2",
    })))
    .expect("codex stop frame");
    assert!(!stopped.spawned);
    assert_eq!(stopped.state.as_deref(), Some("stopped"));
    assert!(crate::acp::types::async_task_state_is_terminal("stopped"));
}

/// The `_auth/status_update` payload codex-acp 1.9+ pushes, unchanged. The
/// only contract that matters is that it DESERIALIZES — a strict struct here
/// would make the typed handler fail on the first field OpenAI adds, and the
/// runtime answers a failing handler by logging `Handler errored` and dropping
/// the frame.
#[test]
fn auth_status_update_deserializes_every_observed_kind() {
    for payload in [
        // Observed live: a `~/.codex/config.toml` selecting a custom provider.
        serde_json::json!({"authStatus": {
            "kind": "gateway", "label": "Custom model gateway", "detail": "codeg"
        }}),
        serde_json::json!({"authStatus": {
            "kind": "account", "label": "ChatGPT Pro",
            "account": {"email": "a@b.c", "plan": "pro"}
        }}),
        serde_json::json!({"authStatus": {"kind": "none", "label": "Not logged in"}}),
        // A future kind, and a future sibling field, must both still parse.
        serde_json::json!({"authStatus": {"kind": "something_new"}, "extra": 1}),
    ] {
        let notif: AuthStatusUpdateNotification = serde_json::from_value(payload.clone())
            .unwrap_or_else(|e| {
                panic!("must not reject {payload}: {e}");
            });
        assert!(notif.auth_status.is_object());
    }
}

/// The interceptor sits in front of EVERY dispatch, so a false positive
/// would silently swallow ordinary session updates.
#[test]
fn async_task_reader_claims_only_its_own_three_variants() {
    for other in ["agent_message_chunk", "tool_call", "usage_update", "plan"] {
        assert!(
            air_async_task_delta(&async_task_notif(serde_json::json!({
                "sessionUpdate": other,
                "asyncTaskId": "t1",
            })))
            .is_none(),
            "{other} must reach the typed pipeline"
        );
    }
    // A frame with no usable id identifies no row, so there is nothing to
    // merge it into — leave it to the normal drop accounting.
    assert!(air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_spawned",
        "asyncTaskId": "   ",
        "name": "orphan",
    })))
    .is_none());
}

/// The adapter drops a `usage` object missing any of its three counters
/// rather than publishing a partial one; a half-read usage would render as
/// a task that used 0 tokens.
#[test]
fn async_task_usage_is_all_or_nothing() {
    let delta = air_async_task_delta(&async_task_notif(serde_json::json!({
        "sessionUpdate": "async_task_progress",
        "asyncTaskId": "t1",
        "usage": { "totalTokens": 10, "toolUses": 1 },
    })))
    .expect("progress frame");
    assert!(delta.usage.is_none());
}

/// The `session/load` replay drains a PAST session. Every grok ext outcome
/// is skipped there: an alert would report a historical failure as if it
/// were happening now, and a compaction card would linger in the live state
/// as in-flight content (grok's transcript gets its cards from the history
/// parser instead).
#[test]
fn grok_ext_notification_skipped_on_replay_covers_every_mapped_outcome() {
    let notif = |variant: &str| {
        Dispatch::Notification(
            UntypedMessage::new(
                "_x.ai/session_notification",
                serde_json::json!({
                    "sessionId": "s",
                    "update": {
                        "sessionUpdate": variant,
                        "tokens_before": 9, "tokens_after": 8, "reason": "x",
                        "notes": ["Image 1 was dropped before send: too small."]
                    }
                }),
            )
            .unwrap(),
        )
    };
    for variant in [
        "image_dropped",
        "auto_compact_completed",
        "auto_compact_failed",
    ] {
        assert!(
            grok_ext_notification_skipped_on_replay(&notif(variant), AgentType::Grok),
            "{variant} must not replay into the live state"
        );
    }
    // Unmapped variants and non-grok agents are left to the normal path.
    assert!(!grok_ext_notification_skipped_on_replay(
        &notif("turn_completed"),
        AgentType::Grok
    ));
    assert!(!grok_ext_notification_skipped_on_replay(
        &notif("image_dropped"),
        AgentType::Codex
    ));
}

/// A failed compaction card travels with a coded `Error`, so the readers of
/// the error stream (chat-channel bridges, `last_error`, the pet) still hear
/// about it; a completed card, or any other event, gets none.
#[test]
fn compaction_failure_error_accompanies_only_a_failed_compaction_card() {
    let raw = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "auto_compact_failed", "reason": "API error (status 503)" }
        }),
    )
    .unwrap();
    let card = map_grok_ext_notification(&raw, AgentType::Grok).expect("failed card");
    match compaction_failure_error(&card, AgentType::Grok) {
        Some(AcpEvent::Error {
            message,
            code,
            terminal,
            ..
        }) => {
            assert_eq!(message, "Context compaction failed: API error (status 503)");
            assert_eq!(code.as_deref(), Some(COMPACTION_FAILED_ERROR_CODE));
            assert!(
                !terminal,
                "a failed compaction must not kill the connection"
            );
        }
        other => panic!("expected the companion Error, got {other:?}"),
    }

    let bare = UntypedMessage::new(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "auto_compact_failed" }
        }),
    )
    .unwrap();
    let bare_card = map_grok_ext_notification(&bare, AgentType::Grok).expect("failed card");
    assert!(matches!(
        compaction_failure_error(&bare_card, AgentType::Grok),
        Some(AcpEvent::Error { message, .. }) if message == "Context compaction failed"
    ));

    let completed = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "auto_compact_completed", "tokens_before": 9, "tokens_after": 8 }
            }),
        )
        .unwrap();
    let completed_card =
        map_grok_ext_notification(&completed, AgentType::Grok).expect("completed card");
    assert!(compaction_failure_error(&completed_card, AgentType::Grok).is_none());
}

/// Grok's cumulative token count rides the OUTER `params._meta` of ordinary
/// updates — the only live context signal it offers, and one the typed
/// pipeline drops. The peek must read it there, hold its fire while the
/// number repeats, and stay silent without a window to divide by.
#[test]
fn grok_live_usage_step_reads_the_outer_meta_and_dedupes() {
    let update = |meta: serde_json::Value| {
        Dispatch::Notification(
            UntypedMessage::new(
                "session/update",
                serde_json::json!({
                    "sessionId": "s",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "hi"}
                    },
                    "_meta": meta,
                }),
            )
            .unwrap(),
        )
    };
    let streaming = update(serde_json::json!({"totalTokens": 4200, "agentTimestampMs": 3}));

    assert_eq!(
        grok_live_usage_step(&streaming, AgentType::Grok, Some(500_000), None),
        Some((4200, 500_000))
    );
    // Same count again → nothing to say (the field rides nearly every chunk).
    assert_eq!(
        grok_live_usage_step(
            &streaming,
            AgentType::Grok,
            Some(500_000),
            Some((4200, 500_000))
        ),
        None
    );
    // A different prior value is a real step → emit.
    assert_eq!(
        grok_live_usage_step(
            &streaming,
            AgentType::Grok,
            Some(500_000),
            Some((3000, 500_000))
        ),
        Some((4200, 500_000))
    );
    // Same count but a NEW window (the user switched model between turns) →
    // re-emit, or the ring would keep dividing by the old model's window.
    assert_eq!(
        grok_live_usage_step(
            &streaming,
            AgentType::Grok,
            Some(256_000),
            Some((4200, 500_000))
        ),
        Some((4200, 256_000))
    );
    // No resolvable window → still report the count, with the frontend's
    // "unknown window" sentinel as the size, so the ring falls back to the
    // parsed session stats instead of freezing on a previous model's window.
    assert_eq!(
        grok_live_usage_step(&streaming, AgentType::Grok, None, None),
        Some((4200, 0))
    );
    assert_eq!(
        grok_live_usage_step(&streaming, AgentType::Grok, None, Some((4200, 0))),
        None
    );
    // `_meta` without the count, and a zero count, are both "no data".
    assert_eq!(
        grok_live_usage_step(
            &update(serde_json::json!({"agentTimestampMs": 3})),
            AgentType::Grok,
            Some(500_000),
            None
        ),
        None
    );
    assert_eq!(
        grok_live_usage_step(
            &update(serde_json::json!({"totalTokens": 0})),
            AgentType::Grok,
            Some(500_000),
            None
        ),
        None
    );
    // Never fires for another agent — they have a real `usage_update`.
    assert_eq!(
        grok_live_usage_step(&streaming, AgentType::ClaudeCode, Some(500_000), None),
        None
    );
}

/// A model switch between turns moves the denominator, and the frontend's
/// reducer drops a `used == 0` update while it holds a positive one — so the
/// previous model's window has to be re-keyed at turn start, not cleared.
#[test]
fn grok_window_change_usage_rekeys_a_live_pair_to_the_new_window() {
    // Same window (no switch) → nothing to do.
    assert_eq!(
        grok_window_change_usage(Some(500_000), Some((4200, 500_000))),
        None
    );
    // Switched to a smaller-window model → carry the count, swap the window.
    assert_eq!(
        grok_window_change_usage(Some(256_000), Some((4200, 500_000))),
        Some((4200, 256_000))
    );
    // Nothing emitted yet → nothing to re-key (the first real count will).
    assert_eq!(grok_window_change_usage(Some(256_000), None), None);
    // Switched to a BYO model nothing can size → relinquish the denominator
    // with the "unknown window" sentinel. This is the case that would
    // otherwise strand the previous model's window forever: no later update
    // can correct it, and a zero-`used` clear would be dropped by the
    // frontend reducer.
    assert_eq!(
        grok_window_change_usage(None, Some((4200, 500_000))),
        Some((4200, 0))
    );
    // …and once relinquished, it stays relinquished (no event storm).
    assert_eq!(grok_window_change_usage(None, Some((4200, 0))), None);
    // Switching BACK to a sized model re-keys off the sentinel.
    assert_eq!(
        grok_window_change_usage(Some(500_000), Some((4200, 0))),
        Some((4200, 500_000))
    );
}

/// A codex-acp 1.13.1 frame (`createUsageUpdate`).
fn codex_usage_frame(used: u64, size: u64) -> serde_json::Value {
    serde_json::json!({"sessionUpdate": "usage_update", "used": used, "size": size})
}

/// A codex-acp 1.13.1 web search frame: its opening `tool_call`
/// (`createWebSearchStartUpdate`) or its completion `tool_call_update`
/// (`createWebSearchCompleteUpdate`) — both carry the same `rawInput`.
fn codex_web_search_frame(session_update: &str) -> serde_json::Value {
    let mut frame = serde_json::json!({
        "sessionUpdate": session_update,
        "toolCallId": "ws_1",
        "title": "Web search: codex context window",
        "status": "completed",
        "rawInput": {
            "type": "webSearch",
            "id": "ws_1",
            "query": "codex context window",
            "action": {"type": "search", "query": "codex context window"},
        },
    });
    if session_update == "tool_call" {
        frame["kind"] = serde_json::json!("search");
        frame["status"] = serde_json::json!("in_progress");
    }
    frame
}

/// Run codex frames through the live dispatcher on a fresh connection:
/// the ring reading it ends on, and every `(used, size)` clients were sent.
async fn codex_usage_after(
    frames: Vec<serde_json::Value>,
) -> (Option<(u64, u64)>, Vec<(u64, u64)>) {
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-codex".to_string(),
        AgentType::Codex,
        None,
        "win".to_string(),
        None,
    )));
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    for frame in frames {
        let update: SessionUpdate = serde_json::from_value(frame).expect("wire shape");
        emit_conversation_update(
            &state,
            &EventEmitter::Noop,
            AgentType::Codex,
            update,
            None,
            &mut cache,
            &mut cb,
        )
        .await;
    }
    let guard = state.read().await;
    let held = guard.usage.as_ref().map(|u| (u.used, u.size));
    let emitted = guard
        .recent_events_after(0)
        .expect("events recorded")
        .iter()
        .filter_map(|e| match &e.payload {
            AcpEvent::UsageUpdate { used, size } => Some((*used, *size)),
            _ => None,
        })
        .collect();
    (held, emitted)
}

/// Issue #846, live. codex-acp forwards each request's
/// `last_token_usage.total_tokens` as `usage_update.used`; the request that
/// ran a web search reports the provider's whole server-side search loop,
/// so it must not move the ring — the next request's report does.
#[tokio::test]
async fn codex_usage_of_a_request_that_ran_a_web_search_leaves_the_ring_alone() {
    // Either frame of the search marks its request on its own.
    for search in ["tool_call", "tool_call_update"] {
        let (held, emitted) = codex_usage_after(vec![
            codex_usage_frame(57_302, 996_147),
            codex_web_search_frame(search),
            codex_usage_frame(530_278, 996_147),
            // Restated as the next request opens.
            codex_usage_frame(530_278, 996_147),
        ])
        .await;
        assert_eq!(held, Some((57_302, 996_147)), "{search}");
        assert_eq!(
            emitted,
            vec![(57_302, 996_147)],
            "{search}: no client is ever sent the searched report"
        );
    }

    let (held, _) = codex_usage_after(vec![
        codex_usage_frame(57_302, 996_147),
        codex_web_search_frame("tool_call"),
        codex_usage_frame(530_278, 996_147),
        codex_usage_frame(79_334, 996_147),
    ])
    .await;
    assert_eq!(held, Some((79_334, 996_147)));
}

/// A model switch moves the denominator even while the figure is held: the
/// set-aside report still carries the new model's window.
#[tokio::test]
async fn a_set_aside_codex_report_still_moves_the_ring_to_its_window() {
    let (held, emitted) = codex_usage_after(vec![
        codex_usage_frame(90_000, 100_000),
        codex_web_search_frame("tool_call"),
        codex_usage_frame(530_278, 996_147),
        codex_usage_frame(530_278, 996_147),
    ])
    .await;
    assert_eq!(held, Some((90_000, 996_147)));
    assert_eq!(emitted, vec![(90_000, 100_000), (90_000, 996_147)]);
}

/// The offline half of the live resolver: Grok's own on-disk catalog, then
/// the id heuristic. A BYO endpoint is keyed by whatever id the user typed,
/// so this genuinely can come back empty — `grok_window_change_usage` is
/// what keeps that from stranding a stale window.
#[test]
fn grok_offline_context_window_falls_through_catalog_then_heuristic() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":{"grok-4.5":{"info":{"context_window":500000}},
                          "deepseek-chat":{"info":{"context_window":131072}}}}"#,
    )
    .expect("write catalog");
    std::fs::write(
        home.path().join("config.toml"),
        "[model.\"my-byo\"]\nmodel = \"my-byo\"\ncontext_window = 64000\n",
    )
    .expect("write config");

    // Catalog wins, including for a non-`grok` id.
    assert_eq!(
        grok_offline_context_window("deepseek-chat", home.path()),
        Some(131_072)
    );
    // BYO block in config.toml is the second source.
    assert_eq!(
        grok_offline_context_window("my-byo", home.path()),
        Some(64_000)
    );
    // Neither file names it, but the id is Grok-shaped → heuristic answers.
    assert_eq!(
        grok_offline_context_window("grok-9-unreleased", home.path()),
        Some(256_000)
    );
    // Neither file names it AND the id belongs to no known family — the BYO
    // case Codex flagged. `None` here is load-bearing, not a gap.
    assert_eq!(
        grok_offline_context_window("some-local-llama", home.path()),
        None
    );
}

// ---- empty-turn diagnosis ----

fn drop_err(msg: &str) -> String {
    msg.to_string()
}

/// The read loop drops at most one update per notification, so an agent
/// speaking a protocol codeg can't decode used to put a WARN on the
/// per-streaming-chunk path — under the DEFAULT level, which is how a field
/// report ended up writing 34GB in 8.8h (issue #427). The throttle collapses
/// the burst; this pins the part that must survive it, the tally.
#[test]
fn dropped_update_log_line_reports_what_the_throttle_swallowed() {
    // Leading edge: the plain line, no tally to report yet.
    let first = dropped_update_log_line("idle", &drop_err("unknown variant"), 1);
    assert_eq!(
        first,
        "[ACP] Ignoring unreadable session update (idle): unknown variant"
    );
    // A coalesced line names how many were suppressed and over what window,
    // so the reader can tell "happened once" from "happening constantly".
    let coalesced = dropped_update_log_line("turn", &drop_err("missing field"), 4213);
    assert!(
        coalesced.starts_with("[ACP] Ignoring unreadable session update (turn): missing field"),
        "{coalesced}"
    );
    assert!(coalesced.contains("+4212 more"), "{coalesced}");
    assert!(
        coalesced.contains(&format!("{}s", DROPPED_UPDATE_LOG_WINDOW.as_secs())),
        "{coalesced}"
    );
}

#[test]
fn dropped_update_logging_emits_once_per_window() {
    let mut throttle = LeadingEdgeThrottle::new(DROPPED_UPDATE_LOG_WINDOW);
    let now = std::time::Instant::now();
    // First hit surfaces instantly...
    assert!(throttle.record_at(now, 1).is_some());
    // ...and a chunk-rate burst inside the window emits nothing at all.
    for i in 1..10_000u64 {
        assert!(
            throttle
                .record_at(now + std::time::Duration::from_micros(i * 100), 1)
                .is_none(),
            "burst hit {i} must be suppressed"
        );
    }
    // Past the window, one line carries the whole suppressed tally.
    let summary = throttle
        .record_at(now + DROPPED_UPDATE_LOG_WINDOW, 1)
        .expect("post-window hit emits");
    assert_eq!(summary.occurrences, 10_000);
}

#[test]
fn note_update_splits_agent_output_from_metadata() {
    use agent_client_protocol::schema::v1::{ContentChunk, Plan};

    let mut probe = TurnOutputProbe::new(0);
    probe.note_update(
        AgentType::ClaudeCode,
        &SessionUpdate::Plan(Plan::new(Vec::new())),
    );
    assert!(!probe.saw_agent_output, "Plan is not agent output");
    assert!(probe.saw_metadata_update);

    probe.note_update(
        AgentType::ClaudeCode,
        &SessionUpdate::AgentMessageChunk(ContentChunk::new("hi".into())),
    );
    assert!(probe.saw_agent_output);
}

/// The classifier's whole purpose is to separate "we went blind" from
/// "nothing came" — a `Plan`-only turn must not read as agent output.
#[test]
fn diagnose_empty_turn_covers_three_causes_and_priority() {
    let mut none = TurnOutputProbe::new(0);
    assert_eq!(diagnose_empty_turn(&none), EmptyTurnCause::NoOutput);

    none.saw_metadata_update = true;
    assert_eq!(diagnose_empty_turn(&none), EmptyTurnCause::MetadataOnly);

    // Dropped updates outrank metadata: not being able to read the output
    // is the stronger signal and points somewhere else entirely.
    none.note_dropped(&drop_err("boom"));
    assert_eq!(diagnose_empty_turn(&none), EmptyTurnCause::ProtocolMismatch);
}

#[test]
fn note_dropped_counts_every_drop_and_keeps_the_first() {
    let mut probe = TurnOutputProbe::new(0);
    probe.note_dropped(&drop_err("missing field `sessionUpdate`"));
    probe.note_dropped(&drop_err("missing field `update`"));
    probe.note_dropped(&drop_err("missing field `content`"));

    assert_eq!(probe.dropped, 3);
    let summary = probe.first_drop.as_ref().expect("first drop recorded");
    assert_eq!(
        summary, "missing field `sessionUpdate`",
        "first wins, not last"
    );
}

/// Drop reasons reach the UI, so they must be redacted at capture time —
/// a parser error inlines the value it choked on, and that value came off
/// the `session/update` channel.
#[test]
fn note_dropped_redacts_the_captured_error() {
    const SECRET: &str = "sk-live-abcdefghijklmnop";
    let mut probe = TurnOutputProbe::new(0);
    probe.note_dropped(&drop_err(&format!(
        r#"invalid type: string "{SECRET}", expected u64 at line 1 column 40"#
    )));
    let summary = probe.first_drop.as_ref().unwrap();
    assert!(!summary.contains(SECRET), "leaked: {summary}");
    assert_eq!(summary, "invalid type, expected u64 at line 1 column 40");
}

#[test]
fn finish_turn_reason_passes_through_non_empty_reasons() {
    let tail = StderrTail::new();
    let mut probe = TurnOutputProbe::new(0);
    probe.saw_agent_output = true;

    for reason in ["end_turn", "cancelled", "refusal", "max_tokens", "unknown"] {
        let (out, report) = finish_turn_reason(&probe, reason, &tail);
        assert_eq!(out, reason);
        assert!(report.is_none());
    }

    // Without agent output, only `end_turn` is rewritten.
    let silent = TurnOutputProbe::new(0);
    assert_eq!(
        finish_turn_reason(&silent, "cancelled", &tail).0,
        "cancelled"
    );
    assert_eq!(finish_turn_reason(&silent, "end_turn", &tail).0, "empty");
}

/// The helper only computes, so calling it twice is identical and
/// side-effect free — the turn exit owns every side effect.
#[test]
fn finish_turn_reason_is_pure() {
    let tail = StderrTail::new();
    tail.push("boom");
    let probe = TurnOutputProbe::new(0);

    let (first_reason, first) = finish_turn_reason(&probe, "end_turn", &tail);
    let (second_reason, second) = finish_turn_reason(&probe, "end_turn", &tail);
    assert_eq!(first_reason, second_reason);
    assert_eq!(
        first.as_ref().map(|r| r.cause),
        second.as_ref().map(|r| r.cause)
    );
    assert_eq!(
        first.as_ref().and_then(|r| r.details.clone()),
        second.as_ref().and_then(|r| r.details.clone())
    );
}

#[test]
fn empty_turn_details_quote_stderr_scoped_to_the_turn() {
    let tail = StderrTail::new();
    tail.push("older line from a previous turn");
    let probe = TurnOutputProbe::new(tail.mark());
    tail.push("Error: 401 Unauthorized");

    let details = build_empty_turn_details(&probe, &tail).expect("details");
    assert!(details.contains("stderr (this turn"), "{details}");
    assert!(details.contains("Error: 401 Unauthorized"));
    assert!(!details.contains("older line"));
}

#[test]
fn empty_turn_details_fall_back_to_recent_stderr() {
    let tail = StderrTail::new();
    tail.push("connect-time failure");
    let probe = TurnOutputProbe::new(tail.mark());

    let details = build_empty_turn_details(&probe, &tail).expect("details");
    assert!(details.contains("stderr (recent"), "{details}");
    assert!(details.contains("connect-time failure"));
}

#[test]
fn empty_turn_details_report_drop_counts() {
    let tail = StderrTail::new();
    let mut probe = TurnOutputProbe::new(0);
    probe.note_dropped(&drop_err("missing field `update`"));
    probe.note_dropped(&drop_err("EOF while parsing a value"));

    let details = build_empty_turn_details(&probe, &tail).expect("details");
    assert!(
        details.contains("dropped 2 unreadable update(s)"),
        "{details}"
    );
    assert!(
        details.contains("first: missing field `update`"),
        "{details}"
    );
}

#[test]
fn empty_turn_details_are_none_without_evidence() {
    let tail = StderrTail::new();
    let probe = TurnOutputProbe::new(0);
    assert!(build_empty_turn_details(&probe, &tail).is_none());
}

#[test]
fn empty_turn_details_are_bounded() {
    let tail = StderrTail::new();
    for i in 0..40 {
        tail.push(&format!("{i:03} {}", "x".repeat(200)));
    }
    let probe = TurnOutputProbe::new(0);
    let details = build_empty_turn_details(&probe, &tail).expect("details");
    assert!(details.len() <= MAX_DETAILS_BYTES + '…'.len_utf8());
}

/// The existing non-empty reasons must keep their exact codes and stay
/// `details`-free; only the empty family gained anything.
#[test]
fn turn_failure_error_event_preserves_existing_reasons() {
    assert!(turn_failure_error_event("end_turn", AgentType::ClaudeCode, None).is_none());
    assert!(turn_failure_error_event("cancelled", AgentType::ClaudeCode, None).is_none());

    for (reason, expected) in [
        ("refusal", "turn_failed_refusal"),
        ("max_tokens", "turn_failed_max_tokens"),
        ("max_turn_requests", "turn_failed_max_turn_requests"),
        ("unknown", "turn_failed_unknown"),
    ] {
        let Some(AcpEvent::Error { code, details, .. }) =
            turn_failure_error_event(reason, AgentType::ClaudeCode, None)
        else {
            panic!("{reason} should produce an error event");
        };
        assert_eq!(code.as_deref(), Some(expected));
        assert!(details.is_none(), "{reason} must not carry details");
    }
}

/// The synthesized reason for an `authRequired` prompt REJECTION
/// (claude-agent-acp 0.74.0's mid-session sign-out). It must produce its
/// own code — reusing `refusal` would tell the user the agent declined the
/// work, when it actually declined the credentials — and it must stay
/// non-terminal, because the whole point of that arm in
/// `run_conversation_loop` is that the connection outlives the turn.
#[test]
fn turn_failure_error_event_maps_auth_required_without_killing_the_connection() {
    let Some(AcpEvent::Error {
        code,
        details,
        terminal,
        message,
        ..
    }) = turn_failure_error_event("auth_required", AgentType::ClaudeCode, None)
    else {
        panic!("auth_required should produce an error event");
    };
    assert_eq!(code.as_deref(), Some("turn_failed_auth_required"));
    assert!(details.is_none());
    assert!(!terminal, "a sign-out never kills the connection");
    assert!(message.contains("sign in"), "message was {message:?}");
}

#[test]
fn turn_failure_error_event_maps_each_empty_cause() {
    for (cause, expected) in [
        (EmptyTurnCause::NoOutput, "turn_failed_empty"),
        (
            EmptyTurnCause::ProtocolMismatch,
            "turn_failed_empty_protocol",
        ),
        (EmptyTurnCause::MetadataOnly, "turn_failed_empty_metadata"),
    ] {
        let report = EmptyTurnReport {
            cause,
            details: Some("evidence".into()),
        };
        let Some(AcpEvent::Error {
            code,
            details,
            terminal,
            ..
        }) = turn_failure_error_event("empty", AgentType::ClaudeCode, Some(&report))
        else {
            panic!("empty should produce an error event");
        };
        assert_eq!(code.as_deref(), Some(expected));
        assert_eq!(details.as_deref(), Some("evidence"));
        assert!(!terminal, "an empty turn never kills the connection");
    }

    // No report (a path that skipped diagnosis) keeps the original code.
    let Some(AcpEvent::Error { code, details, .. }) =
        turn_failure_error_event("empty", AgentType::ClaudeCode, None)
    else {
        panic!("empty should produce an error event");
    };
    assert_eq!(code.as_deref(), Some("turn_failed_empty"));
    assert!(details.is_none());
}

#[test]
fn build_new_session_request_sets_claude_raw_meta() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_new_session_request(AgentType::ClaudeCode, &cwd, Vec::new());

    assert_eq!(
        req.meta
            .as_ref()
            .and_then(|m| m.get("claudeCode"))
            .and_then(|v| v.get("emitRawSDKMessages"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

/// The `loadSession` capability gate hands the failure ladder a synthetic
/// error instead of sending an unsupported RPC. That error must classify as
/// "just open a new session": anything else would put a "session could not
/// be loaded" banner in front of every user whose agent simply does not
/// implement `session/load`.
#[test]
fn a_session_load_never_sent_falls_back_without_alarming_the_user() {
    let e = agent_client_protocol::Error::method_not_found()
        .data("agent does not advertise the loadSession capability");
    let text = e.to_string();
    assert_eq!(classify_session_load_failure(e.code, &text), None);
    assert!(text.contains("Method not found"), "{text}");
    assert!(!text.contains("Authentication required"), "{text}");
}

/// Issue #797, verbatim off the wire: qwen-code's ACP surface throws
/// `Slash command not supported in ACP integration: …` for a `/mcp` it does
/// not implement, and its SDK answers -32603 with the text under
/// `data.details`. The session stays in the agent's map — so this must
/// cost the user a turn, not the whole connection (which greyed out the
/// composer until a full agent respawn finished).
#[test]
fn an_unsupported_slash_command_costs_the_turn_not_the_connection() {
    let e = agent_client_protocol::Error::internal_error().data(serde_json::json!({
        "details": "Slash command not supported in ACP integration: \
                    The command \"/mcp\" is not supported in this mode.",
    }));
    assert!(!prompt_rejection_is_terminal(&e));
    // What the user reads is the agent's own sentence, sanitized — the
    // banner is the only place the reason exists.
    let rendered = AcpError::protocol(e.to_string()).to_string();
    assert!(rendered.contains("/mcp"), "{rendered}");
}

/// The other turn-scoped shapes that used to kill a healthy connection:
/// an adapter wrapping a provider-side turn error as -32603 (opencode,
/// issue #659) and ACP's own `authRequired` (which additionally keeps its
/// dedicated stop reason).
#[test]
fn an_agent_that_answers_at_all_keeps_its_connection() {
    for e in [
        agent_client_protocol::Error::internal_error().data(serde_json::json!({
            "service": "session",
            "errorName": "APIError",
        })),
        agent_client_protocol::Error::auth_required().data("Please sign in"),
        agent_client_protocol::Error::invalid_params().data("unknown model"),
        // The message checks read an agent-controlled string, so a
        // rejection that merely QUOTES one of the terminal markers about
        // something else must not be mistaken for a dead session.
        agent_client_protocol::Error::auth_required()
            .data("the helper process exited; sign in again to restart it"),
        agent_client_protocol::Error::internal_error()
            .data("upstream response never received by the provider, retry"),
    ] {
        assert!(!prompt_rejection_is_terminal(&e), "{e}");
    }
}

/// The three families that stay terminal. Anything looser here would leave
/// a connection whose every later prompt fails exactly the same way — the
/// user would sit on a dead session with no teardown to recover from.
#[test]
fn a_rejection_that_reports_a_dead_session_still_tears_the_connection_down() {
    // The agent has no record of the id codeg just prompted on.
    assert!(prompt_rejection_is_terminal(
        &agent_client_protocol::Error::resource_not_found(None)
    ));
    // The agent answered to say its session/process is gone.
    for marker in SESSION_GONE_MARKERS {
        let e = agent_client_protocol::Error::internal_error()
            .data(format!("Claude Code {marker} — sorry"));
        assert!(prompt_rejection_is_terminal(&e), "{e}");
    }
    // The runtime's own synthesized error: the response channel was dropped, so
    // no answer ever arrived and the transport is what died.
    let dropped = agent_client_protocol::util::internal_error(
        "response to `session/prompt` never received: channel closed",
    );
    assert!(prompt_rejection_is_terminal(&dropped), "{dropped}");
}

/// codex-acp 2.2 rejects a prompt with 1001 when the app-server died. A
/// restartable loss costs that turn only (the next prompt restarts the
/// app-server and goes through, measured live); an unrestartable one means
/// every later prompt on this connection fails the same way, so it ends the
/// connection, as the adapter's own remedy needs. The #604 body is verbatim
/// off 2.2.1: a session that never reached disk, lost in a restart.
#[test]
fn an_unrestartable_codex_app_server_loss_ends_the_connection() {
    let error = |body: serde_json::Value| -> agent_client_protocol::Error {
        serde_json::from_value(body).expect("an ACP error body")
    };
    let lost_empty_session = error(serde_json::json!({
        "code": 1001,
        "message": "Session 01a123c6-aee8-7a81-9007-44230bc64566 had no messages yet and \
            was lost when the Codex app-server restarted. Start a new session.",
        "data": { "exitCode": null, "signal": null, "restartable": false }
    }));
    assert!(prompt_rejection_is_terminal(&lost_empty_session));

    let restarting = error(serde_json::json!({
        "code": 1001,
        "message": "The connection to the Codex app-server was lost. The agent starts \
            it again on the next request.",
        "data": { "exitCode": null, "signal": null, "restartable": true }
    }));
    assert!(!prompt_rejection_is_terminal(&restarting));
}

/// Plays an agent over the raw pipe: waits for codeg's first frame (the
/// prompt), then dies with it unanswered — its output ends mid-prompt, the
/// way it does when the process exits. Fires `exited` once the pipe is gone.
fn agent_that_dies_on_the_prompt(
    pipe: agent_client_protocol::Channel,
    exited: tokio::sync::oneshot::Sender<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use futures::StreamExt;
        let agent_client_protocol::Channel { mut rx, tx } = pipe;
        assert!(rx.next().await.is_some(), "the prompt reaches the agent");
        drop((rx, tx));
        let _ = exited.send(());
    })
}

/// What the 2.x runtime answers a prompt the agent died on is its own
/// `Incoming transport closed`, not the "never received" the classifier
/// already knew — taken here off the real runtime. Turn-scoped, it kept a
/// dead agent's session open, rejecting every later prompt the same way.
#[tokio::test]
async fn a_prompt_the_agent_died_on_is_a_terminal_rejection() {
    let (client_end, agent_end) = agent_client_protocol::Channel::duplex();
    let (exited_tx, _exited_rx) = tokio::sync::oneshot::channel();
    let agent = agent_that_dies_on_the_prompt(agent_end, exited_tx);

    let rejection = Client
        .builder()
        .connect_with(client_end, async |cx: ConnectionTo<Agent>| {
            Ok(cx
                .send_request_to(Agent, PromptRequest::new(SessionId::new("s1"), Vec::new()))
                .block_task()
                .await
                .expect_err("nothing is left to answer the prompt"))
        })
        .await
        .expect("client connection");

    assert!(lost_the_connection(&rejection), "{rejection}");
    assert!(prompt_rejection_is_terminal(&rejection), "{rejection}");
    agent.await.unwrap();
}

/// Codeg's end of a pipe to an agent that crashes the way `AcpAgent`
/// reports it: the agent's output ends first, and only a moment later does
/// the child monitor come back with the exit status, as the transport's
/// own result.
struct CrashingAgent {
    pipe: agent_client_protocol::Channel,
    exited: tokio::sync::oneshot::Receiver<()>,
}

impl agent_client_protocol::ConnectTo<Client> for CrashingAgent {
    async fn connect_to(
        self,
        client: impl agent_client_protocol::ConnectTo<Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Self { pipe, exited } = self;
        let monitor = async {
            let _ = exited.await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Err(agent_client_protocol::util::internal_error(
                "Process exited with exit status: 1: boom",
            ))
        };
        tokio::select! {
            result = agent_client_protocol::ConnectTo::<Client>::connect_to(pipe, client) => result,
            result = monitor => result,
        }
    }
}

/// The main future can hear of a crash before the child monitor has
/// reported it, and `connect_with` ends on whichever side finishes first —
/// so it holds its own error back, and the agent's exit report is what the
/// connection ends with.
#[tokio::test]
async fn a_crash_mid_prompt_ends_the_connection_with_the_agents_exit_report() {
    let (client_end, agent_end) = agent_client_protocol::Channel::duplex();
    let (exited_tx, exited_rx) = tokio::sync::oneshot::channel();
    let agent = agent_that_dies_on_the_prompt(agent_end, exited_tx);

    let ended = Client
        .builder()
        .connect_with(
            CrashingAgent {
                pipe: client_end,
                exited: exited_rx,
            },
            async |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let rejection = cx
                    .send_request_to(Agent, PromptRequest::new(SessionId::new("s1"), Vec::new()))
                    .block_task()
                    .await
                    .expect_err("the agent died before answering");
                assert!(lost_the_connection(&rejection), "{rejection}");
                Err(defer_to_connection_report(rejection).await)
            },
        )
        .await;

    let error = ended.expect_err("the connection ends in an error");
    assert!(error.to_string().contains("Process exited"), "{error}");
    agent.await.unwrap();
}

#[test]
fn the_model_selector_is_the_model_recorded_on_a_turn() {
    let select = |id: &str, category: &str, current: &str| SessionConfigOptionInfo {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        category: Some(category.to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value: current.to_string(),
            options: Vec::new(),
            groups: Vec::new(),
        }),
        recommended_value: None,
    };

    // The model comes from the `model` selector, not from whichever
    // selector happens to be first — agents publish several.
    assert_eq!(
        current_model_id_from_opts(&[
            select("effort", "mode", "high"),
            select("model", "model", "grok-4"),
        ]),
        Some("grok-4".to_string())
    );
    // No model selector (the common case for custom agents) and an empty
    // current value both mean "unknown", never a placeholder.
    assert_eq!(
        current_model_id_from_opts(&[select("effort", "mode", "high")]),
        None
    );
    assert_eq!(
        current_model_id_from_opts(&[select("m", "model", "")]),
        None
    );
    assert_eq!(current_model_id_from_opts(&[]), None);
}

/// A fork continues the conversation, so it must continue the conversation's
/// selectors: `handle_fork_or_exit` reads the parent session's options and
/// replays them onto the forked one as preferred values. That only works if
/// the extraction speaks the shape `apply_preferred_session_options` (and
/// `config_option_already_holds`) consume — a select's value id, and a
/// boolean as "true"/"false".
#[test]
fn current_config_option_values_round_trips_what_the_preference_replay_expects() {
    let opts = vec![
        SessionConfigOptionInfo {
            id: "model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: "opus".to_string(),
                options: Vec::new(),
                groups: Vec::new(),
            }),
            recommended_value: None,
        },
        SessionConfigOptionInfo {
            id: "auto_approve".to_string(),
            name: "Auto-approve".to_string(),
            description: None,
            category: None,
            kind: SessionConfigKindInfo::Boolean(SessionConfigBooleanInfo {
                current_value: true,
            }),
            recommended_value: None,
        },
    ];

    let values = current_config_option_values(&opts);
    assert_eq!(values.get("model").map(String::as_str), Some("opus"));
    assert_eq!(values.get("auto_approve").map(String::as_str), Some("true"));

    // Each extracted value must satisfy the same equality the replay uses to
    // skip a round trip, or restoring a selector would re-set every option.
    for opt in &opts {
        let schema = SessionConfigOption::new(
            opt.id.clone(),
            opt.name.clone(),
            match &opt.kind {
                SessionConfigKindInfo::Select(sel) => SessionConfigKind::Select(
                    agent_client_protocol::schema::v1::SessionConfigSelect::new(
                        sel.current_value.clone(),
                        agent_client_protocol::schema::v1::SessionConfigSelectOptions::Ungrouped(
                            Vec::new(),
                        ),
                    ),
                ),
                SessionConfigKindInfo::Boolean(b) => SessionConfigKind::Boolean(
                    agent_client_protocol::schema::v1::SessionConfigBoolean::new(b.current_value),
                ),
            },
        );
        let extracted = values.get(&opt.id).expect("every option is extracted");
        assert!(
            config_option_already_holds(&schema, extracted),
            "option {} must read back as already holding {extracted}",
            opt.id
        );
    }

    assert!(current_config_option_values(&[]).is_empty());
}

/// codex-acp 1.11.0's `recommendedValue` has to survive the trip from the
/// wire to `SessionConfigOptionInfo`, and a malformed envelope has to be
/// dropped rather than half-read. The JSON below is the SHAPE captured off a
/// live 1.11.0 `session/new` with the capability advertised — verbatim
/// `_meta`, one option trimmed to two values.
#[test]
fn a_select_carries_the_agents_recommended_value_only_from_a_valid_air_envelope() {
    let option = |meta: Option<serde_json::Value>| -> SessionConfigOption {
        let mut raw = serde_json::json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": "gpt-5.5",
            "options": [
                {"value": "gpt-6-astra", "name": "6 Astra"},
                {"value": "gpt-5.5", "name": "5.5"},
            ],
        });
        if let Some(meta) = meta {
            raw["_meta"] = meta;
        }
        serde_json::from_value(raw).expect("config option fixture")
    };
    let recommended = |meta: Option<serde_json::Value>| -> Option<String> {
        map_session_config_option(&option(meta))
            .expect("a select always maps")
            .recommended_value
    };

    // The real envelope. Note it names a value OTHER than `currentValue` —
    // that is the whole point: "recommended" and "selected" are different
    // claims, and the hint must not be collapsed into the selection.
    assert_eq!(
        recommended(Some(serde_json::json!({
            "jetbrains": {"air": {"version": 1, "recommendedValue": "gpt-6-astra"}}
        }))),
        Some("gpt-6-astra".to_string())
    );

    // Everything malformed reads as "no recommendation", never as a
    // half-understood one.
    for bad in [
        // No `_meta` at all — every agent but codex 1.11.0+, and codex
        // itself without the advertisement.
        None,
        // Version missing / below the floor / not an integer: the same
        // gate `air_session_failure` applies, mirroring what the adapters
        // run on codeg's own advertisement.
        Some(serde_json::json!({
            "jetbrains": {"air": {"recommendedValue": "gpt-6-astra"}}
        })),
        Some(serde_json::json!({
            "jetbrains": {"air": {"version": 0, "recommendedValue": "gpt-6-astra"}}
        })),
        Some(serde_json::json!({
            "jetbrains": {"air": {"version": "1", "recommendedValue": "gpt-6-astra"}}
        })),
        // Right key, wrong envelope (no `air` wrapper).
        Some(serde_json::json!({
            "jetbrains": {"recommendedValue": "gpt-6-astra"}
        })),
        // Non-string and blank payloads: nothing a select's values match.
        Some(serde_json::json!({
            "jetbrains": {"air": {"version": 1, "recommendedValue": ["gpt-6-astra"]}}
        })),
        Some(serde_json::json!({
            "jetbrains": {"air": {"version": 1, "recommendedValue": "   "}}
        })),
    ] {
        assert_eq!(
            recommended(bad.clone()),
            None,
            "unexpected read from {bad:?}"
        );
    }

    // A toggle has no value list to recommend into, so the hint is dropped
    // even when the envelope is valid.
    let toggle: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "auto_approve",
        "name": "Auto-approve",
        "type": "boolean",
        "currentValue": true,
        "_meta": {"jetbrains": {"air": {"version": 1, "recommendedValue": "true"}}},
    }))
    .expect("boolean option fixture");
    assert_eq!(
        map_session_config_option(&toggle)
            .expect("a boolean always maps")
            .recommended_value,
        None
    );
}

#[test]
fn build_load_session_request_skips_meta_for_non_claude() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_load_session_request(
        AgentType::Codex,
        SessionId::new("abc".to_string()),
        &cwd,
        Vec::new(),
    );

    assert!(req.meta.is_none());
}

// OpenClaw rejects MCP server *entries* over the ACP wire, not the
// `mcpServers` field itself. The ACP schema does not `skip_serializing_if`
// that field on NewSessionRequest/LoadSessionRequest, so it always
// serializes as `[]`; every agent (OpenClaw included) already receives
// `mcpServers: []` on a fresh install with no servers configured and
// codeg-mcp off — the known-good payload. The connection-layer gate
// (`supports_mcp == false`) forces OpenClaw onto that empty payload
// unconditionally. This pins the wire contract: both builders emit an
// empty list, so no server entry can ever reach OpenClaw.
#[test]
fn openclaw_session_requests_carry_no_mcp_servers() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");

    let new_req = build_new_session_request(AgentType::OpenClaw, &cwd, Vec::new());
    assert!(
        new_req.mcp_servers.is_empty(),
        "OpenClaw session/new must carry no MCP servers"
    );
    let new_json = serde_json::to_value(&new_req).unwrap();
    assert_eq!(
        new_json.get("mcpServers"),
        Some(&serde_json::json!([])),
        "OpenClaw session/new mcpServers must serialize as an empty list"
    );

    let load_req = build_load_session_request(
        AgentType::OpenClaw,
        SessionId::new("openclaw-session".to_string()),
        &cwd,
        Vec::new(),
    );
    assert!(
        load_req.mcp_servers.is_empty(),
        "OpenClaw session/load must carry no MCP servers"
    );
    let load_json = serde_json::to_value(&load_req).unwrap();
    assert_eq!(
        load_json.get("mcpServers"),
        Some(&serde_json::json!([])),
        "OpenClaw session/load mcpServers must serialize as an empty list"
    );
}

fn stdio_server(name: &str) -> McpServer {
    McpServer::Stdio(McpServerStdio::new(
        name,
        std::path::PathBuf::from("/usr/local/bin/node"),
    ))
}

// The `supports_mcp` hint fires only where the declaration could actually
// be wrong: a custom agent that was handed server entries.
#[test]
fn mcp_suspect_tags_only_custom_agents_with_servers() {
    let servers = vec![stdio_server("codeg")];

    let tagged = tag_mcp_suspect(
        agent_client_protocol::util::internal_error(
            "session/new failed: unknown field `mcpServers`",
        ),
        AgentType::Custom("my-agent"),
        &servers,
    );
    assert!(
        tagged.to_string().contains(MCP_SUSPECT_SENTINEL),
        "a custom agent that received MCP servers must be tagged"
    );

    // Nothing was forwarded, so MCP cannot be the cause.
    let empty = tag_mcp_suspect(
        agent_client_protocol::util::internal_error("session/new failed: boom"),
        AgentType::Custom("my-agent"),
        &[],
    );
    assert!(
        !empty.to_string().contains(MCP_SUSPECT_SENTINEL),
        "an empty server list rules MCP out"
    );

    // A built-in's flag is a verified repository constant, and the user has
    // no switch to flip — blaming MCP would misdirect them.
    let builtin = tag_mcp_suspect(
        agent_client_protocol::util::internal_error("session/new failed: boom"),
        AgentType::ClaudeCode,
        &servers,
    );
    assert!(
        !builtin.to_string().contains(MCP_SUSPECT_SENTINEL),
        "built-in agents must never be tagged"
    );
}

// The sentinel is a transport detail: it must be consumed by the
// translation, never shown. This mirrors the `.map_err` in
// `run_connection`, which cannot be called directly from a unit test.
#[test]
fn mcp_suspect_sentinel_translates_to_code_and_is_stripped() {
    let raw = tag_mcp_suspect(
        agent_client_protocol::util::internal_error("Unsupported parameter: mcpServers"),
        AgentType::Custom("my-agent"),
        &[stdio_server("codeg")],
    )
    .to_string();

    let err = AcpError::mcp_rejected(raw.replace(MCP_SUSPECT_SENTINEL, ""));

    assert_eq!(err.code(), Some("mcp_rejected_by_agent"));
    let shown = err.to_string();
    assert!(
        shown.contains("Unsupported parameter: mcpServers"),
        "the agent's own message must survive: {shown}"
    );
    assert!(
        !shown.contains(MCP_SUSPECT_SENTINEL),
        "the sentinel must never reach the user: {shown}"
    );
}

// The reported cursor-agent failure: `session/new` answered -32000 with
// `Please run 'agent login' first` — advice the user cannot take, since
// `agent` is not a command and codeg's managed `cursor-agent` is not on
// PATH. The typed code is the only place that reading is available, so it
// has to be classified here and not from the wire text.
#[test]
fn auth_required_beats_the_mcp_hint_and_carries_its_own_code() {
    // The shape cursor-agent really answers with (wire capture from the
    // report): code -32000, with its advice in `data`.
    let refusal = agent_client_protocol::Error::auth_required().data(serde_json::json!({
        "message": "Authentication required. Please run 'agent login' first, \
                    then call authenticate() with methodId 'cursor_login'."
    }));
    // A custom agent WITH servers attached is exactly the case the MCP hint
    // fires on; the credential diagnosis has to win it.
    let tagged = tag_new_session_failure(
        refusal,
        AgentType::Custom("my-agent"),
        &[stdio_server("codeg")],
    )
    .to_string();
    assert!(tagged.contains(AUTH_REQUIRED_SENTINEL));
    assert!(
        !tagged.contains(MCP_SUSPECT_SENTINEL),
        "the weaker MCP reading must not ride along: {tagged}"
    );

    // Mirrors the `.map_err` in `run_connection`, which a unit test cannot
    // call directly.
    let err = AcpError::agent_auth_required(tagged.replace(AUTH_REQUIRED_SENTINEL, ""));
    assert_eq!(err.code(), Some("agent_auth_required"));
    assert!(
        !err.to_string().contains(AUTH_REQUIRED_SENTINEL),
        "the sentinel must never reach the user: {err}"
    );
}

// Every other refusal keeps the behaviour it had: the auth branch must not
// swallow unrelated `session/new` failures.
#[test]
fn a_non_auth_refusal_still_takes_the_mcp_path() {
    let tagged = tag_new_session_failure(
        agent_client_protocol::util::internal_error(
            "session/new failed: unknown field `mcpServers`",
        ),
        AgentType::Custom("my-agent"),
        &[stdio_server("codeg")],
    )
    .to_string();
    assert!(tagged.contains(MCP_SUSPECT_SENTINEL));
    assert!(!tagged.contains(AUTH_REQUIRED_SENTINEL));
}

#[test]
fn build_resume_session_request_sets_claude_raw_meta() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_resume_session_request(
        AgentType::ClaudeCode,
        SessionId::new("abc".to_string()),
        &cwd,
        Vec::new(),
    );

    assert_eq!(
        req.meta
            .as_ref()
            .and_then(|m| m.get("claudeCode"))
            .and_then(|v| v.get("emitRawSDKMessages"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn build_resume_session_request_skips_meta_for_non_claude() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_resume_session_request(
        AgentType::Codex,
        SessionId::new("abc".to_string()),
        &cwd,
        Vec::new(),
    );

    assert!(req.meta.is_none());
}

// Unlike NewSessionRequest/LoadSessionRequest (whose `mcp_servers` has no
// `skip_serializing_if`, so it always serializes as `[]`),
// ResumeSessionRequest marks `mcp_servers` `skip_serializing_if =
// Vec::is_empty` — an empty list is OMITTED from the wire entirely. OpenClaw
// (which supports session/resume) tolerates both an absent key and `[]`, and
// the connection-layer gate keeps the list empty regardless, so no server
// entry can ever reach it. Pin both the empty-list invariant and the
// documented wire-shape divergence here.
#[test]
fn openclaw_resume_request_carries_no_mcp_servers() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_resume_session_request(
        AgentType::OpenClaw,
        SessionId::new("openclaw-session".to_string()),
        &cwd,
        Vec::new(),
    );
    assert!(
        req.mcp_servers.is_empty(),
        "OpenClaw session/resume must carry no MCP servers"
    );

    let json = serde_json::to_value(&req).unwrap();
    assert!(
        json.get("mcpServers").is_none(),
        "empty mcp_servers must be omitted from the resume wire payload"
    );
    // camelCase round-trip: sanity that the UntypedMessage send produces the
    // ACP-correct shape.
    assert!(
        json.get("sessionId").is_some(),
        "sessionId must serialize in camelCase"
    );
    assert!(json.get("cwd").is_some());
}

#[test]
fn canonical_spec_to_mcp_server_stdio() {
    // Use an absolute path so the test is portable across machines that
    // may or may not have `npx` on PATH.
    let spec = serde_json::json!({
        "type": "stdio",
        "command": "/usr/local/bin/npx",
        "args": ["-y", "@mcp_hub_org/cli@latest", "run", "figma-developer-mcp"],
        "env": {"FIGMA_API_KEY": "secret"},
    });
    let server = canonical_spec_to_mcp_server("figma", &spec).expect("stdio spec should map");
    match server {
        McpServer::Stdio(s) => {
            assert_eq!(s.name, "figma");
            assert_eq!(s.command, std::path::PathBuf::from("/usr/local/bin/npx"));
            assert_eq!(s.args.len(), 4);
            assert_eq!(s.env.len(), 1);
            assert_eq!(s.env[0].name, "FIGMA_API_KEY");
        }
        other => panic!("expected Stdio variant, got {other:?}"),
    }
}

#[test]
fn canonical_spec_resolves_bare_command_to_absolute() {
    // Bare command names get resolved via PATH so the resulting payload
    // satisfies the ACP "command MUST be absolute" requirement. We use
    // `cargo` because the test process must have it on PATH.
    let spec = serde_json::json!({
        "type": "stdio",
        "command": "cargo",
    });
    let server = canonical_spec_to_mcp_server("x", &spec).expect("bare command should resolve");
    match server {
        McpServer::Stdio(s) => assert!(
            s.command.is_absolute(),
            "expected absolute path, got {}",
            s.command.display()
        ),
        other => panic!("expected Stdio variant, got {other:?}"),
    }
}

#[test]
fn grok_incompatible_agent_switch_detects_stable_code() {
    // Exact shape Grok returns when switching to a model whose agentType
    // differs from the established conversation's (captured from a live
    // `session/set_model` probe against grok 0.2.94).
    let err = agent_client_protocol::Error::new(-32600, "Cannot switch to model ...").data(
        serde_json::json!({
            "code": "MODEL_SWITCH_INCOMPATIBLE_AGENT",
            "activeAgentType": "grok-build-plan",
            "requiredAgentType": "cursor",
            "modelId": "grok-composer-2.5-fast",
            "suggestion": "start_new_session"
        }),
    );
    assert!(is_grok_incompatible_agent_switch(&err));

    // A different data.code, or no data at all, must NOT be swallowed —
    // those fall through to the generic error path.
    let other = agent_client_protocol::Error::new(-32603, "boom")
        .data(serde_json::json!({ "code": "SOMETHING_ELSE" }));
    assert!(!is_grok_incompatible_agent_switch(&other));
    assert!(!is_grok_incompatible_agent_switch(
        &agent_client_protocol::Error::internal_error()
    ));
}

#[test]
fn synthesize_grok_config_options_yields_model_and_effort_selectors() {
    // `_meta["x.ai/sessionConfig"].options` as delivered by `session/new`
    // (captured live): both model choices and the "mode" effort choices.
    let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": false},
                        {"id": "high", "category": "mode", "label": "High Effort", "selected": true},
                        {"id": "low", "category": "mode", "label": "Low Effort", "selected": false}
                    ]
                }
            }),
        )
        .unwrap();

    // Empty specs → the effort selector comes from the flat `x.ai/sessionConfig`
    // "mode" list (the no-`models` fallback path).
    let opts =
        synthesize_grok_config_options(Some(&meta), &HashMap::new()).expect("should synthesize");
    assert_eq!(opts.len(), 2, "model + effort selectors");

    let model = &opts[0];
    assert_eq!(model.id, GROK_MODEL_OPTION_ID);
    assert_eq!(model.category.as_deref(), Some("model"));
    let model_sel = expect_select(&model.kind);
    // Both models appear (agent-type filtering is deliberately NOT applied —
    // cross-type switches are handled gracefully at set time instead).
    assert_eq!(model_sel.options.len(), 2);
    assert_eq!(
        model_sel.current_value, "grok-4.5",
        "the `selected` model is current"
    );
    assert!(model_sel
        .options
        .iter()
        .any(|o| o.value == "grok-composer-2.5-fast"));

    let effort = &opts[1];
    assert_eq!(effort.id, GROK_EFFORT_OPTION_ID);
    assert_eq!(effort.category.as_deref(), Some("mode"));
    let effort_sel = expect_select(&effort.kind);
    assert_eq!(effort_sel.options.len(), 2);
    assert_eq!(
        effort_sel.current_value, "high",
        "the `selected` effort is current"
    );
    assert!(effort_sel.options.iter().any(|o| o.value == "low"));
}

#[test]
fn synthesize_grok_config_options_model_only_when_no_effort_offered() {
    // A model that doesn't advertise `supportsReasoningEffort` yields no
    // `category:"mode"` entries → only the model selector is surfaced.
    let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": true}
                    ]
                }
            }),
        )
        .unwrap();
    // Empty specs → the effort selector comes from the flat `x.ai/sessionConfig`
    // "mode" list (the no-`models` fallback path).
    let opts =
        synthesize_grok_config_options(Some(&meta), &HashMap::new()).expect("should synthesize");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].id, GROK_MODEL_OPTION_ID);
}

#[test]
fn grok_set_model_params_carry_effort_override() {
    // Pure model switch → no `_meta`, so grok keeps the current effort.
    let p = build_grok_set_model_params("s1", "grok-4.5", None);
    assert_eq!(p["sessionId"], "s1");
    assert_eq!(p["modelId"], "grok-4.5");
    assert!(p.get("_meta").is_none());
    // Effort override rides in `_meta.reasoningEffort` (the key grok parses).
    let p = build_grok_set_model_params("s1", "grok-4.5", Some("high"));
    assert_eq!(p["modelId"], "grok-4.5");
    assert_eq!(p["_meta"]["reasoningEffort"], "high");
}

// ── config-option verdicts ──────────────────────────────────────────────
//
// `session/set_config_option` is advisory: the agent answers with the option
// list it adopted, and codeg renders that verbatim — so a refused pick reads
// in the composer as the selector springing back for no reason. Only this
// side can tell a request's answer from an unsolicited update, so the
// comparison has to be exactly right here.

fn rejection_fixture(current: &str) -> Vec<SessionConfigOptionInfo> {
    vec![SessionConfigOptionInfo {
        id: "thought_level".to_string(),
        name: "Thinking".to_string(),
        description: None,
        category: Some("thought_level".to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value: current.to_string(),
            options: vec![
                SessionConfigSelectOptionInfo {
                    value: "off".to_string(),
                    name: "Thinking: off".to_string(),
                    description: None,
                },
                SessionConfigSelectOptionInfo {
                    value: "high".to_string(),
                    name: "Thinking: high".to_string(),
                    description: None,
                },
            ],
            groups: vec![],
        }),
        recommended_value: None,
    }]
}

#[test]
fn config_option_rejection_reports_a_clamped_pick_with_labels() {
    // pi clamps every level to `off` for a model that never declared
    // `reasoning` — the bug that made the picker look broken.
    let event = config_option_rejection(&rejection_fixture("off"), "thought_level", "high")
        .expect("a clamped pick is a rejection");
    match event {
        AcpEvent::ConfigOptionRejected {
            config_id,
            option_name,
            requested,
            actual,
            requested_value,
            actual_value,
        } => {
            assert_eq!(config_id, "thought_level");
            assert_eq!(option_name, "Thinking");
            // Labels, not ids: the dropdown showed these strings.
            assert_eq!(requested, "Thinking: high");
            assert_eq!(actual, "Thinking: off");
            // And the raw ids beside them, so a client that localises an
            // agent's own vocabulary has something stable to key on — the
            // labels above have already been resolved away from it.
            assert_eq!(requested_value, "high");
            assert_eq!(actual_value, "off");
        }
        other => panic!("expected ConfigOptionRejected, got {other:?}"),
    }
}

#[test]
fn config_option_rejection_is_silent_when_the_pick_landed() {
    assert!(config_option_rejection(&rejection_fixture("high"), "thought_level", "high").is_none());
}

#[test]
fn config_option_rejection_falls_back_to_the_raw_id_without_a_label() {
    // An agent may settle on a value it never advertised; naming the raw id
    // beats naming nothing.
    let event = config_option_rejection(&rejection_fixture("medium"), "thought_level", "high")
        .expect("still a rejection");
    match event {
        AcpEvent::ConfigOptionRejected { actual, .. } => assert_eq!(actual, "medium"),
        other => panic!("expected ConfigOptionRejected, got {other:?}"),
    }
}

#[test]
fn config_option_rejection_stays_silent_when_there_is_nothing_to_compare() {
    let options = rejection_fixture("off");
    // Option absent from the answer → nothing to compare against.
    assert!(config_option_rejection(&options, "model", "grok-4.5").is_none());
    // A kind with no comparable id → leave it to the agent. A false "your
    // pick was changed" notice is worse than none.
    let toggle = vec![SessionConfigOptionInfo {
        id: "auto_approve".to_string(),
        name: "Auto-approve tools".to_string(),
        description: None,
        category: None,
        kind: SessionConfigKindInfo::Boolean(SessionConfigBooleanInfo {
            current_value: false,
        }),
        recommended_value: None,
    }];
    assert!(config_option_rejection(&toggle, "auto_approve", "true").is_none());
}

#[test]
fn config_option_rejection_reads_a_grouped_select() {
    // Grouped options are flattened by `map_session_config_options` before the
    // comparison, so a grouped model picker resolves its labels too.
    let raw = serde_json::json!([{
        "id": "model",
        "name": "Model",
        "type": "select",
        "currentValue": "openai/gpt-5",
        "options": [{
            "group": "openai",
            "name": "OpenAI",
            "options": [
                {"value": "openai/gpt-5", "name": "GPT-5"},
                {"value": "openai/gpt-5-mini", "name": "GPT-5 Mini"}
            ]
        }]
    }]);
    let parsed: Vec<SessionConfigOption> = serde_json::from_value(raw).expect("parses");
    let mapped = map_session_config_options(&parsed);

    let event = config_option_rejection(&mapped, "model", "openai/gpt-5-mini")
        .expect("the agent kept a different model");
    match event {
        AcpEvent::ConfigOptionRejected {
            requested, actual, ..
        } => {
            assert_eq!(requested, "GPT-5 Mini");
            assert_eq!(actual, "GPT-5");
        }
        other => panic!("expected ConfigOptionRejected, got {other:?}"),
    }
}

/// The claude model selector, as a live adapter advertises it with and
/// without codeg's AIR `recommendedValue` opt-in: 0.76.0+ removes the
/// `default` row for a client that asks, every older build keeps it.
fn claude_model_option(offers_default: bool) -> SessionConfigOption {
    let mut options = vec![serde_json::json!({"value": "opus[1m]", "name": "Opus 5"})];
    if offers_default {
        options.insert(
            0,
            serde_json::json!({"value": "default", "name": "Default (recommended)"}),
        );
    }
    serde_json::from_value(serde_json::json!({
        "id": "model",
        "name": "Model",
        "type": "select",
        "currentValue": "opus[1m]",
        "options": options,
    }))
    .expect("parses")
}

#[test]
fn config_option_rejects_a_value_the_select_no_longer_offers() {
    // The `default` pick a user saved before codeg advertised
    // `recommendedValue`. Replaying it can only error, and it would do so
    // on every connect for good: the row that would let the user overwrite
    // the preference is exactly the one that went away.
    let retired = claude_model_option(false);
    assert!(config_option_rejects_value(&retired, "default"));
    assert!(!config_option_rejects_value(&retired, "opus[1m]"));
}

#[test]
fn config_option_keeps_a_value_an_older_adapter_still_offers() {
    // The registry pin only governs what codeg INSTALLS —
    // `resolve_npx_command` launches whatever `claude-agent-acp` is on
    // PATH. So the same built-in agent id may be speaking to a pre-0.76
    // adapter, where `default` is still a real row and dropping the
    // preference would silently change the user's model.
    assert!(!config_option_rejects_value(
        &claude_model_option(true),
        "default"
    ));
}

#[test]
fn config_option_rejects_nothing_without_a_value_list_to_judge_by() {
    // An agent that announces an empty `SessionConfigOptions` first and
    // pushes the real one later (see `AcpManager::probe_agent_options`)
    // has proven nothing, and a boolean takes both values by construction.
    // Both must fall through to "send it and let the agent decide".
    let empty: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "model",
        "name": "Model",
        "type": "select",
        "currentValue": "",
        "options": [],
    }))
    .expect("parses");
    assert!(!config_option_rejects_value(&empty, "opus[1m]"));

    let toggle: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "auto_approve",
        "name": "Auto-approve tools",
        "type": "boolean",
        "currentValue": false,
    }))
    .expect("parses");
    assert!(!config_option_rejects_value(&toggle, "true"));
}

/// The `availableModes` claude-agent-acp 0.81.1 answered `session/new`
/// with, live, from a cwd whose `.claude/settings.json` sets
/// `permissions.disableBypassPermissionsMode: "disable"` — and, with
/// `bypass`, from the control run without it.
fn claude_modes(bypass: bool) -> SessionModeState {
    let mut modes = vec![
        serde_json::json!({"id": "default", "name": "Manual"}),
        serde_json::json!({"id": "acceptEdits", "name": "Accept edits"}),
        serde_json::json!({"id": "plan", "name": "Plan"}),
        serde_json::json!({"id": "auto", "name": "Auto"}),
    ];
    if bypass {
        modes.push(serde_json::json!({"id": "bypassPermissions", "name": "Bypass permissions"}));
    }
    serde_json::from_value(serde_json::json!({
        "currentModeId": "default",
        "availableModes": modes,
    }))
    .expect("parses")
}

#[test]
fn a_saved_mode_the_session_withdrew_is_not_replayed() {
    assert!(session_modes_reject_id(
        &claude_modes(false),
        "bypassPermissions"
    ));
    // Every mode still on offer replays as before — including on an
    // adapter that never withdrew bypass.
    for mode in ["default", "acceptEdits", "plan", "auto"] {
        assert!(
            !session_modes_reject_id(&claude_modes(false), mode),
            "{mode}"
        );
    }
    assert!(!session_modes_reject_id(
        &claude_modes(true),
        "bypassPermissions"
    ));
}

#[test]
fn an_empty_mode_list_rejects_nothing() {
    let empty: SessionModeState = serde_json::from_value(serde_json::json!({
        "currentModeId": "",
        "availableModes": [],
    }))
    .expect("parses");
    assert!(!session_modes_reject_id(&empty, "bypassPermissions"));
}

#[test]
fn config_option_rejects_value_reads_a_grouped_select() {
    // Groups are one flat value namespace, the same way
    // `config_option_rejection` reads them.
    let grouped: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "model",
        "name": "Model",
        "type": "select",
        "currentValue": "openai/gpt-5",
        "options": [{
            "group": "openai",
            "name": "OpenAI",
            "options": [{"value": "openai/gpt-5", "name": "GPT-5"}]
        }],
    }))
    .expect("parses");
    assert!(!config_option_rejects_value(&grouped, "openai/gpt-5"));
    assert!(config_option_rejects_value(&grouped, "openai/gpt-5-mini"));
}

/// The model selector claude-agent-acp 0.84.0 answers `session/new` with
/// once codeg advertises `recommendedValue` (no `default` row), in either
/// Opus spelling its CLI builds: `opus[1m]` (measured live on a gateway
/// account) or plain `opus` (the adapter's own live test, 0.83.0).
fn claude_model_selector(opus_row: &str) -> SessionConfigOption {
    let rows: Vec<serde_json::Value> = [
        opus_row,
        "claude-fable-5-1[1m]",
        "sonnet",
        "sonnet[1m]",
        "haiku",
    ]
    .iter()
    .map(|value| serde_json::json!({"value": value, "name": value}))
    .collect();
    serde_json::from_value(serde_json::json!({
        "type": "select",
        "id": "model",
        "name": "Model",
        "category": "model",
        "currentValue": opus_row,
        "options": rows,
    }))
    .expect("parses")
}

#[test]
fn a_retired_context_lane_pick_replays_as_its_plain_twin() {
    let renamed = claude_model_selector("opus");
    assert_eq!(
        heal_retired_context_lane_pick(&renamed, "opus[1m]").as_deref(),
        Some("opus")
    );
    // The CLI drops the suffix case-insensitively when it compares aliases.
    assert_eq!(
        heal_retired_context_lane_pick(&renamed, "opus[1M]").as_deref(),
        Some("opus")
    );
}

/// The model selector claude-agent-acp 0.85.0 answers `session/new` with
/// through an `ANTHROPIC_BASE_URL` gateway, verbatim (measured live). Claude
/// Code 2.1.285 gives a gateway session the 1M window of the models that
/// have one, so BOTH `[1m]` rows the same gateway listed on 0.84.0 are gone
/// and the recommendation moved from `opus[1m]` to plain `opus`.
#[test]
fn a_gateway_on_claude_code_2_1_285_heals_both_retired_context_lanes() {
    let gateway: SessionConfigOption = serde_json::from_value(serde_json::json!({
            "id": "model",
            "name": "Model",
            "description": "AI model to use",
            "category": "model",
            "type": "select",
            "currentValue": "opus",
            "_meta": {"jetbrains": {"air": {"version": 1, "recommendedValue": "opus"}}},
            "options": [
                {"value": "opus", "name": "Opus 5.5", "description": "Opus 5.5 · Best for everyday, complex tasks · $4/$20 per Mtok"},
                {"value": "claude-fable-5-1", "name": "Fable 5.1", "description": "Fable 5.1 · Most capable for your hardest and longest-running tasks · $10/$50 per Mtok"},
                {"value": "sonnet", "name": "Sonnet 5.5", "description": "Sonnet 5.5 · Efficient for routine tasks · $2/$10 per Mtok"},
                {"value": "haiku", "name": "Haiku 4.5", "description": "Haiku 4.5 · Fastest for quick answers · $1/$5 per Mtok"},
            ],
        }))
        .expect("parses");
    assert_eq!(
        heal_retired_context_lane_pick(&gateway, "opus[1m]").as_deref(),
        Some("opus")
    );
    assert_eq!(
        heal_retired_context_lane_pick(&gateway, "sonnet[1m]").as_deref(),
        Some("sonnet")
    );
    assert_eq!(heal_retired_context_lane_pick(&gateway, "sonnet"), None);
}

/// The model selector claude-agent-acp 0.86.0 answers `session/new` with
/// through the same kind of `ANTHROPIC_BASE_URL` gateway, verbatim (measured
/// live). Claude Code 2.1.287 spells the Fable row `fable`, where 2.1.286
/// listed `claude-fable-5-1` under the very same name and description.
fn gateway_model_selector_2_1_287() -> SessionConfigOption {
    serde_json::from_value(serde_json::json!({
            "id": "model",
            "name": "Model",
            "description": "AI model to use",
            "category": "model",
            "type": "select",
            "currentValue": "opus",
            "_meta": {"jetbrains": {"air": {"version": 1, "recommendedValue": "opus"}}},
            "options": [
                {"value": "opus", "name": "Opus 5.5", "description": "Opus 5.5 · Best for everyday, complex tasks · $4/$20 per Mtok"},
                {"value": "fable", "name": "Fable 5.1", "description": "Fable 5.1 · Most capable for your hardest and longest-running tasks · $10/$50 per Mtok"},
                {"value": "sonnet", "name": "Sonnet 5.5", "description": "Sonnet 5.5 · Efficient for routine tasks · $2/$10 per Mtok"},
                {"value": "haiku", "name": "Haiku 4.5", "description": "Haiku 4.5 · Fastest for quick answers · $1/$5 per Mtok"},
            ],
        }))
        .expect("parses")
}

#[test]
fn a_gateway_on_claude_code_2_1_287_heals_a_respelled_fable_pick() {
    let gateway = gateway_model_selector_2_1_287();
    assert_eq!(
        heal_retired_model_pick(&gateway, "claude-fable-5-1").as_deref(),
        Some("fable")
    );
    // The pick the 0.84.0 gateway saved lost its lane on 2.1.285 and its
    // plain row was re-spelled on 2.1.287: both renames, one heal.
    assert_eq!(
        heal_retired_model_pick(&gateway, "claude-fable-5-1[1m]").as_deref(),
        Some("fable")
    );
    // The `[1m]` rename still heals on its own.
    assert_eq!(
        heal_retired_model_pick(&gateway, "opus[1m]").as_deref(),
        Some("opus")
    );
    // Picks the selector lists are left alone.
    for listed in ["fable", "opus", "sonnet", "haiku"] {
        assert_eq!(heal_retired_model_pick(&gateway, listed), None, "{listed}");
    }
    // …even when another row also names the model: a listed pick stays.
    let both: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "type": "select",
        "id": "model",
        "name": "Model",
        "category": "model",
        "currentValue": "opus",
        "options": [
            {"value": "claude-fable-5-1", "name": "Fable"},
            {"value": "fable", "name": "Fable 5.1"},
        ],
    }))
    .expect("parses");
    assert_eq!(heal_retired_model_pick(&both, "claude-fable-5-1"), None);
}

#[test]
fn a_respelled_pick_heals_only_onto_the_one_row_that_names_its_model() {
    let selector = |rows: serde_json::Value| -> SessionConfigOption {
        serde_json::from_value(serde_json::json!({
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": "opus",
            "options": rows,
        }))
        .expect("parses")
    };
    // The family row moved on to a newer Fable: another model, so the pick
    // is left for the screen to skip.
    let newer = selector(serde_json::json!([
        {"value": "opus", "name": "Opus 5.5"},
        {"value": "fable", "name": "Fable 5.2"},
    ]));
    assert_eq!(heal_respelled_model_pick(&newer, "claude-fable-5-1"), None);
    // Two rows with the label: no single row to pick.
    let ambiguous = selector(serde_json::json!([
        {"value": "fable", "name": "Fable 5.1"},
        {"value": "gw/fable", "name": "Fable 5.1"},
    ]));
    assert_eq!(
        heal_respelled_model_pick(&ambiguous, "claude-fable-5-1"),
        None
    );
    // A model pinned through `ANTHROPIC_DEFAULT_FABLE_MODEL`: the CLI labels
    // the row with the pinned id itself.
    let pinned = selector(serde_json::json!([
        {"value": "opus", "name": "Opus 5.5"},
        {"value": "fable", "name": "claude-fable-5-1", "description": "Custom Fable model"},
    ]));
    assert_eq!(
        heal_respelled_model_pick(&pinned, "claude-fable-5-1").as_deref(),
        Some("fable")
    );
    // The CLI's built-in Fable row: labelled `Fable`, the catalog name
    // leading its description (the 2.1.287 builder, read from the binary).
    let built_in = selector(serde_json::json!([
        {"value": "opus", "name": "Opus", "description": "Opus 5.5 · Best for everyday, complex tasks"},
        {"value": "fable", "name": "Fable", "description": "Fable 5.1 · Most capable for your hardest and longest-running tasks"},
    ]));
    assert_eq!(
        heal_respelled_model_pick(&built_in, "claude-fable-5-1").as_deref(),
        Some("fable")
    );
    // The name has to be the description's whole first segment: neither a
    // longer version nor a sentence that merely starts with it.
    for description in ["Fable 5.10 · Most capable", "Fable 5.1 is retired"] {
        let lookalike = selector(serde_json::json!([
            {"value": "opus", "name": "Opus 5.5"},
            {"value": "fable", "name": "Fable", "description": description},
        ]));
        assert_eq!(
            heal_respelled_model_pick(&lookalike, "claude-fable-5-1"),
            None,
            "{description}"
        );
    }
    // Two rows naming the model, one by label and one by description (a
    // `default` row on a client without `recommendedValue`): no single row.
    let two_ways = selector(serde_json::json!([
        {"value": "default", "name": "Default (recommended)", "description": "Opus 5.5 · Best for everyday, complex tasks"},
        {"value": "opus", "name": "Opus 5.5", "description": "Best for everyday, complex tasks"},
    ]));
    assert_eq!(
        heal_respelled_model_pick(&two_ways, "claude-opus-5-5"),
        None
    );
    // An id outside the catalog form has no catalog label to look for.
    assert_eq!(
        heal_respelled_model_pick(&gateway_model_selector_2_1_287(), "gw/fable-5-1"),
        None
    );
    // Only the model selector heals.
    let effort: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "type": "select",
        "id": "effort",
        "name": "Effort",
        "category": "thought_level",
        "currentValue": "high",
        "options": [{"value": "high", "name": "claude-fable-5-1"}],
    }))
    .expect("parses");
    assert_eq!(heal_respelled_model_pick(&effort, "claude-fable-5-1"), None);
    // An empty list proves nothing, exactly as for `config_option_rejects_value`.
    assert_eq!(
        heal_respelled_model_pick(&selector(serde_json::json!([])), "claude-fable-5-1"),
        None
    );
}

#[test]
fn claude_catalog_label_reads_only_the_catalog_id_form() {
    for (id, label) in [
        ("claude-fable-5-1", "Fable 5.1"),
        ("claude-opus-5-5", "Opus 5.5"),
        ("claude-sonnet-5", "Sonnet 5"),
        ("claude-haiku-4-5-20251001", "Haiku 4.5"),
    ] {
        assert_eq!(claude_catalog_label(id).as_deref(), Some(label), "{id}");
    }
    for id in [
        "fable",
        "opus[1m]",
        "claude-fable-5-1[1m]",
        "claude-fable",
        "claude-",
        "claude-3-7-sonnet-20250219",
        // Eight characters, but not a snapshot date.
        "claude-fable-5-thinking",
        "claude-fable-5-100",
        "gw/claude-fable-5-1",
    ] {
        assert_eq!(claude_catalog_label(id), None, "{id}");
    }
}

#[test]
fn healing_a_preference_set_follows_the_fable_row_to_its_alias() {
    let options = vec![gateway_model_selector_2_1_287()];
    let preferred = BTreeMap::from([("model".to_string(), "claude-fable-5-1".to_string())]);
    assert_eq!(
        heal_retired_model_picks(AgentType::ClaudeCode, &options, &preferred),
        BTreeMap::from([("model".to_string(), "fable".to_string())])
    );
    // An older CLI that still lists the concrete row keeps the pick.
    let older = vec![
        serde_json::from_value::<SessionConfigOption>(serde_json::json!({
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": "opus",
            "options": [
                {"value": "opus", "name": "Opus 5.5"},
                {"value": "claude-fable-5-1", "name": "Fable 5.1"},
            ],
        }))
        .expect("parses"),
    ];
    assert_eq!(
        heal_retired_model_picks(AgentType::ClaudeCode, &older, &preferred),
        preferred
    );
    assert_eq!(
        heal_retired_model_picks(AgentType::Codex, &options, &preferred),
        preferred
    );
}

#[test]
fn a_context_lane_pick_the_agent_still_lists_is_left_alone() {
    assert_eq!(
        heal_retired_context_lane_pick(&claude_model_selector("opus[1m]"), "opus[1m]"),
        None
    );
    // `sonnet[1m]` is still a row of its own next to `sonnet`.
    assert_eq!(
        heal_retired_context_lane_pick(&claude_model_selector("opus"), "sonnet[1m]"),
        None
    );
}

#[test]
fn a_context_lane_pick_heals_only_onto_a_listed_twin() {
    let renamed = claude_model_selector("opus");
    // Retired, but its plain twin is not listed either: left for the
    // screen to skip, as before.
    assert_eq!(
        heal_retired_context_lane_pick(&renamed, "claude-opus-5[1m]"),
        None
    );
    // A bare suffix leaves no twin at all.
    assert_eq!(heal_retired_context_lane_pick(&renamed, "[1m]"), None);
    // Never the other way round: a plain pick is not moved onto a
    // separately priced `[1m]` lane.
    assert_eq!(
        heal_retired_context_lane_pick(&claude_model_selector("opus[1m]"), "opus"),
        None
    );
}

#[test]
fn only_a_listed_model_selector_heals_a_context_lane_pick() {
    let effort: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "type": "select",
        "id": "effort",
        "name": "Effort",
        "category": "thought_level",
        "currentValue": "high",
        "options": [{"value": "high", "name": "High"}],
    }))
    .expect("parses");
    assert_eq!(heal_retired_context_lane_pick(&effort, "high[1m]"), None);
    // An empty list proves nothing, exactly as for `config_option_rejects_value`.
    let unlisted: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "type": "select",
        "id": "model",
        "name": "Model",
        "category": "model",
        "currentValue": "opus",
        "options": [],
    }))
    .expect("parses");
    assert_eq!(heal_retired_context_lane_pick(&unlisted, "opus[1m]"), None);
}

#[test]
fn healing_a_preference_set_rewrites_only_the_retired_model_pick() {
    let options = vec![
        claude_model_selector("opus"),
        serde_json::from_value(serde_json::json!({
            "type": "select",
            "id": "effort",
            "name": "Effort",
            "category": "thought_level",
            "currentValue": "high",
            "options": [{"value": "high", "name": "High"}, {"value": "xhigh", "name": "xHigh"}],
        }))
        .expect("parses"),
    ];
    let preferred = BTreeMap::from([
        ("effort".to_string(), "xhigh".to_string()),
        ("model".to_string(), "opus[1m]".to_string()),
        // An id the agent never advertised passes through untouched, for
        // the agent to judge as before.
        ("unlisted".to_string(), "x[1m]".to_string()),
    ]);
    assert_eq!(
        heal_retired_model_picks(AgentType::ClaudeCode, &options, &preferred),
        BTreeMap::from([
            ("effort".to_string(), "xhigh".to_string()),
            ("model".to_string(), "opus".to_string()),
            ("unlisted".to_string(), "x[1m]".to_string()),
        ])
    );
    // On an account that still lists the pick, the set is unchanged.
    let still_listed = vec![claude_model_selector("opus[1m]")];
    assert_eq!(
        heal_retired_model_picks(AgentType::ClaudeCode, &still_listed, &preferred),
        preferred
    );
    // `[1m]` is claude's spelling: no other agent's pick is rewritten, even
    // against the very list that heals claude's.
    for agent in [
        AgentType::Codex,
        AgentType::OpenCode,
        AgentType::Custom("acme"),
    ] {
        assert_eq!(
            heal_retired_model_picks(agent, &options, &preferred),
            preferred,
            "{agent:?}"
        );
    }
}

#[test]
fn synthesize_grok_config_options_none_without_sessionconfig() {
    let empty: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    assert!(synthesize_grok_config_options(Some(&empty), &HashMap::new()).is_none());
    assert!(synthesize_grok_config_options(None, &HashMap::new()).is_none());
}

/// Raw top-level `models` mirroring grok 0.2.99's `session/new`: grok-4.5
/// supports effort (default `xhigh`, switchable high/medium/low),
/// grok-composer-2.5-fast supports none.
fn grok_models_fixture() -> serde_json::Value {
    serde_json::json!({
        "currentModelId": "grok-4.5",
        "availableModels": [
            {
                "modelId": "grok-4.5",
                "name": "Grok 4.5",
                "_meta": {
                    "totalContextTokens": 500000,
                    "supportsReasoningEffort": true,
                    "reasoningEffort": "xhigh",
                    "reasoningEfforts": [
                        {"id": "high", "label": "High Effort", "description": "Highest quality", "default": true},
                        {"id": "medium", "label": "Medium Effort", "description": "Balanced"},
                        {"id": "low", "label": "Low Effort", "description": "Fast"}
                    ]
                }
            },
            {
                "modelId": "grok-composer-2.5-fast",
                "name": "Composer 2.5",
                "_meta": {"supportsReasoningEffort": false}
            }
        ]
    })
}

#[test]
fn parse_grok_model_specs_reads_per_model_meta() {
    let specs = parse_grok_model_specs(Some(&grok_models_fixture()));
    let g45 = specs.get("grok-4.5").expect("grok-4.5 present");
    assert!(g45.supports);
    assert_eq!(g45.default.as_deref(), Some("xhigh"));
    assert_eq!(g45.options.len(), 3);
    assert_eq!(g45.options[0].0, "high");
    // Grok's own window rides the same per-model `_meta` — the number the
    // context ring uses instead of inferring one from the model id.
    assert_eq!(g45.context_window, Some(500_000));
    let fast = specs
        .get("grok-composer-2.5-fast")
        .expect("composer present");
    assert!(!fast.supports);
    assert!(fast.default.is_none());
    assert!(fast.options.is_empty());
    // No `totalContextTokens` → no opinion (the id heuristic decides).
    assert_eq!(fast.context_window, None);
    // …and a zero window is "no opinion" too, never a ring divided by zero.
    let zeroed = parse_grok_model_specs(Some(&serde_json::json!({
        "availableModels": [{"modelId": "z", "_meta": {"totalContextTokens": 0}}]
    })));
    assert_eq!(zeroed["z"].context_window, None);
}

#[test]
fn parse_grok_model_specs_absent_models_is_empty() {
    assert!(parse_grok_model_specs(None).is_empty());
    assert!(parse_grok_model_specs(Some(&serde_json::json!({}))).is_empty());
    // Missing `_meta` degrades to supports=false / default=None / options=[].
    let bare = serde_json::json!({ "availableModels": [{"modelId": "m1", "name": "M1"}] });
    let specs = parse_grok_model_specs(Some(&bare));
    let m1 = specs.get("m1").expect("m1 present");
    assert!(!m1.supports);
    assert!(m1.default.is_none());
    assert!(m1.options.is_empty());
}

#[test]
fn build_grok_effort_option_injects_default_and_gates_supports() {
    let specs = parse_grok_model_specs(Some(&grok_models_fixture()));
    // grok-4.5: `xhigh` default is injected at the FRONT (not in the
    // switchable list), current = xhigh, with canonical labels.
    let effort = build_grok_effort_option("grok-4.5", &specs).expect("has effort");
    assert_eq!(effort.id, GROK_EFFORT_OPTION_ID);
    let sel = expect_select(&effort.kind);
    assert_eq!(sel.current_value, "xhigh");
    assert_eq!(sel.options.len(), 4, "high/medium/low + injected xhigh");
    assert_eq!(sel.options[0].value, "xhigh");
    assert_eq!(sel.options[0].name, "Max");
    // The injected default has no grok description, so it gets our canonical
    // one — every tier must have sub-text, not just high/medium/low.
    assert_eq!(
        sel.options[0].description.as_deref(),
        Some("Maximum reasoning for the most complex tasks")
    );
    assert!(sel.options.iter().all(|o| o.description.is_some()));
    // Grok's own per-tier text is preserved for the switchable tiers.
    assert!(sel.options.iter().any(|o| o.value == "high"
        && o.name == "High"
        && o.description.as_deref() == Some("Highest quality")));
    // Unsupported model → no selector; unknown model → None.
    assert!(build_grok_effort_option("grok-composer-2.5-fast", &specs).is_none());
    assert!(build_grok_effort_option("nope", &specs).is_none());
}

#[test]
fn synthesize_grok_config_options_model_reactive_effort_for_4_5() {
    // Flat sessionConfig marks grok-4.5 current; per-model specs drive effort.
    let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": false}
                    ]
                }
            }),
        )
        .unwrap();
    let specs = parse_grok_model_specs(Some(&grok_models_fixture()));
    let opts = synthesize_grok_config_options(Some(&meta), &specs).expect("synthesize");
    assert_eq!(opts.len(), 2, "model + effort");
    let effort = opts
        .iter()
        .find(|o| o.id == GROK_EFFORT_OPTION_ID)
        .expect("effort selector");
    let sel = expect_select(&effort.kind);
    assert_eq!(sel.current_value, "xhigh", "grok-4.5's real default");
    assert!(sel
        .options
        .iter()
        .any(|o| o.value == "xhigh" && o.name == "Max"));
}

#[test]
fn synthesize_grok_config_options_no_effort_for_composer_fast() {
    // Current model is the no-effort composer model → only the model selector.
    let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": false},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": true}
                    ]
                }
            }),
        )
        .unwrap();
    let specs = parse_grok_model_specs(Some(&grok_models_fixture()));
    let opts = synthesize_grok_config_options(Some(&meta), &specs).expect("synthesize");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].id, GROK_MODEL_OPTION_ID);
}

#[test]
fn set_grok_effort_selector_for_model_drops_and_adds() {
    let specs = parse_grok_model_specs(Some(&grok_models_fixture()));
    // Model + grok-4.5 effort → switching to the no-effort model DROPS effort.
    let mut opts = grok_model_options("grok-4.5");
    opts.push(build_grok_effort_option("grok-4.5", &specs).unwrap());
    assert_eq!(opts.len(), 2);
    set_grok_effort_selector_for_model(&mut opts, "grok-composer-2.5-fast", &specs);
    assert_eq!(opts.len(), 1);
    assert!(opts.iter().all(|o| o.id != GROK_EFFORT_OPTION_ID));
    // Switching back to grok-4.5 RE-ADDS it, current = xhigh.
    set_grok_effort_selector_for_model(&mut opts, "grok-4.5", &specs);
    let effort = opts
        .iter()
        .find(|o| o.id == GROK_EFFORT_OPTION_ID)
        .expect("re-added");
    let sel = expect_select(&effort.kind);
    assert_eq!(sel.current_value, "xhigh");
}

fn grok_model_options(current: &str) -> Vec<SessionConfigOptionInfo> {
    vec![SessionConfigOptionInfo {
        id: GROK_MODEL_OPTION_ID.to_string(),
        name: "Model".to_string(),
        description: None,
        category: Some("model".to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value: current.to_string(),
            options: vec![
                SessionConfigSelectOptionInfo {
                    value: "grok-4.5".to_string(),
                    name: "Grok 4.5".to_string(),
                    description: None,
                },
                SessionConfigSelectOptionInfo {
                    value: "grok-composer-2.5-fast".to_string(),
                    name: "Composer 2.5".to_string(),
                    description: None,
                },
            ],
            groups: Vec::new(),
        }),
        recommended_value: None,
    }]
}

#[tokio::test]
async fn grok_incompatible_agent_switch_reverts_and_reports_without_deadlock() {
    use std::time::Duration;

    let mut st = SessionState::new(
        "conn-test".to_string(),
        AgentType::Grok,
        None,
        "win".to_string(),
        None,
    );
    // The conversation is on grok-4.5; the user optimistically picked the
    // cross-agent-type Composer model, which Grok rejected mid-conversation.
    st.config_options = Some(grok_model_options("grok-4.5"));
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;

    // Regression guard: the recovery previously read `config_options` inline
    // in an `if let`, holding the read guard across `emit_*` (which take the
    // write lock) → deadlock. A timeout turns that hang into a failure.
    tokio::time::timeout(
        Duration::from_secs(5),
        emit_grok_incompatible_agent_switch(&state, &emitter),
    )
    .await
    .expect("recovery must complete, not deadlock on the state lock");

    let guard = state.read().await;

    // The optimistic pick is reverted: the authoritative model is unchanged.
    let opts = guard.config_options.as_ref().expect("options preserved");
    let sel = expect_select(&opts[0].kind);
    assert_eq!(sel.current_value, "grok-4.5");

    // Event ordering: the authoritative options (revert) precede the coded
    // error so the composer snaps back before the toast appears.
    let events = guard.recent_events_after(0).expect("events recorded");
    let cfg_idx = events
        .iter()
        .position(|e| matches!(&e.payload, AcpEvent::SessionConfigOptions { .. }))
        .expect("a session_config_options revert is emitted");
    let err_idx = events
        .iter()
        .position(|e| matches!(&e.payload, AcpEvent::Error { .. }))
        .expect("a coded error is emitted");
    assert!(cfg_idx < err_idx, "revert must precede the error");

    // The reverted options carry the original model.
    if let AcpEvent::SessionConfigOptions { config_options } = &events[cfg_idx].payload {
        let sel = expect_select(&config_options[0].kind);
        assert_eq!(sel.current_value, "grok-4.5");
    }

    // Exactly one error, carrying the localizable code (not a raw message)
    // and recoverable — no generic double-emit.
    let errors: Vec<(Option<String>, bool)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            AcpEvent::Error { code, terminal, .. } => Some((code.clone(), *terminal)),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "no double error emit");
    assert_eq!(
        errors[0].0.as_deref(),
        Some(GROK_INCOMPATIBLE_AGENT_ERROR_CODE)
    );
    assert!(!errors[0].1, "recoverable, not terminal");
}

#[test]
fn grok_live_tool_output_prefers_content() {
    // The clean content channel carries the output → don't ship raw_output
    // at all (frontend renders `content`, matching the parser's precedence).
    let content = Some("build ok\n".to_string());
    let raw = Some(serde_json::json!({
        "output_for_prompt": "exit: 0\n\nbuild ok",
        "exit_code": 0,
        "command": "pnpm build",
    }));
    assert_eq!(grok_live_tool_output(&content, &raw), None);
}

#[test]
fn grok_live_tool_output_falls_back_to_output_for_prompt_when_content_empty() {
    // With no content, recover the readable text from the string
    // `output_for_prompt` (NOT the byte-array `output`, NOT the whole blob).
    let raw = Some(serde_json::json!({
        "output": [10, 62, 32],
        "output_for_prompt": "exit: 0\n\nok",
        "exit_code": 0,
        "command": "pnpm build",
    }));
    assert_eq!(
        grok_live_tool_output(&None, &raw).as_deref(),
        Some("exit: 0\n\nok")
    );
    // Whitespace-only content is treated as empty.
    let ws = Some("  \n".to_string());
    assert_eq!(
        grok_live_tool_output(&ws, &raw).as_deref(),
        Some("exit: 0\n\nok")
    );
}

/// A `get_command_or_subagent_output` poll has no `content[]` and no
/// `output_for_prompt` — its whole result sits under the `TaskOutput`
/// envelope, which used to be dropped, streaming an empty card. Live must
/// emit the SAME string the history parser stores so the background-task
/// card renders identically before and after a reload.
#[test]
fn grok_live_tool_output_emits_task_output_envelope() {
    let raw = serde_json::json!({
        "type": "TaskOutput",
        "Result": {
            "task_id": "term_b0d",
            "command": "/bin/bash -lc 'pnpm dev'",
            "status": "failed",
            "exit_code": 1,
            "output": "boom",
        },
    });
    let live = grok_live_tool_output(&None, &Some(raw.clone())).expect("envelope emitted");
    assert_eq!(
        live,
        crate::parsers::grok::grok_task_output_envelope(&raw).unwrap(),
        "live and history must hand the frontend the same string"
    );
    let parsed: serde_json::Value = serde_json::from_str(&live).unwrap();
    assert_eq!(parsed["Result"]["exit_code"], 1);
    // A poll that DOES carry clean content keeps content's precedence.
    assert_eq!(
        grok_live_tool_output(&Some("已完成".to_string()), &Some(raw)),
        None
    );
}

#[test]
fn grok_live_tool_output_none_without_usable_string() {
    // Object without `output_for_prompt` (only the byte-array `output`).
    let no_prompt = Some(serde_json::json!({
        "output": [10, 62],
        "exit_code": 0,
        "command": "x",
    }));
    assert_eq!(grok_live_tool_output(&None, &no_prompt), None);
    // Non-object rawOutput.
    assert_eq!(
        grok_live_tool_output(&None, &Some(serde_json::json!("a string"))),
        None
    );
    // Absent rawOutput.
    assert_eq!(grok_live_tool_output(&None, &None), None);
}

/// The captured opencode 1.18.23 completion envelope for a codeg-mcp
/// `ask_user_question` — the clean answer text on both channels, the
/// `rawOutput` one wrapped in `{output, metadata}`.
fn opencode_ask_raw_output() -> serde_json::Value {
    serde_json::json!({
        "output": "The user answered your question(s):\n1. [框架] 选一个前端框架\n   → 选项 A\n",
        "metadata": {"truncated": false},
    })
}

/// The envelope must never shadow `content`: it wraps the very same string,
/// and the JSON blob is what made an answered question render "no selection".
#[test]
fn opencode_live_tool_output_prefers_content() {
    let content = Some(
        "The user answered your question(s):\n1. [框架] 选一个前端框架\n   → 选项 A\n".to_string(),
    );
    assert_eq!(
        opencode_live_tool_output(&content, &Some(opencode_ask_raw_output())),
        None
    );
}

/// With no `content` the envelope is unwrapped to the bare result text —
/// never the stringified object. Whitespace-only content counts as none.
#[test]
fn opencode_live_tool_output_unwraps_output_when_content_empty() {
    let raw = Some(opencode_ask_raw_output());
    let expected = "The user answered your question(s):\n1. [框架] 选一个前端框架\n   → 选项 A\n";
    assert_eq!(
        opencode_live_tool_output(&None, &raw).as_deref(),
        Some(expected)
    );
    assert_eq!(
        opencode_live_tool_output(&Some("   ".to_string()), &raw).as_deref(),
        Some(expected)
    );
}

/// Failures send `{error, metadata}`, and a command that writes only to
/// stderr leaves `output` empty while the combined stream stays in
/// `metadata.output` — both mirror `parsers/opencode.rs`.
#[test]
fn opencode_live_tool_output_falls_back_to_error_and_metadata_output() {
    let failed = Some(serde_json::json!({
        "error": "The tool call was aborted",
        "metadata": {},
    }));
    assert_eq!(
        opencode_live_tool_output(&None, &failed).as_deref(),
        Some("The tool call was aborted")
    );

    let stderr_only = Some(serde_json::json!({
        "output": "",
        "metadata": {"output": "boom\n", "exit": 1},
    }));
    assert_eq!(
        opencode_live_tool_output(&None, &stderr_only).as_deref(),
        Some("boom\n")
    );
}

/// An unrecognized payload still stringifies exactly as before — the fix
/// unwraps a known envelope, it never drops a result on the floor.
#[test]
fn opencode_live_tool_output_keeps_unknown_payloads() {
    let unknown = Some(serde_json::json!({"weird": {"shape": 1}}));
    assert_eq!(
        opencode_live_tool_output(&None, &unknown).as_deref(),
        Some(r#"{"weird":{"shape":1}}"#)
    );
    assert_eq!(opencode_live_tool_output(&None, &None), None);
}

/// The `read` exception to the "let `content` render" parity rule: OpenCode
/// hands the client the file body with its line numbers stripped, so only
/// `metadata.display` still knows where the excerpt starts. Frame captured
/// from opencode 1.18.30 driven over real ACP.
#[test]
fn opencode_live_read_output_keeps_the_line_numbers_history_shows() {
    let raw = Some(serde_json::json!({
        "output": "<path>/w/notes.txt</path>\n<type>file</type>\n<content>\n1: hello world\n2: second line\n</content>",
        "metadata": {
            "preview": "hello world\nsecond line",
            "truncated": false,
            "display": {
                "type": "file",
                "path": "/w/notes.txt",
                "text": "hello world\nsecond line",
                "lineStart": 1,
                "lineEnd": 2,
                "totalLines": 2
            }
        }
    }));
    // Wins over the clean `content` block, which carries the same text
    // WITHOUT `start_line` — the reason a finished read rendered one way
    // live and another after a reload.
    let content = Some("hello world\nsecond line".to_string());
    assert_eq!(
        opencode_live_tool_output(&content, &raw).as_deref(),
        Some(r#"{"start_line":1,"content":"hello world\nsecond line"}"#)
    );
}

/// OpenCode spells its edit arguments in camelCase on the wire, so the
/// canonical-key lookup found nothing and a live edit's hunks restarted at
/// line 1 — while the same call, reloaded from history, was labelled with
/// the real line (`parsers::opencode` reads it out of `metadata.diff`).
#[test]
fn live_start_line_resolves_opencodes_camel_case_edit_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("app.ts");
    std::fs::write(&file, "one\ntwo\nthree\nneedle\nfive\n").expect("write");

    let mut camel = serde_json::json!({
        "filePath": file.to_string_lossy(),
        "oldString": "needle",
        "newString": "haystack",
    });
    assert!(inject_start_line(&mut camel, None));
    assert_eq!(camel["_start_line"], serde_json::json!(4));

    // The canonical spelling every other agent uses is untouched.
    let mut snake = serde_json::json!({
        "file_path": file.to_string_lossy(),
        "old_string": "three",
    });
    assert!(inject_start_line(&mut snake, None));
    assert_eq!(snake["_start_line"], serde_json::json!(3));

    // A string that is not in the file leaves the input alone rather than
    // stamping a wrong number.
    let mut absent = serde_json::json!({
        "filePath": file.to_string_lossy(),
        "oldString": "not-in-the-file",
    });
    assert!(!inject_start_line(&mut absent, None));
    assert!(absent.get("_start_line").is_none());
}

#[test]
fn opencode_tool_name_is_stamped_only_on_the_arg_less_opening_frame() {
    let stamped = |status: &str, raw_input: serde_json::Value, title: &str| {
        stamp_opencode_tool_name(AgentType::OpenCode, status, &Some(raw_input), title, None)
    };
    // The real opening frame: `pending`, `rawInput: {}`, title = tool id.
    assert_eq!(
        stamped("pending", serde_json::json!({}), "glob"),
        Some(
            serde_json::json!({ "opencode": { "toolName": "glob" } })
                .as_object()
                .unwrap()
                .clone()
        )
    );
    // A replayed (loadSession) frame is also `pending`, but it is built from
    // the COMPLETED state — display title, populated input — so the empty
    // -input gate is what keeps the marker off it.
    assert_eq!(
        stamped(
            "pending",
            serde_json::json!({"pattern": "*.txt"}),
            "notes.txt"
        ),
        None
    );
    // Later frames in the lifecycle: nothing to record, the reducer keeps
    // the opening frame's meta.
    assert_eq!(stamped("in_progress", serde_json::json!({}), "glob"), None);
    assert_eq!(stamped("pending", serde_json::json!({}), "   "), None);
}

#[test]
fn opencode_tool_name_stamp_leaves_other_agents_and_existing_meta_alone() {
    for agent in [AgentType::ClaudeCode, AgentType::Codex, AgentType::Grok] {
        assert_eq!(
            stamp_opencode_tool_name(agent, "pending", &Some(serde_json::json!({})), "glob", None),
            None
        );
    }
    // Merges into whatever the adapter already sent, and never overwrites an
    // `opencode` key the adapter published itself.
    let with_sibling = stamp_opencode_tool_name(
        AgentType::OpenCode,
        "pending",
        &None,
        "read",
        Some(
            serde_json::json!({ "vendor": { "x": 1 } })
                .as_object()
                .unwrap()
                .clone(),
        ),
    )
    .expect("meta");
    assert_eq!(with_sibling["vendor"], serde_json::json!({ "x": 1 }));
    assert_eq!(
        with_sibling["opencode"],
        serde_json::json!({ "toolName": "read" })
    );

    let preexisting = stamp_opencode_tool_name(
        AgentType::OpenCode,
        "pending",
        &None,
        "read",
        Some(
            serde_json::json!({ "opencode": { "toolName": "theirs" } })
                .as_object()
                .unwrap()
                .clone(),
        ),
    )
    .expect("meta");
    assert_eq!(
        preexisting["opencode"],
        serde_json::json!({ "toolName": "theirs" })
    );
}

/// End-to-end over the frames opencode 1.18.23 actually put on the wire for a
/// codeg-mcp `ask_user_question` (captured by driving `opencode acp` against a
/// stub MCP server). The card reconstructs the answer from the result TEXT —
/// opencode drops the MCP `structuredContent` — so the completion must hand
/// the frontend that text, not the `{output, metadata}` blob that shadows it
/// and made an answered question render "no selection" while streaming.
#[tokio::test]
async fn opencode_ask_question_completion_emits_the_answer_text() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();

    let (_, _, opening_output, _) = pi_emit(
        AgentType::OpenCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_probe_1",
            "title": "codeg-mcp_ask_user_question",
            "kind": "other",
            "status": "pending",
            "locations": [],
            "rawInput": {},
        }),
    )
    .await;
    assert!(
        opening_output.is_none(),
        "the pending frame carries no result: {opening_output:?}"
    );

    let (content, _, raw_output, _) = pi_emit(
            AgentType::OpenCode,
            &mut cache,
            &mut cb,
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_probe_1",
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": "The user answered your question(s):\n1. [框架] 选一个前端框架\n   → 选项 A\n",
                    },
                }],
                "rawOutput": opencode_ask_raw_output(),
            }),
        )
        .await;

    assert_eq!(
        content.as_deref(),
        Some("The user answered your question(s):\n1. [框架] 选一个前端框架\n   → 选项 A\n"),
        "the clean answer text reaches the card"
    );
    assert!(
        raw_output.is_none(),
        "the envelope must not shadow it: {raw_output:?}"
    );
}

/// The unwrap is agent-gated: every other agent keeps the existing
/// `json_value_to_text` behavior for an object `rawOutput`.
#[tokio::test]
async fn non_opencode_keeps_the_stringified_envelope() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "rawOutput": {"output": "hi", "metadata": {"truncated": false}},
        }),
    )
    .await;
    assert_eq!(
        raw_output.as_deref(),
        Some(r#"{"output":"hi","metadata":{"truncated":false}}"#)
    );
}

/// A finished Grok terminal `tool_call_update` carries the readable output in
/// BOTH the `content[]` channel and a structured `rawOutput` object (its
/// `output` field a byte array, text only under `output_for_prompt`).
/// Regression: the live path must NOT ship the stringified object as
/// `raw_output` (which shadows `content` and renders empty) — it emits `None`
/// so the frontend renders the clean `content`.
#[tokio::test]
async fn grok_terminal_update_emits_content_not_raw_output_blob() {
    let st = SessionState::new(
        "conn-grok".to_string(),
        AgentType::Grok,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();

    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "\n> build\nbuild ok\n"}}],
            "rawOutput": {
                "output": [10, 62, 32],
                "output_for_prompt": "exit: 0\n\nbuild ok",
                "exit_code": 0,
                "command": "pnpm build",
            },
        }))
        .expect("valid tool_call_update wire shape");

    emit_conversation_update(
        &state,
        &emitter,
        AgentType::Grok,
        update,
        None,
        &mut cache,
        &mut cb,
    )
    .await;

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");
    let (raw_output, content) = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCallUpdate {
                raw_output,
                content,
                ..
            } => Some((raw_output.clone(), content.clone())),
            _ => None,
        })
        .expect("a tool_call_update event is emitted");

    assert!(
        raw_output.is_none(),
        "Grok must not ship the rawOutput object blob (it shadows content \
             and the terminal renderer drops it): {raw_output:?}"
    );
    assert!(
        content.as_deref().is_some_and(|c| c.contains("build ok")),
        "the clean content channel carries the executed command's output: {content:?}"
    );
}

// ---- the AIR tool-call contract (claude 0.82.0 / codex 2.0.0) --------
//
// Every frame below is copied from the adapters' own scenario harnesses,
// re-run with codeg's exact `clientCapabilities` against the new pins.

/// Drive `frames` (raw `session/update` payloads) through
/// `emit_conversation_update` on one connection and return every emitted
/// tool-call event, in order.
async fn drive_tool_frames(
    agent: AgentType,
    cb: &mut CodeBuddyLiveState,
    frames: Vec<serde_json::Value>,
) -> (Arc<RwLock<SessionState>>, Vec<AcpEvent>) {
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-air".to_string(),
        agent,
        None,
        "win".to_string(),
        None,
    )));
    let mut cache = ToolCallOutputCache::default();
    for frame in frames {
        let update: SessionUpdate =
            serde_json::from_value(frame).expect("recorded wire shape parses");
        emit_conversation_update(
            &state,
            &EventEmitter::Noop,
            agent,
            update,
            None,
            &mut cache,
            cb,
        )
        .await;
    }
    let events = state
        .read()
        .await
        .recent_events_after(0)
        .expect("events recorded")
        .into_iter()
        .map(|e| e.payload.clone())
        .filter(|e| {
            matches!(
                e,
                AcpEvent::ToolCall { .. } | AcpEvent::ToolCallUpdate { .. }
            )
        })
        .collect();
    (state, events)
}

#[tokio::test]
async fn a_claude_subagent_child_keeps_its_parent_across_partial_meta() {
    // claude 0.82.0 `subagent-task-legacy`: the child names its parent on
    // the opening frame only; the next `_meta` it sends is the AIR
    // command title alone. Taken whole, that update would un-nest the
    // child from the capsule mid-flight.
    let mut cb = CodeBuddyLiveState::default();
    let (state, events) = drive_tool_frames(
            AgentType::ClaudeCode,
            &mut cb,
            vec![
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "toolu_sub_bash",
                    "_meta": {"claudeCode": {"parentToolUseId": "toolu_task", "toolName": "Bash"}},
                    "content": [], "kind": "execute", "name": "Bash", "status": "pending", "title": "Terminal"}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_sub_bash", "title": "rg parser"}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_sub_bash",
                    "_meta": {"jetbrains": {"air": {"commandTitle": "Search", "version": 1}}},
                    "content": [{"type": "content", "content": {"type": "text", "text": "Search"}}],
                    "rawInput": {"command": "rg parser", "description": "Search"}}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_sub_bash",
                    "content": [{"type": "content", "content": {"type": "text", "text": "```console\nx.ts\n```"}}],
                    "status": "completed"}),
            ],
        )
        .await;
    let metas: Vec<Option<serde_json::Value>> = events
        .iter()
        .map(|e| match e {
            AcpEvent::ToolCall { meta, .. } | AcpEvent::ToolCallUpdate { meta, .. } => meta.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(metas.len(), 4);
    // A frame without `_meta` still says nothing about it…
    assert!(metas[1].is_none());
    assert!(metas[3].is_none());
    // …and the one that carries a partial `_meta` is handed over whole,
    // with the AIR title under its legacy name.
    let merged = metas[2].as_ref().expect("the frame carried _meta");
    assert_eq!(merged["claudeCode"]["parentToolUseId"], "toolu_task");
    assert_eq!(merged["claudeCode"]["toolName"], "Bash");
    assert_eq!(merged["claudeCode"]["title"], "Search");
    // The snapshot (reconnect / refresh) holds the same.
    let guard = state.read().await;
    let call = guard
        .active_tool_calls
        .get("toolu_sub_bash")
        .expect("tracked");
    assert_eq!(
        call.meta
            .as_ref()
            .and_then(|m| m.pointer("/claudeCode/parentToolUseId")),
        Some(&serde_json::json!("toolu_task"))
    );
}

#[tokio::test]
async fn a_claude_edit_renders_from_the_diff_when_its_input_lost_the_text() {
    // claude 0.82.0 `edit-with-permission`: `rawInput` is `{file_path}`
    // alone; the strings ride in the diff block of the same frame.
    let mut cb = CodeBuddyLiveState::default();
    let (_, events) = drive_tool_frames(
        AgentType::ClaudeCode,
        &mut cb,
        vec![
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "toolu_edit",
                    "_meta": {"claudeCode": {"toolName": "Edit"}}, "content": [], "kind": "edit",
                    "locations": [], "name": "Edit", "status": "pending", "title": "Edit"}),
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_edit",
                    "locations": [{"path": "/w/src/app.ts"}], "title": "Edit src/app.ts"}),
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_edit",
                    "content": [{"type": "diff", "path": "/w/src/app.ts",
                        "oldText": "const value = 1;", "newText": "const value = 2;"}],
                    "rawInput": {"file_path": "/w/src/app.ts"}}),
        ],
    )
    .await;
    let AcpEvent::ToolCallUpdate {
        raw_input, content, ..
    } = &events[2]
    else {
        panic!("expected the diff-bearing update");
    };
    let input: serde_json::Value =
        serde_json::from_str(raw_input.as_deref().expect("input rebuilt")).unwrap();
    assert_eq!(input["file_path"], "/w/src/app.ts");
    assert_eq!(input["old_string"], "const value = 1;");
    assert_eq!(input["new_string"], "const value = 2;");
    // The diff now lives in the input; it is not shipped a second time.
    assert!(content.is_none());
}

#[tokio::test]
async fn a_claude_read_gets_its_withheld_result_from_the_raw_sdk_stream() {
    // claude 0.82.0 `read`: the completion is a bare status. The raw SDK
    // `user` message carrying the tool_result arrives just before it.
    let mut cb = CodeBuddyLiveState::default();
    let (_, _) = drive_tool_frames(
        AgentType::ClaudeCode,
        &mut cb,
        vec![
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "toolu_read",
                "_meta": {"claudeCode": {"toolName": "Read"}}, "content": [], "kind": "read",
                "locations": [], "name": "Read", "status": "pending", "title": "Read File"}),
        ],
    )
    .await;
    stash_claude_viewed_results(
        &serde_json::json!({"sessionId": "s", "message": {"type": "user", "message": {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "toolu_read",
                "content": "1\tconst a = 1;\n"}]
        }}}),
        &mut cb,
    );
    let (_, events) = drive_tool_frames(
        AgentType::ClaudeCode,
        &mut cb,
        vec![
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_read",
                "status": "completed"}),
        ],
    )
    .await;
    let AcpEvent::ToolCallUpdate { raw_output, .. } = &events[0] else {
        panic!("expected the completion");
    };
    let raw = raw_output
        .as_deref()
        .expect("the withheld result is filled in");
    assert!(raw.contains("const a = 1;"), "{raw}");
    assert!(cb.claude_viewed_results.is_empty(), "spent");
}

#[tokio::test]
async fn a_codex_activity_follow_up_is_classified_by_its_id() {
    // codex 2.0.0 `subagent-activity`: no `_meta.codex.subagent`; the
    // follow-ups are bare statuses.
    let mut cb = CodeBuddyLiveState::default();
    let (_, events) = drive_tool_frames(
            AgentType::Codex,
            &mut cb,
            vec![
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "act-1",
                    "_meta": {"jetbrains": {"air": {"subagent": true, "version": 1}}}, "kind": "other",
                    "rawInput": {"activityKind": "started", "agentPath": "/root/weather", "agentThreadId": "child-thread"},
                    "status": "in_progress", "title": "Start subagent weather"}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "act-1", "status": "completed"}),
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "act-2",
                    "_meta": {"jetbrains": {"air": {"subagent": true, "version": 1}}}, "kind": "other",
                    "rawInput": {"activityKind": "completed", "agentPath": "/root/weather", "agentThreadId": "child-thread"},
                    "status": "in_progress", "title": "Complete subagent weather"}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "act-2", "status": "completed"}),
            ],
        )
        .await;
    let ids: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AcpEvent::ToolCall { tool_call_id, .. }
            | AcpEvent::ToolCallUpdate { tool_call_id, .. } => tool_call_id.as_str(),
            _ => unreachable!(),
        })
        .collect();
    // The launch opens a capsule and its bare follow-up settles it; the
    // child's `completed` activity is folded onto the launch (another
    // `act-1` update) and neither of its own frames renders.
    assert_eq!(ids, vec!["act-1", "act-1", "act-1"], "{events:?}");
    let AcpEvent::ToolCallUpdate { raw_input, .. } = &events[1] else {
        panic!("expected the launch's follow-up");
    };
    let input: serde_json::Value =
        serde_json::from_str(raw_input.as_deref().expect("launch input re-asserted")).unwrap();
    assert_eq!(input["subagent_type"], "weather");
    assert_eq!(input["agent_id"], "child-thread");
    let AcpEvent::ToolCallUpdate { raw_input, .. } = &events[2] else {
        panic!("expected the settle");
    };
    let settled: serde_json::Value = serde_json::from_str(raw_input.as_deref().unwrap()).unwrap();
    assert_eq!(
        settled[crate::parsers::codex::CODEX_SUBAGENT_STATE_KEY],
        "completed"
    );
}

#[tokio::test]
async fn a_reannounced_id_forgets_its_earlier_activity_classification() {
    let mut cb = CodeBuddyLiveState::default();
    let (_, events) = drive_tool_frames(
            AgentType::Codex,
            &mut cb,
            vec![
                // A dropped activity (a `completed` for a launch never seen)…
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "x",
                    "kind": "other", "status": "in_progress", "title": "Complete subagent w",
                    "rawInput": {"activityKind": "completed", "agentPath": "/root/w", "agentThreadId": "t"}}),
                // …then the id is announced again as an ordinary command.
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "x",
                    "kind": "execute", "status": "in_progress", "title": "ls",
                    "rawInput": {"command": "ls", "cwd": "/w"}}),
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "x", "status": "completed"}),
            ],
        )
        .await;
    let statuses: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AcpEvent::ToolCallUpdate { status, .. } => Some(status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, vec![Some("completed".to_string())], "{events:?}");
}

#[tokio::test]
async fn codex_per_hunk_blocks_and_patches_render_as_one_edit() {
    // codex 2.0.0 without a patch: one block per hunk of ONE file.
    let mut cb = CodeBuddyLiveState::default();
    let (_, events) = drive_tool_frames(
            AgentType::Codex,
            &mut cb,
            vec![serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "fc-1",
                "content": [
                    {"type": "diff", "path": "/w/a.ts", "oldText": "a\nb\n", "newText": "a\nB\n", "_meta": {"kind": "update"}},
                    {"type": "diff", "path": "/w/a.ts", "oldText": "y\nz\n", "newText": "Y\nz\n", "_meta": {"kind": "update"}}
                ],
                "kind": "edit", "status": "in_progress", "title": "Editing files"})],
        )
        .await;
    let AcpEvent::ToolCall { raw_input, .. } = &events[0] else {
        panic!("expected the opening frame");
    };
    let input: serde_json::Value = serde_json::from_str(raw_input.as_deref().unwrap()).unwrap();
    assert_eq!(
        input["old_string"],
        format!("a\nb\n{HUNK_SEPARATOR}\ny\nz\n")
    );
    assert_eq!(
        input["new_string"],
        format!("a\nB\n{HUNK_SEPARATOR}\nY\nz\n")
    );
}

/// Contrast guard: the Grok-only extraction must not change other agents.
/// A non-Grok agent that sends the same object-shaped `rawOutput` still gets
/// it stringified into `raw_output` (existing `json_value_to_text` behavior).
#[tokio::test]
async fn non_grok_object_raw_output_is_stringified_unchanged() {
    let st = SessionState::new(
        "conn-claude".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();

    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-1",
        "status": "completed",
        "rawOutput": {"output_for_prompt": "exit: 0\n\nok", "command": "x"},
    }))
    .expect("valid tool_call_update wire shape");

    emit_conversation_update(
        &state,
        &emitter,
        AgentType::ClaudeCode,
        update,
        None,
        &mut cache,
        &mut cb,
    )
    .await;

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");
    let raw_output = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCallUpdate { raw_output, .. } => Some(raw_output.clone()),
            _ => None,
        })
        .expect("a tool_call_update event is emitted");
    assert!(
        raw_output.is_some(),
        "non-Grok agents keep the existing json_value_to_text behavior"
    );
}

/// Drive one `SessionUpdate` through `emit_conversation_update` and return
/// the tool-call event fields the pi bridge tests assert on. Shared so each
/// case reads as wire-in / card-out.
async fn pi_emit(
    agent_type: AgentType,
    cache: &mut ToolCallOutputCache,
    cb: &mut CodeBuddyLiveState,
    wire: serde_json::Value,
) -> (Option<String>, Option<String>, Option<String>, Option<bool>) {
    let st = SessionState::new(
        "conn-pi".to_string(),
        agent_type,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let update: SessionUpdate = serde_json::from_value(wire).expect("valid tool-call wire shape");

    emit_conversation_update(&state, &emitter, agent_type, update, None, cache, cb).await;

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");
    events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCall {
                content,
                raw_input,
                raw_output,
                ..
            } => Some((content.clone(), raw_input.clone(), raw_output.clone(), None)),
            AcpEvent::ToolCallUpdate {
                content,
                raw_input,
                raw_output,
                raw_output_append,
                ..
            } => Some((
                content.clone(),
                raw_input.clone(),
                raw_output.clone(),
                *raw_output_append,
            )),
            _ => None,
        })
        .expect("a tool-call event is emitted")
}

fn pi_open_bash(tool_call_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionUpdate": "tool_call",
        "toolCallId": tool_call_id,
        "title": title,
        "kind": "execute",
        "status": "pending",
        "content": [{"type": "terminal", "terminalId": tool_call_id}],
        "_meta": {"terminal_info": {"terminal_id": tool_call_id, "cwd": "/w"}},
    })
}

/// #519: pi hosts its own terminal, so the `[Terminal: <id>]` placeholder can
/// never be superseded from the terminal channel — it must not be rendered.
/// And pi sends no `rawInput`, so the command has to be synthesized from the
/// title or the card becomes a generic tool literally NAMED `node --version`.
#[tokio::test]
async fn pi_bash_open_strips_placeholder_and_synthesizes_command_input() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (content, raw_input, _, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        pi_open_bash("call_Q0KKW", "node --version"),
    )
    .await;

    assert!(
        content.is_none(),
        "the dead [Terminal: …] placeholder must not reach the card: {content:?}"
    );
    assert_eq!(
        raw_input.as_deref(),
        Some(r#"{"command":"node --version"}"#),
        "the command is synthesized so the call classifies as bash"
    );
    assert!(
        cb.hosted_terminal_calls.contains_key("call_Q0KKW"),
        "the call is registered for the later output frames, which carry only its id"
    );
}

/// pi's first frame titles the call with the bare tool name (its arguments
/// are still partial JSON). Synthesizing there would flash `$ bash`.
#[tokio::test]
async fn pi_bash_open_titled_bash_synthesizes_no_input() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, raw_input, _, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        pi_open_bash("call_1", "bash"),
    )
    .await;
    assert!(
        raw_input.is_none(),
        "the bare tool-name title is not a command: {raw_input:?}"
    );
}

/// The whole point of #519: the output lives ONLY in `_meta.terminal_output`,
/// and has to reach the card's `raw_output` stream — first chunk replacing,
/// later chunks appending.
#[tokio::test]
async fn pi_terminal_output_meta_streams_as_raw_output() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        pi_open_bash("call_1", "node --version"),
    )
    .await;

    let (_, _, first, first_append) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "in_progress",
            "_meta": {"terminal_output": {"terminal_id": "call_1", "data": "v24.14.0\n"}},
        }),
    )
    .await;
    assert_eq!(first.as_deref(), Some("v24.14.0\n"));
    assert_eq!(
        first_append,
        Some(false),
        "the first chunk replaces whatever the opening frame left on the card"
    );

    let (_, _, second, second_append) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "in_progress",
            "_meta": {"terminal_output": {"terminal_id": "call_1", "data": "more\n"}},
        }),
    )
    .await;
    assert_eq!(second.as_deref(), Some("more\n"));
    assert_eq!(second_append, Some(true), "later chunks append");
}

/// A failing command must show its stderr AND its exit code, and the final
/// frame must release the tracking entry.
#[tokio::test]
async fn pi_terminal_exit_meta_appends_exit_line_and_releases_entry() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        pi_open_bash("call_1", "some-command"),
    )
    .await;

    let (_, _, raw_output, append) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "failed",
            "_meta": {
                "terminal_output": {"terminal_id": "call_1", "data": "command not found"},
                "terminal_exit": {"terminal_id": "call_1", "exit_code": 127, "signal": null},
            },
        }),
    )
    .await;

    assert_eq!(
        raw_output.as_deref(),
        Some("command not found\n[terminal exited: exit code: 127]"),
        "stderr and the exit code both reach the card"
    );
    assert_eq!(append, Some(false));
    assert!(
        !cb.hosted_terminal_calls.contains_key("call_1"),
        "a final status releases the entry"
    );
}

/// A command that prints nothing still has to supersede the placeholder,
/// otherwise the card would sit on `[Terminal: …]` forever.
#[tokio::test]
async fn pi_silent_command_still_emits_the_exit_line() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        pi_open_bash("call_1", "mkdir out"),
    )
    .await;

    let (_, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "_meta": {
                "terminal_exit": {"terminal_id": "call_1", "exit_code": 0, "signal": null},
            },
        }),
    )
    .await;
    assert_eq!(
        raw_output.as_deref(),
        Some("[terminal exited: exit code: 0]")
    );
}

/// pi's terminal ids are its own tool-call ids — `TerminalRuntime` only ever
/// mints `term_<uuid>`, so registering them bought ten guaranteed-miss polls
/// per bash call. Every other agent must keep being tracked, and so must a pi
/// terminal that arrives WITHOUT the self-hosted marker (a future pi-acp that
/// delegates `terminal/*` for real).
#[test]
fn pi_virtual_terminals_are_not_registered_for_host_polling() {
    let wire = pi_open_bash("call_1", "pwd");
    let update: SessionUpdate =
        serde_json::from_value(wire.clone()).expect("valid tool_call wire shape");
    let mut tracked = HashMap::new();
    assert!(!track_terminal_tool_calls(
        AgentType::Pi,
        &update,
        &mut tracked
    ));
    assert!(tracked.is_empty(), "pi's terminals are not host-owned");

    let update: SessionUpdate = serde_json::from_value(wire).expect("valid tool_call wire shape");
    let mut tracked = HashMap::new();
    assert!(track_terminal_tool_calls(
        AgentType::ClaudeCode,
        &update,
        &mut tracked
    ));
    assert!(
        tracked.contains_key("call_1"),
        "a host-owned terminal is still polled"
    );

    let mut host_owned = pi_open_bash("call_1", "pwd");
    host_owned
        .as_object_mut()
        .expect("wire object")
        .remove("_meta");
    let update: SessionUpdate =
        serde_json::from_value(host_owned).expect("valid tool_call wire shape");
    let mut tracked = HashMap::new();
    assert!(track_terminal_tool_calls(
        AgentType::Pi,
        &update,
        &mut tracked
    ));
    assert!(
        tracked.contains_key("call_1"),
        "an unmarked terminal is polled even on pi — the gate is the marker, not the agent"
    );
}

/// Collision guard: pi-acp's meta keys are UNNAMESPACED (`terminal_output`,
/// not `pi/terminalOutput`), so the bridge must be inert on every other agent.
#[tokio::test]
async fn non_pi_agents_ignore_the_unnamespaced_terminal_meta() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (content, raw_input, raw_output, _) = pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        pi_open_bash("call_1", "node --version"),
    )
    .await;

    assert_eq!(
        content.as_deref(),
        Some("[Terminal: call_1]"),
        "a host-owned terminal keeps its placeholder until the poller supersedes it"
    );
    assert!(raw_input.is_none(), "no command is synthesized for non-pi");
    assert!(raw_output.is_none());

    let (_, _, raw_output, _) = pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "_meta": {"terminal_output": {"terminal_id": "call_1", "data": "leak"}},
        }),
    )
    .await;
    assert!(
        raw_output.is_none(),
        "another agent's identically-named meta must not stream: {raw_output:?}"
    );
    assert!(cb.hosted_terminal_calls.is_empty());
}

/// codex's opening frame for a shell command, per
/// `createTerminalCommandEvent`: the terminal is named by the item's own id
/// and — unlike pi — `rawInput` survives, so nothing has to be synthesized.
fn codex_open_command(tool_call_id: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionUpdate": "tool_call",
        "toolCallId": tool_call_id,
        "title": command,
        "kind": "execute",
        "status": "in_progress",
        "content": [{"type": "terminal", "terminalId": tool_call_id}],
        "rawInput": {"command": command, "cwd": "/w"},
        "_meta": {"terminal_info": {"terminal_id": tool_call_id, "cwd": "/w"}},
    })
}

/// codex has pi's #519 shape too: it names a terminal codeg never created,
/// so the `[Terminal: …]` placeholder can never be superseded from the
/// terminal channel. Its own `rawInput` must survive untouched.
#[tokio::test]
async fn codex_command_open_strips_placeholder_and_keeps_its_raw_input() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (content, raw_input, _, _) = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        codex_open_command("exec_1", "npm test"),
    )
    .await;

    assert!(
        content.is_none(),
        "the dead [Terminal: …] placeholder must not reach the card: {content:?}"
    );
    assert_eq!(
        raw_input.as_deref(),
        Some(r#"{"command":"npm test","cwd":"/w"}"#),
        "codex sends its own rawInput; nothing is synthesized over it"
    );
    assert!(cb.hosted_terminal_calls.contains_key("exec_1"));
}

/// The win: codex streams the command's output live over
/// `_meta.terminal_output_delta` (its DEFAULT mode, no capability involved),
/// and codeg used to drop every one of those frames — the card sat on the
/// placeholder until the command ended.
#[tokio::test]
async fn codex_terminal_output_delta_streams_as_raw_output() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        codex_open_command("exec_1", "npm test"),
    )
    .await;

    let delta = |data: &str| {
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "exec_1",
            "_meta": {"terminal_output_delta": {"terminal_id": "exec_1", "data": data}},
        })
    };

    let (_, _, first, first_append) =
        pi_emit(AgentType::Codex, &mut cache, &mut cb, delta("PASS a\n")).await;
    assert_eq!(first.as_deref(), Some("PASS a\n"));
    assert_eq!(
        first_append,
        Some(false),
        "the first chunk replaces whatever the opening frame left on the card"
    );

    let (_, _, second, second_append) =
        pi_emit(AgentType::Codex, &mut cache, &mut cb, delta("PASS b\n")).await;
    assert_eq!(second.as_deref(), Some("PASS b\n"));
    assert_eq!(second_append, Some(true), "later chunks append");
}

/// codex's completion frame repeats the WHOLE aggregated output as
/// `rawOutput` next to `_meta.terminal_exit`. Taking it would re-send
/// everything the deltas already delivered, so for a self-hosted call the
/// `_meta` channel is the only one that may speak.
#[tokio::test]
async fn codex_completion_raw_output_does_not_repeat_the_streamed_output() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        codex_open_command("exec_1", "npm test"),
    )
    .await;
    let _ = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "exec_1",
            "_meta": {"terminal_output_delta": {"terminal_id": "exec_1", "data": "PASS a\n"}},
        }),
    )
    .await;

    let (_, _, raw_output, append) = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "exec_1",
            "status": "completed",
            "rawOutput": {"formatted_output": "PASS a\n", "exit_code": 0},
            "_meta": {
                "terminal_exit": {"terminal_id": "exec_1", "exit_code": 0, "signal": null},
            },
        }),
    )
    .await;
    assert_eq!(
        raw_output.as_deref(),
        Some("[terminal exited: exit code: 0]"),
        "only the exit line is new; the aggregated output is a duplicate"
    );
    assert_eq!(append, Some(true));
    assert!(
        !cb.hosted_terminal_calls.contains_key("exec_1"),
        "a final status releases the entry"
    );
}

/// The narrow gate matters: codex ALSO reports `search` / `listFiles` as
/// command executions, and those carry no `terminal_info`. When one that
/// streamed nothing completes with the `{formatted_output, exit_code}`
/// envelope (any codex that ignores the advertised `terminal_output_delta`)
/// the envelope is all there is — grep's "No matches" reads exit 1 off it —
/// so the bridge must leave that channel completely alone.
#[tokio::test]
async fn codex_non_terminal_commands_keep_their_raw_output_envelope() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "search_1",
            "status": "completed",
            "rawOutput": {"formatted_output": "src/a.rs", "exit_code": 0},
        }),
    )
    .await;
    assert_eq!(
        raw_output.as_deref(),
        Some(r#"{"formatted_output":"src/a.rs","exit_code":0}"#),
        "a command with no self-hosted terminal keeps the envelope the card parses"
    );
    assert!(cb.hosted_terminal_calls.is_empty());
}

/// codex streams `item/commandExecution/outputDelta` for EVERY command it
/// runs, search / listFiles / read included (`createCommandOutputDeltaEvent`
/// has no action filter). So a search that printed something reaches codeg as
/// plain-text deltas while it runs — no `terminal_info` needed — and the card
/// fills in live. Once a delta has spoken, the completion's aggregated
/// `rawOutput` is the same text again, and must not be appended a second
/// time.
#[tokio::test]
async fn codex_search_output_streams_and_is_not_repeated_on_completion() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let _ = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "search_1",
            "title": "Search for 'foo' in src",
            "kind": "search",
            "status": "in_progress",
        }),
    )
    .await;
    let (_, _, streamed, append) = pi_emit(
            AgentType::Codex,
            &mut cache,
            &mut cb,
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "search_1",
                "_meta": {"terminal_output_delta": {"terminal_id": "search_1", "data": "src/a.rs:1:foo\n"}},
            }),
        )
        .await;
    assert_eq!(streamed.as_deref(), Some("src/a.rs:1:foo\n"));
    assert_eq!(append, Some(false));

    let (_, _, raw_output, _) = pi_emit(
        AgentType::Codex,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "search_1",
            "status": "completed",
            "rawOutput": {"formatted_output": "src/a.rs:1:foo\n", "exit_code": 0},
        }),
    )
    .await;
    assert!(
        raw_output.is_none(),
        "the aggregated output repeats what the delta delivered: {raw_output:?}"
    );
    assert!(
        cb.hosted_terminal_calls.is_empty(),
        "a final status releases the entry"
    );
}

/// Only codex's own `search` command actions carry the marker the
/// frontend's no-output "no matches" rule requires: another agent's search,
/// codex's `read`/`listFiles` actions (`kind: "read"`) and a self-hosted
/// shell must all stay unmarked, and whatever `_meta` the frame carried is
/// kept.
#[test]
fn only_codex_search_actions_are_stamped() {
    let marked = |meta: &Option<serde_json::Map<String, serde_json::Value>>| {
        meta.as_ref()
            .and_then(|m| m.get(CODEX_SEARCH_ACTION_META_KEY))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    };
    assert!(marked(&stamp_codex_search_action(
        AgentType::Codex,
        &ToolKind::Search,
        false,
        None
    )));
    let existing = serde_json::json!({"other": 1}).as_object().cloned();
    let kept = stamp_codex_search_action(AgentType::Codex, &ToolKind::Search, false, existing);
    assert!(marked(&kept));
    assert_eq!(
        kept.as_ref().and_then(|m| m.get("other")),
        Some(&serde_json::json!(1))
    );

    for (agent, kind, hosted) in [
        (AgentType::ClaudeCode, ToolKind::Search, false),
        (AgentType::Gemini, ToolKind::Search, false),
        (AgentType::Codex, ToolKind::Read, false),
        (AgentType::Codex, ToolKind::Search, true),
    ] {
        assert!(
            !marked(&stamp_codex_search_action(agent, &kind, hosted, None)),
            "{agent:?} {kind:?} hosted={hosted}"
        );
    }
}

/// With `_meta.terminal_output_delta` advertised, codex stops sending
/// `rawOutput` on ANY command completion. A command that streamed nothing
/// gets its whole output as one delta on the completion frame instead
/// (`!commandHadOutput && aggregatedOutput && deltaSupported`) — so a quick
/// search still lands, on the same bridge.
#[tokio::test]
async fn codex_search_output_bundled_into_the_completion_frame_still_lands() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, append) = pi_emit(
            AgentType::Codex,
            &mut cache,
            &mut cb,
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "search_2",
                "status": "completed",
                "_meta": {"terminal_output_delta": {"terminal_id": "search_2", "data": "src/b.rs:9:bar\n"}},
            }),
        )
        .await;
    assert_eq!(raw_output.as_deref(), Some("src/b.rs:9:bar\n"));
    assert_eq!(append, Some(false));
    assert!(cb.hosted_terminal_calls.is_empty());
}

/// Same reasoning as the pi case: codex's terminal ids are its own tool-call
/// ids, so registering them buys `TERMINAL_POLL_MISSING_LIMIT` guaranteed
/// misses per command.
#[test]
fn codex_virtual_terminals_are_not_registered_for_host_polling() {
    let update: SessionUpdate = serde_json::from_value(codex_open_command("exec_1", "npm test"))
        .expect("valid tool_call wire shape");
    let mut tracked = HashMap::new();
    assert!(!track_terminal_tool_calls(
        AgentType::Codex,
        &update,
        &mut tracked
    ));
    assert!(tracked.is_empty(), "codex's terminals are not host-owned");
}

/// A pi `bash` that does NOT ride the `_meta` channel (every pi-acp build
/// before the terminal bridge — and every non-bash pi tool on all of them)
/// reports its result twice: flattened into `content[]`, and verbatim as the
/// MCP envelope in `rawOutput`. Regression: the envelope was stringified into
/// `raw_output`, which WINS over `content` in the live store, so the terminal
/// card printed `{"content":[{"text":"$ next build…","type":"text"}]}`.
#[tokio::test]
async fn pi_tool_result_envelope_does_not_shadow_content() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (content, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_fb7",
            "status": "completed",
            "content": [{
                "type": "content",
                "content": {"type": "text", "text": "$ next build\n✓ Compiled\n"},
            }],
            "rawOutput": {
                "content": [{"type": "text", "text": "$ next build\n✓ Compiled\n"}],
            },
        }),
    )
    .await;

    assert!(
        raw_output.is_none(),
        "the envelope is a second copy of `content` — shipping it renders the \
             JSON source as the terminal body: {raw_output:?}"
    );
    assert_eq!(
        content.as_deref(),
        Some("$ next build\n✓ Compiled\n"),
        "the clean content channel carries the command output"
    );
}

/// Sequence guard for the whole command, not one frame in isolation.
///
/// pi's bash tool fires `onUpdate({content: []})` before the process writes a
/// byte, so frame 1 of EVERY command is the empty envelope — and pi-acp, with
/// nothing to flatten, puts its own `JSON.stringify(result)` on `content[]`.
/// Emitting either one poisons the card: the stringified `{"content":[]}`
/// becomes the tool call's raw-output chunk, every later frame prefers
/// `content` and emits `None`, the reducer keeps the last chunks, and the
/// chunk outranks `content` — so the finished build would render
/// `{"content":[]}` instead of its log.
#[tokio::test]
async fn pi_empty_opening_frame_never_outranks_the_real_output() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (content, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_fb7",
            "status": "in_progress",
            // pi-acp's stringify fallback over the empty envelope, verbatim.
            "content": [{
                "type": "content",
                "content": {"type": "text", "text": "{\n  \"content\": []\n}"},
            }],
            "rawOutput": {"content": []},
        }),
    )
    .await;
    assert!(
        raw_output.is_none(),
        "the empty envelope must not be seeded as a chunk — it would outrank \
             every later frame's content: {raw_output:?}"
    );
    assert!(
        content.is_none(),
        "pi-acp's JSON.stringify fallback is not command output: {content:?}"
    );

    // Frame 2 — the real result, on the SAME tool call id.
    let (content, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_fb7",
            "status": "completed",
            "content": [{
                "type": "content",
                "content": {"type": "text", "text": "$ next build\n✓ Compiled\n"},
            }],
            "rawOutput": {"content": [{"type": "text", "text": "$ next build\n✓ Compiled\n"}]},
        }),
    )
    .await;
    assert_eq!(
        content.as_deref(),
        Some("$ next build\n✓ Compiled\n"),
        "the command output reaches the card"
    );
    assert!(
        raw_output.is_none(),
        "and nothing is left in the chunk stream to shadow it: {raw_output:?}"
    );
}

/// A frame whose envelope is empty because pi has produced nothing yet must
/// not fall through to the stringify branch even without a `content` channel.
#[tokio::test]
async fn pi_empty_envelope_is_recognized_by_shape_not_by_yielding_text() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_fb7",
            "status": "in_progress",
            "rawOutput": {"content": []},
        }),
    )
    .await;
    assert!(raw_output.is_none(), "{raw_output:?}");
}

/// The suppression must catch ONLY pi's literal `{"content":[]}` opening
/// announcement. `toolResultToText` also flattens `details.diff`, `stdout` /
/// `stderr` (with the exit code appended) and `output` from BOTH the result
/// and its `details` — every one of those reaches real text that the block
/// array alone does not, so an empty block array is not by itself evidence
/// that pi had nothing to say. Anything else it cannot flatten it dumps as
/// JSON, which is ugly but still carries the result, so that is kept too.
#[tokio::test]
async fn only_pis_empty_opening_announcement_counts_as_noise() {
    // (label, rawOutput, the text pi-acp's `toolResultToText` flattens)
    let real_results = [
        (
            "details.diff",
            serde_json::json!({"content": [], "details": {"diff": "--- a\n+++ b\n+line\n"}}),
            "--- a\n+++ b\n+line\n",
        ),
        (
            "details.stdout + exitCode",
            serde_json::json!({"content": [], "details": {"stdout": "build succeeded", "exitCode": 0}}),
            "build succeeded\n\nexit code: 0",
        ),
        (
            "top-level stdout",
            serde_json::json!({"content": [], "stdout": "ok"}),
            "ok",
        ),
        (
            "details.output",
            serde_json::json!({"content": [], "details": {"output": "from details"}}),
            "from details",
        ),
        (
            "top-level output",
            serde_json::json!({"content": [], "output": "from result"}),
            "from result",
        ),
        (
            "stderr + exitCode",
            serde_json::json!({"content": [], "details": {"stderr": "boom", "exitCode": 3}}),
            "stderr:\nboom\n\nexit code: 3",
        ),
        (
            // Nothing flattenable: pi-acp dumps the JSON. Ugly, but it is the
            // result — the exit code would be lost if we called it noise.
            "exit-code-only stringify fallback",
            serde_json::json!({"content": [], "details": {"exitCode": 3}}),
            "{\n  \"content\": [],\n  \"details\": {\n    \"exitCode\": 3\n  }\n}",
        ),
    ];

    for (label, raw_output, flattened) in real_results {
        for (surface, wire) in [
            (
                "tool_call",
                serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_x",
                    "title": "bash",
                    "kind": "execute",
                    "status": "completed",
                    "content": [{
                        "type": "content",
                        "content": {"type": "text", "text": flattened},
                    }],
                    "rawOutput": raw_output.clone(),
                }),
            ),
            (
                "tool_call_update",
                serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call_x",
                    "status": "completed",
                    "content": [{
                        "type": "content",
                        "content": {"type": "text", "text": flattened},
                    }],
                    "rawOutput": raw_output.clone(),
                }),
            ),
        ] {
            let mut cache = ToolCallOutputCache::default();
            let mut cb = CodeBuddyLiveState::default();
            let (content, _, _, _) = pi_emit(AgentType::Pi, &mut cache, &mut cb, wire).await;
            assert_eq!(
                content.as_deref(),
                Some(flattened),
                "{label} is a real result, not pi's empty announcement ({surface})"
            );
        }
    }

    // The one shape that IS the announcement — and only in its exact form.
    assert!(pi_result_is_empty_announcement(&serde_json::json!({
        "content": []
    })));
    assert!(
        pi_result_is_empty_announcement(&serde_json::json!({"content": [], "details": null})),
        "an explicit null `details` is the same announcement"
    );
    assert!(
        !pi_result_is_empty_announcement(&serde_json::json!({"content": [], "details": {}})),
        "any other non-null member means pi is reporting something"
    );
    assert!(
        !pi_result_is_empty_announcement(&serde_json::json!({
            "content": [{"type": "text", "text": ""}]
        })),
        "a populated block array is not the announcement, empty text or not"
    );
    assert!(!pi_result_is_empty_announcement(&serde_json::json!({})));
    assert!(!pi_result_is_empty_announcement(&serde_json::json!("text")));
}

/// With no `content` the envelope IS the result, so it must be unwrapped to
/// the same text the history parser produces — not dropped, and not shipped
/// as JSON.
#[tokio::test]
async fn pi_tool_result_envelope_unwrapped_when_content_absent() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_fb7",
            "status": "completed",
            "rawOutput": {"content": [{"type": "text", "text": "v24.14.0\n"}]},
        }),
    )
    .await;
    assert_eq!(raw_output.as_deref(), Some("v24.14.0\n"));

    // An unrecognized shape keeps the generic stringified behavior rather
    // than silently dropping output we have never seen on this wire.
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::Pi,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_other",
            "status": "completed",
            "rawOutput": {"details": {"exitCode": 0}},
        }),
    )
    .await;
    assert_eq!(raw_output.as_deref(), Some(r#"{"details":{"exitCode":0}}"#));
}

/// Contrast guard: the unwrap is pi-gated — `{"content":[…]}` is a generic
/// MCP shape, and another agent's identical `rawOutput` still stringifies.
#[tokio::test]
async fn non_pi_agents_keep_the_stringified_mcp_envelope() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, _, raw_output, _) = pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "rawOutput": {"content": [{"type": "text", "text": "ok"}]},
        }),
    )
    .await;
    assert_eq!(
        raw_output.as_deref(),
        Some(r#"{"content":[{"type":"text","text":"ok"}]}"#),
        "non-pi agents keep the existing json_value_to_text behavior"
    );
}

// ---- claude file-tool argument aliases (CLI 2.1.280, adapter 0.81.1) ----

/// A Write the model spelled with the text-editor names reaches codeg as
/// raw `rawInput`; the card must still get `file_path` / `content`. Both
/// frame kinds carry input — the opening `tool_call` and the refining
/// `tool_call_update` the streamed `tool_use` produces — and the update
/// names its tool only through `_meta.claudeCode.toolName` (the ACP `name`
/// rides the opening frame alone), which is why that is the key read.
#[tokio::test]
async fn claude_write_aliases_reach_the_card_as_canonical_arguments() {
    for session_update in ["tool_call", "tool_call_update"] {
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let mut wire = serde_json::json!({
            "sessionUpdate": session_update,
            "toolCallId": "toolu_w",
            "rawInput": {"path": "/w/notes.md", "file_text": "# Notes\n"},
            "_meta": {"claudeCode": {"toolName": "Write"}},
        });
        if session_update == "tool_call" {
            wire["title"] = serde_json::json!("Write /w/notes.md");
        }
        let (_, raw_input, _, _) = pi_emit(AgentType::ClaudeCode, &mut cache, &mut cb, wire).await;
        let input: serde_json::Value =
            serde_json::from_str(raw_input.as_deref().expect("raw_input")).unwrap();
        assert_eq!(
            input,
            serde_json::json!({"file_path": "/w/notes.md", "content": "# Notes\n"}),
            "{session_update}"
        );
    }
}

/// A Grep sent with `file_path` (CLI 2.1.292 runs it as `path`) reaches the
/// search card as `path`. Under the AIR contract the update that carries
/// the input names no tool of its own, so the tool name has to come from
/// the opening frame through the `_meta` ledger.
#[tokio::test]
async fn claude_grep_file_path_reaches_the_card_as_path() {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "toolu_g",
            "title": "grep",
            "kind": "search",
            "status": "pending",
            "_meta": {"claudeCode": {"toolName": "Grep"}},
        }),
    )
    .await;
    let (_, raw_input, _, _) = pi_emit(
        AgentType::ClaudeCode,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "toolu_g",
            "title": "grep \"needle\" /w/a.txt",
            "rawInput": {"pattern": "needle", "file_path": "/w/a.txt", "output_mode": "content"},
        }),
    )
    .await;
    let input: serde_json::Value =
        serde_json::from_str(raw_input.as_deref().expect("raw_input")).unwrap();
    assert_eq!(
        input,
        serde_json::json!({"pattern": "needle", "path": "/w/a.txt", "output_mode": "content"})
    );
}

/// The renames are keyed on the claude tool's NAME: Grep's own `path`
/// argument is not a Write's `file_path`, and another agent's `_meta` that
/// happens to use the same key is not claude's.
#[tokio::test]
async fn claude_alias_settling_is_scoped_to_its_file_tools() {
    let grep_input = serde_json::json!({"pattern": "TODO", "path": "/w/src"});
    let cases = [
        (AgentType::ClaudeCode, "Grep", grep_input.clone()),
        (
            AgentType::Codex,
            "Write",
            serde_json::json!({"path": "/w/a", "file_text": "x"}),
        ),
    ];
    for (agent_type, tool_name, raw_input) in cases {
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let (_, emitted, _, _) = pi_emit(
            agent_type,
            &mut cache,
            &mut cb,
            serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "toolu_x",
                "rawInput": raw_input,
                "_meta": {"claudeCode": {"toolName": tool_name}},
            }),
        )
        .await;
        let emitted: serde_json::Value =
            serde_json::from_str(emitted.as_deref().expect("raw_input")).unwrap();
        assert_eq!(emitted, raw_input, "{agent_type:?} {tool_name}");
    }
}

// ---- Antigravity 1.2's flattened MCP input ------------------------------

/// The `_meta` Antigravity puts on every MCP frame (`unwrap_mcp_tool_call`).
fn antigravity_mcp_meta(prompt: Option<&str>) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "mcp": {"tool": "browser_navigate", "server": "codeg-mcp"},
        "is_mcp_tool_call": true,
    });
    if let Some(prompt) = prompt {
        meta["prompt"] = serde_json::json!(prompt);
    }
    meta
}

async fn antigravity_emitted_input(
    agent_type: AgentType,
    raw_input: serde_json::Value,
    meta: serde_json::Value,
) -> serde_json::Value {
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let (_, emitted, _, _) = pi_emit(
        agent_type,
        &mut cache,
        &mut cb,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_nav",
            "rawInput": raw_input,
            "_meta": meta,
        }),
    )
    .await;
    serde_json::from_str(emitted.as_deref().expect("raw_input")).unwrap()
}

/// 1.2 sends each MCP argument twice — flat, and inside `arguments` — with
/// the sentence moved to `_meta.prompt`. Both frame kinds carry it (the
/// opening `tool_call` and the in-progress update that reconciles a
/// permission prompt), and both must reach the card in the shape history
/// has, or the generic card lists every argument twice.
#[tokio::test]
async fn antigravity_mcp_input_folds_back_into_its_history_shape() {
    let arguments = serde_json::json!({"tabId": "t1", "url": "https://example.com/"});
    let mut wire_input = arguments.clone();
    wire_input["arguments"] = arguments.clone();
    for session_update in ["tool_call", "tool_call_update"] {
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let mut wire = serde_json::json!({
            "sessionUpdate": session_update,
            "toolCallId": "call_nav",
            "status": "in_progress",
            "rawInput": wire_input,
            "_meta": antigravity_mcp_meta(Some("Opening the example page")),
        });
        if session_update == "tool_call" {
            wire["title"] = serde_json::json!("codeg-mcp_browser_navigate");
        }
        let (_, emitted, _, _) = pi_emit(AgentType::Antigravity, &mut cache, &mut cb, wire).await;
        let emitted: serde_json::Value =
            serde_json::from_str(emitted.as_deref().expect("raw_input")).unwrap();
        assert_eq!(
            emitted,
            serde_json::json!({
                "arguments": arguments,
                "prompt": "Opening the example page",
            }),
            "{session_update}"
        );
    }
}

/// Only an exact flattened copy on an MCP-marked Antigravity call is
/// folded; every other input reaches the card as sent.
#[tokio::test]
async fn antigravity_mcp_input_fold_leaves_every_other_input_alone() {
    let flattened = serde_json::json!({
        "arguments": {"task_id": "t-1"},
        "task_id": "t-1",
    });
    let cases = [
        // 1.1's own shape is the target already.
        (
            AgentType::Antigravity,
            serde_json::json!({"arguments": {"task_id": "t-1"}, "prompt": "Cancel it"}),
            antigravity_mcp_meta(None),
        ),
        // Not marked as MCP: an ordinary tool may have an `arguments` key.
        (
            AgentType::Antigravity,
            flattened.clone(),
            serde_json::json!({"is_mcp_tool_call": false}),
        ),
        // A tool's own `arguments` argument overwrites the wrapper
        // upstream, so the object under that key is not a copy of the rest.
        (
            AgentType::Antigravity,
            serde_json::json!({"arguments": {"depth": 2}, "task_id": "t-1"}),
            antigravity_mcp_meta(Some("Inspect it")),
        ),
        // A near-copy is not a copy.
        (
            AgentType::Antigravity,
            serde_json::json!({"arguments": {"task_id": "t-1"}, "task_id": "t-2"}),
            antigravity_mcp_meta(Some("Cancel it")),
        ),
        // The same bytes from another agent.
        (
            AgentType::Codex,
            flattened.clone(),
            antigravity_mcp_meta(Some("Cancel it")),
        ),
    ];
    for (agent_type, raw_input, meta) in cases {
        let emitted = antigravity_emitted_input(agent_type, raw_input.clone(), meta).await;
        assert_eq!(emitted, raw_input, "{agent_type:?} {raw_input}");
    }
}

// ---- #525: pi's lifecycle announcements ride the prose channel ----------

/// Wire-in / events-out for a pi `agent_message_chunk`, the counterpart of
/// `pi_emit` for the message channel. Returns EVERY event the update
/// produced, so a test can assert on "nothing at all" as easily as on a
/// specific event.
async fn pi_emit_chunk(agent_type: AgentType, wire: serde_json::Value) -> Vec<AcpEvent> {
    let st = SessionState::new(
        "conn-pi".to_string(),
        agent_type,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();
    let update: SessionUpdate =
        serde_json::from_value(wire).expect("valid agent_message_chunk wire shape");

    emit_conversation_update(
        &state, &emitter, agent_type, update, None, &mut cache, &mut cb,
    )
    .await;

    let guard = state.read().await;
    guard
        .recent_events_after(0)
        .map(|events| events.iter().map(|e| e.payload.clone()).collect())
        .unwrap_or_default()
}

fn pi_chunk(text: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text},
    })
}

fn pi_notify_chunk(text: &str, level: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text},
        "_meta": {"piAcp": {"notify": {"level": level}}},
    })
}

/// Every announcement pi-acp puts on the prose channel, classified. This is
/// the inventory the fix is built from — if pi-acp reworks its wording, this
/// is the test that says so.
#[test]
fn pi_chunk_route_classifies_every_pi_acp_announcement() {
    let route = |text: &str| pi_message_chunk_route(AgentType::Pi, text, None);

    // auto_retry_start, both shapes.
    assert_eq!(
        route("Retrying (attempt 1/3, waiting 2s)..."),
        PiChunkRoute::Retrying {
            attempt: Some(1),
            max: Some(3),
            delay_ms: Some(2000),
        }
    );
    assert_eq!(
        route("Retrying..."),
        PiChunkRoute::Retrying {
            attempt: None,
            max: None,
            delay_ms: None,
        },
        "pi-acp's shapeless fallback still opens the banner, just without counters"
    );
    // auto_retry_end + both compaction sentences + all three queue messages.
    for text in [
        "Retry finished, resuming.",
        "Context nearing limit, running automatic compaction...",
        "Automatic compaction finished; context was summarized to continue the session.",
        "Queued message (position 2).",
        "Starting queued message. (1 remaining)",
        "Cleared queued prompts.",
    ] {
        assert_eq!(
            route(text),
            PiChunkRoute::Drop,
            "{text:?} must not be prose"
        );
    }
}

/// The notify marker is authoritative and level-independent: an extension's
/// message is arbitrary text, so nothing but `_meta` can identify it. This is
/// the exact frame from the issue screenshot.
#[test]
fn pi_extension_notify_is_dropped_by_marker_at_every_level() {
    for level in ["info", "warning", "error"] {
        let meta = serde_json::json!({"piAcp": {"notify": {"level": level}}});
        let meta = meta.as_object().cloned().expect("object meta");
        assert_eq!(
            pi_message_chunk_route(
                AgentType::Pi,
                "Released pi-caffeinate (agent finished).",
                Some(&meta)
            ),
            PiChunkRoute::Drop,
            "level {level}"
        );
        // …even when the extension's text reads exactly like an answer.
        assert_eq!(
            pi_message_chunk_route(AgentType::Pi, "是的，插件已加载。", Some(&meta)),
            PiChunkRoute::Drop,
            "the marker wins over the text, level {level}"
        );
    }
}

/// The far more dangerous direction: text that must survive. pi-acp's
/// slash-command replies and prelude ride the SAME channel, and the model
/// itself can say anything.
#[test]
fn pi_chunk_route_leaves_real_prose_and_command_replies_alone() {
    for text in [
        // The model's own words, including the reply from the screenshot.
        "你好。有什么需要我帮你处理?",
        "是的，OV（OpenViking）插件当前已正式加载并可用。",
        // Slash-command output (pi-acp `handleCommand`): the user asked.
        "Usage: /name <name>",
        "Cleared queued prompts. Session exported: /tmp/x.md",
        "Session: abc\nMessages: 12",
        "Compaction completed.",
        // A rare but real failure notice, deliberately kept (see fn doc).
        "Pi input UI request is not supported in ACP yet; cancelling it.",
        // Near-misses: same opening, not the announcement.
        "Retrying the request by hand is also an option.",
        "Retrying (attempt one of three)...",
        "Queued message (position two).",
        // A whole-chunk match means an embedded sentence is still prose.
        "I will say: Retry finished, resuming. Then continue.",
    ] {
        assert_eq!(
            pi_message_chunk_route(AgentType::Pi, text, None),
            PiChunkRoute::Prose,
            "{text:?} is the agent speaking"
        );
    }
}

/// The startup prelude. pi-acp reports the identical text twice — once as
/// `_meta.piAcp.startupInfo` on the `session/new` response and once as a bare
/// `agent_message_chunk` — so the chunk is recognized by comparison, never by
/// shape. Text captured from pi-acp 0.0.33 driven over real stdio ACP.
#[tokio::test]
async fn pi_startup_banner_is_captured_and_dropped_exactly_once() {
    let banner = "pi v0.84.2\n---\n\n## Context\n- /tmp/scratch/AGENTS.md\n\n## Skills\n- /Users/x/.agents/skills/officecli/SKILL.md\n";
    let meta = serde_json::json!({"piAcp": {"startupInfo": banner}});
    let meta = meta.as_object().cloned().expect("object meta");

    assert_eq!(
        pi_startup_banner(AgentType::Pi, Some(&meta)).as_deref(),
        Some(banner.trim()),
    );
    assert_eq!(
        pi_startup_banner(AgentType::ClaudeCode, Some(&meta)),
        None,
        "`piAcp` is pi-acp's namespace; never read it off another agent"
    );
    assert_eq!(
        pi_startup_banner(AgentType::Pi, None),
        None,
        "a `quietStartup` pi sends no prelude and no meta"
    );

    let state = Arc::new(RwLock::new(SessionState::new(
        "conn".to_string(),
        AgentType::Pi,
        None,
        "main".to_string(),
        None,
    )));
    state.write().await.pi_startup_banner = pi_startup_banner(AgentType::Pi, Some(&meta));

    assert!(
        !pi_take_startup_banner(AgentType::Pi, &state, "你好，我能帮你做什么？").await,
        "prose must not be mistaken for the prelude"
    );
    assert!(
        pi_take_startup_banner(AgentType::Pi, &state, banner).await,
        "the prelude chunk is recognized"
    );
    assert!(
        !pi_take_startup_banner(AgentType::Pi, &state, banner).await,
        "taken, not filtered: the same text later is the user's, and renders"
    );
}

/// gemini-cli announces an approval-mode switch as a prose chunk reading
/// `[MODE_UPDATE] <mode>` (`handleApprovalModeChanged`,
/// packages/cli/src/acp/acpSession.ts 0.60.0). The ids are the four
/// `ApprovalMode` values, and `plan` is only advertised when
/// `isPlanEnabled()` (`buildAvailableModes`, packages/cli/src/acp/acpUtils.ts).
#[tokio::test]
async fn gemini_mode_update_marker_is_recognized_only_as_a_whole_chunk() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "conn".to_string(),
        AgentType::Gemini,
        None,
        "main".to_string(),
        None,
    )));

    assert_eq!(
        gemini_mode_update_chunk(AgentType::Gemini, &state, "[MODE_UPDATE] yolo").await,
        None,
        "before the modes handshake there is nothing to match against: prose"
    );

    // A session WITHOUT plan mode, which is what `isPlanEnabled() == false`
    // advertises.
    state.write().await.modes = Some(SessionModeStateInfo {
        current_mode_id: "default".to_string(),
        available_modes: ["default", "autoEdit", "yolo"]
            .into_iter()
            .map(|id| SessionModeInfo {
                id: id.to_string(),
                name: id.to_string(),
                description: None,
            })
            .collect(),
    });

    assert_eq!(
        gemini_mode_update_chunk(AgentType::Gemini, &state, "[MODE_UPDATE] yolo").await,
        Some("yolo".to_string()),
        "the real marker is the whole chunk and nothing else"
    );
    assert_eq!(
        gemini_mode_update_chunk(AgentType::Gemini, &state, "  [MODE_UPDATE] autoEdit\n").await,
        Some("autoEdit".to_string()),
        "surrounding whitespace is not prose"
    );

    assert_eq!(
        gemini_mode_update_chunk(AgentType::Gemini, &state, "[MODE_UPDATE] plan").await,
        None,
        "plan is not advertised by this session, so this is the user's text"
    );
    assert_eq!(
        gemini_mode_update_chunk(AgentType::Gemini, &state, "[MODE_UPDATE] ").await,
        None,
        "an empty id matches no advertised mode"
    );
    assert_eq!(
        gemini_mode_update_chunk(
            AgentType::Gemini,
            &state,
            "Run it again and you'll see [MODE_UPDATE] yolo in the output.",
        )
        .await,
        None,
        "a reply that QUOTES the marker mid-sentence must still render"
    );
    assert_eq!(
        gemini_mode_update_chunk(
            AgentType::Gemini,
            &state,
            "[MODE_UPDATE] yolo\n\nSwitched. Want me to continue?",
        )
        .await,
        None,
        "the marker never arrives glued to prose; matching a prefix would eat the reply"
    );
    assert_eq!(
        gemini_mode_update_chunk(AgentType::ClaudeCode, &state, "[MODE_UPDATE] yolo").await,
        None,
        "only gemini-cli speaks this; never filter another agent's prose"
    );
}

fn gemini_location(path: &str, line: Option<u32>) -> ToolCallLocation {
    ToolCallLocation::new(path).line(line)
}

/// gemini ships no `rawInput`, so the arguments have to come back out of the
/// title. Titles below are the tools' own `getDisplayTitle()` in 0.60.0.
///
/// The negative half is the point of the whitelist: synthesizing the wrong
/// shape is worse than synthesizing nothing, because codeg classifies on
/// shape and would render a card that names the wrong tool.
#[test]
fn gemini_tool_input_is_synthesized_only_from_identifying_titles() {
    let synth = |kind: ToolKind, title: &str, locations: &[ToolCallLocation]| {
        gemini_synthesize_tool_input(AgentType::Gemini, &kind, title, locations)
    };

    // shell: `getDisplayTitle()` returns `params.command` verbatim, so a
    // command over the 150-char `getDescription()` threshold still arrives
    // whole rather than as the model's prose description.
    assert_eq!(
        synth(ToolKind::Execute, "pnpm eslint .", &[]),
        Some(r#"{"command":"pnpm eslint ."}"#.to_string())
    );
    let long = format!("echo {}", "x".repeat(200));
    assert_eq!(
        synth(ToolKind::Execute, &long, &[]),
        Some(serde_json::json!({ "command": long }).to_string()),
        "the wire title is the raw command at any length"
    );
    assert_eq!(synth(ToolKind::Execute, "   ", &[]), None);

    // write-file is deliberately NOT synthesized: gemini ships a `diff`
    // block that `synthesize_edit_input_from_diffs` turns into a real edit
    // input, and a bare `{file_path}` under `kind: edit` would render an
    // EditToolInput card with nothing in it.
    assert_eq!(
        synth(
            ToolKind::Edit,
            "Writing to src/main.rs",
            &[gemini_location("/repo/src/main.rs", None)],
        ),
        None,
        "the diff path owns edits; a path-only input renders a blank edit card"
    );

    // read-file reports exactly one location; read_many_files shares the
    // kind but reports several.
    assert_eq!(
        synth(
            ToolKind::Read,
            "src/lib.rs",
            &[gemini_location("/repo/src/lib.rs", Some(42))],
        ),
        Some(r#"{"file_path":"/repo/src/lib.rs","offset":42}"#.to_string()),
        "the line is reported as `offset` — the key the file card actually reads"
    );
    assert_eq!(
        synth(ToolKind::Read, "2 files", &[]),
        None,
        "read_many_files inherits `toolLocations() -> []`, so a Read with no \
             location is what it actually looks like on the wire"
    );

    assert_eq!(
        synth(
            ToolKind::Search,
            "Searching the web for: \"rust borrow\"",
            &[]
        ),
        Some(r#"{"query":"rust borrow"}"#.to_string())
    );
    assert_eq!(
        synth(
            ToolKind::Fetch,
            "Fetching content from: https://example.com",
            &[]
        ),
        Some(r#"{"url":"https://example.com"}"#.to_string())
    );

    // The deliberate omissions. glob and grep are BOTH `'<pattern>'` under
    // kind `search`, so a `{"pattern": …}` guess would render every glob as
    // a grep; web-fetch's prompt variant has already truncated its argument.
    assert_eq!(
        synth(ToolKind::Search, "'**/*.rs'", &[]),
        None,
        "a bare pattern is glob, or grep on a machine without ripgrep — do not guess"
    );
    assert_eq!(
        synth(ToolKind::Search, "'TODO' within ./", &[]),
        None,
        "ripgrep's ` within …` shape is still a pattern search, not a web search"
    );
    assert_eq!(
        synth(
            ToolKind::Fetch,
            "Processing URLs and instructions from prompt: \"summarize http://a...\"",
            &[],
        ),
        None,
        "the prompt variant is truncated to 97 chars; the argument is already gone"
    );
    // Every MCP tool is hardcoded to `other` (`DiscoveredMCPTool`), and its
    // `getDisplayTitle()` returns a `command` PARAM when the server happens
    // to define one — which would look exactly like a shell call.
    assert_eq!(
        synth(ToolKind::Other, "rm -rf /tmp/cache", &[]),
        None,
        "an MCP tool's title can impersonate a command; never synthesize for `other`"
    );

    assert_eq!(
        gemini_synthesize_tool_input(
            AgentType::ClaudeCode,
            &ToolKind::Execute,
            "pnpm eslint .",
            &[],
        ),
        None,
        "agents that send a real rawInput must never get a synthesized one"
    );
}

/// Contrast guard: the classifier is pi-gated, so another agent that happens
/// to say one of these sentences — or that uses a `piAcp` meta key of its own
/// — keeps today's behavior.
#[test]
fn pi_chunk_route_is_inert_for_other_agents() {
    let meta = serde_json::json!({"piAcp": {"notify": {"level": "info"}}});
    let meta = meta.as_object().cloned().expect("object meta");
    for agent in [
        AgentType::ClaudeCode,
        AgentType::Codex,
        AgentType::Grok,
        // Same known limitation the rest of the pi bridge carries: pi-acp
        // registered under a CUSTOM id is not `AgentType::Pi`, so it keeps
        // the old behavior rather than an unnamespaced marker applying to
        // arbitrary agents (see `hosted_terminal_meta_marks_shell`).
        AgentType::Custom("my-pi"),
    ] {
        for text in [
            "Retry finished, resuming.",
            "Retrying (attempt 1/3, waiting 2s)...",
            "Cleared queued prompts.",
        ] {
            assert_eq!(
                pi_message_chunk_route(agent, text, Some(&meta)),
                PiChunkRoute::Prose,
                "{agent:?} must be unaffected by the pi bridge"
            );
        }
    }
}

/// #525 proper: the caffeinate frame must produce NO event, so it can neither
/// paint a bubble of its own nor splice into the reply already on screen.
#[tokio::test]
async fn pi_notify_chunk_emits_nothing_at_all() {
    let events = pi_emit_chunk(
        AgentType::Pi,
        pi_notify_chunk("Released pi-caffeinate (agent finished).", "info"),
    )
    .await;
    assert!(
        events.is_empty(),
        "an extension notify must not reach any channel: {events:?}"
    );
}

/// Retry leaves the transcript and lands on the shared banner, carrying pi's
/// own counters so the banner can render its localized line.
#[tokio::test]
async fn pi_retry_chunk_becomes_the_retry_banner_with_counters() {
    let events = pi_emit_chunk(
        AgentType::Pi,
        pi_chunk("Retrying (attempt 2/3, waiting 4s)..."),
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AcpEvent::ContentDelta { .. })),
        "the retry sentence must not reach the transcript: {events:?}"
    );
    let retrying = events
        .iter()
        .find_map(|e| match e {
            AcpEvent::TurnRetrying {
                message,
                attempt,
                max_retries,
                retry_delay_ms,
                ..
            } => Some((message.clone(), *attempt, *max_retries, *retry_delay_ms)),
            _ => None,
        })
        .expect("a TurnRetrying event is emitted");
    assert_eq!(retrying, (String::new(), Some(2), Some(3), Some(4000)));
}

/// The other half of the contract: ordinary pi prose is untouched.
#[tokio::test]
async fn pi_prose_chunk_still_emits_content_delta() {
    let events = pi_emit_chunk(AgentType::Pi, pi_chunk("你好。有什么需要我帮你处理?")).await;
    let text = events
        .iter()
        .find_map(|e| match e {
            AcpEvent::ContentDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("prose still reaches the transcript");
    assert_eq!(text, "你好。有什么需要我帮你处理?");
}

/// The invariant that keeps the renderer and the empty-turn diagnosis in
/// agreement: a chunk codeg does not render must not count as the agent
/// having produced output, or a status-only turn ends blank AND successful.
#[test]
fn pi_status_chunks_do_not_count_as_agent_output() {
    let status: SessionUpdate = serde_json::from_value(pi_notify_chunk(
        "Keeping computer awake (display-awake).",
        "info",
    ))
    .expect("valid wire shape");
    let retry: SessionUpdate =
        serde_json::from_value(pi_chunk("Retrying (attempt 1/3, waiting 2s)..."))
            .expect("valid wire shape");
    let prose: SessionUpdate =
        serde_json::from_value(pi_chunk("是的，插件已加载。")).expect("valid wire shape");

    assert!(!is_agent_output_update(AgentType::Pi, &status));
    assert!(!is_agent_output_update(AgentType::Pi, &retry));
    assert!(is_agent_output_update(AgentType::Pi, &prose));
    // Same frames, another agent: unchanged.
    assert!(is_agent_output_update(AgentType::ClaudeCode, &status));
    assert!(is_agent_output_update(AgentType::ClaudeCode, &retry));
}

/// The frames below are a VERBATIM capture from a real `pi-acp@0.0.33`,
/// driven against a stub `pi --mode rpc` via the supported `PI_ACP_PI_COMMAND`
/// override, so this test asserts against the wire rather than against my
/// reading of pi-acp's source.
///
/// It pins the two properties the whole fix rests on:
///
/// 1. `_meta.piAcp.notify.level` really does reach the client, so the notify
///    rule has a structured handle and never has to guess from the text.
/// 2. Each announcement arrives as ONE COMPLETE chunk, while real prose
///    arrives in fragments (`是的，` / `OV 插件` / `已加载。`) — which is what
///    makes whole-string matching safe. A prose delta is a fragment of a
///    sentence; it is not a whole sentence with terminal punctuation.
#[test]
fn pi_captured_wire_frames_route_as_expected() {
    let captured = serde_json::json!([
        {"sessionUpdate": "agent_message_chunk",
         "content": {"type": "text", "text": "Released pi-caffeinate (agent finished)."},
         "_meta": {"piAcp": {"notify": {"level": "info"}}}},
        {"sessionUpdate": "agent_message_chunk",
         "content": {"type": "text", "text": "Retrying (attempt 1/3, waiting 2s)..."}},
        {"sessionUpdate": "agent_message_chunk",
         "content": {"type": "text", "text": "Retry finished, resuming."}},
        {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "是的，"}},
        {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "OV 插件"}},
        {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "已加载。"}},
    ]);

    let routes: Vec<PiChunkRoute> = captured
        .as_array()
        .expect("captured frames")
        .iter()
        .map(|frame| {
            let text = frame["content"]["text"].as_str().expect("text chunk");
            let meta = frame.get("_meta").and_then(|m| m.as_object()).cloned();
            pi_message_chunk_route(AgentType::Pi, text, meta.as_ref())
        })
        .collect();

    assert_eq!(
        routes,
        vec![
            PiChunkRoute::Drop,
            PiChunkRoute::Retrying {
                attempt: Some(1),
                max: Some(3),
                delay_ms: Some(2000),
            },
            PiChunkRoute::Drop,
            PiChunkRoute::Prose,
            PiChunkRoute::Prose,
            PiChunkRoute::Prose,
        ],
        "the reply survives whole; only pi-acp's announcements are taken out"
    );
}

/// End to end: a turn whose ONLY chunks were pi status is diagnosed as
/// metadata-only, so it reports "sent only status updates and no reply"
/// instead of completing as a silent blank turn.
#[test]
fn pi_status_only_turn_diagnoses_as_metadata_only() {
    let mut probe = TurnOutputProbe::new(0);
    for wire in [
        pi_notify_chunk("Keeping computer awake (display-awake).", "info"),
        pi_chunk("Retrying (attempt 1/3, waiting 2s)..."),
        pi_chunk("Retry finished, resuming."),
    ] {
        probe.note_update(
            AgentType::Pi,
            &serde_json::from_value(wire).expect("valid wire shape"),
        );
    }
    assert!(!probe.saw_agent_output);
    assert_eq!(diagnose_empty_turn(&probe), EmptyTurnCause::MetadataOnly);

    // One line of real prose is all it takes to make the turn non-empty.
    probe.note_update(
        AgentType::Pi,
        &serde_json::from_value(pi_chunk("是的，插件已加载。")).expect("valid wire shape"),
    );
    assert!(probe.saw_agent_output);
}

#[test]
fn unwrap_grok_use_tool_peels_mcp_envelope() {
    // Grok's `use_tool` envelope nests the real MCP tool name + args.
    let raw = serde_json::json!({
        "tool_name": "codeg-mcp__delegate_to_agent",
        "tool_input": {"agent_type": "codex", "task": "build", "working_dir": "/w"},
    });
    let (name, input) = unwrap_grok_use_tool(Some(&raw)).expect("envelope peels");
    assert_eq!(name, "codeg-mcp__delegate_to_agent");
    assert_eq!(input.get("task").and_then(|v| v.as_str()), Some("build"));
    assert_eq!(
        input.get("agent_type").and_then(|v| v.as_str()),
        Some("codex")
    );
}

#[test]
fn unwrap_grok_use_tool_ignores_native_tools() {
    // Native Grok tools carry args directly (no tool_name/tool_input shape) —
    // they must pass through untouched.
    let terminal = serde_json::json!({"command": "pnpm build"});
    assert!(unwrap_grok_use_tool(Some(&terminal)).is_none());
    // Missing tool_input.
    assert!(unwrap_grok_use_tool(Some(&serde_json::json!({"tool_name": "x"}))).is_none());
    // Empty tool_name.
    assert!(unwrap_grok_use_tool(Some(
        &serde_json::json!({"tool_name": "", "tool_input": {}})
    ))
    .is_none());
    // Absent / non-object.
    assert!(unwrap_grok_use_tool(None).is_none());
    assert!(unwrap_grok_use_tool(Some(&serde_json::json!("s"))).is_none());
}

#[test]
fn grok_mcp_output_text_extracts_result() {
    // `{type:MCP, output:{OkayOutput:"…"}}` — text is the first string value.
    let ok = serde_json::json!({
        "type": "MCP",
        "tool_name": "delegate_to_agent",
        "output": {"OkayOutput": "Delegation successful. task_id=abc-123."},
    });
    assert_eq!(
        grok_mcp_output_text(&ok).as_deref(),
        Some("Delegation successful. task_id=abc-123.")
    );
    // `output` may be a bare string.
    let bare = serde_json::json!({"type": "MCP", "output": "done"});
    assert_eq!(grok_mcp_output_text(&bare).as_deref(), Some("done"));
    // An empty-string sibling (sorted before the real key) must not shadow
    // the populated result.
    let empty_first = serde_json::json!({
        "type": "MCP",
        "output": {"AErr": "", "OkayOutput": "real result"},
    });
    assert_eq!(
        grok_mcp_output_text(&empty_first).as_deref(),
        Some("real result")
    );
    // A pure error variant (any `*Output` key) is surfaced too.
    let err = serde_json::json!({"type": "MCP", "output": {"ErrOutput": "boom"}});
    assert_eq!(grok_mcp_output_text(&err).as_deref(), Some("boom"));
    // Non-MCP rawOutput → None (caller falls through to output_for_prompt).
    let bash = serde_json::json!({"type": "Bash", "output_for_prompt": "ok"});
    assert_eq!(grok_mcp_output_text(&bash), None);
}

#[test]
fn cursor_companion_title_resolves_delegate_ack() {
    // The broker's running ack (broker.rs::running_ack) — leading
    // whitespace tolerated, the prefix is the contract.
    let ack = "Delegation successful. task_id=799467c7-0188-4e7a-b5ef-241d4b141a83. \
                   Call get_delegation_status with this id in the task_ids array.";
    assert_eq!(
        cursor_companion_title_from_content(Some(ack)),
        Some("codeg-mcp__delegate_to_agent")
    );
    assert_eq!(
        cursor_companion_title_from_content(Some(&format!("  {ack}"))),
        Some("codeg-mcp__delegate_to_agent")
    );
}

#[test]
fn cursor_companion_title_resolves_status_report() {
    // Real-device shape: companion.rs::render_batch_report's compact JSON.
    let report = r#"{"tasks":[{"agent_type":"claude_code","child_conversation_id":1576,"duration_ms":27288,"status":"completed","task_id":"799467c7-0188-4e7a-b5ef-241d4b141a83","text":"done"}]}"#;
    assert_eq!(
        cursor_companion_title_from_content(Some(report)),
        Some("codeg-mcp__get_delegation_status")
    );
    // Mixed batch with a running item still resolves.
    let mixed =
        r#"{"tasks":[{"task_id":"a","status":"running"},{"task_id":"b","status":"unknown"}]}"#;
    assert_eq!(
        cursor_companion_title_from_content(Some(mixed)),
        Some("codeg-mcp__get_delegation_status")
    );
}

#[test]
fn cursor_companion_title_rejects_lookalikes() {
    // Foreign task-manager output: status outside the report vocabulary.
    let foreign =
        r#"{"tasks":[{"task_id":"T-1","status":"todo"},{"task_id":"T-2","status":"done"}]}"#;
    assert_eq!(cursor_companion_title_from_content(Some(foreign)), None);
    // Item missing task_id.
    let missing = r#"{"tasks":[{"status":"completed"}]}"#;
    assert_eq!(cursor_companion_title_from_content(Some(missing)), None);
    // Empty batch carries nothing to verify — leave the title alone.
    assert_eq!(
        cursor_companion_title_from_content(Some(r#"{"tasks":[]}"#)),
        None
    );
    // Plain text / absent / non-JSON.
    assert_eq!(cursor_companion_title_from_content(Some("ls -la ok")), None);
    assert_eq!(cursor_companion_title_from_content(None), None);
    // Ack prefix must match from the start, not mid-string.
    assert_eq!(
        cursor_companion_title_from_content(Some("Note: Delegation successful. task_id=x.")),
        None
    );
}

#[test]
fn grok_live_tool_output_recovers_mcp_result() {
    // An MCP call (delegate ack) has empty content and no output_for_prompt;
    // the readable text lives under `output.OkayOutput`.
    let raw = Some(serde_json::json!({
        "type": "MCP",
        "tool_name": "delegate_to_agent",
        "server_name": "codeg-mcp",
        "output": {"OkayOutput": "Delegation successful. task_id=2dc85849-5426."},
    }));
    assert_eq!(
        grok_live_tool_output(&None, &raw).as_deref(),
        Some("Delegation successful. task_id=2dc85849-5426.")
    );
}

/// Grok wraps `delegate_to_agent` in a `use_tool` envelope. The live path must
/// peel it so the emitted event carries the MCP tool name as its title and the
/// real `{agent_type, task}` as raw_input — the exact shape the delegation
/// broker (`lifecycle.rs`) correlates on and the frontend classifies into the
/// delegation card — and must surface the MCP ack (carrying `task_id`) as
/// output instead of dropping it.
#[tokio::test]
async fn grok_use_tool_delegate_unwraps_to_direct_mcp_call() {
    let st = SessionState::new(
        "conn-grok".to_string(),
        AgentType::Grok,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();

    // Initial tool_call carries the use_tool envelope (real Grok wire shape —
    // no kind/status on the update object; they default).
    let call: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-d",
        "title": "use_tool",
        "rawInput": {
            "tool_name": "codeg-mcp__delegate_to_agent",
            "tool_input": {"agent_type": "codex", "working_dir": "/w", "task": "run build"},
        },
    }))
    .expect("valid tool_call wire shape");
    emit_conversation_update(
        &state,
        &emitter,
        AgentType::Grok,
        call,
        None,
        &mut cache,
        &mut cb,
    )
    .await;

    // The ack arrives on the completed update as an MCP rawOutput. Real Grok
    // updates re-send the generic `use_tool` wrapper title and carry NO
    // raw_input — the recorded override must re-assert the peeled name so the
    // frontend reducer doesn't revert the delegation card to a generic tool.
    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-d",
        "title": "use_tool",
        "status": "completed",
        "rawOutput": {
            "type": "MCP",
            "tool_name": "delegate_to_agent",
            "server_name": "codeg-mcp",
            "output": {"OkayOutput": "Delegation successful. task_id=2dc85849-5426-44f7."},
        },
    }))
    .expect("valid tool_call_update wire shape");
    emit_conversation_update(
        &state,
        &emitter,
        AgentType::Grok,
        update,
        None,
        &mut cache,
        &mut cb,
    )
    .await;

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");

    // Initial ToolCall: title unwrapped to the MCP tool name; raw_input the
    // real delegation args (the `use_tool` wrapper gone).
    let (title, raw_input) = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCall {
                title, raw_input, ..
            } => Some((title.clone(), raw_input.clone())),
            _ => None,
        })
        .expect("a tool_call event is emitted");
    assert_eq!(title, "codeg-mcp__delegate_to_agent");
    let raw_input = raw_input.expect("raw_input present after unwrap");
    assert!(
        raw_input.contains("\"agent_type\":\"codex\""),
        "raw_input carries agent_type: {raw_input}"
    );
    assert!(
        raw_input.contains("\"task\":\"run build\""),
        "raw_input carries task: {raw_input}"
    );
    assert!(
        !raw_input.contains("tool_input"),
        "the use_tool wrapper is peeled: {raw_input}"
    );

    // Update: the MCP ack (with task_id) surfaces as output, AND the emitted
    // title re-asserts the peeled name — the sparse `use_tool` wrapper title
    // must not win.
    let (upd_title, raw_output) = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCallUpdate {
                title, raw_output, ..
            } => raw_output.clone().map(|o| (title.clone(), o)),
            _ => None,
        })
        .expect("a tool_call_update with output is emitted");
    assert!(
        raw_output.contains("task_id=2dc85849"),
        "the delegate ack (with task_id) surfaces as output: {raw_output}"
    );
    assert_eq!(
        upd_title.as_deref(),
        Some("codeg-mcp__delegate_to_agent"),
        "the sparse-update wrapper title is overridden by the recorded name"
    );
    // No emitted event ever ships the generic `use_tool` wrapper title.
    assert!(
        events.iter().all(|e| !matches!(
            &e.payload,
            AcpEvent::ToolCall { title, .. } if title == "use_tool"
        ) && !matches!(
            &e.payload,
            AcpEvent::ToolCallUpdate { title: Some(t), .. } if t == "use_tool"
        )),
        "no event ships the generic use_tool wrapper title"
    );
}

/// The unwrap is symmetric on the ToolCallUpdate arm: an update that itself
/// carries the `use_tool` envelope (rawInput) is peeled the same way — title →
/// MCP name, raw_input → the inner args.
#[tokio::test]
async fn grok_use_tool_envelope_on_update_is_unwrapped() {
    let st = SessionState::new(
        "conn-grok".to_string(),
        AgentType::Grok,
        None,
        "win".to_string(),
        None,
    );
    let state = Arc::new(RwLock::new(st));
    let emitter = EventEmitter::Noop;
    let mut cache = ToolCallOutputCache::default();
    let mut cb = CodeBuddyLiveState::default();

    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-u",
        "title": "use_tool",
        "status": "in_progress",
        "rawInput": {
            "tool_name": "codeg-mcp__cancel_delegation",
            "tool_input": {"task_id": "abc-123"},
        },
    }))
    .expect("valid tool_call_update wire shape");
    emit_conversation_update(
        &state,
        &emitter,
        AgentType::Grok,
        update,
        None,
        &mut cache,
        &mut cb,
    )
    .await;

    let guard = state.read().await;
    let events = guard.recent_events_after(0).expect("events recorded");
    let (title, raw_input) = events
        .iter()
        .find_map(|e| match &e.payload {
            AcpEvent::ToolCallUpdate {
                title, raw_input, ..
            } => Some((title.clone(), raw_input.clone())),
            _ => None,
        })
        .expect("a tool_call_update event is emitted");
    assert_eq!(title.as_deref(), Some("codeg-mcp__cancel_delegation"));
    let raw_input = raw_input.expect("raw_input present after unwrap");
    assert!(
        raw_input.contains("\"task_id\":\"abc-123\""),
        "inner args surface as raw_input: {raw_input}"
    );
    assert!(
        !raw_input.contains("tool_input"),
        "the use_tool wrapper is peeled: {raw_input}"
    );
}

#[test]
fn canonical_spec_to_mcp_server_http_with_headers() {
    let spec = serde_json::json!({
        "type": "http",
        "url": "https://example.com/mcp",
        "headers": {"Authorization": "Bearer token"},
    });
    let server = canonical_spec_to_mcp_server("remote", &spec).expect("http spec should map");
    match server {
        McpServer::Http(s) => {
            assert_eq!(s.url, "https://example.com/mcp");
            assert_eq!(s.headers.len(), 1);
            assert_eq!(s.headers[0].name, "Authorization");
        }
        other => panic!("expected Http variant, got {other:?}"),
    }
}

#[test]
fn canonical_spec_to_mcp_server_rejects_unknown_type() {
    let spec = serde_json::json!({"type": "websocket", "url": "wss://x"});
    assert!(canonical_spec_to_mcp_server("x", &spec).is_err());
}

#[test]
fn stdio_server_serializes_to_acp_wire_format() {
    // Replicates the Figma MCP entry shipped to the agent and asserts the
    // exact JSON shape claude-agent-acp expects (no `type` tag for stdio,
    // env as [{name, value}] array, command as a string path).
    let spec = serde_json::json!({
        "type": "stdio",
        "command": "/usr/local/bin/npx",
        "args": ["-y", "@mcp_hub_org/cli@latest", "run", "figma-developer-mcp"],
    });
    let server = canonical_spec_to_mcp_server("figma", &spec).expect("stdio spec should map");
    let json = serde_json::to_value(&server).expect("server should serialize");
    assert_eq!(json["name"], "figma");
    assert_eq!(json["command"], "/usr/local/bin/npx");
    assert_eq!(json["args"][0], "-y");
    assert_eq!(json["args"][1], "@mcp_hub_org/cli@latest");
    assert!(
        json.get("type").is_none(),
        "stdio variant must serialize without a `type` tag (claude-agent-acp \
             treats absence-of-type as stdio); got {json:#?}"
    );
}

// ─── ToolCallOutputCache ────────────────────────────────────────────

#[test]
fn cache_first_update_emits_full_replace() {
    let mut cache = ToolCallOutputCache::default();
    let (payload, append) = cache.consume("t1", "hello world").expect("should emit");
    assert_eq!(payload, "hello world");
    assert!(!append, "first emit must be replacement");
}

#[test]
fn cache_repeated_identical_snapshot_is_noop() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "same").unwrap();
    assert!(
        cache.consume("t1", "same").is_none(),
        "identical snapshot must not emit"
    );
}

#[test]
fn cache_prefix_extension_emits_suffix_with_append() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "line-1\n").unwrap();
    let (payload, append) = cache
        .consume("t1", "line-1\nline-2\n")
        .expect("should emit");
    assert_eq!(payload, "line-2\n");
    assert!(append, "prefix extension must emit with append=true");
}

#[test]
fn cache_divergent_snapshot_falls_back_to_replace() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "hello world").unwrap();
    let (payload, append) = cache.consume("t1", "foo bar baz").expect("should emit");
    assert_eq!(payload, "foo bar baz");
    assert!(!append, "non-extension snapshot must replace");
}

#[test]
fn cache_tracks_extensions_past_cached_tail_boundary() {
    // Regression test for the original bug: when cumulative raw_output
    // exceeds MAX_CACHED_TAIL_BYTES, subsequent extensions must still be
    // detectable by comparing the cached tail against the expected
    // offset in the incoming snapshot.
    let mut cache = ToolCallOutputCache::default();
    // First snapshot: 10 KB of 'a' + unique 4 KB marker at the end.
    let prefix = "a".repeat(10 * 1024);
    let marker = "M".repeat(4 * 1024);
    let first = format!("{prefix}{marker}");
    cache.consume("t1", &first).unwrap();

    // Second snapshot extends first by 16 KB of 'Z'.
    let delta = "Z".repeat(16 * 1024);
    let second = format!("{first}{delta}");
    let (payload, append) = cache.consume("t1", &second).expect("should emit");
    assert!(
        append,
        "extension beyond cached tail must still be detected"
    );
    // The emitted payload should carry the delta (or its tail when
    // truncated at MAX_SINGLE_EMIT_BYTES). For a 16 KB delta that's
    // well below the 64 KB cap, we expect it verbatim.
    assert_eq!(payload, delta);
}

#[test]
fn cache_extension_larger_than_emit_cap_gets_truncated() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "seed").unwrap();
    // Build a delta much larger than MAX_SINGLE_EMIT_BYTES.
    let big_delta = "X".repeat(MAX_SINGLE_EMIT_BYTES * 2);
    let second = format!("seed{big_delta}");
    let (payload, append) = cache.consume("t1", &second).expect("should emit");
    assert!(append);
    assert!(
        payload.starts_with(TRUNCATION_MARKER),
        "oversized delta must be prefixed with truncation marker"
    );
    // Payload length: marker + at most MAX_SINGLE_EMIT_BYTES of tail.
    assert!(payload.len() <= TRUNCATION_MARKER.len() + MAX_SINGLE_EMIT_BYTES);
}

#[test]
fn cache_respects_utf8_char_boundary_on_truncation() {
    let mut cache = ToolCallOutputCache::default();
    // Single first-update whose byte length forces truncation at a
    // position that would otherwise fall mid-codepoint. 中 is 3 bytes
    // (E4 B8 AD) and MAX_SINGLE_EMIT_BYTES (65536) is not a multiple
    // of 3, so naïve byte slicing would land mid-char.
    let chinese_block = "中".repeat((MAX_SINGLE_EMIT_BYTES / 3) + 100);
    let (payload, _append) = cache.consume("t1", &chinese_block).expect("should emit");
    // Payload must start with the truncation marker (since size > cap).
    assert!(
        payload.starts_with(TRUNCATION_MARKER),
        "oversized snapshot must be truncated"
    );
    // Body after the marker must be valid UTF-8 consisting only of 中.
    let body = &payload[TRUNCATION_MARKER.len()..];
    assert!(!body.is_empty());
    assert!(
        body.chars().all(|c| c == '中'),
        "truncation boundary must land on a UTF-8 codepoint edge"
    );
}

#[test]
fn cache_final_status_clears_entry() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "hello").unwrap();
    assert!(cache.entries.contains_key("t1"));
    cache.remove_if_final("t1", Some("completed"));
    assert!(!cache.entries.contains_key("t1"));

    cache.consume("t2", "x").unwrap();
    cache.remove_if_final("t2", Some("cancelled"));
    assert!(!cache.entries.contains_key("t2"));

    cache.consume("t3", "x").unwrap();
    cache.remove_if_final("t3", Some("in_progress"));
    assert!(
        cache.entries.contains_key("t3"),
        "in-progress status must not clear cache"
    );
}

#[test]
fn cache_enforces_entry_cap_via_fifo_eviction() {
    let mut cache = ToolCallOutputCache::default();
    for i in 0..(MAX_CACHE_ENTRIES + 50) {
        cache.consume(&format!("tool-{i}"), "body").unwrap();
    }
    assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
    // Oldest entries should have been evicted; newest must still exist.
    assert!(!cache.entries.contains_key("tool-0"));
    assert!(cache
        .entries
        .contains_key(&format!("tool-{}", MAX_CACHE_ENTRIES + 49)));
}

#[test]
fn cache_seed_always_replaces_and_caches() {
    let mut cache = ToolCallOutputCache::default();
    cache.consume("t1", "stale").unwrap();
    // A hypothetical replay would send another ToolCall for the same
    // id — seed() must install the new snapshot without trying to
    // diff against the stale prior entry.
    let payload = cache.seed("t1", "fresh").expect("seed emits");
    assert_eq!(payload, "fresh");
    // Next consume should diff against "fresh", not "stale".
    let (p2, append) = cache.consume("t1", "fresh+more").expect("emit");
    assert!(append, "should detect extension of freshly seeded entry");
    assert_eq!(p2, "+more");
}

// ─── merge_agent_env_with_color ─────────────────────────────────────

fn merged_value<'a>(merged: &'a [(String, String)], key: &str) -> Option<&'a str> {
    merged
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// The default launch must NOT force color — with EVERY variable checked,
/// not just the first one. Both force flags are unvetoable downstream
/// (`CLICOLOR_FORCE` outranks `NO_COLOR` by convention; `supports-color`
/// reads `FORCE_COLOR` first), so injecting either leaves no way for an
/// agent-run `gh … --json` to get parseable output back.
///
/// `TERM` is here for a different reason: it is the one entry that
/// overwrites an inherited value rather than filling in a missing one, so
/// leaking it into the default path would change what every un-opted-in
/// launch reports about its terminal.
/// The precedence that IS the temp-leak fix.
///
/// A self-extracting agent resolves its unpack root from `TMP` before
/// `TEMP` (Windows `GetTempPathW`). If a per-agent `env_json` `TMP` were
/// allowed to win — which it would under the ordinary `runtime_env`-last
/// rule every other variable follows — the extraction would land wherever
/// that points while codeg deleted an empty scratch directory and reported
/// the leak fixed. The whole fix is this one ordering, so it gets a test.
#[test]
fn scratch_dir_outranks_a_per_agent_temp_override() {
    let mut runtime_env = BTreeMap::new();
    for key in crate::acp::scratch_dir::TEMP_ENV_KEYS {
        runtime_env.insert(key.to_string(), "/somewhere/the/user/picked".to_string());
    }
    let scratch = Path::new("/scratch/codeg-acp/123-deadbeef");

    let merged = merge_agent_env_with_color(false, &[], &runtime_env, Some(scratch));

    for key in crate::acp::scratch_dir::TEMP_ENV_KEYS {
        assert_eq!(
            merged_value(&merged, key),
            Some("/scratch/codeg-acp/123-deadbeef"),
            "{key} must point at the scratch dir, not the per-agent override"
        );
    }
}

/// All three names, every time. Setting only `TMPDIR` would leave `TMP`
/// inherited from codeg's own environment, and `GetTempPathW` reads `TMP`
/// first.
#[test]
fn scratch_dir_sets_every_temp_variable_the_child_might_read() {
    let merged =
        merge_agent_env_with_color(false, &[], &BTreeMap::new(), Some(Path::new("/scratch/x")));
    for key in crate::acp::scratch_dir::TEMP_ENV_KEYS {
        assert_eq!(merged_value(&merged, key), Some("/scratch/x"), "{key}");
    }
}

/// No scratch dir (isolation off, or the directory could not be created)
/// must leave the environment exactly as it was — the child then inherits
/// the ambient temp dir, which is the pre-fix behaviour and a working
/// launch.
#[test]
fn without_a_scratch_dir_the_temp_variables_are_untouched() {
    let mut runtime_env = BTreeMap::new();
    runtime_env.insert("TMP".to_string(), "/user/choice".to_string());

    let merged = merge_agent_env_with_color(false, &[], &runtime_env, None);

    assert_eq!(merged_value(&merged, "TMP"), Some("/user/choice"));
    assert_eq!(merged_value(&merged, "TEMP"), None);
    assert_eq!(merged_value(&merged, "TMPDIR"), None);
}

#[test]
fn merge_agent_env_omits_the_color_env_by_default() {
    let merged = merge_agent_env_with_color(false, &[], &BTreeMap::new(), None);
    for key in ["CLICOLOR", "CLICOLOR_FORCE", "FORCE_COLOR", "TERM"] {
        assert_eq!(merged_value(&merged, key), None, "{key} must not be set");
    }
}

/// All four, because they cover disjoint decisions: `CLICOLOR` enables color
/// for the BSD family at all, `CLICOLOR_FORCE` waives the `isatty` check
/// that family (and go-gh) makes, `FORCE_COLOR` covers everything npm-based
/// (which ignores both `CLICOLOR*` outright), and `TERM` is what the
/// terminfo lookup needs before any of them can pick a palette.
///
/// Splitting `CLICOLOR` out is not pedantry: with the other three but not
/// it, a bare `ls` still comes back monochrome, which is the exact symptom
/// this setting exists to fix.
#[test]
fn merge_agent_env_injects_the_whole_color_env_when_opted_in() {
    let merged = merge_agent_env_with_color(true, &[], &BTreeMap::new(), None);
    assert_eq!(merged_value(&merged, "CLICOLOR"), Some("1"));
    assert_eq!(merged_value(&merged, "CLICOLOR_FORCE"), Some("1"));
    assert_eq!(merged_value(&merged, "FORCE_COLOR"), Some("1"));
    assert_eq!(merged_value(&merged, "TERM"), Some("xterm-256color"));
}

/// The opt-in is a DEFAULT, not an override: a per-agent env row still
/// wins — for every injected variable, so the escape hatch is not half a
/// hatch. A user who turned the toggle on globally can exempt one agent and
/// get machine-parseable output back from it.
///
/// Empty rather than `"0"` for the BSD pair on purpose: those are
/// presence-checked, so `CLICOLOR_FORCE=0` still reads as forced. An empty
/// value is what the spawn layer turns into `env_remove`, and this asserts
/// the row reaches the merge intact so that removal can happen.
#[test]
fn runtime_env_still_outranks_the_color_default() {
    let runtime_env = BTreeMap::from([
        ("CLICOLOR".to_string(), String::new()),
        ("CLICOLOR_FORCE".to_string(), String::new()),
        ("FORCE_COLOR".to_string(), "0".to_string()),
        ("TERM".to_string(), "dumb".to_string()),
    ]);
    let merged = merge_agent_env_with_color(true, &[], &runtime_env, None);
    assert_eq!(merged_value(&merged, "CLICOLOR"), Some(""));
    assert_eq!(merged_value(&merged, "CLICOLOR_FORCE"), Some(""));
    assert_eq!(merged_value(&merged, "FORCE_COLOR"), Some("0"));
    assert_eq!(merged_value(&merged, "TERM"), Some("dumb"));
}

/// Turning the toggle off must not disturb anything else the merge does —
/// the registry env and the per-agent row still land.
#[test]
fn merge_agent_env_without_color_keeps_other_layers() {
    let runtime_env = BTreeMap::from([("FROM_ROW".to_string(), "row".to_string())]);
    let merged =
        merge_agent_env_with_color(false, &[("FROM_REGISTRY", "registry")], &runtime_env, None);
    assert_eq!(merged_value(&merged, "FROM_REGISTRY"), Some("registry"));
    assert_eq!(merged_value(&merged, "FROM_ROW"), Some("row"));
}

/// A launch that carries a proxy leaves with the loopback exception, in
/// both spellings — the agent's own local services (OpenCode's embedded
/// server, Antigravity's harness socket) must never be sent to it. The proxy
/// comes from the per-agent row so the test does not depend on this
/// machine's environment; `*` is what an inherited `NO_PROXY=*` yields.
#[test]
fn a_proxied_launch_env_carries_the_loopback_exception() {
    let runtime_env =
        BTreeMap::from([("HTTP_PROXY".to_string(), "http://10.0.0.2:3128".to_string())]);
    let merged = merge_agent_env_with_color(false, &[], &runtime_env, None);
    for key in ["NO_PROXY", "no_proxy"] {
        let value = merged_value(&merged, key).unwrap_or_default();
        assert!(
            value.starts_with("localhost,127.0.0.1,::1,[::1]") || value == "*",
            "{key}: {value:?}"
        );
    }
}

// ─── trim_partial_ansi_tail ─────────────────────────────────────────

#[test]
fn ansi_trim_leaves_pure_text_unchanged() {
    assert_eq!(trim_partial_ansi_tail("plain text"), "plain text");
}

#[test]
fn ansi_trim_keeps_completed_sequences() {
    let s = "\x1b[31mRED\x1b[0m done";
    assert_eq!(trim_partial_ansi_tail(s), s);
}

#[test]
fn ansi_trim_cuts_unterminated_trailing_sequence() {
    let s = "hello \x1b[31";
    assert_eq!(trim_partial_ansi_tail(s), "hello ");
}

#[test]
fn ansi_trim_handles_bare_escape_at_end() {
    let s = "hello\x1b";
    assert_eq!(trim_partial_ansi_tail(s), "hello");
}

// ─── truncate_tail_at_char_boundary ─────────────────────────────────

#[test]
fn truncate_under_cap_returns_as_is() {
    assert_eq!(truncate_tail_at_char_boundary("abc", 10), "abc");
}

#[test]
fn truncate_returns_tail_on_overflow() {
    assert_eq!(truncate_tail_at_char_boundary("abcdef", 3), "def");
}

#[test]
fn truncate_respects_multibyte_utf8_boundary() {
    // "中中中" is 9 bytes; asking for 4 bytes would land mid-char.
    let s = "中中中";
    let out = truncate_tail_at_char_boundary(s, 4);
    // Must be valid UTF-8 (indexing an invalid boundary would have
    // panicked at slicing time).
    assert!(out.chars().all(|c| c == '中'));
    assert!(out.len() <= 6); // at most 2 chars (6 bytes)
}

// ─── is_subagent_invocation ─────────────────────────────────

#[test]
fn subagent_detects_opencode_with_subagent_type_regardless_of_title() {
    // OpenCode's ACP title is the user-facing description (e.g. the
    // task's `description` field), NOT the internal tool name. The
    // historical-parser equivalent at parsers/opencode.rs:425-429
    // anchors on `tool == "task"`, which we can't replicate here
    // because ACP doesn't expose the internal tool name — so we rely
    // solely on agent_type + subagent_type. Verify the detection
    // triggers regardless of the title shape.
    let input = Some(r#"{"subagent_type":"researcher","prompt":"x"}"#.to_string());
    assert!(is_subagent_invocation(AgentType::OpenCode, &input));
}

#[test]
fn subagent_gates_on_supported_agent_types() {
    // OpenCode and CodeBuddy both rewrite a `subagent_type`-bearing call to
    // the Agent card; other agents stay excluded so a generic `subagent_type`
    // field never triggers a cross-agent collision.
    let input = Some(r#"{"subagent_type":"x"}"#.to_string());
    assert!(is_subagent_invocation(AgentType::OpenCode, &input));
    assert!(is_subagent_invocation(AgentType::CodeBuddy, &input));
    assert!(!is_subagent_invocation(AgentType::ClaudeCode, &input));
    assert!(!is_subagent_invocation(AgentType::Codex, &input));
}

#[test]
fn subagent_rejects_empty_or_non_string_subagent_type() {
    for raw in [
        r#"{"subagent_type":""}"#,
        r#"{"subagent_type":null}"#,
        r#"{"subagent_type":42}"#,
        r#"{"subagent_type":["a"]}"#,
    ] {
        assert!(
            !is_subagent_invocation(AgentType::OpenCode, &Some(raw.to_string())),
            "expected false for raw_input={raw}"
        );
    }
}

#[test]
fn subagent_rejects_none_malformed_or_non_object_root() {
    assert!(!is_subagent_invocation(AgentType::OpenCode, &None));
    for raw in [
        "not json",
        "{}",
        r#""string""#,
        "[1,2,3]",
        // Substring guard short-circuits this before JSON parsing;
        // verify both code paths agree on the result.
        "12345",
        // Field name present as substring but not as object key — the
        // substring guard lets this through but JSON parsing rejects
        // it (the value is a number, not an object with that key).
        r#"{"note":"contains the word subagent_type as text"}"#,
    ] {
        assert!(
            !is_subagent_invocation(AgentType::OpenCode, &Some(raw.to_string())),
            "expected false for raw_input={raw}"
        );
    }
}

#[test]
fn subagent_rejects_when_subagent_type_appears_only_as_value() {
    // The cheap substring guard lets this through (the bytes
    // "subagent_type" appear in the JSON text), but JSON parsing
    // correctly finds no top-level `subagent_type` key, so the helper
    // returns false. Regression guard against any future "optimisation"
    // that conflates the substring check with the field check.
    let input = Some(r#"{"description":"use subagent_type=foo"}"#.to_string());
    assert!(!is_subagent_invocation(AgentType::OpenCode, &input));
}

#[test]
fn subagent_detects_when_raw_input_has_other_fields_ahead_of_subagent_type() {
    // Mirrors the OpenCode wire shape `{description, prompt, subagent_type}`
    // — the field order in JSON doesn't matter, but exercise a realistic
    // payload (with non-trivial sizes) end-to-end.
    let input = Some(
            r#"{"description":"Explore project structure","prompt":"Look at the repo layout and summarise the stack.","subagent_type":"general-purpose"}"#
                .to_string(),
        );
    assert!(is_subagent_invocation(AgentType::OpenCode, &input));
}

// ─── codebuddy_deferred_tool_name ────────────────────────────────────

#[test]
fn deferred_unwraps_codebuddy_mcp_tool_name() {
    // CodeBuddy wraps MCP calls as `{toolName, params}` via DeferExecuteTool.
    let input = Some(
            r#"{"params":{"agent_type":"codex","task":"build"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
    assert_eq!(
        codebuddy_deferred_tool_name(AgentType::CodeBuddy, &input).as_deref(),
        Some("mcp__codeg-mcp__delegate_to_agent")
    );
}

#[test]
fn deferred_gates_on_codebuddy_and_shape() {
    let wrapped = Some(
        r#"{"params":{"task_id":"a"},"toolName":"mcp__codeg-mcp__cancel_delegation"}"#.to_string(),
    );
    // Only CodeBuddy is unwrapped.
    assert!(codebuddy_deferred_tool_name(AgentType::OpenCode, &wrapped).is_none());
    // Missing `params`, missing/blank `toolName`, or non-wrapper shapes → None.
    for raw in [
        r#"{"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#, // no params
        r#"{"params":{"x":1},"toolName":""}"#,                 // blank toolName
        r#"{"params":{"x":1}}"#,                               // no toolName
        r#"{"command":"ls"}"#,                                 // plain tool
        "not json",
    ] {
        assert!(
            codebuddy_deferred_tool_name(AgentType::CodeBuddy, &Some(raw.to_string())).is_none(),
            "expected None for raw_input={raw}"
        );
    }
    assert!(codebuddy_deferred_tool_name(AgentType::CodeBuddy, &None).is_none());
}

// ─── unwrap_codebuddy_deferred_output ────────────────────────────────

#[test]
fn deferred_output_peels_codebuddy_content_wrapper() {
    // The exact live shape from the bug report: a `get_delegation_status`
    // batch result double-wrapped as a `{text,type}` content part, whose
    // inner `text` is the compact `{tasks:[...]}` JSON. Peeling it yields the
    // bare report JSON the frontend `parseStatusReports` already understands.
    let inner = r#"{"tasks":[{"status":"completed","task_id":"666da381","child_conversation_id":18,"text":"ok"}]}"#;
    let wrapped = serde_json::json!({ "text": inner, "type": "text" }).to_string();
    assert_eq!(
        unwrap_codebuddy_deferred_output(AgentType::CodeBuddy, &wrapped).as_deref(),
        Some(inner)
    );
}

#[test]
fn deferred_output_gates_on_codebuddy_and_wrapper_shape() {
    let wrapped =
        serde_json::json!({ "text": "{\"status\":\"running\"}", "type": "text" }).to_string();
    // Only CodeBuddy is unwrapped — the wrapper is a CodeBuddy quirk.
    assert!(unwrap_codebuddy_deferred_output(AgentType::OpenCode, &wrapped).is_none());
    assert!(unwrap_codebuddy_deferred_output(AgentType::ClaudeCode, &wrapped).is_none());
    for raw in [
        // Plain (non-deferred) tool output passes through untouched.
        "build succeeded",
        // A delegation report has no top-level `type` discriminator.
        r#"{"status":"completed","task_id":"x","text":"done"}"#,
        // A batch envelope is already in the bare shape — no `type` either.
        r#"{"tasks":[{"status":"completed","task_id":"x"}]}"#,
        // Wrong discriminator value.
        r#"{"type":"image","text":"x"}"#,
        // Missing inner `text`.
        r#"{"type":"text"}"#,
        "not json",
    ] {
        assert!(
            unwrap_codebuddy_deferred_output(AgentType::CodeBuddy, raw).is_none(),
            "expected pass-through (None) for output={raw}"
        );
    }
}

// ─── resolve_rewritten_title (title state across updates) ────────────

#[test]
fn rewritten_title_persists_across_status_only_updates() {
    let mut overrides: HashMap<String, String> = HashMap::new();
    let subagent =
        Some(r#"{"description":"Run pnpm build","subagent_type":"general-purpose"}"#.to_string());
    // Initial event carrying the subagent marker → "agent", recorded.
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &subagent,
            "tc1",
            false,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("agent")
    );
    // The bug: a later status-only update lost the marker (raw_input None).
    // The override must be RE-ASSERTED, not downgraded to the event's title.
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &None,
            "tc1",
            true,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("agent"),
        "a status-only update must not downgrade the Agent card mid-stream"
    );
    // Even an update whose raw_input looks like a different tool keeps it.
    let bash = Some(r#"{"command":"ls"}"#.to_string());
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &bash,
            "tc1",
            true,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("agent")
    );
    // A never-classified tool call returns None → caller uses its own title.
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &None,
            "tc2",
            true,
            false,
            &mut overrides
        ),
        None
    );
    // Deferred MCP tool: inner name recorded, then re-asserted on a bare update.
    let deferred = Some(
            r#"{"params":{"agent_type":"codex","task":"x"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &deferred,
            "tc3",
            false,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("mcp__codeg-mcp__delegate_to_agent")
    );
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &None,
            "tc3",
            true,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("mcp__codeg-mcp__delegate_to_agent")
    );
    // Non-CodeBuddy agent with no prior classification: never rewritten.
    assert_eq!(
        resolve_rewritten_title(
            AgentType::OpenCode,
            &None,
            "tc9",
            true,
            false,
            &mut overrides
        ),
        None
    );
}

// ─── codebuddy_meta_marks_subagent ───────────────────────────────────

#[test]
fn meta_marks_subagent_reads_codebuddy_keys() {
    // Any one of the three CodeBuddy sub-agent markers is sufficient.
    let tool_name = serde_json::json!({ "codebuddy.ai/toolName": "Agent" });
    let is_sub = serde_json::json!({ "codebuddy.ai/isSubagent": true });
    let sub_type = serde_json::json!({ "codebuddy.ai/subagentType": "general-purpose" });
    for meta in [&tool_name, &is_sub, &sub_type] {
        assert!(codebuddy_meta_marks_subagent(
            AgentType::CodeBuddy,
            meta.as_object()
        ));
    }
    // Gated on CodeBuddy: the generic `codebuddy.ai/*` keys can't classify
    // another agent.
    assert!(!codebuddy_meta_marks_subagent(
        AgentType::OpenCode,
        tool_name.as_object()
    ));
    // Negative shapes: non-Agent toolName, empty subagentType, absent meta.
    let other = serde_json::json!({
        "codebuddy.ai/toolName": "Bash",
        "codebuddy.ai/subagentType": "",
        "codebuddy.ai/isSubagent": false,
    });
    assert!(!codebuddy_meta_marks_subagent(
        AgentType::CodeBuddy,
        other.as_object()
    ));
    assert!(!codebuddy_meta_marks_subagent(AgentType::CodeBuddy, None));
}

#[test]
fn rewritten_title_fires_on_meta_before_raw_input() {
    let mut overrides: HashMap<String, String> = HashMap::new();
    // Frame 1: `raw_input` has NO `subagent_type` yet, but `_meta` already
    // marks it (the early, reliable signal). Title must already be "agent".
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &None,
            "tc1",
            false,
            true,
            &mut overrides
        )
        .as_deref(),
        Some("agent")
    );
    // Later sparse frames carry NEITHER signal — the override is re-asserted,
    // so the pill never flickers back to a generic tool mid-stream.
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &None,
            "tc1",
            true,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("agent"),
        "meta-classified Agent pill must stay 'agent' across signal-less frames"
    );
    // DeferExecuteTool still wins over the meta path (distinct mechanism).
    let deferred = Some(
            r#"{"params":{"agent_type":"codex","task":"x"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
    assert_eq!(
        resolve_rewritten_title(
            AgentType::CodeBuddy,
            &deferred,
            "tc2",
            false,
            false,
            &mut overrides
        )
        .as_deref(),
        Some("mcp__codeg-mcp__delegate_to_agent")
    );
}

// ─── track_subagent_window / should_suppress_subagent_chunk ──────────

#[test]
fn subagent_window_opens_and_closes_by_status() {
    let mut open: HashSet<String> = HashSet::new();
    let mut closed: HashSet<String> = HashSet::new();
    let fg = false; // foreground (not background)
                    // A non-final foreground agent frame opens the window.
    track_subagent_window(
        AgentType::CodeBuddy,
        true,
        fg,
        Some("in_progress"),
        "a",
        &mut open,
        &mut closed,
    );
    assert!(open.contains("a"));
    // A final frame closes it.
    track_subagent_window(
        AgentType::CodeBuddy,
        true,
        fg,
        Some("completed"),
        "a",
        &mut open,
        &mut closed,
    );
    assert!(!open.contains("a"));
    // A stray late non-final frame must NOT re-open a finished sub-agent.
    track_subagent_window(
        AgentType::CodeBuddy,
        true,
        fg,
        Some("in_progress"),
        "a",
        &mut open,
        &mut closed,
    );
    assert!(!open.contains("a"), "completed sub-agent must not re-open");
    // Non-agent tool calls never enter the window.
    track_subagent_window(
        AgentType::CodeBuddy,
        false,
        fg,
        Some("in_progress"),
        "b",
        &mut open,
        &mut closed,
    );
    assert!(!open.contains("b"));
    // Other agents are inert.
    track_subagent_window(
        AgentType::OpenCode,
        true,
        fg,
        Some("in_progress"),
        "c",
        &mut open,
        &mut closed,
    );
    assert!(!open.contains("c"));
}

#[test]
fn subagent_window_excludes_background_subagents() {
    // A BACKGROUND sub-agent runs concurrently with the main agent, so it must
    // never open the suppression window — otherwise interleaved MAIN-agent
    // chunks would be wrongly dropped (the case the reviewer flagged).
    let mut open: HashSet<String> = HashSet::new();
    let mut closed: HashSet<String> = HashSet::new();
    track_subagent_window(
        AgentType::CodeBuddy,
        true,
        true, // is_background
        Some("in_progress"),
        "bg",
        &mut open,
        &mut closed,
    );
    assert!(
        !open.contains("bg"),
        "a background sub-agent must not open the window"
    );
    // And once known-background, a later (still non-final, no-longer-marked)
    // frame must not re-open it either.
    track_subagent_window(
        AgentType::CodeBuddy,
        true,
        false,
        Some("in_progress"),
        "bg",
        &mut open,
        &mut closed,
    );
    assert!(
        !open.contains("bg"),
        "a sub-agent seen as background must stay excluded"
    );
}

#[test]
fn suppress_subagent_chunk_by_window_or_chunk_meta() {
    // Inside an open FOREGROUND window → suppress. This is safe because the
    // window only ever holds foreground (blocking) sub-agents, during which
    // the parent model is suspended — so every chunk in the window is the
    // sub-agent's, never main-agent output (background sub-agents, which could
    // interleave main output, are excluded from the window upstream).
    assert!(should_suppress_subagent_chunk(
        AgentType::CodeBuddy,
        true,
        None
    ));
    // Window closed and no chunk meta → emit (e.g. main-agent text before the
    // sub-agent opens or after it closes).
    assert!(!should_suppress_subagent_chunk(
        AgentType::CodeBuddy,
        false,
        None
    ));
    // Window closed but the chunk's own meta marks it → suppress (precision
    // supplement; never relied on alone).
    let sub = serde_json::json!({ "codebuddy.ai/isSubagent": true });
    let parented = serde_json::json!({ "codebuddy.ai/parentToolCallId": "call_x" });
    for meta in [&sub, &parented] {
        assert!(should_suppress_subagent_chunk(
            AgentType::CodeBuddy,
            false,
            meta.as_object()
        ));
    }
    // Other agents never suppress, even inside a (spurious) open window.
    assert!(!should_suppress_subagent_chunk(
        AgentType::OpenCode,
        true,
        None
    ));
}

#[test]
fn claude_chunk_parent_reads_only_wellformed_claude_meta() {
    let valid = serde_json::json!({ "claudeCode": { "parentToolUseId": "toolu_01A" } });
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, valid.as_object()),
        Some("toolu_01A".to_string())
    );
    // Gated on ClaudeCode — the same meta on another agent must not alias
    // into parented routing.
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::CodeBuddy, valid.as_object()),
        None
    );
    // Absent meta / absent key / wrong type / empty string → None.
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, None),
        None
    );
    let no_key = serde_json::json!({ "claudeCode": { "toolName": "Agent" } });
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, no_key.as_object()),
        None
    );
    let wrong_type = serde_json::json!({ "claudeCode": { "parentToolUseId": 42 } });
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, wrong_type.as_object()),
        None
    );
    let empty = serde_json::json!({ "claudeCode": { "parentToolUseId": "" } });
    assert_eq!(
        claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, empty.as_object()),
        None
    );
}

#[test]
fn meta_marks_background_reads_codebuddy_flag() {
    let bg = serde_json::json!({ "codebuddy.ai/isBackground": true });
    let fg = serde_json::json!({ "codebuddy.ai/isBackground": false });
    assert!(codebuddy_meta_marks_background(
        AgentType::CodeBuddy,
        bg.as_object()
    ));
    // Foreground (the user-reported case), absent flag, and other agents → false.
    assert!(!codebuddy_meta_marks_background(
        AgentType::CodeBuddy,
        fg.as_object()
    ));
    assert!(!codebuddy_meta_marks_background(AgentType::CodeBuddy, None));
    assert!(!codebuddy_meta_marks_background(
        AgentType::OpenCode,
        bg.as_object()
    ));
}

#[test]
fn a_pinned_cline_provider_is_read_off_the_variable_that_pins_it() {
    let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };

    // The BYO case: codeg exports the provider, so cline refuses every
    // choice the selector offers.
    assert_eq!(
        env_pinned_config_option_ids(
            AgentType::Cline,
            &env(&[("CLINE_PROVIDER", "openai-compatible")])
        ),
        vec!["provider".to_string()]
    );
    // Sign-in: nothing is exported, the selector works, keep it.
    assert!(
        env_pinned_config_option_ids(AgentType::Cline, &env(&[("CLINE_MODEL", "gpt-4o")]))
            .is_empty()
    );
    // Emptiness follows the agent's own test, a bare `if (env.CLINE_PROVIDER)`:
    // the empty string is falsy and leaves the selector usable…
    assert!(
        env_pinned_config_option_ids(AgentType::Cline, &env(&[("CLINE_PROVIDER", "")])).is_empty()
    );
    // …but whitespace is TRUTHY in JS, so cline refuses every choice and
    // `buildConfig` uses "  " verbatim as the provider id. Trimming here
    // would leave a dead dropdown on screen.
    assert_eq!(
        env_pinned_config_option_ids(AgentType::Cline, &env(&[("CLINE_PROVIDER", "  ")])),
        vec!["provider".to_string()]
    );
    // No other agent freezes a selector this way.
    assert!(env_pinned_config_option_ids(
        AgentType::Codex,
        &env(&[("CLINE_PROVIDER", "openai-compatible")])
    )
    .is_empty());
}

#[test]
fn a_pinned_option_is_withheld_from_the_composer() {
    let options = || {
        vec![
            SessionConfigOption::boolean(
                SessionConfigId::new("auto_approve"),
                "Auto-approve tools",
                false,
            ),
            SessionConfigOption::boolean(SessionConfigId::new("provider"), "Provider", false),
        ]
    };
    let ids = |opts: Vec<SessionConfigOption>| -> Vec<String> {
        opts.iter().map(|o| o.id.to_string()).collect()
    };

    assert_eq!(
        ids(visible_config_options(&["provider".to_string()], options())),
        vec!["auto_approve".to_string()],
        "a dropdown whose every choice errors must not reach the composer"
    );
    // Nothing pinned → the agent's list travels untouched, which is every
    // agent but cline-with-a-BYO-provider.
    assert_eq!(
        ids(visible_config_options(&[], options())),
        vec!["auto_approve".to_string(), "provider".to_string()]
    );
}

struct TestConversationDepthLookup;

#[async_trait::async_trait]
impl crate::acp::delegation::broker::ConversationDepthLookup for TestConversationDepthLookup {
    async fn parent_of(
        &self,
        _id: i32,
    ) -> Result<Option<i32>, crate::acp::delegation::types::DelegationError> {
        Ok(None)
    }
}

struct TestNoQuestions;

#[async_trait::async_trait]
impl crate::acp::question::SessionQuestionAccess for TestNoQuestions {
    async fn register_question(
        &self,
        _parent_connection_id: &str,
        _questions: Vec<crate::acp::question::QuestionSpec>,
    ) -> Option<crate::acp::question::RegisteredQuestion> {
        None
    }

    async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}

    async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
}

struct TestNoPlanApprovals;

#[async_trait::async_trait]
impl crate::acp::plan_approval::SessionPlanApprovalAccess for TestNoPlanApprovals {
    async fn register_plan_approval(
        &self,
        _parent_connection_id: &str,
        _tool_call_id: String,
        _plan_markdown: String,
    ) -> Option<crate::acp::plan_approval::RegisteredPlanApproval> {
        None
    }

    async fn cancel_plan_approvals_by_parent(&self, _parent_connection_id: &str) {}
}

struct TestAllAgentsAvailable;

#[async_trait::async_trait]
impl AgentAvailabilityLookup for TestAllAgentsAvailable {
    async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
        Vec::new()
    }
}

fn test_delegation_injection(
    agent_availability: Arc<dyn AgentAvailabilityLookup>,
) -> DelegationInjection {
    use crate::acp::delegation::broker::DelegationBroker;
    use crate::acp::delegation::listener::TokenRegistry;
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};

    let broker = Arc::new(DelegationBroker::new(
        Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
        Arc::new(TestConversationDepthLookup)
            as Arc<dyn crate::acp::delegation::broker::ConversationDepthLookup>,
    ));
    DelegationInjection {
        broker,
        tokens: Arc::new(TokenRegistry::default()),
        socket_path: std::path::PathBuf::from("/tmp/codeg-mcp.sock"),
        agent_availability,
        feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
        ask: crate::acp::question::QuestionRuntimeConfig::new(),
        sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
        authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig::new(),
        browser: crate::acp::browser_tools::BrowserToolsRuntimeConfig::new(),
        computer: crate::acp::computer_tools::ComputerToolsRuntimeConfig::new(),
        questions: Arc::new(TestNoQuestions)
            as Arc<dyn crate::acp::question::SessionQuestionAccess>,
        plan_approvals: Arc::new(TestNoPlanApprovals)
            as Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
    }
}

// ─── inject_codeg_mcp: enabled=false short-circuit ──────────
//
// Guards the "default off" product contract: when the broker config has
// `enabled: false` (the new production default for fresh installs), the
// delegate-MCP injection must not push a server entry and must not
// register a per-launch token. The early return at the top of
// `inject_codeg_mcp` is the single chokepoint that keeps a
// codeg-mcp stdio MCP out of every ACP session until the user
// opts in via the settings panel.
#[tokio::test]
async fn inject_codeg_delegate_skipped_when_broker_disabled() {
    // No set_config call: broker carries its default config, which is
    // `enabled: false` after the product-default flip. This is the
    // exact state a fresh install reaches before the user touches the
    // settings panel. Feedback is likewise disabled by default, so with
    // BOTH features off the companion isn't injected at all.
    let injection = test_delegation_injection(
        Arc::new(TestAllAgentsAvailable) as Arc<dyn AgentAvailabilityLookup>
    );

    let mut servers: Vec<McpServer> = Vec::new();
    let result = inject_codeg_mcp(
        &mut servers,
        &injection,
        "parent-conn",
        std::path::Path::new("/tmp"),
        false,
        HostToolsPolicy::Default,
    )
    .await;

    assert!(result.is_none(), "disabled broker must return None");
    assert!(
        servers.is_empty(),
        "disabled broker must not push any MCP server entry; got {servers:?}"
    );
    // Token registry stays untouched — no lookup should resolve to a
    // valid entry because nothing was registered.
    assert!(
        injection.tokens.lookup("any-token").await.is_none(),
        "disabled broker must not register a delegate token"
    );
}

// ─── delegate_target_args: enable-toggle filtering ──────────
//
// The companion's delegate enum must only advertise launchable targets:
// enabled customs are appended, disabled customs are never appended (no
// subtraction entry either), and disabled builtins ride the sorted
// `--disabled-agents` list for companion-side subtraction.
#[test]
fn delegate_target_args_filter_disabled_agents() {
    use crate::acp::custom_registry::{
        hydrate, hydrate_test_guard, CustomAgentDef, CustomAgentSpec, CustomDistributionKind,
        NpxSpec,
    };
    let _guard = hydrate_test_guard();
    let def = |id: &str| CustomAgentDef {
        registry_id: id.into(),
        name: id.into(),
        description: String::new(),
        version: "1.0.0".into(),
        distribution_kind: CustomDistributionKind::Npx,
        spec: CustomAgentSpec {
            npx: Some(NpxSpec {
                package: format!("{id}@1.0.0"),
                ..Default::default()
            }),
            ..Default::default()
        },
        icon_url: None,
        skills_shared_store: false,
        skills_dir: None,
        source: Default::default(),
        version_probe: None,
        supports_mcp: true,
    };
    assert!(hydrate(&[def("delegate-on"), def("delegate-off")]).is_empty());

    let disabled = vec![
        "grok".to_string(),
        "codex".to_string(),
        "custom:delegate-off".to_string(),
    ];
    let (custom_slugs, disabled_builtins) = delegate_target_args(&disabled);
    assert_eq!(custom_slugs, vec!["custom:delegate-on".to_string()]);
    assert_eq!(
        disabled_builtins,
        vec!["codex".to_string(), "grok".to_string()],
        "builtins only, sorted for a deterministic arg string"
    );

    // Nothing disabled → both flags stay omitted (customs all advertised).
    let (custom_slugs, disabled_builtins) = delegate_target_args(&[]);
    assert_eq!(
        custom_slugs,
        vec![
            "custom:delegate-off".to_string(),
            "custom:delegate-on".to_string()
        ]
    );
    assert!(disabled_builtins.is_empty());

    hydrate(&[]);
}

#[test]
fn host_tools_agent_also_withholds_the_delegation_group() {
    // `delegate_to_agent` is the third door into the room `fs/*` and
    // `terminal/*` open: it has codeg spawn a SECOND agent, in codeg's
    // process tree under that agent's own policy, and relays its output
    // back. Without this gate a sandboxed agent that cannot read `.env`
    // itself just asks a sibling to read it, and the switch's promise is
    // false. This pins the exact boolean `inject_codeg_mcp` computes.
    let delegation_for = |broker_enabled: bool, host_tools: HostToolsPolicy| {
        broker_enabled && host_tools.hosts_channels()
    };
    assert!(delegation_for(true, HostToolsPolicy::Default));
    assert!(!delegation_for(true, HostToolsPolicy::Agent));
    // The settings toggle still wins when it is the one saying no.
    assert!(!delegation_for(false, HostToolsPolicy::Default));

    // With delegation the only enabled group, withholding it must skip the
    // companion entirely rather than launch it with an empty `--features`
    // (which `CompanionFeatures::parse` would see as absent and default
    // back to delegation-only — re-opening the hole).
    let mut flags = CompanionFeatureFlags {
        delegation: delegation_for(true, HostToolsPolicy::Agent),
        ..CompanionFeatureFlags::default()
    };
    assert_eq!(companion_features_arg(flags), None);

    // But the groups that only surface codeg's OWN state keep working —
    // they execute nothing on the user's machine, so withholding them
    // would cost function for no boundary.
    flags.ask = true;
    flags.feedback = true;
    assert_eq!(
        companion_features_arg(flags),
        Some("feedback,ask".to_string())
    );
}

// ─── companion_features_arg: inject/skip decision + --features value ──
//
// The companion now carries two independently-toggled tool groups. It is
// injected when EITHER is on, and the `--features` arg names exactly the
// enabled groups so the companion hides the rest. Crucially, feedback alone
// must still inject the companion (the historical delegation-only gate would
// have skipped it).
#[test]
fn companion_features_arg_inject_skip_decision() {
    let only = |f: fn(&mut CompanionFeatureFlags)| {
        let mut flags = CompanionFeatureFlags::default();
        f(&mut flags);
        companion_features_arg(flags)
    };
    // All off → no companion at all.
    assert_eq!(
        companion_features_arg(CompanionFeatureFlags::default()),
        None
    );
    // Delegation only.
    assert_eq!(
        only(|f| f.delegation = true),
        Some("delegation".to_string())
    );
    // Feedback only — the decoupling: companion injected for feedback even
    // when delegation is off.
    assert_eq!(only(|f| f.feedback = true), Some("feedback".to_string()));
    // Ask only — likewise injects the companion on its own.
    assert_eq!(only(|f| f.ask = true), Some("ask".to_string()));
    // Sessions only — likewise injects the companion on its own.
    assert_eq!(only(|f| f.sessions = true), Some("sessions".to_string()));
    // Per-spawn tasks group: injects alone.
    assert_eq!(only(|f| f.tasks = true), Some("tasks".to_string()));
    // Each chat-authoring group injects the companion on its own too, so a
    // user who only wants "create a task from chat" still gets the tool.
    assert_eq!(
        only(|f| f.automations = true),
        Some("automations".to_string())
    );
    assert_eq!(only(|f| f.taskboard = true), Some("taskboard".to_string()));
    // The browser group too — a user who only shares browser tabs still
    // gets a companion.
    assert_eq!(only(|f| f.browser = true), Some("browser".to_string()));
    // All on → comma-joined, in the order the companion parses.
    assert_eq!(
        companion_features_arg(CompanionFeatureFlags {
            delegation: true,
            feedback: true,
            ask: true,
            sessions: true,
            tasks: true,
            automations: true,
            taskboard: true,
            browser: true,
            browser_eval: true,
            computer: true,
            computer_launch: true,
            computer_clipboard: true,
        }),
        Some(
            "delegation,feedback,ask,sessions,tasks,automations,taskboard,browser,browser_eval,\
                 computer,computer_launch,computer_clipboard"
                .to_string()
        )
    );
    assert_eq!(only(|f| f.computer_clipboard = true), None);
    // Computer use alone still gets a companion.
    assert_eq!(only(|f| f.computer = true), Some("computer".to_string()));
    // Its launch switch never travels on its own.
    assert_eq!(only(|f| f.computer_launch = true), None);
    // `browser_eval` never travels on its own: the companion requires both
    // tokens, and a lone one in an agent's MCP config would read as if it
    // granted something.
    assert_eq!(only(|f| f.browser_eval = true), None);
    assert_eq!(
        only(|f| {
            f.browser = true;
            f.browser_eval = true;
        }),
        Some("browser,browser_eval".to_string())
    );
}

// ── Boolean config options (cline 3.0.50 `auto_approve`) ──

/// The exact `configOptions` entry cline 3.0.50 ships. Before codeg's schema
/// could parse boolean options (then behind `unstable_boolean_config`,
/// stable in the 1.x schema) this failed to deserialize with
/// `unknown variant 'boolean', expected 'select'` — and because
/// `SessionConfigOption::kind` is a required flattened field, that one entry
/// failed the WHOLE `session/new` response and left cline unusable.
fn cline_auto_approve_json() -> serde_json::Value {
    serde_json::json!({
        "type": "boolean",
        "id": "auto_approve",
        "name": "Auto-approve tools",
        "description": "Automatically approve all tool calls without asking for permission",
        "currentValue": false
    })
}

#[test]
fn cline_boolean_config_option_maps_to_a_toggle() {
    let option: SessionConfigOption =
        serde_json::from_value(cline_auto_approve_json()).expect("boolean kind must parse");

    let mapped = map_session_config_option(&option).expect("boolean options are surfaced");
    assert_eq!(mapped.id, "auto_approve");
    assert_eq!(mapped.name, "Auto-approve tools");
    match mapped.kind {
        SessionConfigKindInfo::Boolean(toggle) => assert!(!toggle.current_value),
        other => panic!("expected a boolean kind, got {other:?}"),
    }
}

/// The whole point of the fix: one boolean option must not take the session
/// down with it.
#[test]
fn new_session_response_with_a_boolean_option_parses() {
    let raw = serde_json::json!({
        "sessionId": "sess-1",
        "configOptions": [
            {
                "type": "select",
                "id": "model",
                "name": "Model",
                "currentValue": "anthropic/claude-sonnet-5",
                "options": [{"value": "anthropic/claude-sonnet-5", "name": "Claude Sonnet 5"}]
            },
            cline_auto_approve_json(),
        ]
    });
    let resp: NewSessionResponse = serde_json::from_value(raw).expect("must parse");
    assert_eq!(resp.config_options.map(|o| o.len()), Some(2));
}

#[test]
fn select_values_keep_their_pre_boolean_wire_shape() {
    // Regression guard for every agent that is NOT cline: boolean options
    // (then behind `unstable_boolean_config`, stable in the 1.x schema)
    // changed `SetSessionConfigOptionRequest.value` from a plain
    // `SessionConfigValueId` to a flattened enum. The untagged
    // `ValueId` variant must still serialize to a bare `"value"` string, or
    // codex / claude / opencode model switching silently breaks.
    let req = SetSessionConfigOptionRequest::new(
        SessionId::new("sess-1"),
        SessionConfigId::new("model"),
        encode_config_option_value(false, "anthropic/claude-sonnet-5"),
    );
    assert_eq!(
        serde_json::to_value(&req).unwrap(),
        serde_json::json!({
            "sessionId": "sess-1",
            "configId": "model",
            "value": "anthropic/claude-sonnet-5"
        })
    );
}

#[test]
fn boolean_values_carry_a_type_discriminator() {
    let on = SetSessionConfigOptionRequest::new(
        SessionId::new("sess-1"),
        SessionConfigId::new("auto_approve"),
        encode_config_option_value(true, "true"),
    );
    assert_eq!(
        serde_json::to_value(&on).unwrap(),
        serde_json::json!({
            "sessionId": "sess-1",
            "configId": "auto_approve",
            "type": "boolean",
            "value": true
        })
    );
    // Anything that is not the literal "true" is off — the selector only
    // ever emits "true"/"false".
    assert_eq!(
        encode_config_option_value(true, "false").as_bool(),
        Some(false)
    );
}

/// An option kind newer than this build must cost its own selector, never
/// the whole response — a failed `session/new` parse would leave the agent
/// unusable. The schema (1.9+) guarantees that on every channel options
/// arrive on (`configOptions` skips an undecodable entry), and codeg relies
/// on it rather than sanitizing the raw JSON first, as it once had to. This
/// pins that guarantee through a schema bump: all five responses (new, load,
/// resume, fork, set_config_option) and the `config_option_update` push must
/// keep the known options and drop only the unknown one.
#[test]
fn an_unknown_config_option_kind_costs_only_its_own_selector() {
    use agent_client_protocol::schema::v1::{
        ForkSessionResponse, LoadSessionResponse, SetSessionConfigOptionResponse,
    };
    let options = serde_json::json!([
        {
            "type": "select",
            "id": "model",
            "name": "Model",
            "currentValue": "m1",
            "options": [{"value": "m1", "name": "M1"}]
        },
        cline_auto_approve_json(),
        // A kind newer than the schema — today this is what `boolean` was
        // before it was stabilized.
        {"type": "radio", "id": "future", "name": "Future", "currentValue": "a"},
    ]);
    let ids =
        |opts: &[SessionConfigOption]| opts.iter().map(|o| o.id.to_string()).collect::<Vec<_>>();
    let expected = vec!["model".to_string(), "auto_approve".to_string()];
    let response = serde_json::json!({"sessionId": "sess-1", "configOptions": options});

    let new: NewSessionResponse = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(ids(new.config_options.as_deref().unwrap()), expected);
    let load: LoadSessionResponse = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(ids(load.config_options.as_deref().unwrap()), expected);
    let resume: ResumeSessionResponse = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(ids(resume.config_options.as_deref().unwrap()), expected);
    let fork: ForkSessionResponse = serde_json::from_value(response).unwrap();
    assert_eq!(ids(fork.config_options.as_deref().unwrap()), expected);
    let set: SetSessionConfigOptionResponse =
        serde_json::from_value(serde_json::json!({"configOptions": options})).unwrap();
    assert_eq!(ids(&set.config_options), expected);

    let push: SessionNotification = serde_json::from_value(serde_json::json!({
        "sessionId": "sess-1",
        "update": {"sessionUpdate": "config_option_update", "configOptions": options},
    }))
    .unwrap();
    let SessionUpdate::ConfigOptionUpdate(update) = push.update else {
        panic!("a config_option_update");
    };
    assert_eq!(ids(&update.config_options), expected);
}

/// The untyped send must put the same params on the wire the typed send
/// would — `UntypedMessage::new` runs the very same `serde_json::to_value`
/// on the request, so this pins the payload rather than the mechanism.
#[test]
fn untyped_new_session_carries_the_typed_request_payload() {
    let cwd = std::path::PathBuf::from("/tmp/codeg");
    let req = build_new_session_request(AgentType::Cline, &cwd, Vec::new());
    let expected = serde_json::to_value(&req).unwrap();

    let untyped = UntypedMessage::new(AGENT_METHOD_NAMES.session_new, req).expect("builds");
    assert_eq!(untyped.method(), "session/new");
    assert_eq!(untyped.params(), &expected);
}

/// The saved-preference replay must set the model BEFORE anything scoped to
/// it. Cursor's parameterized picker (unlocked by
/// `_meta.parameterizedModelPicker`) ships `fast` / thinking options that
/// belong to the CURRENT model: setting `model` reloads that model's own
/// parameter values, and setting a parameter the model in effect does not
/// define is rejected outright. Raw key order is alphabetical, which puts
/// `fast` first — exactly backwards.
#[test]
fn preferred_config_values_apply_the_model_first() {
    let options: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
        {
            "type": "select",
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "currentValue": "agent",
            "options": [{"value": "agent", "name": "Agent"}]
        },
        {
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": "composer-2.5",
            "options": [{"value": "composer-2.5", "name": "Composer 2.5"}]
        },
        {
            "type": "select",
            "id": "fast",
            "name": "Fast",
            "category": "model_config",
            "currentValue": "true",
            "options": [{"value": "true", "name": "On"}, {"value": "false", "name": "Off"}]
        },
    ]))
    .expect("parses");

    let preferred = BTreeMap::from([
        ("fast".to_string(), "false".to_string()),
        ("mode".to_string(), "plan".to_string()),
        ("model".to_string(), "composer-2.5".to_string()),
    ]);
    let ordered: Vec<&str> = order_preferred_config_values(&options, &preferred)
        .into_iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        ordered,
        vec!["model", "fast", "mode"],
        "model leads; the rest keep their sorted order"
    );

    // An agent that labels its model selector only by category still leads.
    let by_category: Vec<SessionConfigOption> = serde_json::from_value(serde_json::json!([
        {
            "type": "select",
            "id": "llm",
            "name": "Model",
            "category": "model",
            "currentValue": "a",
            "options": [{"value": "a", "name": "A"}]
        },
    ]))
    .expect("parses");
    let preferred = BTreeMap::from([
        ("effort".to_string(), "high".to_string()),
        ("llm".to_string(), "b".to_string()),
    ]);
    let ordered: Vec<&str> = order_preferred_config_values(&by_category, &preferred)
        .into_iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(ordered, vec!["llm", "effort"]);

    // Nothing model-shaped: the order is untouched, and an id the agent
    // never advertised is still replayed (it is not codeg's call to drop).
    let preferred = BTreeMap::from([
        ("a_thing".to_string(), "1".to_string()),
        ("z_thing".to_string(), "2".to_string()),
    ]);
    let ordered: Vec<&str> = order_preferred_config_values(&[], &preferred)
        .into_iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(ordered, vec!["a_thing", "z_thing"]);
}

/// Establishment keeps what the agent picked on its own, read BEFORE the
/// saved preferences replay over it. The options probe reports that for
/// each option it applied (`report_agent_chosen_values` in the manager), so
/// the delegation panel's "Default" names the agent's model rather than the
/// user's. Shaped like a live opencode 2.0.24: switching the model re-lists
/// `effort` for it.
#[tokio::test]
async fn establishment_keeps_the_agents_own_picks_from_before_the_preference_replay() {
    use agent_client_protocol::schema::v1::SetSessionConfigOptionResponse;

    fn options(model: &str, effort: &str, efforts: &[&str]) -> Vec<SessionConfigOption> {
        let efforts: Vec<serde_json::Value> = efforts
            .iter()
            .map(|value| serde_json::json!({"value": value, "name": value}))
            .collect();
        serde_json::from_value(serde_json::json!([
            {
                "type": "select",
                "id": "model",
                "name": "Model",
                "category": "model",
                "currentValue": model,
                "options": [
                    {"value": "opencode/exo-free", "name": "Exo"},
                    {"value": "opencode/step-5-preview-free", "name": "Step 5"}
                ]
            },
            {
                "type": "select",
                "id": "effort",
                "name": "Effort",
                "category": "thought_level",
                "currentValue": effort,
                "options": efforts
            },
        ]))
        .expect("parses")
    }

    let (client_end, agent_end) = agent_client_protocol::Channel::duplex();
    let agent = tokio::spawn(async move {
        let _ = Agent
            .builder()
            .on_receive_request(
                async |_req: NewSessionRequest,
                       responder: Responder<NewSessionResponse>,
                       _cx: ConnectionTo<Client>| {
                    let opened = options("opencode/exo-free", "high", &["high", "default"]);
                    responder.respond(
                        NewSessionResponse::new(SessionId::new("s1")).config_options(opened),
                    )
                },
                on_receive_request!(),
            )
            .on_receive_request(
                async |_req: SetSessionConfigOptionRequest,
                       responder: Responder<SetSessionConfigOptionResponse>,
                       _cx: ConnectionTo<Client>| {
                    // Only the model is replayed: the answer re-lists effort.
                    let switched = options(
                        "opencode/step-5-preview-free",
                        "default",
                        &["low", "medium", "high", "default"],
                    );
                    responder.respond(SetSessionConfigOptionResponse::new(switched))
                },
                on_receive_request!(),
            )
            .connect_with(agent_end, async |_cx: ConnectionTo<Client>| {
                std::future::pending::<Result<(), agent_client_protocol::Error>>().await
            })
            .await;
    });

    let state = Arc::new(RwLock::new(SessionState::new(
        "conn-probe".to_string(),
        AgentType::OpenCode,
        None,
        "delegation-probe".to_string(),
        None,
    )));
    let establishment_state = Arc::clone(&state);
    Client
        .builder()
        .connect_with(client_end, async move |cx: ConnectionTo<Agent>| {
            let raw = cx
                .send_request_to(
                    Agent,
                    UntypedMessage::new("session/new", NewSessionRequest::new("/tmp"))?,
                )
                .block_task()
                .await?;
            let response: NewSessionResponse = serde_json::from_value(raw)
                .map_err(agent_client_protocol::Error::into_internal_error)?;
            let initial = response.config_options.clone().unwrap_or_default();
            let mut session = AgentSession::attach(&cx, response)?;
            let preferred = BTreeMap::from([(
                "model".to_string(),
                "opencode/step-5-preview-free".to_string(),
            )]);
            apply_and_emit_session_config_options(
                &cx,
                &mut session,
                &establishment_state,
                &EventEmitter::Noop,
                AgentType::OpenCode,
                None,
                None,
                None,
                &preferred,
                initial,
            )
            .await;
            Ok(())
        })
        .await
        .expect("the establishment runs");
    agent.abort();

    let state = state.read().await;
    assert_eq!(
        state.agent_chosen_config_values,
        BTreeMap::from([
            ("model".to_string(), "opencode/exo-free".to_string()),
            ("effort".to_string(), "high".to_string()),
        ]),
        "the agent's own picks, from before the replay"
    );
    // …while the session itself runs the preference, effort re-listed for it.
    let current = current_config_option_values(state.config_options.as_deref().unwrap_or_default());
    assert_eq!(
        current.get("model").map(String::as_str),
        Some("opencode/step-5-preview-free")
    );
    assert_eq!(current.get("effort").map(String::as_str), Some("default"));
}

/// The claude shape: a model select plus the effort option that hangs off it.
fn asserted_drift_options(model: &str, effort: &str) -> Vec<SessionConfigOption> {
    serde_json::from_value(serde_json::json!([
        {
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": model,
            "options": [
                {"value": "sonnet[1m]", "name": "Sonnet"},
                {"value": "claude-fable-5-1[1m]", "name": "Fable"},
            ],
        },
        {
            "type": "select",
            "id": "effort",
            "name": "Effort",
            "currentValue": effort,
            "options": [{"value": "high", "name": "High"}, {"value": "medium", "name": "Medium"}],
        },
    ]))
    .expect("parses")
}

fn asserted_drift_state(asserted: &[(&str, &str)]) -> Arc<RwLock<SessionState>> {
    let mut st = SessionState::new(
        "conn-drift".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    );
    st.asserted_config_values = asserted
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Arc::new(RwLock::new(st))
}

/// The fork symptom in miniature: codeg applies the parent's `sonnet[1m]`,
/// claude answers OK, then ~2ms later pushes its own re-pin. Effort rides
/// along because a model switch re-scopes it — one push, two reverted
/// options, and BOTH have to come back.
///
/// Order is the load-bearing half: alphabetically `effort` precedes `model`,
/// and replaying it first lets the model switch re-scope effort straight
/// back — with both ledger entries already spent, so nothing defends it a
/// second time. Model must lead, exactly as in the establishment replay.
#[tokio::test]
async fn an_agent_push_that_reverts_an_asserted_value_is_reported_as_drift() {
    let state = asserted_drift_state(&[("model", "sonnet[1m]"), ("effort", "high")]);

    let drift = take_asserted_config_drift(
        &state,
        &asserted_drift_options("claude-fable-5-1[1m]", "medium"),
    )
    .await;

    assert_eq!(
        drift,
        vec![
            ("model".to_string(), "sonnet[1m]".to_string()),
            ("effort".to_string(), "high".to_string()),
        ],
        "both reverted options come back, at the values codeg applied, model first"
    );
}

/// Removing on read is the whole ping-pong bound: an agent that re-pins
/// unconditionally costs exactly one extra round-trip, then wins.
#[tokio::test]
async fn each_asserted_option_is_defended_at_most_once() {
    let state = asserted_drift_state(&[("model", "sonnet[1m]")]);
    let pushed = asserted_drift_options("claude-fable-5-1[1m]", "high");

    assert_eq!(take_asserted_config_drift(&state, &pushed).await.len(), 1);
    assert!(
        take_asserted_config_drift(&state, &pushed).await.is_empty(),
        "a second identical push must not start a set_config_option loop"
    );
    assert!(state.read().await.asserted_config_values.is_empty());
}

/// Silence in the two cases that must stay silent: a push that agrees, and
/// the post-prompt state where the agent owns the selectors outright.
#[tokio::test]
async fn an_agreeing_push_and_an_empty_ledger_produce_no_drift() {
    let agreeing = asserted_drift_state(&[("model", "sonnet[1m]"), ("effort", "high")]);
    assert!(
        take_asserted_config_drift(&agreeing, &asserted_drift_options("sonnet[1m]", "high"))
            .await
            .is_empty()
    );
    assert_eq!(
        agreeing.read().await.asserted_config_values.len(),
        2,
        "an agreeing push consumes nothing — the next one still gets defended"
    );

    let after_first_prompt = asserted_drift_state(&[]);
    assert!(take_asserted_config_drift(
        &after_first_prompt,
        &asserted_drift_options("claude-fable-5-1[1m]", "medium")
    )
    .await
    .is_empty());
}

/// An option the push doesn't mention at all is not drift — a partial list
/// is the agent narrowing what it reports, not reverting what it omits.
#[tokio::test]
async fn an_option_absent_from_the_push_is_not_drift() {
    let state = asserted_drift_state(&[("model", "sonnet[1m]"), ("sandbox", "read-only")]);

    let drift = take_asserted_config_drift(
        &state,
        &asserted_drift_options("claude-fable-5-1[1m]", "high"),
    )
    .await;

    assert_eq!(drift, vec![("model".to_string(), "sonnet[1m]".to_string())]);
    assert_eq!(
        state
            .read()
            .await
            .asserted_config_values
            .keys()
            .collect::<Vec<_>>(),
        vec!["sandbox"],
        "the unmentioned option stays defended"
    );
}

/// `emit_session_modes` is a no-op for a modes-less session, so
/// `current_mode` outlives a transition into one. Without the parent's
/// capability gate a fork would hand that ancestor's mode to a child that
/// does advertise modes.
#[test]
fn a_modes_less_parent_does_not_pass_an_ancestors_mode_to_the_fork() {
    let mut st = SessionState::new(
        "conn-fork-mode".to_string(),
        AgentType::ClaudeCode,
        None,
        "win".to_string(),
        None,
    );
    st.apply_event(&AcpEvent::ModeChanged {
        mode_id: "bypassPermissions".to_string(),
    });
    assert_eq!(st.current_mode.as_deref(), Some("bypassPermissions"));

    assert_eq!(
        live_mode_for_fork(&st, None),
        None,
        "a parent with no modes has no mode to inherit, stale value or not"
    );

    let modes = SessionModeState::new(
        "default".to_string(),
        vec![agent_client_protocol::schema::v1::SessionMode::new(
            "bypassPermissions",
            "Bypass",
        )],
    );
    assert_eq!(
        live_mode_for_fork(&st, Some(&modes)).as_deref(),
        Some("bypassPermissions"),
        "the event-tracked mode is inherited, not the attach-time snapshot's"
    );
}

#[test]
fn saved_boolean_preference_skips_the_redundant_round_trip() {
    let off: SessionConfigOption =
        serde_json::from_value(cline_auto_approve_json()).expect("parses");
    assert!(config_option_already_holds(&off, "false"));
    assert!(!config_option_already_holds(&off, "true"));

    let mut on_json = cline_auto_approve_json();
    on_json["currentValue"] = serde_json::json!(true);
    let on: SessionConfigOption = serde_json::from_value(on_json).expect("parses");
    assert!(config_option_already_holds(&on, "true"));
    assert!(!config_option_already_holds(&on, "false"));
}

// ── Grok background workflows and the turns Grok starts itself (#859) ───
//
// Driven through `run_conversation_loop` over the real runtime and an
// in-memory pipe. The scripted agent replays frames captured from grok
// 1.0.41 running `/workflow <name>` against a two-phase, one-agent script.

/// One scripted Grok frame: JSON-RPC method + params.
type GrokFrame = (&'static str, serde_json::Value);

const GROK_RUN_ID: &str = "wf_01a0e8a2c2ef74e3bf300ffd5204f1dd";
const GROK_FOLLOW_UP_ID: &str = "workflow-completed-wf_01a0e8a2c2ef74e3bf300ffd5204f1dd-9";

/// What the scripted Grok sends, and when.
#[derive(Default)]
struct GrokScript {
    /// While the prompt is open, before its response.
    in_turn: Vec<GrokFrame>,
    /// Hold the prompt response until the test releases it.
    hold_response: bool,
    /// `_meta.promptId` on the prompt response.
    prompt_id: &'static str,
    /// After the response, once the test has seen the turn end — frames of
    /// the prompt's turn that the IDLE loop reads. Grok sends them just
    /// before the response, but the turn loop picks a ready response over
    /// queued updates at random, so this is where they can end up.
    after_response: Vec<GrokFrame>,
    /// On the test's go-ahead: work Grok does on its own between turns.
    later: Vec<GrokFrame>,
    /// When the client sends `session/cancel`.
    on_cancel: Vec<GrokFrame>,
    /// When the client sends `session/fork`: sent while the request is
    /// outstanding, before it is answered with an error.
    on_fork: Vec<GrokFrame>,
    /// Whether the session supports `session/fork`.
    forks: bool,
}

fn grok_frame(
    method: &'static str,
    update: serde_json::Value,
    meta: serde_json::Value,
) -> GrokFrame {
    (
        method,
        serde_json::json!({"sessionId": "s1", "update": update, "_meta": meta}),
    )
}

/// A `workflow_updated` snapshot as grok 1.0.41 sends it (trimmed to the
/// fields codeg reads plus a few it ignores).
fn grok_workflow_update(
    revision: u64,
    status: &str,
    phase: Option<&str>,
    agent: Option<&str>,
) -> GrokFrame {
    let mut update = serde_json::json!({
        "sessionUpdate": "workflow_updated",
        "run_id": GROK_RUN_ID,
        "revision": revision,
        "name": "codeg-probe",
        "objective": "codeg wire probe: two phases, one tiny agent",
        "status": status,
        "foreground": false,
        "phases": [{"title": "Alpha", "state": "active"}, {"title": "Beta", "state": "pending"}],
        "agent_budget": 128,
        "agents_used": 1,
        "active_agents": u64::from(agent.is_some()),
        "last_event": "log",
    });
    if let Some(phase) = phase {
        update["current_phase"] = phase.into();
    }
    if let Some(agent) = agent {
        update["current_agent_label"] = agent.into();
    }
    grok_frame(
        "_x.ai/session_notification",
        update,
        serde_json::json!({"eventId": format!("s1-{revision}")}),
    )
}

/// A streamed chunk. Grok stamps the running prompt's id on every chunk's
/// OUTER `_meta` — except a straggler it flushes after cancelling a turn.
fn grok_chunk(kind: &str, text: &str, prompt_id: Option<&str>) -> GrokFrame {
    let mut meta = serde_json::json!({"eventId": "s1-chunk", "totalTokens": 2597});
    if let Some(prompt_id) = prompt_id {
        meta["promptId"] = prompt_id.into();
    }
    grok_frame(
        "session/update",
        serde_json::json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}),
        meta,
    )
}

fn grok_turn_completed_frame(prompt_id: &str, stop_reason: &str) -> GrokFrame {
    grok_frame(
        "_x.ai/session_notification",
        serde_json::json!({
            "sessionUpdate": "turn_completed",
            "prompt_id": prompt_id,
            "stop_reason": stop_reason,
            "elapsed_ms": 5798,
        }),
        serde_json::json!({"eventId": "s1-done"}),
    )
}

/// The last frame of a script: its `AvailableCommands` event tells the
/// test that everything before it has been through the loop.
fn grok_barrier() -> GrokFrame {
    grok_frame(
        "session/update",
        serde_json::json!({"sessionUpdate": "available_commands_update", "availableCommands": []}),
        serde_json::json!({}),
    )
}

fn send_grok_frames(
    cx: &ConnectionTo<Client>,
    frames: &[GrokFrame],
) -> Result<(), agent_client_protocol::Error> {
    for (method, params) in frames {
        cx.send_notification(UntypedMessage::new(method, params.clone())?)?;
    }
    Ok(())
}

/// Records the connections whose parked questions / plan approvals were
/// reclaimed — what a loop test can see of `DelegationInjection`'s ask
/// registries.
#[derive(Default)]
struct RecordingAsks {
    questions: std::sync::Mutex<Vec<String>>,
    plan_approvals: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl crate::acp::question::SessionQuestionAccess for RecordingAsks {
    async fn register_question(
        &self,
        _parent_connection_id: &str,
        _questions: Vec<crate::acp::question::QuestionSpec>,
    ) -> Option<crate::acp::question::RegisteredQuestion> {
        None
    }

    async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}

    async fn cancel_questions_by_parent(&self, parent_connection_id: &str) {
        self.questions
            .lock()
            .unwrap()
            .push(parent_connection_id.to_string());
    }
}

#[async_trait::async_trait]
impl crate::acp::plan_approval::SessionPlanApprovalAccess for RecordingAsks {
    async fn register_plan_approval(
        &self,
        _parent_connection_id: &str,
        _tool_call_id: String,
        _plan_markdown: String,
    ) -> Option<crate::acp::plan_approval::RegisteredPlanApproval> {
        None
    }

    async fn cancel_plan_approvals_by_parent(&self, parent_connection_id: &str) {
        self.plan_approvals
            .lock()
            .unwrap()
            .push(parent_connection_id.to_string());
    }
}

/// `session/set_model` as the agent receives it, the raw params object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "session/set_model", response = serde_json::Value)]
#[serde(transparent)]
struct TestSetModelRequest(serde_json::Value);

/// `run_conversation_loop` for a Grok session, attached to a scripted agent.
struct GrokLoop {
    state: Arc<RwLock<SessionState>>,
    asks: Arc<RecordingAsks>,
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    events: tokio::sync::broadcast::Receiver<Arc<crate::acp::types::EventEnvelope>>,
    /// Every event the loop emitted so far, in order.
    seen: Vec<AcpEvent>,
    /// Every `session/set_model` the agent received, in order.
    set_models: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    go: Arc<tokio::sync::Notify>,
    respond: Arc<tokio::sync::Notify>,
    after: Arc<tokio::sync::Notify>,
    client: tokio::task::JoinHandle<()>,
    agent: tokio::task::JoinHandle<()>,
}

impl GrokLoop {
    async fn start(script: GrokScript) -> Self {
        use agent_client_protocol::schema::v1::{
            ForkSessionRequest, ForkSessionResponse, PromptResponse,
        };

        let supports_fork = script.forks;
        let script = Arc::new(script);
        let go = Arc::new(tokio::sync::Notify::new());
        let respond = Arc::new(tokio::sync::Notify::new());
        let after = Arc::new(tokio::sync::Notify::new());
        let (client_end, agent_end) = agent_client_protocol::Channel::duplex();

        let prompt_script = Arc::clone(&script);
        let cancel_script = Arc::clone(&script);
        let fork_script = Arc::clone(&script);
        let agent_go = Arc::clone(&go);
        let agent_respond = Arc::clone(&respond);
        let agent_after = Arc::clone(&after);
        let set_models = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_set_models = Arc::clone(&set_models);
        let agent = tokio::spawn(async move {
            let _ = Agent
                .builder()
                .on_receive_request(
                    async |_req: NewSessionRequest,
                           responder: Responder<NewSessionResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(NewSessionResponse::new(SessionId::new("s1")))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |req: TestSetModelRequest,
                                responder: Responder<serde_json::Value>,
                                _cx: ConnectionTo<Client>| {
                        agent_set_models.lock().unwrap().push(req.0);
                        responder.respond(serde_json::json!({}))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: PromptRequest,
                                responder: Responder<PromptResponse>,
                                cx: ConnectionTo<Client>| {
                        send_grok_frames(&cx, &prompt_script.in_turn)?;
                        let script = Arc::clone(&prompt_script);
                        let respond = Arc::clone(&agent_respond);
                        let after = Arc::clone(&agent_after);
                        // Off the handler, so a held response doesn't hold
                        // up the agent's other incoming messages.
                        tokio::spawn(async move {
                            if script.hold_response {
                                respond.notified().await;
                            }
                            let mut meta = serde_json::Map::new();
                            meta.insert("promptId".into(), script.prompt_id.into());
                            responder
                                .respond(PromptResponse::new(StopReason::EndTurn).meta(meta))?;
                            if script.after_response.is_empty() {
                                return Ok(());
                            }
                            after.notified().await;
                            send_grok_frames(&cx, &script.after_response)
                        });
                        Ok(())
                    },
                    on_receive_request!(),
                )
                .on_receive_notification(
                    async move |_cancel: CancelNotification, cx: ConnectionTo<Client>| {
                        send_grok_frames(&cx, &cancel_script.on_cancel)
                    },
                    on_receive_notification!(),
                )
                .on_receive_request(
                    async move |_req: ForkSessionRequest,
                                responder: Responder<ForkSessionResponse>,
                                cx: ConnectionTo<Client>| {
                        send_grok_frames(&cx, &fork_script.on_fork)?;
                        responder.respond_with_error(
                            agent_client_protocol::Error::internal_error()
                                .data("scripted fork failure"),
                        )
                    },
                    on_receive_request!(),
                )
                .connect_with(agent_end, async move |cx: ConnectionTo<Client>| {
                    agent_go.notified().await;
                    send_grok_frames(&cx, &script.later)?;
                    std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                })
                .await;
        });

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-grok".to_string(),
            AgentType::Grok,
            None,
            "win".to_string(),
            None,
        )));
        let events = state.read().await.event_stream().subscribe();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let loop_state = Arc::clone(&state);
        let catalog_state = Arc::clone(&state);
        let asks = Arc::new(RecordingAsks::default());
        let injection = DelegationInjection {
            questions: Arc::clone(&asks) as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            plan_approvals: Arc::clone(&asks)
                as Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
            ..test_delegation_injection(
                Arc::new(TestAllAgentsAvailable) as Arc<dyn AgentAvailabilityLookup>
            )
        };
        let client = tokio::spawn(async move {
            let _ = Client
                .builder()
                // As `run_connection` registers it: the catalog broadcast
                // names no session, so no session router sees it.
                .with_handler(GrokModelCatalogBroadcasts {
                    agent_type: AgentType::Grok,
                    state: catalog_state,
                    emitter: EventEmitter::Noop,
                })
                .connect_with(client_end, async move |cx: ConnectionTo<Agent>| {
                    let raw = cx
                        .send_request_to(
                            Agent,
                            UntypedMessage::new("session/new", NewSessionRequest::new("/tmp"))?,
                        )
                        .block_task()
                        .await?;
                    let response: NewSessionResponse = serde_json::from_value(raw)
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    let mut session = AgentSession::attach(&cx, response)?;
                    let perms = PendingPermissions::default();
                    let ledger = background_watch::PromptLedger::shared();
                    let stderr_tail = Arc::new(StderrTail::new());
                    run_conversation_loop(
                        &mut session,
                        "conn-grok",
                        &EventEmitter::Noop,
                        &loop_state,
                        AgentType::Grok,
                        &perms,
                        &mut cmd_rx,
                        Arc::new(TerminalRuntime::with_base_env(BTreeMap::new())),
                        "/tmp",
                        supports_fork,
                        &ledger,
                        Some(&injection),
                        &stderr_tail,
                    )
                    .await?;
                    Ok(())
                })
                .await;
        });

        Self {
            state,
            asks,
            cmd_tx,
            events,
            seen: Vec::new(),
            set_models,
            go,
            respond,
            after,
            client,
            agent,
        }
    }

    /// Admit a prompt the way `send_prompt_inner` does, then hand it over.
    async fn prompt(&self, text: &str) {
        self.state.write().await.turn_in_flight = true;
        self.cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text { text: text.into() }],
                user_message: None,
            })
            .await
            .expect("the loop is running");
    }

    async fn send(&self, cmd: ConnectionCommand) {
        self.cmd_tx.send(cmd).await.expect("the loop is running");
    }

    /// Let the scripted agent send its between-turns frames.
    fn release_later(&self) {
        self.go.notify_one();
    }

    /// Let a held prompt response go out.
    fn release_response(&self) {
        self.respond.notify_one();
    }

    /// How many times this connection's parked questions and plan
    /// approvals were reclaimed.
    fn reclaimed_asks(&self) -> (usize, usize) {
        let count = |log: &std::sync::Mutex<Vec<String>>| {
            log.lock()
                .unwrap()
                .iter()
                .filter(|c| *c == "conn-grok")
                .count()
        };
        (
            count(&self.asks.questions),
            count(&self.asks.plan_approvals),
        )
    }

    /// Wait for the prompt's turn to end, then let the frames the idle
    /// loop is to read after it go out; returns the turn end's index.
    async fn after_turn_end(&mut self) -> usize {
        let end = self
            .until("the prompt's turn end", |e| {
                matches!(e, AcpEvent::TurnComplete { .. })
            })
            .await;
        self.after.notify_one();
        end
    }

    /// Record events until one matches, and return its index in `seen`.
    async fn until(&mut self, what: &str, matches: impl Fn(&AcpEvent) -> bool) -> usize {
        let deadline = std::time::Duration::from_secs(10);
        loop {
            let envelope = tokio::time::timeout(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}; saw {:#?}", self.seen))
                .expect("event stream");
            self.seen.push(envelope.payload.clone());
            if matches(&envelope.payload) {
                return self.seen.len() - 1;
            }
        }
    }

    async fn until_barrier(&mut self) -> usize {
        self.until("the script's barrier", |e| {
            matches!(e, AcpEvent::AvailableCommands { .. })
        })
        .await
    }

    async fn shutdown(self) {
        let _ = self.cmd_tx.send(ConnectionCommand::Disconnect).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.client).await;
        self.agent.abort();
    }
}

fn turn_completes(seen: &[AcpEvent]) -> Vec<(usize, String)> {
    seen.iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            AcpEvent::TurnComplete { stop_reason, .. } => Some((i, stop_reason.clone())),
            _ => None,
        })
        .collect()
}

fn prompting_edges(seen: &[AcpEvent]) -> Vec<usize> {
    seen.iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Prompting
                }
            )
        })
        .map(|(i, _)| i)
        .collect()
}

/// The captured `/workflow codeg-probe` run: the launch turn, the run
/// ticking between turns, and the follow-up turn Grok starts on its own
/// once the run stops.
fn grok_workflow_script() -> GrokScript {
    GrokScript {
        in_turn: vec![
            grok_workflow_update(1, "active", None, None),
            grok_workflow_update(2, "active", Some("Alpha"), None),
            grok_chunk(
                "agent_message_chunk",
                "Workflow 'codeg-probe' started in the background.",
                Some("p1"),
            ),
        ],
        prompt_id: "p1",
        // Grok sends its `turn_completed` just BEFORE the response. The
        // turn loop picks between a ready response and a queued update at
        // random, so this ordering is what the idle loop reads whenever
        // the response wins.
        after_response: vec![
            grok_chunk(
                "agent_message_chunk",
                " Watch it in /workflow runs.",
                Some("p1"),
            ),
            grok_turn_completed_frame("p1", "end_turn"),
            grok_workflow_update(5, "active", Some("Alpha"), Some("probe-agent")),
            grok_barrier(),
        ],
        later: vec![
            grok_workflow_update(7, "active", Some("Beta"), None),
            grok_workflow_update(9, "complete", Some("Beta"), None),
            grok_chunk(
                "agent_thought_chunk",
                "Report the result.",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_chunk(
                "agent_message_chunk",
                "**codeg-probe** finished",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_chunk(
                "agent_message_chunk",
                " successfully.",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_turn_completed_frame(GROK_FOLLOW_UP_ID, "end_turn"),
            grok_barrier(),
        ],
        on_cancel: Vec::new(),
        on_fork: Vec::new(),
        // Released once the test has seen the launch turn's own frames, so
        // they are read in-turn rather than racing the response.
        hold_response: true,
        forks: false,
    }
}

/// Send the `/workflow codeg-probe` launch and see its turn through: its
/// frames read in-turn, then the response, then the frames the idle loop
/// reads after it. Returns the launch turn's `TurnComplete` index.
async fn launch_grok_workflow(grok: &mut GrokLoop) -> usize {
    grok.prompt("/workflow codeg-probe").await;
    grok.until("the launch turn's reply", |e| {
            matches!(e, AcpEvent::ContentDelta { text, .. } if text.starts_with("Workflow 'codeg-probe'"))
        })
        .await;
    grok.release_response();
    let launch_end = grok.after_turn_end().await;
    grok.until_barrier().await;
    launch_end
}

/// A workflow the `/workflow` prompt launched is one row on the async-task
/// strip for its whole life — including the ticks that arrive after the
/// launch turn ended — with its phase and running agent, and no stop.
#[tokio::test]
async fn a_grok_workflow_run_lands_on_the_async_task_strip() {
    let mut grok = GrokLoop::start(grok_workflow_script()).await;

    let launch_end = launch_grok_workflow(&mut grok).await;
    assert!(
        grok.seen[..launch_end]
            .iter()
            .any(|e| matches!(e, AcpEvent::AsyncTask { delta } if delta.task_id == GROK_RUN_ID)),
        "the run is on the strip while its launch turn is still open"
    );
    let running = grok
        .state
        .read()
        .await
        .async_tasks
        .get(GROK_RUN_ID)
        .cloned();
    let running = running.expect("the launched workflow has a strip row");
    assert_eq!(running.state, "running");
    assert_eq!(running.name, "codeg-probe");
    assert_eq!(running.task_type, "workflow");
    assert_eq!(
        running.description,
        "codeg wire probe: two phases, one tiny agent"
    );
    assert!(!running.can_stop, "Grok has no stop request");
    assert_eq!(running.phase.as_deref(), Some("Alpha"));
    assert_eq!(running.current_agent.as_deref(), Some("probe-agent"));

    grok.release_later();
    grok.until_barrier().await;
    let s = grok.state.read().await;
    assert_eq!(s.async_tasks.len(), 1, "one row per run");
    let done = &s.async_tasks[GROK_RUN_ID];
    assert_eq!(done.state, "completed");
    assert_eq!(done.phase.as_deref(), Some("Beta"));
    assert_eq!(
        done.current_agent.as_deref(),
        Some(""),
        "the finished agent is cleared, not left on the row"
    );
    drop(s);
    grok.shutdown().await;
}

/// The follow-up turn Grok starts once the workflow stops streams into a
/// turn of its own instead of being dropped between turns, and ends on its
/// `turn_completed` — while the launch prompt's own late `turn_completed`
/// ends nothing a second time.
#[tokio::test]
async fn a_grok_follow_up_turn_opens_before_its_first_chunk_and_closes_once() {
    let mut grok = GrokLoop::start(grok_workflow_script()).await;

    launch_grok_workflow(&mut grok).await;
    grok.release_later();
    let end = grok.until_barrier().await;
    let seen = grok.seen.clone();

    let completes = turn_completes(&seen);
    assert_eq!(
        completes.len(),
        2,
        "the prompt's own turn, then the follow-up — nothing twice: {completes:?}"
    );
    let (parent_end, follow_up_end) = (completes[0].0, completes[1].0);
    assert_eq!(
        completes[0].1, "end_turn",
        "the launch turn was not misread"
    );
    assert_eq!(completes[1].1, "end_turn");
    assert_eq!(
        prompting_edges(&seen).len(),
        2,
        "the launch prompt's turn and the follow-up, nothing else: {seen:#?}"
    );
    let follow_up_start = seen
        .iter()
        .position(|e| matches!(e, AcpEvent::Thinking { text, .. } if text == "Report the result."))
        .expect("the follow-up's first chunk is emitted");
    let opened: Vec<usize> = prompting_edges(&seen)
        .into_iter()
        .filter(|&i| i > parent_end)
        .collect();
    assert_eq!(
        opened.len(),
        1,
        "only the follow-up opens a turn — not the launch turn's straggler: {seen:#?}"
    );
    assert!(
        parent_end < opened[0] && opened[0] < follow_up_start,
        "the follow-up turn opens BEFORE its first chunk: {seen:#?}"
    );
    assert!(follow_up_start < follow_up_end && follow_up_end < end);
    assert!(
        matches!(
            seen[follow_up_end + 1],
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Connected
            }
        ),
        "it ends the way a prompt's turn does"
    );

    let s = grok.state.read().await;
    assert_eq!(s.status, ConnectionStatus::Connected);
    assert!(s.live_message.is_none(), "the follow-up turn was closed");
    assert!(!s.turn_in_flight);
    assert_eq!(
        s.last_assistant_text.as_deref(),
        Some("**codeg-probe** finished successfully.")
    );
    drop(s);
    assert_eq!(
        grok.reclaimed_asks(),
        (1, 0),
        "only the launch prompt's turn end sweeps the questions; a turn Grok \
             ended itself is waiting on nothing"
    );
    grok.shutdown().await;
}

/// A follow-up that fails reports it the way a prompt's failed turn does:
/// the coded `Error` first, then `TurnComplete` with the same reason. Grok's
/// own `error` reason has no ACP counterpart, so it reads as `unknown`.
#[tokio::test]
async fn a_failed_grok_follow_up_turn_reports_the_error_before_ending() {
    for (grok_reason, reason, code) in [
        ("max_tokens", "max_tokens", "turn_failed_max_tokens"),
        ("error", "unknown", "turn_failed_unknown"),
    ] {
        let mut grok = GrokLoop::start(GrokScript {
            later: vec![
                grok_chunk(
                    "agent_message_chunk",
                    "Partial report",
                    Some(GROK_FOLLOW_UP_ID),
                ),
                grok_turn_completed_frame(GROK_FOLLOW_UP_ID, grok_reason),
                grok_barrier(),
            ],
            ..GrokScript::default()
        })
        .await;
        grok.release_later();
        grok.until_barrier().await;

        let completes = turn_completes(&grok.seen);
        assert_eq!(completes.len(), 1, "{grok_reason}: {completes:?}");
        let (end, stop_reason) = &completes[0];
        assert_eq!(stop_reason, reason);
        assert!(
            matches!(
                &grok.seen[end - 1],
                AcpEvent::Error { code: Some(c), terminal: false, .. } if c == code
            ),
            "{grok_reason}: the failure Error lands right before TurnComplete: {:#?}",
            grok.seen
        );
        grok.shutdown().await;
    }
}

/// Stop on a turn Grok is running on its own ends it at once, like Stop on a
/// prompt's turn. What Grok sends after the cancel — its own `cancelled`
/// `turn_completed`, then a chunk it flushes with NO prompt id (captured
/// from 1.0.41) — must neither end a second turn nor open a new one that
/// nothing would ever end.
#[tokio::test]
async fn stopping_a_grok_follow_up_turn_ends_it_once_and_ignores_the_stragglers() {
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![
            grok_chunk(
                "agent_thought_chunk",
                "The user wants",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_barrier(),
        ],
        on_cancel: vec![
            grok_chunk("agent_thought_chunk", " me", Some(GROK_FOLLOW_UP_ID)),
            grok_turn_completed_frame(GROK_FOLLOW_UP_ID, "cancelled"),
            grok_chunk("agent_thought_chunk", " to", None),
            grok_barrier(),
        ],
        ..GrokScript::default()
    })
    .await;
    grok.release_later();
    grok.until_barrier().await;
    assert_eq!(prompting_edges(&grok.seen).len(), 1, "the follow-up opened");

    grok.send(ConnectionCommand::Cancel).await;
    grok.until_barrier().await;
    let completes = turn_completes(&grok.seen);
    assert_eq!(completes.len(), 1, "ended once: {completes:?}");
    assert_eq!(completes[0].1, "cancelled");
    let first_reaction = grok
        .seen
        .iter()
        .position(|e| matches!(e, AcpEvent::Thinking { text, .. } if text == " me"))
        .expect("Grok's post-cancel frames were read");
    assert!(
        completes[0].0 < first_reaction,
        "Stop ends the turn at once, not whenever Grok gets round to it"
    );
    assert_eq!(
        prompting_edges(&grok.seen).len(),
        1,
        "no straggler reopened a turn: {:#?}",
        grok.seen
    );
    assert_eq!(
        grok.reclaimed_asks(),
        (1, 1),
        "a question or plan approval the stopped turn was parked on is answered"
    );
    let s = grok.state.read().await;
    assert_eq!(s.status, ConnectionStatus::Connected);
    assert!(!s.turn_in_flight);
    drop(s);
    grok.shutdown().await;
}

/// A prompt the gate admits while Grok runs a turn of its own (a chat
/// channel doesn't queue behind it the way the composer does) first ends
/// that turn — so it stays a turn of its own — and KEEPS the gate it was
/// admitted with: the agent's turn never held it, so its end releases
/// nothing a prompt still owns.
#[tokio::test]
async fn a_prompt_admitted_during_a_grok_follow_up_turn_ends_it_first() {
    let mut grok = GrokLoop::start(GrokScript {
        in_turn: vec![grok_chunk("agent_message_chunk", "Hi.", Some("p2"))],
        hold_response: true,
        prompt_id: "p2",
        after_response: vec![grok_barrier()],
        later: vec![
            grok_chunk(
                "agent_thought_chunk",
                "Report the result.",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_barrier(),
        ],
        ..GrokScript::default()
    })
    .await;
    grok.release_later();
    grok.until_barrier().await;

    grok.prompt("hello").await;
    let agent_turn_end = grok
        .until("the follow-up's end", |e| {
            matches!(e, AcpEvent::TurnComplete { .. })
        })
        .await;
    grok.until(
        "the prompt's reply",
        |e| matches!(e, AcpEvent::ContentDelta { text, .. } if text == "Hi."),
    )
    .await;
    // The prompt's turn is still open (its response is held), and the
    // follow-up's end did not release the gate out from under it.
    assert!(
        grok.state.read().await.turn_in_flight,
        "the admitted prompt still owns the gate"
    );
    assert_eq!(
        grok.reclaimed_asks(),
        (1, 1),
        "nothing the closed turn asked can keep Grok, and so this prompt, waiting"
    );
    grok.release_response();
    grok.after_turn_end().await;
    grok.until_barrier().await;

    let seen = grok.seen.clone();
    let completes = turn_completes(&seen);
    assert_eq!(
        completes,
        vec![
            (agent_turn_end, "cancelled".to_string()),
            (completes[1].0, "end_turn".to_string())
        ]
    );
    let prompt_opened = prompting_edges(&seen)
        .into_iter()
        .find(|&i| i > agent_turn_end)
        .expect("the prompt's own turn opens");
    let reply = seen
        .iter()
        .position(|e| matches!(e, AcpEvent::ContentDelta { text, .. } if text == "Hi."))
        .expect("the prompt's reply streams");
    assert!(agent_turn_end < prompt_opened && prompt_opened < reply);
    assert!(!grok.state.read().await.turn_in_flight);
    grok.shutdown().await;
}

/// A fork between turns is refused while Grok runs one of its own: the
/// manager's between-turns check cannot see such a turn, and forking would
/// leave it open on the session being left.
#[tokio::test]
async fn a_fork_is_refused_while_a_grok_follow_up_turn_runs() {
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![
            grok_chunk(
                "agent_thought_chunk",
                "Report the result.",
                Some(GROK_FOLLOW_UP_ID),
            ),
            grok_barrier(),
        ],
        forks: true,
        ..GrokScript::default()
    })
    .await;
    grok.release_later();
    grok.until_barrier().await;

    let (reply, answer) = oneshot::channel();
    grok.send(ConnectionCommand::Fork {
        fork_point: None,
        reply,
    })
    .await;
    let refused = answer.await.expect("the loop answers");
    assert!(
        matches!(refused, Err(AcpError::TurnInProgress)),
        "refused as busy, not attempted"
    );
    grok.shutdown().await;
}

// ── Grok's model catalog broadcast, `_x.ai/models/update` (#876) ────────
//
// Shapes captured live off `grok agent stdio` (1.0.40): the broadcast is
// connection-level — NO `sessionId` — and grok repeats it within a few
// milliseconds. Its per-model `_meta.reasoningEffort` is the CATALOG
// default, while the same field in a handshake's `models` folds in the
// session's own effort.

/// One `availableModels[]` entry as grok 1.0.40 ships it.
fn grok_catalog_entry(id: &str, name: &str, effort_default: Option<&str>) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "totalContextTokens": 256000,
        "agentType": "grok-build-plan",
        "supportsReasoningEffort": effort_default.is_some(),
    });
    if let Some(default) = effort_default {
        meta["reasoningEffort"] = default.into();
        meta["reasoningEfforts"] = serde_json::json!([
            {"id": "xhigh", "value": "xhigh", "label": "Extra High", "description": "Maximum reasoning for the hardest tasks.", "default": false},
            {"id": "high", "value": "high", "label": "High", "description": "Thorough reasoning and quality. Recommended.", "default": true},
            {"id": "medium", "value": "medium", "label": "Medium", "description": "Strong quality with a faster turnaround.", "default": false},
            {"id": "low", "value": "low", "label": "Low", "description": "Fastest responses. Best for simple tasks.", "default": false}
        ]);
    }
    serde_json::json!({
        "modelId": id,
        "name": name,
        "description": format!("{name} description"),
        "_meta": meta,
    })
}

/// The catalog the issue's session received once grok had refreshed it.
fn grok_remote_catalog() -> Vec<serde_json::Value> {
    vec![
        grok_catalog_entry("grok-4.7", "Grok 4.7", Some("high")),
        grok_catalog_entry("grok-4.7-build-fast", "Grok 4.7 Build Fast", None),
        grok_catalog_entry("grok-4.6", "Grok 4.6", Some("high")),
        grok_catalog_entry("grok-4.5", "Grok 4.5", Some("high")),
    ]
}

fn grok_models_update_frame(current: &str, models: Vec<serde_json::Value>) -> GrokFrame {
    (
        "_x.ai/models/update",
        serde_json::json!({"currentModelId": current, "availableModels": models}),
    )
}

/// What `session/new` / `session/load` answers while grok still has only
/// its bundled catalog — Grok 4.6 (checked) and Grok 4.5 — as
/// `(_meta, models)`.
fn grok_bundled_handshake() -> (
    serde_json::Map<String, serde_json::Value>,
    serde_json::Value,
) {
    let meta = serde_json::from_value(serde_json::json!({
        "x.ai/sessionConfig": {"options": [
            {"id": "grok-4.6", "category": "model", "label": "Grok 4.6", "selected": true},
            {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": false},
            {"id": "xhigh", "category": "mode", "label": "Extra High", "selected": false},
            {"id": "high", "category": "mode", "label": "High", "selected": true},
            {"id": "medium", "category": "mode", "label": "Medium", "selected": false},
            {"id": "low", "category": "mode", "label": "Low", "selected": false}
        ]}
    }))
    .unwrap();
    let models = serde_json::json!({
        "currentModelId": "grok-4.6",
        "availableModels": [
            grok_catalog_entry("grok-4.6", "Grok 4.6", Some("high")),
            grok_catalog_entry("grok-4.5", "Grok 4.5", Some("high")),
        ],
    });
    (meta, models)
}

/// The picker the bundled handshake built, with the user's own effort pick
/// applied the way `set_grok_config_option` leaves it.
fn grok_handshake_picker(
    effort: &str,
) -> (Vec<SessionConfigOptionInfo>, HashMap<String, GrokModelSpec>) {
    let (meta, models) = grok_bundled_handshake();
    let specs = parse_grok_model_specs(Some(&models));
    let mut opts = synthesize_grok_config_options(Some(&meta), &specs).expect("a picker");
    let effort_selector = opts
        .iter_mut()
        .find(|o| o.id == GROK_EFFORT_OPTION_ID)
        .expect("an effort selector");
    let SessionConfigKindInfo::Select(sel) = &mut effort_selector.kind else {
        panic!("effort is a select");
    };
    sel.current_value = effort.to_string();
    (opts, specs)
}

fn select_values(option: &SessionConfigOptionInfo) -> Vec<String> {
    expect_select(&option.kind)
        .options
        .iter()
        .map(|o| o.value.clone())
        .collect()
}

fn config_option_events(seen: &[AcpEvent]) -> Vec<Vec<SessionConfigOptionInfo>> {
    seen.iter()
        .filter_map(|e| match e {
            AcpEvent::SessionConfigOptions { config_options } => Some(config_options.clone()),
            _ => None,
        })
        .collect()
}

/// The issue's sequence end to end, over the real runtime: the composer
/// was built from the bundled handshake, then the real catalog arrives on
/// the session-less `_x.ai/models/update`, three times. The model list is
/// replaced once; the checkmark stays on the model the composer shows (the
/// broadcast's `currentModelId` is no session switch), and so does the
/// effort the user picked (the catalog default is not the session's).
#[tokio::test]
async fn a_grok_catalog_broadcast_refreshes_the_model_picker() {
    let update = grok_models_update_frame("grok-4.6", grok_remote_catalog());
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![update.clone(), update.clone(), update, grok_barrier()],
        ..GrokScript::default()
    })
    .await;
    let (opts, specs) = grok_handshake_picker("medium");
    {
        let mut st = grok.state.write().await;
        st.config_options = Some(opts);
        st.grok_model_specs = Some(specs);
    }
    grok.release_later();
    grok.until_barrier().await;

    let pickers = config_option_events(&grok.seen);
    assert_eq!(
        pickers.len(),
        1,
        "three identical broadcasts refresh once: {pickers:#?}"
    );
    let picker = &pickers[0];
    assert_eq!(picker[0].id, GROK_MODEL_OPTION_ID);
    assert_eq!(
        select_values(&picker[0]),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
    assert_eq!(expect_select(&picker[0].kind).current_value, "grok-4.6");
    assert_eq!(picker[1].id, GROK_EFFORT_OPTION_ID);
    assert_eq!(expect_select(&picker[1].kind).current_value, "medium");
    assert!(
        grok.set_models.lock().unwrap().is_empty(),
        "a catalog broadcast switches nothing on the agent"
    );
    // The refresh is the connection's state, not just an event.
    assert_eq!(
        grok.state.read().await.config_options.as_ref(),
        Some(picker)
    );
    grok.shutdown().await;
}

/// The models the broadcast added are real choices: switching to one goes
/// out as a plain `session/set_model`, lands with that model's effort
/// selector — known only from the broadcast — and the switch's own re-emit
/// keeps the refreshed list.
#[tokio::test]
async fn switching_to_a_model_only_the_catalog_broadcast_listed_keeps_the_refreshed_picker() {
    // grok-4.7's efforts differ from grok-4.6's, so the selector after the
    // switch can only have come from grok-4.7's own spec.
    let mut catalog = grok_remote_catalog();
    catalog[0]["_meta"]["reasoningEffort"] = "low".into();
    catalog[0]["_meta"]["reasoningEfforts"] = serde_json::json!([
        {"id": "high", "label": "High", "description": "Thorough."},
        {"id": "low", "label": "Low", "description": "Fast."}
    ]);
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![
            grok_models_update_frame("grok-4.6", catalog),
            grok_barrier(),
        ],
        ..GrokScript::default()
    })
    .await;
    let (opts, specs) = grok_handshake_picker("medium");
    {
        let mut st = grok.state.write().await;
        st.config_options = Some(opts);
        st.grok_model_specs = Some(specs);
    }
    grok.release_later();
    grok.until_barrier().await;

    grok.send(ConnectionCommand::SetConfigOption {
        config_id: GROK_MODEL_OPTION_ID.to_string(),
        value_id: "grok-4.7".to_string(),
    })
    .await;
    grok.until("the switch's re-emit", |e| {
        matches!(e, AcpEvent::SessionConfigOptions { config_options }
                if expect_select(&config_options[0].kind).current_value == "grok-4.7")
    })
    .await;

    let pickers = config_option_events(&grok.seen);
    let picker = pickers.last().expect("the switch re-emits");
    assert_eq!(
        select_values(&picker[0]),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
    assert_eq!(picker[1].id, GROK_EFFORT_OPTION_ID);
    assert_eq!(select_values(&picker[1]), ["high", "low"]);
    assert_eq!(expect_select(&picker[1].kind).current_value, "low");
    assert_eq!(
        *grok.set_models.lock().unwrap(),
        [serde_json::json!({"sessionId": "s1", "modelId": "grok-4.7"})]
    );
    grok.shutdown().await;
}

#[test]
fn a_grok_catalog_broadcast_reads_as_picker_rows_and_specs() {
    let mut models = grok_remote_catalog();
    // A repeated id keeps its first row; an entry without one is skipped;
    // a blank name falls back to the id.
    models.push(grok_catalog_entry("grok-4.7", "Grok 4.7 again", None));
    models.push(serde_json::json!({"name": "no id"}));
    models.push(serde_json::json!({"modelId": "byo-model", "name": " "}));
    let catalog = parse_grok_model_catalog(&serde_json::json!({
        "currentModelId": "grok-4.6",
        "availableModels": models,
    }))
    .expect("a usable catalog");
    let rows: Vec<(&str, &str)> = catalog
        .models
        .iter()
        .map(|row| (row.value.as_str(), row.name.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("grok-4.7", "Grok 4.7"),
            ("grok-4.7-build-fast", "Grok 4.7 Build Fast"),
            ("grok-4.6", "Grok 4.6"),
            ("grok-4.5", "Grok 4.5"),
            ("byo-model", "byo-model"),
        ]
    );
    // Rows read like the handshake's `x.ai/sessionConfig` ones: no
    // description, though the broadcast carries one.
    assert!(catalog.models.iter().all(|row| row.description.is_none()));
    let g47 = &catalog.specs["grok-4.7-build-fast"];
    assert!(!g47.supports);
    assert_eq!(catalog.specs["grok-4.6"].default.as_deref(), Some("high"));
    assert_eq!(catalog.specs["grok-4.6"].context_window, Some(256_000));
}

/// An empty or unusable list can never wipe the picker: nothing parses.
#[test]
fn a_grok_catalog_broadcast_without_a_usable_model_is_ignored() {
    for params in [
        serde_json::json!({}),
        serde_json::json!({"currentModelId": "grok-4.6"}),
        serde_json::json!({"availableModels": null}),
        serde_json::json!({"availableModels": {"modelId": "grok-4.6"}}),
        serde_json::json!({"availableModels": []}),
        serde_json::json!({"availableModels": [{"name": "Grok 4.6"}, {"modelId": ""}, {"modelId": 7}]}),
    ] {
        assert!(parse_grok_model_catalog(&params).is_none(), "{params}");
    }
}

#[tokio::test]
async fn an_unusable_grok_catalog_broadcast_leaves_the_picker_alone() {
    let (opts, specs) = grok_handshake_picker("medium");
    let mut st = SessionState::new("c".into(), AgentType::Grok, None, "w".into(), None);
    st.config_options = Some(opts.clone());
    st.grok_model_specs = Some(specs);
    let state = Arc::new(RwLock::new(st));

    apply_grok_model_catalog(
        &state,
        &EventEmitter::Noop,
        &serde_json::json!({"currentModelId": "grok-4.6", "availableModels": []}),
    )
    .await;

    let st = state.read().await;
    assert_eq!(st.config_options.as_ref(), Some(&opts));
    assert!(st.grok_catalog_broadcast.is_none());
    assert_eq!(st.event_seq, 0, "nothing emitted");
}

fn grok_catalog(models: Vec<serde_json::Value>) -> GrokModelCatalog {
    parse_grok_model_catalog(&serde_json::json!({ "availableModels": models }))
        .expect("a usable catalog")
}

/// A session whose model the catalog dropped is still running on it: the
/// row stays (first) and so does everything about its effort.
#[test]
fn a_grok_catalog_that_drops_the_session_model_keeps_it_on_the_picker() {
    let (mut opts, mut specs) = grok_handshake_picker("medium");
    let before_effort = opts[1].clone();
    let catalog = grok_catalog(vec![grok_catalog_entry(
        "grok-4.7",
        "Grok 4.7",
        Some("high"),
    )]);
    fold_grok_catalog_specs(&mut specs, &catalog.specs);
    fold_grok_catalog_into_picker(&mut opts, &catalog, &specs);

    assert_eq!(select_values(&opts[0]), ["grok-4.6", "grok-4.7"]);
    let model = expect_select(&opts[0].kind);
    assert_eq!(model.current_value, "grok-4.6");
    assert_eq!(model.options[0].name, "Grok 4.6", "the row it had is kept");
    assert_eq!(opts[1], before_effort);
}

#[test]
fn a_grok_catalog_refresh_keeps_the_session_effort_and_follows_the_model_capabilities() {
    // The user's effort is kept even where the refreshed list drops it,
    // shown with the row it had.
    let (mut opts, mut specs) = grok_handshake_picker("medium");
    let mut narrower = grok_catalog_entry("grok-4.6", "Grok 4.6", Some("high"));
    narrower["_meta"]["reasoningEfforts"] = serde_json::json!([
        {"id": "high", "label": "High", "description": "Thorough."},
        {"id": "low", "label": "Low", "description": "Fast."}
    ]);
    let catalog = grok_catalog(vec![narrower]);
    fold_grok_catalog_specs(&mut specs, &catalog.specs);
    fold_grok_catalog_into_picker(&mut opts, &catalog, &specs);
    assert_eq!(opts[1].id, GROK_EFFORT_OPTION_ID);
    assert_eq!(select_values(&opts[1]), ["medium", "high", "low"]);
    let effort = expect_select(&opts[1].kind);
    assert_eq!(effort.current_value, "medium");
    assert_eq!(
        effort.options[0].description.as_deref(),
        Some("Strong quality with a faster turnaround.")
    );

    // A model the catalog says takes no effort loses the selector.
    let (mut opts, mut specs) = grok_handshake_picker("medium");
    let catalog = grok_catalog(vec![grok_catalog_entry("grok-4.6", "Grok 4.6", None)]);
    fold_grok_catalog_specs(&mut specs, &catalog.specs);
    fold_grok_catalog_into_picker(&mut opts, &catalog, &specs);
    assert_eq!(opts.len(), 1, "no effort selector: {opts:#?}");

    // With no effort selector to keep a value from, the model's default
    // fills in.
    let (mut opts, mut specs) = grok_handshake_picker("medium");
    opts.retain(|o| o.id != GROK_EFFORT_OPTION_ID);
    let catalog = grok_catalog(grok_remote_catalog());
    fold_grok_catalog_specs(&mut specs, &catalog.specs);
    fold_grok_catalog_into_picker(&mut opts, &catalog, &specs);
    assert_eq!(expect_select(&opts[1].kind).current_value, "high");
}

/// The catalog refreshes what each model offers, but a default the session
/// already had stays — the handshake's folds in the session's own effort.
#[test]
fn grok_catalog_specs_refresh_capabilities_and_keep_known_defaults() {
    let mut specs = parse_grok_model_specs(Some(&serde_json::json!({
        "availableModels": [
            {"modelId": "grok-4.6", "_meta": {
                "totalContextTokens": 131072, "supportsReasoningEffort": true,
                "reasoningEffort": "xhigh",
                "reasoningEfforts": [{"id": "high"}, {"id": "low"}]
            }},
            {"modelId": "retired", "_meta": {"totalContextTokens": 8192}}
        ]
    })));
    let catalog = grok_catalog(grok_remote_catalog());
    fold_grok_catalog_specs(&mut specs, &catalog.specs);

    let g46 = &specs["grok-4.6"];
    assert_eq!(
        g46.default.as_deref(),
        Some("xhigh"),
        "the session's own default stays"
    );
    assert_eq!(
        g46.options.len(),
        4,
        "what it offers comes from the catalog"
    );
    assert_eq!(g46.context_window, Some(256_000));
    assert_eq!(
        specs["grok-4.7"].default.as_deref(),
        Some("high"),
        "a new model takes the catalog's"
    );
    assert!(!specs["grok-4.7-build-fast"].supports);
    assert_eq!(
        specs["retired"].context_window,
        Some(8192),
        "a dropped model keeps its spec"
    );
}

#[test]
fn a_picker_without_a_model_selector_is_left_alone_by_a_grok_catalog() {
    let (mut opts, mut specs) = grok_handshake_picker("medium");
    opts.retain(|o| o.id != GROK_MODEL_OPTION_ID);
    let before = opts.clone();
    let catalog = grok_catalog(grok_remote_catalog());
    fold_grok_catalog_specs(&mut specs, &catalog.specs);
    fold_grok_catalog_into_picker(&mut opts, &catalog, &specs);
    assert_eq!(opts, before);
}

/// A fork is a new establishment: a broadcast from before it is already on
/// the session's picker, and must not be folded into the forked session's
/// handshake, which grok answers from the catalog that broadcast brought.
/// One landing while `session/fork` is outstanding may be newer than that
/// handshake, so it must survive for the fork's picker.
#[tokio::test]
async fn a_fork_keeps_only_the_grok_catalog_broadcast_sent_during_it() {
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![
            grok_models_update_frame("grok-4.6", grok_remote_catalog()),
            grok_barrier(),
        ],
        on_fork: vec![grok_models_update_frame(
            "grok-4.8",
            vec![grok_catalog_entry("grok-4.8", "Grok 4.8", Some("high"))],
        )],
        forks: true,
        ..GrokScript::default()
    })
    .await;
    grok.release_later();
    grok.until_barrier().await;
    assert!(grok.state.read().await.grok_catalog_broadcast.is_some());

    let (reply, answer) = oneshot::channel();
    grok.send(ConnectionCommand::Fork {
        fork_point: None,
        reply,
    })
    .await;
    // The scripted fork fails, after grok broadcast its catalog mid-request.
    let refused = answer.await.expect("the loop answers");
    assert!(refused.is_err(), "the scripted fork fails");
    let st = grok.state.read().await;
    let kept: Vec<&str> = st
        .grok_catalog_broadcast
        .as_ref()
        .expect("the broadcast sent during the fork survives")
        .models
        .iter()
        .map(|row| row.value.as_str())
        .collect();
    assert_eq!(kept, ["grok-4.8"], "the one from before the fork is gone");
    drop(st);
    grok.shutdown().await;
}

/// `currentModelId` on the broadcast is grok's process-wide pick, not a
/// switch of this session: one naming another model moves neither the
/// checkmark nor the effort, and sends the agent nothing.
#[tokio::test]
async fn a_grok_catalog_broadcast_naming_another_current_model_moves_nothing() {
    let mut grok = GrokLoop::start(GrokScript {
        later: vec![
            grok_models_update_frame("grok-4.7", grok_remote_catalog()),
            grok_barrier(),
        ],
        ..GrokScript::default()
    })
    .await;
    let (opts, specs) = grok_handshake_picker("medium");
    {
        let mut st = grok.state.write().await;
        st.config_options = Some(opts);
        st.grok_model_specs = Some(specs);
    }
    grok.release_later();
    grok.until_barrier().await;

    let pickers = config_option_events(&grok.seen);
    assert_eq!(pickers.len(), 1, "the list still refreshes: {pickers:#?}");
    let picker = &pickers[0];
    assert_eq!(
        select_values(&picker[0]),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
    assert_eq!(expect_select(&picker[0].kind).current_value, "grok-4.6");
    assert_eq!(expect_select(&picker[1].kind).current_value, "medium");
    assert!(grok.set_models.lock().unwrap().is_empty());
    grok.shutdown().await;
}

/// A broadcast that lands while a session is being established — before
/// the picker exists — is folded into the picker the establishment emits,
/// and taken, so a later establishment never re-applies it.
#[tokio::test]
async fn a_grok_catalog_broadcast_during_establishment_reaches_its_picker() {
    let state = Arc::new(RwLock::new(SessionState::new(
        "c".into(),
        AgentType::Grok,
        None,
        "w".into(),
        None,
    )));
    let update = grok_models_update_frame("grok-4.6", grok_remote_catalog());
    apply_grok_model_catalog(&state, &EventEmitter::Noop, &update.1).await;
    {
        let st = state.read().await;
        assert_eq!(st.event_seq, 0, "no picker yet, so nothing to emit");
        assert!(st.grok_catalog_broadcast.is_some());
    }

    // The establishment caches the handshake's specs, then emits — with an
    // effort that is not the catalog's default.
    let (opts, specs) = grok_handshake_picker("medium");
    state.write().await.grok_model_specs = Some(specs);
    emit_grok_established_picker(&state, &EventEmitter::Noop, opts).await;

    let st = state.read().await;
    let picker = st.config_options.as_ref().expect("emitted");
    assert_eq!(
        select_values(&picker[0]),
        ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
    );
    assert_eq!(expect_select(&picker[0].kind).current_value, "grok-4.6");
    assert_eq!(expect_select(&picker[1].kind).current_value, "medium");
    assert!(
        st.grok_catalog_broadcast.is_none(),
        "taken by the establishment"
    );
    assert!(
        st.grok_model_specs
            .as_ref()
            .unwrap()
            .contains_key("grok-4.7"),
        "a later switch to a new model finds its spec"
    );
}

/// `_session/steering` as the agent receives it, the raw params object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "_session/steering", response = serde_json::Value)]
#[serde(transparent)]
struct TestSteeringRequest(serde_json::Value);

/// A claude connection's conversation loop behind an agent that holds each
/// prompt response until told to answer and accepts every steer — for the
/// places a question parked by codeg's `ask_user_question` is swept.
struct AskSweepLoop {
    state: Arc<RwLock<SessionState>>,
    asks: Arc<RecordingAsks>,
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    events: tokio::sync::broadcast::Receiver<Arc<crate::acp::types::EventEnvelope>>,
    respond: Arc<tokio::sync::Notify>,
    /// How many question sweeps had run when each steer reached the agent.
    sweeps_at_steer: Arc<std::sync::Mutex<Vec<usize>>>,
    client: tokio::task::JoinHandle<()>,
    agent: tokio::task::JoinHandle<()>,
}

impl AskSweepLoop {
    const CONN: &'static str = "conn-claude";

    async fn start() -> Self {
        use agent_client_protocol::schema::v1::PromptResponse;

        let asks = Arc::new(RecordingAsks::default());
        let respond = Arc::new(tokio::sync::Notify::new());
        let sweeps_at_steer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (client_end, agent_end) = agent_client_protocol::Channel::duplex();

        let agent_respond = Arc::clone(&respond);
        let agent_asks = Arc::clone(&asks);
        let agent_sweeps = Arc::clone(&sweeps_at_steer);
        let agent = tokio::spawn(async move {
            let _ = Agent
                .builder()
                .on_receive_request(
                    async |_req: NewSessionRequest,
                           responder: Responder<NewSessionResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(NewSessionResponse::new(SessionId::new("s1")))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: PromptRequest,
                                responder: Responder<PromptResponse>,
                                _cx: ConnectionTo<Client>| {
                        let respond = Arc::clone(&agent_respond);
                        // Off the handler, so the held response doesn't hold
                        // up the steer behind it.
                        tokio::spawn(async move {
                            respond.notified().await;
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        });
                        Ok(())
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: TestSteeringRequest,
                                responder: Responder<serde_json::Value>,
                                _cx: ConnectionTo<Client>| {
                        let swept = agent_asks.questions.lock().unwrap().len();
                        agent_sweeps.lock().unwrap().push(swept);
                        responder.respond(serde_json::json!({"outcome": "injected"}))
                    },
                    on_receive_request!(),
                )
                .connect_with(agent_end, async |_cx: ConnectionTo<Client>| {
                    std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                })
                .await;
        });

        let state = Arc::new(RwLock::new(SessionState::new(
            Self::CONN.to_string(),
            AgentType::ClaudeCode,
            None,
            "win".to_string(),
            None,
        )));
        let events = state.read().await.event_stream().subscribe();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let loop_state = Arc::clone(&state);
        let injection = DelegationInjection {
            questions: Arc::clone(&asks) as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            plan_approvals: Arc::clone(&asks)
                as Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
            ..test_delegation_injection(
                Arc::new(TestAllAgentsAvailable) as Arc<dyn AgentAvailabilityLookup>
            )
        };
        let client = tokio::spawn(async move {
            let _ = Client
                .builder()
                .connect_with(client_end, async move |cx: ConnectionTo<Agent>| {
                    let raw = cx
                        .send_request_to(
                            Agent,
                            UntypedMessage::new("session/new", NewSessionRequest::new("/tmp"))?,
                        )
                        .block_task()
                        .await?;
                    let response: NewSessionResponse = serde_json::from_value(raw)
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    let mut session = AgentSession::attach(&cx, response)?;
                    let perms = PendingPermissions::default();
                    let ledger = background_watch::PromptLedger::shared();
                    let stderr_tail = Arc::new(StderrTail::new());
                    run_conversation_loop(
                        &mut session,
                        Self::CONN,
                        &EventEmitter::Noop,
                        &loop_state,
                        AgentType::ClaudeCode,
                        &perms,
                        &mut cmd_rx,
                        Arc::new(TerminalRuntime::with_base_env(BTreeMap::new())),
                        "/tmp",
                        false,
                        &ledger,
                        Some(&injection),
                        &stderr_tail,
                    )
                    .await?;
                    Ok(())
                })
                .await;
        });

        Self {
            state,
            asks,
            cmd_tx,
            events,
            respond,
            sweeps_at_steer,
            client,
            agent,
        }
    }

    /// Admit a prompt the way `send_prompt_inner` does, hand it over, and
    /// wait until its turn is open.
    async fn open_turn(&mut self, text: &str) {
        self.state.write().await.turn_in_flight = true;
        self.cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text { text: text.into() }],
                user_message: None,
            })
            .await
            .expect("the loop is running");
        self.until("the turn opening", |e| {
            matches!(
                e,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Prompting
                }
            )
        })
        .await;
    }

    async fn steer(&self, text: &str) -> Result<SteerOutcome, AcpError> {
        let (reply, outcome) = oneshot::channel();
        self.cmd_tx
            .send(ConnectionCommand::Steer {
                blocks: vec![PromptInputBlock::Text { text: text.into() }],
                reply,
            })
            .await
            .expect("the loop is running");
        outcome.await.expect("the loop answers")
    }

    /// How many times this connection's parked questions were swept.
    fn question_sweeps(&self) -> usize {
        self.asks
            .questions
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == Self::CONN)
            .count()
    }

    /// Wait — bounded — until `count` question sweeps have run. The turn
    /// end's sweep follows `TurnComplete`, so seeing that event does not
    /// yet mean the sweep ran.
    async fn until_question_sweeps(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.question_sweeps() < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {count} question sweeps; saw {}",
                self.question_sweeps()
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    async fn until(&mut self, what: &str, matches: impl Fn(&AcpEvent) -> bool) {
        let deadline = std::time::Duration::from_secs(10);
        loop {
            let envelope = tokio::time::timeout(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("event stream");
            if matches(&envelope.payload) {
                return;
            }
        }
    }

    async fn shutdown(self) {
        let _ = self.cmd_tx.send(ConnectionCommand::Disconnect).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.client).await;
        self.agent.abort();
    }
}

/// A steer sweeps the questions parked on its connection BEFORE the steer
/// goes out. Claude Code 2.1.286 (claude-agent-acp 0.85.0) moves the blocked
/// MCP call of an `ask_user_question` to the background when a steer lands,
/// where 2.1.284 interrupted it and so cancelled the ask; sweeping first
/// keeps redirecting the agent meaning "drop the question". What the sweep
/// does to a parked entry — decline it, close its card — is the manager's,
/// and tested there (`cancel_questions_by_parent_*`).
#[tokio::test]
async fn a_steer_sweeps_the_parked_questions_before_it_reaches_the_agent() {
    let mut claude = AskSweepLoop::start().await;
    claude.open_turn("ask me which database to use").await;

    let outcome = claude.steer("never mind, keep SQLite").await;
    assert!(matches!(outcome, Ok(SteerOutcome::Injected)), "{outcome:?}");
    assert_eq!(
        *claude.sweeps_at_steer.lock().unwrap(),
        vec![1],
        "the question was declined before the steer reached the agent"
    );

    // The turn's own end sweeps again; with nothing parked, a no-op.
    claude.respond.notify_one();
    claude.until_question_sweeps(2).await;
    claude.shutdown().await;
}

/// A turn that ends sweeps a question still parked on its connection:
/// `TurnComplete` took the card off every client, so nothing could answer it,
/// and while it stayed parked every later ask on the connection would be
/// refused. Reachable since Claude Code 2.1.286, whose MCP calls can move to
/// the background and outlive their turn.
#[tokio::test]
async fn a_turn_end_sweeps_the_questions_parked_on_its_connection() {
    let mut claude = AskSweepLoop::start().await;
    claude.open_turn("hello").await;
    assert_eq!(
        claude.question_sweeps(),
        0,
        "nothing is swept while the turn runs"
    );

    claude.respond.notify_one();
    claude
        .until("the turn end", |e| {
            matches!(e, AcpEvent::TurnComplete { .. })
        })
        .await;
    claude.until_question_sweeps(1).await;
    assert!(
        claude.sweeps_at_steer.lock().unwrap().is_empty(),
        "no steer was sent"
    );
    claude.shutdown().await;
}

/// How the agent behind [`ForkTransition`] answers `session/close`.
#[derive(Clone, Copy)]
enum CloseAnswer {
    Closed,
    Refused,
    /// Never: the request stays open for the rest of the test.
    Unanswered,
}

/// One fork, the way `run_connection` drives it: `run_conversation_loop`
/// on `parent` until the fork, then `handle_fork_or_exit` on to the
/// session the agent forks to. The agent records every `session/close`.
struct ForkTransition {
    state: Arc<RwLock<SessionState>>,
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    events: tokio::sync::broadcast::Receiver<Arc<crate::acp::types::EventEnvelope>>,
    closes: Arc<std::sync::Mutex<Vec<String>>>,
    close_seen: Arc<tokio::sync::Notify>,
    /// The `Unanswered` closes' responders, kept alive so the requests
    /// stay open.
    _unanswered: Arc<
        std::sync::Mutex<Vec<Responder<agent_client_protocol::schema::v1::CloseSessionResponse>>>,
    >,
    client: tokio::task::JoinHandle<()>,
    agent: tokio::task::JoinHandle<()>,
}

impl ForkTransition {
    async fn start(fork_id: &'static str, supports_close: bool, answer: CloseAnswer) -> Self {
        use agent_client_protocol::schema::v1::{
            CloseSessionResponse, ForkSessionRequest, ForkSessionResponse, PromptResponse,
        };

        let closes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let close_seen = Arc::new(tokio::sync::Notify::new());
        let unanswered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (client_end, agent_end) = agent_client_protocol::Channel::duplex();
        let agent_closes = Arc::clone(&closes);
        let agent_close_seen = Arc::clone(&close_seen);
        let agent_unanswered = Arc::clone(&unanswered);
        let agent = tokio::spawn(async move {
            let _ = Agent
                .builder()
                .on_receive_request(
                    async |_req: NewSessionRequest,
                           responder: Responder<NewSessionResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(NewSessionResponse::new(SessionId::new("parent")))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: ForkSessionRequest,
                                responder: Responder<ForkSessionResponse>,
                                _cx: ConnectionTo<Client>| {
                        responder.respond(ForkSessionResponse::new(SessionId::new(fork_id)))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async |_req: ResumeSessionRequest,
                           responder: Responder<ResumeSessionResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(ResumeSessionResponse::new())
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |req: CloseSessionRequest,
                                responder: Responder<CloseSessionResponse>,
                                _cx: ConnectionTo<Client>| {
                        agent_closes
                            .lock()
                            .unwrap()
                            .push(req.session_id.0.to_string());
                        agent_close_seen.notify_one();
                        match answer {
                            CloseAnswer::Closed => responder.respond(CloseSessionResponse::new()),
                            CloseAnswer::Refused => responder.respond_with_error(
                                agent_client_protocol::util::internal_error("not closing"),
                            ),
                            CloseAnswer::Unanswered => {
                                agent_unanswered.lock().unwrap().push(responder);
                                Ok(())
                            }
                        }
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async |_req: PromptRequest,
                           responder: Responder<PromptResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    },
                    on_receive_request!(),
                )
                .connect_with(agent_end, async |_cx: ConnectionTo<Client>| {
                    std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                })
                .await;
        });

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-fork".to_string(),
            AgentType::Codex,
            None,
            "win".to_string(),
            None,
        )));
        let events = state.read().await.event_stream().subscribe();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let loop_state = Arc::clone(&state);
        let client = tokio::spawn(async move {
            let _ = Client
                .builder()
                .connect_with(client_end, async move |cx: ConnectionTo<Agent>| {
                    let raw = cx
                        .send_request_to(
                            Agent,
                            UntypedMessage::new("session/new", NewSessionRequest::new("/tmp"))?,
                        )
                        .block_task()
                        .await?;
                    let response: NewSessionResponse = serde_json::from_value(raw)
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    let mut session = AgentSession::attach(&cx, response)?;
                    let perms = PendingPermissions::default();
                    let ledger = background_watch::PromptLedger::shared();
                    let stderr_tail = Arc::new(StderrTail::new());
                    let terminals = Arc::new(TerminalRuntime::with_base_env(BTreeMap::new()));
                    let loop_result = run_conversation_loop(
                        &mut session,
                        "conn-fork",
                        &EventEmitter::Noop,
                        &loop_state,
                        AgentType::Codex,
                        &perms,
                        &mut cmd_rx,
                        Arc::clone(&terminals),
                        "/tmp",
                        true,
                        &ledger,
                        None,
                        &stderr_tail,
                    )
                    .await;
                    drop(session);
                    handle_fork_or_exit(
                        loop_result,
                        "conn-fork",
                        &EventEmitter::Noop,
                        &loop_state,
                        AgentType::Codex,
                        &perms,
                        &mut cmd_rx,
                        terminals,
                        Path::new("/tmp"),
                        "/tmp",
                        true,
                        supports_close,
                        &[],
                        &ledger,
                        None,
                        &stderr_tail,
                    )
                    .await
                })
                .await;
        });

        Self {
            state,
            cmd_tx,
            events,
            closes,
            close_seen,
            _unanswered: unanswered,
            client,
            agent,
        }
    }

    /// Fork the session the loop is on; returns the reply's `(forked,
    /// original)` session ids.
    async fn fork(&self) -> (String, String) {
        let (reply, answer) = oneshot::channel();
        self.cmd_tx
            .send(ConnectionCommand::Fork {
                fork_point: None,
                reply,
            })
            .await
            .expect("the loop is running");
        let forked = answer
            .await
            .expect("the loop answers")
            .expect("the fork succeeds");
        (forked.forked_session_id, forked.original_session_id)
    }

    /// The first event that matches, waiting for it if need be.
    async fn until(&mut self, what: &str, matches: impl Fn(&AcpEvent) -> bool) -> AcpEvent {
        loop {
            let envelope =
                tokio::time::timeout(std::time::Duration::from_secs(10), self.events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                    .expect("event stream");
            if matches(&envelope.payload) {
                return envelope.payload.clone();
            }
        }
    }

    /// Prompt the session the loop is on and wait out the turn; returns
    /// the session it ran on and its stop reason. The agent answers every
    /// prompt with `end_turn` and nothing else, which codeg reports as
    /// `empty` — what counts is that the answer arrives.
    async fn prompt_to_end(&mut self) -> (String, String) {
        self.state.write().await.turn_in_flight = true;
        self.cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "go on".into(),
                }],
                user_message: None,
            })
            .await
            .expect("the loop is running");
        match self
            .until("the turn's end", |e| {
                matches!(e, AcpEvent::TurnComplete { .. })
            })
            .await
        {
            AcpEvent::TurnComplete {
                session_id,
                stop_reason,
                ..
            } => (session_id, stop_reason),
            _ => unreachable!(),
        }
    }

    /// Wait until the agent has been asked to close a session.
    async fn close_requested(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.close_seen.notified(),
        )
        .await
        .expect("timed out waiting for a session/close");
    }

    /// End the loop, then the agent; returns every session the agent was
    /// asked to close while the connection lasted.
    async fn shutdown(self) -> Vec<String> {
        let _ = self.cmd_tx.send(ConnectionCommand::Disconnect).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.client).await;
        self.agent.abort();
        let closes = self.closes.lock().unwrap().clone();
        closes
    }
}

/// What [`ForkTransition::prompt_to_end`] returns for a turn the agent
/// answered on `session`.
fn answered(session: &str) -> (String, String) {
    (session.to_string(), "empty".to_string())
}

/// The parent a fork leaves is closed on the agent — nothing else is: the
/// child the loop moves on to stays open and keeps working.
#[tokio::test]
async fn a_fork_closes_the_session_it_leaves() {
    let mut fork = ForkTransition::start("child", true, CloseAnswer::Closed).await;
    assert_eq!(
        fork.fork().await,
        ("child".to_string(), "parent".to_string())
    );
    fork.close_requested().await;
    fork.until(
        "the child's start",
        |e| matches!(e, AcpEvent::SessionStarted { session_id } if session_id == "child"),
    )
    .await;
    assert_eq!(fork.prompt_to_end().await, answered("child"));
    assert_eq!(fork.shutdown().await, vec!["parent".to_string()]);
}

/// An agent that does not advertise `session/close` is never sent one.
#[tokio::test]
async fn a_fork_sends_no_close_the_agent_did_not_advertise() {
    let mut fork = ForkTransition::start("child", false, CloseAnswer::Closed).await;
    fork.fork().await;
    fork.until(
        "the child's start",
        |e| matches!(e, AcpEvent::SessionStarted { session_id } if session_id == "child"),
    )
    .await;
    assert_eq!(fork.prompt_to_end().await, answered("child"));
    let closed = fork.shutdown().await;
    assert!(closed.is_empty(), "closed {closed:?}");
}

/// A fork that answers with the parent's own id has not left it, so
/// nothing is closed — closing would close the session the loop goes on with.
#[tokio::test]
async fn a_fork_onto_the_same_session_closes_nothing() {
    let mut fork = ForkTransition::start("parent", true, CloseAnswer::Closed).await;
    assert_eq!(
        fork.fork().await,
        ("parent".to_string(), "parent".to_string())
    );
    assert_eq!(fork.prompt_to_end().await, answered("parent"));
    let closed = fork.shutdown().await;
    assert!(closed.is_empty(), "closed {closed:?}");
}

/// Nothing waits on the close: an agent that never answers it still runs
/// the child's first turn.
#[tokio::test]
async fn an_unanswered_close_does_not_hold_up_the_fork() {
    let mut fork = ForkTransition::start("child", true, CloseAnswer::Unanswered).await;
    fork.fork().await;
    fork.close_requested().await;
    assert_eq!(fork.prompt_to_end().await, answered("child"));
    assert_eq!(fork.shutdown().await, vec!["parent".to_string()]);
}

/// The close is best-effort: an agent refusing it leaves the connection,
/// and the child on it, working.
#[tokio::test]
async fn a_refused_close_leaves_the_fork_working() {
    let mut fork = ForkTransition::start("child", true, CloseAnswer::Refused).await;
    fork.fork().await;
    fork.close_requested().await;
    assert_eq!(fork.prompt_to_end().await, answered("child"));
    assert_eq!(fork.shutdown().await, vec!["parent".to_string()]);
}

#[test]
fn grok_workflow_update_maps_to_one_restating_async_task_delta() {
    let (method, params) = grok_workflow_update(5, "active", Some("Alpha"), Some("probe-agent"));
    let dispatch = Dispatch::Notification(UntypedMessage::new(method, params).unwrap());
    let delta = grok_workflow_task_delta(&dispatch, AgentType::Grok).expect("maps");
    assert_eq!(delta.task_id, GROK_RUN_ID);
    assert!(delta.spawned, "every frame restates the run");
    assert_eq!(delta.name.as_deref(), Some("codeg-probe"));
    assert_eq!(delta.task_type.as_deref(), Some("workflow"));
    assert_eq!(delta.can_stop, Some(false));
    assert_eq!(delta.state.as_deref(), Some("running"));
    assert_eq!(delta.phase.as_deref(), Some("Alpha"));
    assert_eq!(delta.current_agent.as_deref(), Some("probe-agent"));
    assert_eq!(delta.summary.as_deref(), Some(""));

    // Grok's own gated agent and other agents' frames are left alone.
    assert!(grok_workflow_task_delta(&dispatch, AgentType::Codex).is_none());
    // Grok persists these frames on the other private method, and a
    // `session/load` replay must be able to recognize them to drop them.
    let (_, params) = grok_workflow_update(9, "complete", Some("Beta"), None);
    let persisted = Dispatch::Notification(
        UntypedMessage::new("_x.ai/session/update", params.clone()).unwrap(),
    );
    assert!(grok_workflow_task_delta(&persisted, AgentType::Grok).is_some());
    // Never on the standard channel, and never without a run id.
    let standard = Dispatch::Notification(UntypedMessage::new("session/update", params).unwrap());
    assert!(grok_workflow_task_delta(&standard, AgentType::Grok).is_none());
    let mut anonymous = grok_workflow_update(1, "active", None, None).1;
    anonymous["update"]
        .as_object_mut()
        .unwrap()
        .remove("run_id");
    let anonymous = Dispatch::Notification(
        UntypedMessage::new("_x.ai/session_notification", anonymous).unwrap(),
    );
    assert!(grok_workflow_task_delta(&anonymous, AgentType::Grok).is_none());
}

/// The pause and failure frames as grok 1.0.41 sends them: the reason
/// rides `pause_message` for both.
#[test]
fn grok_workflow_pause_and_failure_carry_their_reason() {
    let frame = |status: &str, extra: serde_json::Value| {
        let (method, mut params) = grok_workflow_update(4, status, Some("Wait"), None);
        for (k, v) in extra.as_object().unwrap() {
            params["update"][k] = v.clone();
        }
        grok_workflow_task_delta(
            &Dispatch::Notification(UntypedMessage::new(method, params).unwrap()),
            AgentType::Grok,
        )
        .unwrap()
    };
    let paused = frame(
        "user_paused",
        serde_json::json!({"pause_message": "codeg probe gate: resume me"}),
    );
    assert_eq!(paused.state.as_deref(), Some("paused"));
    assert_eq!(
        paused.summary.as_deref(),
        Some("codeg probe gate: resume me")
    );
    let failed = frame(
        "failed",
        serde_json::json!({"pause_message": "Runtime error: codeg probe failure (line 8, position 1)"}),
    );
    assert_eq!(failed.state.as_deref(), Some("failed"));
    assert_eq!(
        failed.summary.as_deref(),
        Some("Runtime error: codeg probe failure (line 8, position 1)")
    );
    let complete = frame(
        "complete",
        serde_json::json!({"result_summary": "{\"summary\":\"gate passed\"}"}),
    );
    assert_eq!(complete.state.as_deref(), Some("completed"));
    assert_eq!(
        complete.summary.as_deref(),
        Some("{\"summary\":\"gate passed\"}")
    );
}

#[test]
fn grok_workflow_statuses_map_onto_the_async_task_states() {
    for (status, state) in [
        ("active", "running"),
        ("user_paused", "paused"),
        ("back_off_paused", "paused"),
        ("no_progress_paused", "paused"),
        ("infra_paused", "paused"),
        ("blocked", "paused"),
        ("budget_limited", "paused"),
        ("complete", "completed"),
        ("failed", "failed"),
        ("interrupted", "failed"),
        ("cancelled", "stopped"),
        ("paused", "paused"),
        // Unknown: passed through, so it reads as still live.
        ("warming_up", "warming_up"),
        ("unpaused", "unpaused"),
    ] {
        assert_eq!(grok_workflow_state(status), state, "{status}");
    }
    for state in ["completed", "failed", "stopped"] {
        assert!(crate::acp::types::async_task_state_is_terminal(state));
    }
}

#[test]
fn grok_turn_completed_reads_the_prompt_and_maps_the_reason() {
    let read = |frame: GrokFrame, agent_type| {
        grok_turn_completed(
            &Dispatch::Notification(UntypedMessage::new(frame.0, frame.1).unwrap()),
            agent_type,
        )
    };
    assert_eq!(
        read(
            grok_turn_completed_frame(GROK_FOLLOW_UP_ID, "end_turn"),
            AgentType::Grok
        ),
        Some(GrokTurnCompleted {
            prompt_id: Some(GROK_FOLLOW_UP_ID.to_string()),
            stop_reason: "end_turn",
        })
    );
    assert_eq!(
        read(grok_turn_completed_frame("p1", "error"), AgentType::Grok)
            .unwrap()
            .stop_reason,
        "unknown"
    );
    assert!(read(
        grok_turn_completed_frame("p1", "end_turn"),
        AgentType::Codex
    )
    .is_none());
    assert!(read(
        grok_workflow_update(1, "active", None, None),
        AgentType::Grok
    )
    .is_none());
}

#[test]
fn only_a_stamped_new_chunk_opens_a_grok_turn() {
    let opener = |frame: GrokFrame, cb: &CodeBuddyLiveState| {
        grok_agent_turn_opener(
            &Dispatch::Notification(UntypedMessage::new(frame.0, frame.1).unwrap()),
            AgentType::Grok,
            cb,
        )
    };
    let mut cb = CodeBuddyLiveState::default();
    remember_grok_ended_prompt(&mut cb.grok_ended_prompt_ids, "p1");
    assert_eq!(
        opener(
            grok_chunk("agent_thought_chunk", "x", Some(GROK_FOLLOW_UP_ID)),
            &cb
        )
        .as_deref(),
        Some(GROK_FOLLOW_UP_ID)
    );
    assert!(opener(grok_chunk("agent_message_chunk", "x", Some("p2")), &cb).is_some());
    // A straggler of a turn that already ended, and one with no id at all.
    assert!(opener(grok_chunk("agent_message_chunk", "x", Some("p1")), &cb).is_none());
    assert!(opener(grok_chunk("agent_message_chunk", "x", None), &cb).is_none());
    // Not a chunk.
    assert!(opener(grok_barrier(), &cb).is_none());
    // Already open.
    cb.grok_agent_turn = Some(GROK_FOLLOW_UP_ID.to_string());
    assert!(opener(grok_chunk("agent_message_chunk", "x", Some("p2")), &cb).is_none());
    // Never another agent.
    cb.grok_agent_turn = None;
    let frame = grok_chunk("agent_message_chunk", "x", Some("p2"));
    assert!(grok_agent_turn_opener(
        &Dispatch::Notification(UntypedMessage::new(frame.0, frame.1).unwrap()),
        AgentType::ClaudeCode,
        &cb,
    )
    .is_none());
}

/// Prompt A's response can win the turn loop's select while A's last
/// frames are still queued; if prompt B then answers at once (a slash
/// command) its response can win the same way. B ending must not make A's
/// stragglers look new — they would open a turn A's own `turn_completed`
/// then ends a second time.
#[test]
fn a_turn_ending_right_after_another_does_not_forget_it() {
    let straggler = |prompt_id: &str| {
        let frame = grok_chunk("agent_message_chunk", "x", Some(prompt_id));
        Dispatch::Notification(UntypedMessage::new(frame.0, frame.1).unwrap())
    };
    let mut cb = CodeBuddyLiveState::default();
    note_grok_turn_prompt_id(&straggler("a"), &mut cb.grok_ended_prompt_ids);
    remember_grok_ended_prompt(&mut cb.grok_ended_prompt_ids, "b");
    assert!(grok_agent_turn_opener(&straggler("a"), AgentType::Grok, &cb).is_none());
    assert!(grok_agent_turn_opener(&straggler("b"), AgentType::Grok, &cb).is_none());

    // However many instant turns end before A's frames drain.
    for i in 0..64 {
        remember_grok_ended_prompt(&mut cb.grok_ended_prompt_ids, &format!("later-{i}"));
    }
    assert!(grok_agent_turn_opener(&straggler("a"), AgentType::Grok, &cb).is_none());
    // A turn nobody has seen end still opens.
    assert!(grok_agent_turn_opener(&straggler("c"), AgentType::Grok, &cb).is_some());
}

/// One strip row per run: every frame of a run revises its row, and a
/// second run gets its own.
#[test]
fn each_grok_workflow_run_is_one_row() {
    let mut s = SessionState::new(
        "conn-grok".to_string(),
        AgentType::Grok,
        None,
        "win".to_string(),
        None,
    );
    let apply = |s: &mut SessionState, frame: GrokFrame| {
        let dispatch = Dispatch::Notification(UntypedMessage::new(frame.0, frame.1).unwrap());
        let delta = grok_workflow_task_delta(&dispatch, AgentType::Grok).expect("maps");
        s.apply_event(&AcpEvent::AsyncTask { delta });
    };
    apply(&mut s, grok_workflow_update(1, "active", None, None));
    apply(
        &mut s,
        grok_workflow_update(5, "active", Some("Alpha"), Some("probe-agent")),
    );
    let (method, mut second) = grok_workflow_update(1, "active", None, None);
    second["update"]["run_id"] = "wf_second".into();
    apply(&mut s, (method, second));
    assert_eq!(s.async_tasks.len(), 2);
    assert_eq!(s.async_tasks[GROK_RUN_ID].phase.as_deref(), Some("Alpha"));
    assert_eq!(s.async_tasks["wf_second"].phase.as_deref(), Some(""));

    // A frame with no status says nothing about its run: no row from it.
    let (method, mut blank) = grok_workflow_update(1, "active", None, None);
    blank["update"]["run_id"] = "wf_blank".into();
    blank["update"].as_object_mut().unwrap().remove("status");
    let dispatch = Dispatch::Notification(UntypedMessage::new(method, blank).unwrap());
    assert!(grok_workflow_task_delta(&dispatch, AgentType::Grok).is_none());
}

#[test]
fn a_grok_turn_completed_ends_only_the_open_agent_turn() {
    let done = |id: Option<&str>| GrokTurnCompleted {
        prompt_id: id.map(str::to_string),
        stop_reason: "end_turn",
    };
    assert!(!grok_turn_completed_ends(None, &done(Some("p1"))));
    assert!(grok_turn_completed_ends(Some("f"), &done(Some("f"))));
    assert!(
        !grok_turn_completed_ends(Some("f"), &done(Some("p1"))),
        "a prompt's late turn_completed ends nothing"
    );
    assert!(grok_turn_completed_ends(Some("f"), &done(None)));
}

// ── A custom agent's history and the idle loop (#903) ───────────────────
//
// A custom agent has no store of its own for codeg to parse later: its
// history IS the ACP transcript this connection writes as frames go by.
// Work the agent does between prompts — the follow-up to a background job,
// the late completion of a call an earlier turn started — reaches codeg on
// the IDLE loop with no `session/prompt` around it. Driven over the real
// runtime with the scripted-agent plumbing of the Grok loop tests above.

/// The custom agent these loop tests drive. Deliberately unregistered:
/// nothing on the recording path looks the id up.
const IDLE_PROBE_AGENT: &str = "idle-probe";

fn custom_update(update: serde_json::Value) -> GrokFrame {
    (
        "session/update",
        serde_json::json!({"sessionId": "s1", "update": update}),
    )
}

fn custom_text(text: &str) -> GrokFrame {
    custom_update(serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text},
    }))
}

/// One line per turn, for assertions that read like the conversation.
fn turn_shape(turns: &[crate::models::message::MessageTurn]) -> Vec<String> {
    use crate::models::message::ContentBlock as Block;
    turns
        .iter()
        .map(|turn| {
            let blocks: Vec<String> = turn
                .blocks
                .iter()
                .map(|block| match block {
                    Block::Text { text } => format!("text {text:?}"),
                    Block::ToolUse {
                        tool_use_id,
                        tool_name,
                        ..
                    } => format!("call {} {tool_name}", tool_use_id.as_deref().unwrap_or("?")),
                    Block::ToolResult {
                        tool_use_id,
                        output_preview,
                        ..
                    } => format!(
                        "result {} {:?}",
                        tool_use_id.as_deref().unwrap_or("?"),
                        output_preview.as_deref().unwrap_or("")
                    ),
                    other => format!("{other:?}"),
                })
                .collect();
            format!("{:?}: {}", turn.role, blocks.join(" | "))
        })
        .collect()
}

/// `run_conversation_loop` for a custom agent. Each prompt streams the next
/// turn of the script, then holds its response until the test releases it,
/// so every frame of a prompted turn is read INSIDE that turn. Frames handed
/// to [`CustomLoop::agent_sends`] go out at once, outside any prompt.
struct CustomLoop {
    state: Arc<RwLock<SessionState>>,
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    events: tokio::sync::broadcast::Receiver<Arc<crate::acp::types::EventEnvelope>>,
    /// Every event the loop emitted so far, in order.
    seen: Vec<AcpEvent>,
    unprompted: tokio::sync::mpsc::UnboundedSender<Vec<GrokFrame>>,
    respond: Arc<tokio::sync::Notify>,
    /// What the agent answers the next `_session/steering` requests with,
    /// in order; `injected` once these run out.
    steer_outcomes: Arc<std::sync::Mutex<VecDeque<serde_json::Value>>>,
    client: tokio::task::JoinHandle<()>,
    agent: tokio::task::JoinHandle<()>,
}

impl CustomLoop {
    async fn start(turns: Vec<Vec<GrokFrame>>) -> Self {
        use agent_client_protocol::schema::v1::PromptResponse;

        let agent_type = AgentType::custom(IDLE_PROBE_AGENT).expect("a valid custom id");
        let turns = Arc::new(std::sync::Mutex::new(VecDeque::from(turns)));
        let respond = Arc::new(tokio::sync::Notify::new());
        let agent_respond = Arc::clone(&respond);
        let steer_outcomes = Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let agent_steer_outcomes = Arc::clone(&steer_outcomes);
        let (unprompted, mut unprompted_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<GrokFrame>>();
        let (client_end, agent_end) = agent_client_protocol::Channel::duplex();

        let agent = tokio::spawn(async move {
            let _ = Agent
                .builder()
                .on_receive_request(
                    async |_req: NewSessionRequest,
                           responder: Responder<NewSessionResponse>,
                           _cx: ConnectionTo<Client>| {
                        responder.respond(NewSessionResponse::new(SessionId::new("s1")))
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: PromptRequest,
                                responder: Responder<PromptResponse>,
                                cx: ConnectionTo<Client>| {
                        let frames = turns.lock().unwrap().pop_front().unwrap_or_default();
                        send_grok_frames(&cx, &frames)?;
                        let respond = Arc::clone(&agent_respond);
                        // Off the handler, so a held response doesn't hold
                        // up the agent's other incoming messages.
                        tokio::spawn(async move {
                            respond.notified().await;
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        });
                        Ok(())
                    },
                    on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: TestSteeringRequest,
                                responder: Responder<serde_json::Value>,
                                _cx: ConnectionTo<Client>| {
                        let outcome = agent_steer_outcomes
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or_else(|| serde_json::json!({"outcome": "injected"}));
                        responder.respond(outcome)
                    },
                    on_receive_request!(),
                )
                .connect_with(agent_end, async move |cx: ConnectionTo<Client>| {
                    while let Some(frames) = unprompted_rx.recv().await {
                        send_grok_frames(&cx, &frames)?;
                    }
                    std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                })
                .await;
        });

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-custom".to_string(),
            agent_type,
            None,
            "win".to_string(),
            None,
        )));
        let events = state.read().await.event_stream().subscribe();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let loop_state = Arc::clone(&state);
        let client = tokio::spawn(async move {
            let _ = Client
                .builder()
                .connect_with(client_end, async move |cx: ConnectionTo<Agent>| {
                    let raw = cx
                        .send_request_to(
                            Agent,
                            UntypedMessage::new("session/new", NewSessionRequest::new("/tmp"))?,
                        )
                        .block_task()
                        .await?;
                    let response: NewSessionResponse = serde_json::from_value(raw)
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    let mut session = AgentSession::attach(&cx, response)?;
                    let perms = PendingPermissions::default();
                    let ledger = background_watch::PromptLedger::shared();
                    let stderr_tail = Arc::new(StderrTail::new());
                    run_conversation_loop(
                        &mut session,
                        "conn-custom",
                        &EventEmitter::Noop,
                        &loop_state,
                        agent_type,
                        &perms,
                        &mut cmd_rx,
                        Arc::new(TerminalRuntime::with_base_env(BTreeMap::new())),
                        "/tmp",
                        false,
                        &ledger,
                        None,
                        &stderr_tail,
                    )
                    .await?;
                    Ok(())
                })
                .await;
        });

        Self {
            state,
            cmd_tx,
            events,
            seen: Vec::new(),
            unprompted,
            respond,
            steer_outcomes,
            client,
            agent,
        }
    }

    /// Have the agent send `frames` now, outside any prompt.
    fn agent_sends(&self, frames: Vec<GrokFrame>) {
        self.unprompted.send(frames).expect("the agent is running");
    }

    /// Run one prompted turn to its end: admit the prompt the way
    /// `send_prompt_inner` does, wait until `last_text` has been read
    /// inside the turn, then release the response and wait for the end.
    async fn run_turn(&mut self, prompt: &str, last_text: &str) {
        self.open_turn(prompt, last_text).await;
        self.close_turn().await;
    }

    /// The first half of [`Self::run_turn`]: the turn stays open.
    async fn open_turn(&mut self, prompt: &str, last_text: &str) {
        self.state.write().await.turn_in_flight = true;
        self.cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: prompt.into(),
                }],
                user_message: None,
            })
            .await
            .expect("the loop is running");
        self.until_text(last_text).await;
    }

    /// The second half of [`Self::run_turn`].
    async fn close_turn(&mut self) {
        self.respond.notify_one();
        self.until("the turn end", |e| {
            matches!(e, AcpEvent::TurnComplete { .. })
        })
        .await;
    }

    async fn until_text(&mut self, text: &str) -> usize {
        self.until(
            "the reply",
            |e| matches!(e, AcpEvent::ContentDelta { text: delta, .. } if delta == text),
        )
        .await
    }

    /// Steer the running turn the way `submit_feedback_native` does.
    async fn steer(&self, text: &str) -> Result<SteerOutcome, AcpError> {
        let (reply, outcome) = oneshot::channel();
        self.cmd_tx
            .send(ConnectionCommand::Steer {
                blocks: vec![PromptInputBlock::Text { text: text.into() }],
                reply,
            })
            .await
            .expect("the loop is running");
        outcome.await.expect("the loop answers")
    }

    /// Record events until one matches, and return its index in `seen`.
    async fn until(&mut self, what: &str, matches: impl Fn(&AcpEvent) -> bool) -> usize {
        let deadline = std::time::Duration::from_secs(10);
        loop {
            let envelope = tokio::time::timeout(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}; saw {:#?}", self.seen))
                .expect("event stream");
            self.seen.push(envelope.payload.clone());
            if matches(&envelope.payload) {
                return self.seen.len() - 1;
            }
        }
    }

    /// Wait until a [`grok_barrier`] the agent sent has been through the
    /// loop — and with it every frame sent before it.
    async fn until_barrier(&mut self) -> usize {
        self.until("the agent's barrier", |e| {
            matches!(e, AcpEvent::AvailableCommands { .. })
        })
        .await
    }

    async fn shutdown(self) {
        let _ = self.cmd_tx.send(ConnectionCommand::Disconnect).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.client).await;
        self.agent.abort();
    }
}

/// Issue #903: a turn the agent runs on its own between two prompts — here
/// a background job's follow-up — together with the completion of the call
/// that started the job. Both reach codeg on the idle loop, and both have to
/// be in the history the conversation is rebuilt from once the next turn
/// ends (or the conversation is reopened).
#[tokio::test]
async fn a_turn_a_custom_agent_runs_on_its_own_stays_in_its_history() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_str = home.path().to_string_lossy().into_owned();
    // The recorder resolves its root from the process-global `CODEG_HOME`;
    // temp_env serializes this against every other test that pins it.
    let turns = temp_env::async_with_vars([("CODEG_HOME", Some(home_str.as_str()))], async {
        let mut lp = CustomLoop::start(vec![
            vec![
                custom_update(serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "job-1",
                    "title": "Bash",
                    "kind": "execute",
                    "status": "in_progress",
                    "rawInput": {"command": "sleep 30 && echo done"},
                })),
                custom_text("Started the job."),
            ],
            vec![custom_text("You're welcome.")],
        ])
        .await;

        // Opening a session does not start a conversation: a greeting sent
        // before the first prompt stays out of the history, as it stays
        // out of the live view.
        lp.agent_sends(vec![custom_text("Welcome!"), grok_barrier()]);
        lp.until_barrier().await;

        lp.run_turn("start the job", "Started the job.").await;

        // The job outlives its turn: the agent reports the call done and
        // carries on by itself, with no `session/prompt` around any of it.
        lp.agent_sends(vec![
            custom_update(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "job-1",
                "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": "done"}}],
            })),
            custom_text("The job finished."),
            grok_barrier(),
        ]);
        lp.until_barrier().await;

        // The end of the next turn is when the conversation is rebuilt from
        // history.
        lp.run_turn("thanks", "You're welcome.").await;
        lp.shutdown().await;
        recorded_custom_turns().await
    })
    .await;

    assert_eq!(
        turn_shape(&turns),
        vec![
            r#"User: text "start the job""#,
            r#"Assistant: call job-1 Bash | text "Started the job." | result job-1 "done""#,
            r#"Assistant: text "The job finished.""#,
            r#"User: text "thanks""#,
            r#"Assistant: text "You're welcome.""#,
        ]
    );
}

/// Wait until everything the probe session recorded is on disk, then
/// rebuild its history the way a reopened conversation does.
///
/// Everything goes through the one writer thread, which takes its queue in
/// order and writes a turn end before the next job, so a turn end queued
/// after the session's records lands only once they have. Waiting for that
/// one with room for a stalled CI runner — the loop's own wait on its turn
/// end gives up after 2s — is what lets the read see the whole file.
async fn recorded_custom_turns() -> Vec<crate::models::message::MessageTurn> {
    use crate::parsers::AgentParser as _;

    let root = crate::paths::codeg_acp_transcripts_root();
    let barrier = crate::acp_transcript::record_entry_in(
        &root,
        IDLE_PROBE_AGENT,
        "writer-barrier",
        crate::acp_transcript::EntryKind::TurnEnd,
        serde_json::json!({"stopReason": "end_turn"}),
    );
    tokio::time::timeout(std::time::Duration::from_secs(30), barrier)
        .await
        .expect("the transcript writer kept up")
        .expect("the barrier record landed");

    crate::parsers::acp_native::AcpNativeParser::new_in(
        AgentType::custom(IDLE_PROBE_AGENT).expect("a valid custom id"),
        root,
    )
    .get_conversation("s1")
    .expect("the session has a transcript")
    .turns
}

/// #892: a custom agent running claude-agent-acp steers natively now, and
/// the adapter keeps a steer to itself — no user chunk echoes it. codeg's
/// transcript is the whole of such an agent's history, so the loop records
/// the steer there: once reopened, the conversation shows it as the user
/// turn the live view drew, between the two parts of the reply, and the
/// card the first part opened carries the update that came after the
/// steer. A steer the agent did not take in is not recorded.
#[tokio::test]
async fn a_steer_a_custom_agent_takes_in_stays_in_its_history() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_str = home.path().to_string_lossy().into_owned();
    let (turns, transcript) =
        temp_env::async_with_vars([("CODEG_HOME", Some(home_str.as_str()))], async {
            let mut lp = CustomLoop::start(vec![vec![
                custom_update(serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "job-1",
                    "title": "Bash",
                    "kind": "execute",
                    "status": "in_progress",
                    "rawInput": {"command": "pnpm build"},
                })),
                custom_text("Building the page."),
            ]])
            .await;
            lp.steer_outcomes.lock().unwrap().extend([
                serde_json::json!({"outcome": "promptRequired"}),
                serde_json::json!({"outcome": "startedNewTurn"}),
                serde_json::json!({"outcome": "bogus"}),
            ]);

            lp.open_turn("build the page", "Building the page.").await;
            assert!(matches!(
                lp.steer("not taken in").await,
                Ok(SteerOutcome::PromptRequired)
            ));
            assert!(matches!(
                lp.steer("detached").await,
                Ok(SteerOutcome::StartedNewTurn)
            ));
            assert!(lp.steer("garbled").await.is_err());
            assert!(matches!(
                lp.steer("make it blue").await,
                Ok(SteerOutcome::Injected)
            ));
            lp.agent_sends(vec![
                custom_update(serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "job-1",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "built"}}],
                })),
                custom_text("Blue it is."),
            ]);
            lp.until_text("Blue it is.").await;
            lp.close_turn().await;
            lp.shutdown().await;

            let turns = recorded_custom_turns().await;
            let transcript = std::fs::read_to_string(
                crate::paths::codeg_acp_transcripts_root()
                    .join(IDLE_PROBE_AGENT)
                    .join("s1.jsonl"),
            )
            .expect("the transcript file");
            (turns, transcript)
        })
        .await;

    assert_eq!(
        turn_shape(&turns),
        vec![
            r#"User: text "build the page""#,
            r#"Assistant: call job-1 Bash | text "Building the page." | result job-1 "built""#,
            r#"User: text "make it blue""#,
            r#"Assistant: text "Blue it is.""#,
        ]
    );
    let steers: Vec<serde_json::Value> = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry.get("s") == Some(&serde_json::Value::Bool(true)))
        .collect();
    assert_eq!(steers.len(), 1, "{transcript}");
    assert_eq!(steers[0]["k"], "prompt");
    assert_eq!(
        steers[0]["p"],
        serde_json::json!([{"type": "text", "text": "make it blue"}])
    );
}
