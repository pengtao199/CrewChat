    use super::*;
    use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationConfig};
    use crate::acp::delegation::spawner::{
        mock::MockSpawner, ConnectionSpawner, ResumedSpawn, SpawnerError,
    };
    use crate::acp::browser_tools::{NoBrowserTabs, ERROR_GRANT_REQUIRED};
    use crate::acp::computer_tools::NoComputerDesktop;
    use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::duplex;

    struct AlwaysRootLookup;
    #[async_trait]
    impl ConversationDepthLookup for AlwaysRootLookup {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    struct StaticParentLookup(Option<i32>);
    #[async_trait]
    impl ParentSessionLookup for StaticParentLookup {
        async fn current_conversation_id(&self, _parent_connection_id: &str) -> Option<i32> {
            self.0
        }
    }

    /// In-memory feedback stub. `read_pending_feedback` returns the seeded notes
    /// WITHOUT draining (read-only, matching production), recording the conn id;
    /// `commit_feedback_delivered` records the (conn_id, ids) it was committed
    /// with so tests can assert delivery happens only after a successful write.
    /// Default is empty (the delegation tests don't exercise feedback).
    #[derive(Default)]
    struct StubFeedback {
        items: tokio::sync::Mutex<Vec<PendingFeedback>>,
        read_conn: tokio::sync::Mutex<Option<String>>,
        committed: tokio::sync::Mutex<Vec<(String, Vec<String>)>>,
    }
    #[async_trait]
    impl SessionFeedbackAccess for StubFeedback {
        async fn read_pending_feedback(
            &self,
            parent_connection_id: &str,
        ) -> Vec<PendingFeedback> {
            *self.read_conn.lock().await = Some(parent_connection_id.to_string());
            self.items.lock().await.clone()
        }
        async fn commit_feedback_delivered(&self, parent_connection_id: &str, ids: Vec<String>) {
            self.committed
                .lock()
                .await
                .push((parent_connection_id.to_string(), ids));
        }
    }

    /// In-memory question stub. `register_question` mints a sequential id,
    /// stashes the answer sender (so a test can resolve it via `answer`), and
    /// records the (parent_conn, questions); `cancel_question` removes the
    /// sender and records the canceled id. Lets the listener's `Ask` arm be
    /// driven without a real `ConnectionManager`.
    #[derive(Default)]
    struct StubQuestion {
        pending: tokio::sync::Mutex<HashMap<String, oneshot::Sender<QuestionOutcome>>>,
        registered: tokio::sync::Mutex<
            Vec<(String, Vec<crate::acp::question::QuestionSpec>)>,
        >,
        canceled: tokio::sync::Mutex<Vec<String>>,
    }
    #[async_trait]
    impl SessionQuestionAccess for StubQuestion {
        async fn register_question(
            &self,
            parent_connection_id: &str,
            questions: Vec<crate::acp::question::QuestionSpec>,
        ) -> Option<crate::acp::question::RegisteredQuestion> {
            let question_id = format!("q-{}", self.registered.lock().await.len() + 1);
            let (tx, rx) = oneshot::channel();
            self.pending.lock().await.insert(question_id.clone(), tx);
            self.registered
                .lock()
                .await
                .push((parent_connection_id.to_string(), questions));
            Some(crate::acp::question::RegisteredQuestion {
                question_id,
                answer_rx: rx,
            })
        }
        async fn cancel_question(&self, _parent_connection_id: &str, question_id: &str) {
            self.pending.lock().await.remove(question_id);
            self.canceled.lock().await.push(question_id.to_string());
        }
        async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {
            // Not exercised by the listener unit tests (the teardown sweep lives
            // in connection.rs); drop all parked senders to satisfy the trait.
            self.pending.lock().await.clear();
        }
    }
    impl StubQuestion {
        async fn answer(&self, question_id: &str, outcome: QuestionOutcome) {
            if let Some(tx) = self.pending.lock().await.remove(question_id) {
                let _ = tx.send(outcome);
            }
        }
    }

    /// In-memory session-info stub. Records every `(session_id, max_messages)` it
    /// was asked to resolve and returns a seeded outcome — `found` sessions echo
    /// their id, unknown ids return `not_found`. Default knows about no sessions.
    #[derive(Default)]
    struct StubSessionInfo {
        known: std::collections::HashSet<i32>,
        calls: tokio::sync::Mutex<Vec<(i32, u32)>>,
    }
    #[async_trait]
    impl SessionInfoAccess for StubSessionInfo {
        async fn resolve(&self, session_id: i32, max_messages: u32) -> SessionInfo {
            self.calls.lock().await.push((session_id, max_messages));
            if self.known.contains(&session_id) {
                SessionInfo {
                    found: true,
                    session_id,
                    title: Some(format!("session {session_id}")),
                    ..Default::default()
                }
            } else {
                SessionInfo::not_found(session_id)
            }
        }
    }

    /// A browser with one shared tab and one that nobody shared, recording
    /// every call so a test can prove the token gate never reached it.
    #[derive(Default)]
    struct StubBrowser {
        calls: tokio::sync::Mutex<Vec<String>>,
    }
    #[async_trait]
    impl BrowserToolAccess for StubBrowser {
        async fn list_tabs(&self) -> BrowserTabsOutcome {
            self.calls.lock().await.push("list".into());
            BrowserTabsOutcome {
                tabs: vec![crate::browser::agent::AgentTabSummary {
                    tab_id: "t1".into(),
                    origin: Some("https://example.com".into()),
                    level: crate::browser::agent::GrantLevel::Read,
                    title: Some("Example".into()),
                }],
                note: None,
            }
        }
        async fn snapshot(
            &self,
            tab_id: &str,
            max_chars: Option<usize>,
        ) -> BrowserSnapshotOutcome {
            self.calls
                .lock()
                .await
                .push(format!("snapshot {tab_id} {max_chars:?}"));
            BrowserSnapshotOutcome::grant_required(tab_id)
        }
        async fn eval(
            &self,
            tab_id: &str,
            request: crate::browser::eval::EvalRequest,
        ) -> BrowserEvalOutcome {
            self.calls
                .lock()
                .await
                .push(format!("eval {tab_id} {}", request.code));
            BrowserEvalOutcome::grant_required(tab_id)
        }
        async fn console(
            &self,
            tab_id: &str,
            query: crate::browser::console::ConsoleQuery,
        ) -> BrowserConsoleOutcome {
            self.calls.lock().await.push(format!(
                "console {tab_id} since={} min={:?} limit={:?}",
                query.since, query.min_level, query.limit
            ));
            BrowserConsoleOutcome::grant_required(tab_id)
        }
        async fn capture(
            &self,
            tab_id: &str,
            request: crate::browser::capture::CaptureRequest,
        ) -> BrowserCaptureOutcome {
            self.calls.lock().await.push(format!(
                "capture {tab_id} {:?} {:?} {:?}",
                request.clip_target(),
                request.max_width,
                request.format
            ));
            BrowserCaptureOutcome::grant_required(tab_id)
        }
        async fn act(
            &self,
            tab_id: &str,
            request: crate::browser::agent::ActionRequest,
        ) -> BrowserActOutcome {
            self.calls.lock().await.push(format!(
                "act {tab_id} {} {:?}",
                request.generation,
                serde_json::to_value(&request.action).unwrap()
            ));
            BrowserActOutcome::control_required(tab_id)
        }
        async fn tab_op(
            &self,
            op: crate::acp::browser_tools::BrowserTabOp,
        ) -> BrowserTabOutcome {
            self.calls
                .lock()
                .await
                .push(format!("tab_op {}", serde_json::to_value(&op).unwrap()));
            match op.tab_id() {
                Some(tab_id) => BrowserTabOutcome::control_required(tab_id),
                None => BrowserTabOutcome::tab(
                    crate::browser::agent::AgentTabSummary {
                        tab_id: "t9".into(),
                        origin: Some("http://localhost:3000".into()),
                        level: crate::browser::agent::GrantLevel::Control,
                        title: Some("dev".into()),
                    },
                    None,
                ),
            }
        }
    }

    /// No-engine stub: every report is rejected, mirroring a process without a
    /// running task engine.
    struct StubTaskTools;
    #[async_trait]
    impl WorkTaskToolAccess for StubTaskTools {
        async fn report_progress(&self, _parent: &str, _message: &str) -> TaskReportAck {
            TaskReportAck::rejected("no task engine in this process")
        }
        async fn complete(
            &self,
            _parent: &str,
            _verdict: &str,
            _summary: Option<&str>,
        ) -> TaskReportAck {
            TaskReportAck::rejected("no task engine in this process")
        }
    }

    use crate::acp::chat_authoring::{NewAutomationSpec, NewWorkTaskSpec};

    /// Records what the listener handed down and returns a canned outcome, so
    /// authoring tests can assert the token → context resolution without a DB.
    #[derive(Default)]
    struct StubAuthoring {
        automations: tokio::sync::Mutex<Vec<(AuthoringContext, NewAutomationSpec)>>,
        work_tasks: tokio::sync::Mutex<Vec<(AuthoringContext, NewWorkTaskSpec)>>,
    }
    #[async_trait]
    impl ChatAuthoringAccess for StubAuthoring {
        async fn create_automation(
            &self,
            ctx: AuthoringContext,
            spec: NewAutomationSpec,
        ) -> AuthoringOutcome {
            self.automations.lock().await.push((ctx, spec));
            AuthoringOutcome {
                created: true,
                kind: "automation".into(),
                id: Some(7),
                ..Default::default()
            }
        }
        async fn create_work_task(
            &self,
            ctx: AuthoringContext,
            spec: NewWorkTaskSpec,
        ) -> AuthoringOutcome {
            self.work_tasks.lock().await.push((ctx, spec));
            AuthoringOutcome {
                created: true,
                kind: "work_task".into(),
                id: Some(9),
                ..Default::default()
            }
        }
    }

    use tokio::sync::oneshot;

    async fn make_broker(mock: Arc<MockSpawner>) -> Arc<DelegationBroker> {
        let broker = Arc::new(DelegationBroker::new(
            mock as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        // Production default is `enabled: false`; listener tests that don't
        // explicitly set their own config need the switch flipped on so
        // `handle_request` parks pending entries instead of returning
        // `Canceled { reason: "delegation disabled" }` straight away.
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        broker
    }

    fn make_listener(
        broker: Arc<DelegationBroker>,
        tokens: Arc<TokenRegistry>,
        parent_conversation: Option<i32>,
    ) -> Arc<DelegationListener> {
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(parent_conversation)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            Arc::new(NoBrowserTabs),
            Arc::new(NoComputerDesktop),
        )
    }

    /// Build a listener whose feedback access is the given stub, so feedback
    /// tests can seed notes and assert the drain. Delegation pieces are minimal.
    fn make_feedback_listener(
        tokens: Arc<TokenRegistry>,
        feedback: Arc<StubFeedback>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(Some(1))),
            feedback,
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            Arc::new(NoBrowserTabs),
            Arc::new(NoComputerDesktop),
        )
    }

    /// Build a listener whose question access is the given stub, so ask tests
    /// can register/answer questions and assert the round-trip. Delegation and
    /// feedback pieces are minimal.
    fn make_question_listener(
        tokens: Arc<TokenRegistry>,
        questions: Arc<StubQuestion>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            questions,
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            Arc::new(NoBrowserTabs),
            Arc::new(NoComputerDesktop),
        )
    }

    /// Build a listener whose session-info access is the given stub, so
    /// `get_session_info` tests can seed known sessions and assert the round-trip.
    fn make_session_listener(
        tokens: Arc<TokenRegistry>,
        session_info: Arc<StubSessionInfo>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            session_info,
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            Arc::new(NoBrowserTabs),
            Arc::new(NoComputerDesktop),
        )
    }

    /// Build a listener whose authoring access is the given stub, so
    /// `create_automation` / `create_work_task` tests can assert what the
    /// listener resolved and passed down.
    fn make_authoring_listener(
        tokens: Arc<TokenRegistry>,
        authoring: Arc<StubAuthoring>,
        parent_conversation: Option<i32>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(parent_conversation)),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            authoring,
            Arc::new(NoBrowserTabs),
            Arc::new(NoComputerDesktop),
        )
    }

    /// Build a listener whose browser access is the given stub, so
    /// `browser_list_tabs` / `browser_snapshot` tests can assert what the token
    /// gate let through.
    fn make_browser_listener(
        tokens: Arc<TokenRegistry>,
        browser: Arc<dyn BrowserToolAccess>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            browser,
            Arc::new(NoComputerDesktop),
        )
    }

    async fn make_request(input: serde_json::Value) -> BrokerRequest {
        BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "parent-conn".into(),
            parent_tool_use_id: "pt-1".into(),
            external_handle: None,
            input,
        }
    }

    #[tokio::test]
    async fn invalid_token_rejected() {
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            Arc::new(TokenRegistry::default()),
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert_eq!(report.error_code.as_deref(), Some("canceled"));
        assert!(report.message.unwrap().contains("invalid token"));
    }

    #[tokio::test]
    async fn token_parent_mismatch_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "other-parent".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(report.message.unwrap().contains("does not match"));
    }

    #[tokio::test]
    async fn missing_parent_conversation_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        // parent_conversation = None: parent has no live conversation.
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            None,
        );
        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Canceled);
        assert!(report.message.unwrap().contains("no active conversation"));
    }

    #[tokio::test]
    async fn invalid_agent_type_rejected() {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            tokens,
            Some(1),
        );
        let report = listener
            .process(make_request(json!({"agent_type": "garbage", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("invalid_agent_type"));
    }

    /// Full async round-trip through the listener: `delegate_to_agent` returns a
    /// Running ack, the lifecycle resolves the child via `complete_call`, and a
    /// follow-up `get_delegation_status` collects the Completed result.
    #[tokio::test]
    async fn happy_path_ack_then_status_collects_result() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;

        // 1. delegate_to_agent → Running ack carrying the child conversation id.
        let listener = make_listener(broker.clone(), tokens.clone(), Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Call(BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "parent-conn".into(),
            parent_tool_use_id: "pt-1".into(),
            external_handle: None,
            input: json!({"agent_type": "codex", "task": "do x"}),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let ack: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(ack.outcome["status"], "running");
        assert_eq!(ack.outcome["child_conversation_id"], 42);
        let task_id = ack.outcome["task_id"].as_str().unwrap().to_string();

        // 2. The lifecycle resolves the child on TurnComplete.
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "result-text".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;

        // 3. get_delegation_status → Completed with the result text.
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id.clone()],
            wait_ms: Some(1_000),
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        // The Status arm returns a `{ tasks: [..] }` envelope; a single id is
        // the first (only) entry.
        assert_eq!(resp.outcome["tasks"][0]["status"], "completed");
        assert_eq!(resp.outcome["tasks"][0]["text"], "result-text");
        assert_eq!(resp.outcome["tasks"][0]["child_conversation_id"], 42);
    }

    /// Start a running task directly and return `(broker, tokens, task_id)`.
    /// Shared setup for the `wait_ms` mapping tests below.
    async fn running_task_fixture() -> (Arc<DelegationBroker>, Arc<TokenRegistry>, String) {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker = make_broker(mock).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let ack = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-1".into(),
                agent_type: AgentType::Codex,
                task: "do x".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
            })
            .await;
        let task_id = ack.task_id.clone().expect("running task carries an id");
        (broker, tokens, task_id)
    }

    /// Omitted `wait_ms` (the safe default) maps to an immediate snapshot: the
    /// status of a still-running task returns `running` right away rather than
    /// blocking.
    #[tokio::test]
    async fn status_omitted_wait_returns_immediately() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker, tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id],
            wait_ms: None,
        });
        write_frame(&mut client, &status).await.unwrap();
        // No completion ever happens — an immediate poll must still return.
        let resp: BrokerResponse = tokio::time::timeout(Duration::from_secs(2), async {
            read_frame::<_, BrokerResponse>(&mut client).await.unwrap()
        })
        .await
        .expect("omitted wait_ms must return immediately");
        server_task.await.unwrap().unwrap();
        assert_eq!(resp.outcome["tasks"][0]["status"], "running");
    }

    /// An explicit `wait_ms = 0` maps to an unbounded wait: the call blocks
    /// while the task is running and only resolves once it reaches a terminal
    /// state, returning the completed report through the wire.
    #[tokio::test]
    async fn status_explicit_zero_blocks_until_terminal() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id.clone()],
            wait_ms: Some(0),
        });
        write_frame(&mut client, &status).await.unwrap();

        // While the task runs, the wait must NOT resolve.
        let early = tokio::time::timeout(Duration::from_millis(50), async {
            read_frame::<_, BrokerResponse>(&mut client).await
        })
        .await;
        assert!(
            early.is_err(),
            "wait_ms=0 must block while the task is still running"
        );

        // Resolving the task wakes the parked wait, which returns completed.
        broker
            .complete_call(
                &task_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap().unwrap();
        assert_eq!(resp.outcome["tasks"][0]["status"], "completed");
        assert_eq!(resp.outcome["tasks"][0]["text"], "done");
    }

    /// A `wait_ms = 0` status call that the companion cancels (dropping the
    /// request socket) must not leave `serve_one` parked until the task is
    /// terminal. The peer-close race abandons the wait while leaving the task
    /// itself untouched — there's no broker-side side effect from a status
    /// query.
    #[tokio::test]
    async fn infinite_status_wait_abandoned_when_peer_closes() {
        let (broker, tokens, task_id) = running_task_fixture().await;
        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });

        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![task_id],
            wait_ms: Some(0),
        });
        write_frame(&mut client, &status).await.unwrap();

        // Let the server park inside the unbounded wait.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !server_task.is_finished(),
            "server must be parked on the unbounded wait"
        );

        // Companion cancels: drop the request socket without completing the task.
        drop(client);

        // serve_one must observe the peer-close and return promptly instead of
        // hanging until the (never-completing) task is terminal.
        let result = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve_one must return after the peer closes");
        result.unwrap().unwrap();

        // The task itself was not touched by the abandoned status query.
        assert_eq!(broker.pending_count().await, 1);
    }

    /// Batch status over the listener: two tasks, one completed and one still
    /// running, return as a `{ tasks: [..] }` envelope with both reports in
    /// request order.
    #[tokio::test]
    async fn batch_status_over_listener_multi_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-1".into())).await;
        mock.queue_send(Ok(1)).await;
        mock.queue_spawn(Ok("child-2".into())).await;
        mock.queue_send(Ok(2)).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let start = |tool_use: &'static str| {
            let broker = broker.clone();
            async move {
                broker
                    .start_delegation(DelegationRequest {
                        parent_connection_id: "parent-conn".into(),
                        parent_conversation_id: 1,
                        parent_tool_use_id: tool_use.into(),
                        agent_type: AgentType::Codex,
                        task: "do x".into(),
                        working_dir: None,
                        requested_working_dir: None,
                        external_handle: None,
                    })
                    .await
                    .task_id
                    .unwrap()
            }
        };
        let t1 = start("pt-1").await;
        let t2 = start("pt-2").await;
        broker
            .complete_call(
                &t1,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "first".into(),
                    child_conversation_id: 1,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 3,
                    token_usage: None,
                }),
            )
            .await;

        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "tok".into(),
            task_ids: vec![t1.clone(), t2.clone()],
            wait_ms: None,
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        let tasks = resp.outcome["tasks"].as_array().expect("tasks array");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["task_id"], t1.as_str());
        assert_eq!(tasks[1]["status"], "running");
        assert_eq!(tasks[1]["task_id"], t2.as_str());
    }

    /// An invalid token over a batch status reports `Unknown` for EACH requested
    /// id (preserving order) rather than collapsing to a single report — so the
    /// companion can still render one row per task.
    #[tokio::test]
    async fn batch_status_invalid_token_returns_unknown_per_id() {
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            Arc::new(TokenRegistry::default()),
            Some(1),
        );
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let status = BrokerMessage::Status(BrokerStatusRequest {
            token: "bad-token".into(),
            task_ids: vec!["a".into(), "b".into()],
            wait_ms: None,
        });
        write_frame(&mut client, &status).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        let tasks = resp.outcome["tasks"].as_array().expect("tasks array");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["status"], "unknown");
        assert_eq!(tasks[0]["task_id"], "a");
        assert_eq!(tasks[1]["status"], "unknown");
        assert_eq!(tasks[1]["task_id"], "b");
    }

    /// `cancel_delegation` over the listener: a running task is canceled by id
    /// and reports `canceled`.
    #[tokio::test]
    async fn cancel_task_by_id_over_listener() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        // Start a task directly so we hold its id.
        let ack = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "pt-1".into(),
                agent_type: AgentType::Codex,
                task: "do x".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
            })
            .await;
        let task_id = ack.task_id.clone().unwrap();

        let listener = make_listener(broker.clone(), tokens, Some(1));
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let cancel = BrokerMessage::CancelTask(BrokerCancelTaskRequest {
            token: "tok".into(),
            task_id: task_id.clone(),
        });
        write_frame(&mut client, &cancel).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["status"], "canceled");
        assert_eq!(broker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn cancel_message_routed_to_broker() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-cancel".into())).await;
        mock.queue_send(Ok(99)).await;
        let broker = make_broker(mock.clone()).await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));

        // Park a delegation call with a known external_handle.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move {
                let req = DelegationRequest {
                    parent_connection_id: "parent-conn".into(),
                    parent_conversation_id: 1,
                    parent_tool_use_id: "pt-cancel".into(),
                    agent_type: AgentType::Codex,
                    task: "do x".into(),
                    working_dir: None,
                    requested_working_dir: None,
                    external_handle: Some("h-1".into()),
                };
                broker.handle_request(req).await
            })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Drive a cancel through the listener — listener should ack with
        // an empty BrokerResponse and the broker should drain the pending.
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });

        let cancel_msg = BrokerMessage::Cancel(BrokerCancelRequest {
            token: "tok".into(),
            external_handle: "h-1".into(),
            reason: Some("from test".into()),
        });
        write_frame(&mut client, &cancel_msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        assert!(resp.outcome.is_null(), "cancel ack must be null");
        server_task.await.unwrap();

        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            other => panic!("expected canceled, got {other:?}"),
        }
    }

    /// Full `resume_delegation` round-trip over the wire: the listener
    /// validates the token, resolves the caller's conversation, and hands the
    /// broker a `ResumeDelegationRequest` — which resumes the interrupted child
    /// and acks Running under the unchanged task id.
    #[tokio::test]
    async fn resume_task_round_trip_resumes_interrupted_child() {
        use crate::acp::delegation::broker::{ChildResumeContext, ChildStatusLookup};
        use crate::acp::delegation::types::TaskStatus as Ts;

        /// Minimal resume-context stub: every call_id resolves to one canceled
        /// child owned by conversation 1.
        struct StubResumeLookup;
        #[async_trait]
        impl ChildStatusLookup for StubResumeLookup {
            async fn find_by_call_id(
                &self,
                _call_id: &str,
            ) -> Option<crate::acp::delegation::broker::ChildStatusRecord> {
                None
            }
            async fn find_resume_context_by_call_id(
                &self,
                _call_id: &str,
            ) -> Option<ChildResumeContext> {
                Some(ChildResumeContext {
                    child_conversation_id: 42,
                    status: Ts::Canceled,
                    agent_type: AgentType::Codex,
                    parent_id: Some(1),
                    parent_tool_use_id: Some("pt-orig".into()),
                    external_id: Some("ext-1".into()),
                    folder_id: 7,
                    working_dir: Some("/work".into()),
                    title: Some("do x".into()),
                    member_role_id: None,
                })
            }
        }

        let mock = Arc::new(MockSpawner::new());
        mock.queue_resume_spawn(Ok(ResumedSpawn::fresh("child-conn-2"))).await;
        mock.queue_resume_send(Ok(())).await;
        let broker = Arc::new(
            DelegationBroker::new(
                mock.clone() as Arc<dyn ConnectionSpawner>,
                Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_status_lookup(Arc::new(StubResumeLookup)),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_listener(broker.clone(), tokens, Some(1));

        let (mut client, mut server) = duplex(16 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::ResumeTask(BrokerResumeTaskRequest {
            token: "tok".into(),
            task_id: "task-1".into(),
            reason: Some("session crashed".into()),
            external_handle: Some("h-resume".into()),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let ack: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(ack.outcome["status"], "running");
        assert_eq!(ack.outcome["task_id"], "task-1");
        assert_eq!(ack.outcome["child_conversation_id"], 42);
        // The child was re-spawned by loading its recorded session, and the
        // caller's reason reached the continuation prompt.
        let spawns = mock.resume_spawn_args.lock().await;
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].external_session_id, "ext-1");
        drop(spawns);
        let sends = mock.resume_send_args.lock().await;
        assert_eq!(sends[0].child_conversation_id, 42);
        assert!(sends[0].prompt.contains("session crashed"));
    }

    /// An invalid token on the resume arm stays opaque — `unknown`, exactly
    /// like the status/cancel arms (no leak of whether the task exists).
    #[tokio::test]
    async fn resume_task_invalid_token_reports_unknown() {
        let listener = make_listener(
            make_broker(Arc::new(MockSpawner::new())).await,
            Arc::new(TokenRegistry::default()),
            Some(1),
        );
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::ResumeTask(BrokerResumeTaskRequest {
            token: "bad".into(),
            task_id: "task-1".into(),
            reason: None,
            external_handle: None,
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["status"], "unknown");
    }

    #[tokio::test]
    async fn token_registry_revoke_and_revoke_by_parent() {
        let registry = TokenRegistry::default();
        registry
            .register(
                "t1".into(),
                TokenEntry {
                    parent_connection_id: "p1".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        registry
            .register(
                "t2".into(),
                TokenEntry {
                    parent_connection_id: "p1".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        registry
            .register(
                "t3".into(),
                TokenEntry {
                    parent_connection_id: "p2".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;

        registry.revoke("t1").await;
        assert!(registry.lookup("t1").await.is_none());
        assert!(registry.lookup("t2").await.is_some());

        registry.revoke_by_parent("p1").await;
        assert!(registry.lookup("t2").await.is_none());
        assert!(registry.lookup("t3").await.is_some());
    }

    // Sanity: spawn failure surfaces as spawn_failed when the listener path
    // is exercised. Exercises the full process() → broker.handle_request chain.
    #[tokio::test]
    async fn spawn_failure_surfaces_through_listener() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Err(SpawnerError::Spawn("agent missing".into())))
            .await;
        // `make_broker` already enables delegation; this call narrows the
        // depth limit (8 instead of the helper's default) without changing
        // the enable bit.
        let broker = make_broker(mock).await;
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 8,
                ..DelegationConfig::default()
            })
            .await;
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_listener(broker, tokens, Some(1));

        let report = listener
            .process(make_request(json!({"agent_type": "codex", "task": "x"})).await)
            .await;
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
    }

    // --- check_user_feedback over the listener -----------------------------

    use crate::acp::feedback::PendingFeedback;

    fn pending(id: &str, text: &str) -> PendingFeedback {
        PendingFeedback {
            id: id.into(),
            text: text.into(),
            created_at: chrono::Utc::now(),
        }
    }

    /// The manager chunks each response via `bounded_feedback_batch`. The
    /// serialized `feedback_response` of any such chunk must stay under the
    /// transport cap (`MAX_FRAME_BYTES` = 16 MiB) so the companion's `read_frame`
    /// never rejects it after the listener committed delivery — for BOTH
    /// worst-case-escaping notes AND a flood of tiny notes (whose per-note JSON
    /// overhead, not text length, is what a naive text-only bound would miss).
    #[test]
    fn bounded_feedback_response_always_fits_a_transport_frame() {
        use crate::acp::delegation::transport::MAX_FRAME_BYTES;
        use crate::acp::feedback::{bounded_feedback_batch, MAX_FEEDBACK_RESPONSE_BYTES};

        // Worst-case escaping: many MAX_FEEDBACK_CHARS-sized control-char notes.
        let worst = "\u{0001}".repeat(4096);
        let big: Vec<PendingFeedback> = (0..5_000)
            .map(|i| pending(&format!("b{i}"), &worst))
            .collect();
        // A flood of tiny notes: little text, lots of per-note JSON overhead.
        let tiny: Vec<PendingFeedback> = (0..200_000)
            .map(|i| pending(&format!("t{i}"), "x"))
            .collect();

        for (label, set) in [("worst-case", big), ("tiny-flood", tiny)] {
            let total = set.len();
            let batch = bounded_feedback_batch(set, MAX_FEEDBACK_RESPONSE_BYTES);
            assert!(batch.len() < total, "{label}: batch must be chunked");
            let encoded = serde_json::to_vec(&feedback_response(&batch).unwrap()).unwrap();
            assert!(
                encoded.len() < MAX_FRAME_BYTES,
                "{label}: bounded response must fit a transport frame: {} >= {}",
                encoded.len(),
                MAX_FRAME_BYTES
            );
        }
    }

    /// A valid `check_user_feedback` returns the parent's notes in a
    /// `{ count, feedback: [..] }` envelope (lean text, no ids) scoped to the
    /// token's parent connection, and — crucially — commits them delivered ONLY
    /// after the response is written, with the exact note ids.
    #[tokio::test]
    async fn feedback_returns_notes_then_commits_after_write() {
        let feedback = Arc::new(StubFeedback::default());
        *feedback.items.lock().await = vec![
            pending("f1", "use the existing UserService"),
            pending("f2", "skip the migration"),
        ];
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Feedback(BrokerFeedbackRequest {
            token: "tok".into(),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["count"], 2);
        let notes = resp.outcome["feedback"].as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["text"], "use the existing UserService");
        // The lean note shape carries no internal id...
        assert!(notes[0].get("id").is_none());
        // ...but the envelope carries `_commit_ids` for the companion to echo
        // back in a CommitFeedback after it delivers the result.
        let commit_ids = resp.outcome["_commit_ids"].as_array().unwrap();
        assert_eq!(commit_ids, &vec!["f1", "f2"]);
        // Read was scoped to the token's parent connection id.
        assert_eq!(feedback.read_conn.lock().await.as_deref(), Some("parent-conn"));
        // The Feedback arm is READ-ONLY — it does NOT commit (delivery is
        // committed later, by the companion's CommitFeedback).
        assert!(feedback.committed.lock().await.is_empty());
    }

    /// A valid `get_session_info` resolves the session by id and returns its
    /// metadata; the resolver is called with the requested id + max_messages.
    #[tokio::test]
    async fn session_info_valid_token_resolves_by_id() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([42]),
            ..Default::default()
        });
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_session_listener(tokens, session_info.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
            token: "tok".into(),
            session_id: 42,
            max_messages: Some(15),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["found"], true);
        assert_eq!(resp.outcome["session_id"], 42);
        assert_eq!(resp.outcome["title"], "session 42");
        // The resolver saw the id + the requested message budget.
        assert_eq!(session_info.calls.lock().await.as_slice(), &[(42, 15)]);
    }

    /// Accepted-policy coverage (deliberate single-tenant scope): a single valid
    /// token resolves ANY non-deleted session id — not only ids "referenced" in the
    /// prompt. Three unrelated ids all resolve through one token.
    #[tokio::test]
    async fn session_info_resolves_any_session_id_not_just_referenced() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([7, 42, 1000]),
            ..Default::default()
        });
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_session_listener(tokens, session_info.clone());

        for id in [7, 42, 1000] {
            let (mut client, mut server) = duplex(8 * 1024);
            let l = listener.clone();
            let server_task = tokio::spawn(async move {
                l.serve_one(&mut server).await.unwrap();
            });
            let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
                token: "tok".into(),
                session_id: id,
                max_messages: Some(0),
            });
            write_frame(&mut client, &msg).await.unwrap();
            let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
            server_task.await.unwrap();
            assert_eq!(resp.outcome["found"], true, "id {id} should resolve");
            assert_eq!(resp.outcome["session_id"], id);
        }
    }

    /// An invalid token yields a `found:false` outcome WITHOUT touching the
    /// resolver (no leak of whether the session exists).
    #[tokio::test]
    async fn session_info_invalid_token_is_not_found_without_resolving() {
        let session_info = Arc::new(StubSessionInfo {
            known: std::collections::HashSet::from([42]),
            ..Default::default()
        });
        // No token registered.
        let tokens = Arc::new(TokenRegistry::default());
        let listener = make_session_listener(tokens, session_info.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::SessionInfo(BrokerSessionRequest {
            token: "bogus".into(),
            session_id: 42,
            max_messages: None,
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["found"], false);
        assert_eq!(resp.outcome["session_id"], 42);
        // The resolver was never consulted for an unauthenticated caller.
        assert!(session_info.calls.lock().await.is_empty());
    }

    /// A valid token resolves the caller's conversation + working dir and hands
    /// both down as the [`AuthoringContext`], so the impl can default the target
    /// project to the project this chat is in.
    #[tokio::test]
    async fn create_automation_resolves_caller_context() {
        let authoring = Arc::new(StubAuthoring::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/repo/app"),
                },
            )
            .await;
        let listener = make_authoring_listener(tokens, authoring.clone(), Some(42));

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::CreateAutomation(BrokerCreateAutomationRequest {
            token: "tok".into(),
            spec: NewAutomationSpec {
                name: "Nightly audit".into(),
                prompt: "audit deps".into(),
                cron: Some("0 3 * * *".into()),
                timezone: None,
                action: Default::default(),
                agent_type: None,
                folder_path: None,
                enabled: true,
            },
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["created"], true);
        assert_eq!(resp.outcome["id"], 7);
        let calls = authoring.automations.lock().await;
        let (ctx, spec) = calls.first().expect("impl was called");
        assert_eq!(ctx.conversation_id, Some(42));
        assert_eq!(ctx.working_dir, PathBuf::from("/repo/app"));
        assert_eq!(spec.name, "Nightly audit");
    }

    /// A caller with no conversation yet still reaches the impl — the working
    /// directory alone can resolve a project — so the arm must not gate on it.
    #[tokio::test]
    async fn create_work_task_passes_through_without_a_conversation() {
        let authoring = Arc::new(StubAuthoring::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/repo/app"),
                },
            )
            .await;
        let listener = make_authoring_listener(tokens, authoring.clone(), None);

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::CreateWorkTask(BrokerCreateWorkTaskRequest {
            token: "tok".into(),
            spec: NewWorkTaskSpec {
                title: "Fix the flake".into(),
                prompt: "the retry test is flaky".into(),
                agent_type: None,
                folder_path: None,
            },
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["created"], true);
        assert_eq!(resp.outcome["id"], 9);
        let calls = authoring.work_tasks.lock().await;
        let (ctx, spec) = calls.first().expect("impl was called");
        assert_eq!(ctx.conversation_id, None);
        assert_eq!(spec.title, "Fix the flake");
    }

    /// An invalid token is a soft refusal that NEVER reaches the impl — nothing
    /// gets written on behalf of an unauthenticated caller.
    #[tokio::test]
    async fn create_automation_invalid_token_never_reaches_impl() {
        let authoring = Arc::new(StubAuthoring::default());
        // No token registered.
        let tokens = Arc::new(TokenRegistry::default());
        let listener = make_authoring_listener(tokens, authoring.clone(), Some(1));

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::CreateAutomation(BrokerCreateAutomationRequest {
            token: "bogus".into(),
            spec: NewAutomationSpec {
                name: "n".into(),
                prompt: "p".into(),
                cron: None,
                timezone: None,
                action: Default::default(),
                agent_type: None,
                folder_path: None,
                enabled: true,
            },
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["created"], false);
        assert_eq!(resp.outcome["kind"], "automation");
        assert!(authoring.automations.lock().await.is_empty());
    }

    /// `CommitFeedback` marks the named ids delivered, scoped (via the token) to
    /// the parent connection — the companion sends this only after it delivers.
    #[tokio::test]
    async fn commit_feedback_marks_delivered_scoped_to_parent() {
        let feedback = Arc::new(StubFeedback::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::CommitFeedback(BrokerCommitFeedbackRequest {
            token: "tok".into(),
            ids: vec!["f1".into(), "f2".into()],
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert!(resp.outcome.is_null(), "commit ack is empty");

        let committed = feedback.committed.lock().await;
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].0, "parent-conn");
        assert_eq!(committed[0].1, vec!["f1".to_string(), "f2".to_string()]);
    }

    /// An invalid token on `CommitFeedback` is a silent no-op (no commit).
    #[tokio::test]
    async fn commit_feedback_invalid_token_is_noop() {
        let feedback = Arc::new(StubFeedback::default());
        let listener = make_feedback_listener(Arc::new(TokenRegistry::default()), feedback.clone());
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(
            &mut client,
            &BrokerMessage::CommitFeedback(BrokerCommitFeedbackRequest {
                token: "bad".into(),
                ids: vec!["f1".into()],
            }),
        )
        .await
        .unwrap();
        let _: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert!(feedback.committed.lock().await.is_empty());
    }

    /// An invalid token returns an empty `{ count: 0 }` envelope (no leak of
    /// whether any feedback exists), never reads the store, and commits nothing.
    #[tokio::test]
    async fn feedback_invalid_token_returns_empty() {
        let feedback = Arc::new(StubFeedback::default());
        *feedback.items.lock().await = vec![pending("f1", "should never be returned")];
        let tokens = Arc::new(TokenRegistry::default());
        let listener = make_feedback_listener(tokens, feedback.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        let msg = BrokerMessage::Feedback(BrokerFeedbackRequest {
            token: "bad-token".into(),
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();

        assert_eq!(resp.outcome["count"], 0);
        assert!(resp.outcome["feedback"].as_array().unwrap().is_empty());
        // The store was never read or committed for an unknown token.
        assert!(feedback.read_conn.lock().await.is_none());
        assert!(feedback.committed.lock().await.is_empty());
    }

    // --- ask_user_question over the listener -------------------------------

    fn ask_msg(token: &str) -> BrokerMessage {
        BrokerMessage::Ask(BrokerAskRequest {
            token: token.into(),
            questions: vec![crate::acp::question::QuestionSpec {
                id: "qq-1".into(),
                question: "Which approach?".into(),
                header: "Approach".into(),
                multi_select: false,
                options: vec![
                    crate::acp::question::QuestionOption {
                        label: "Incremental".into(),
                        description: String::new(),
                    },
                    crate::acp::question::QuestionOption {
                        label: "Rewrite".into(),
                        description: String::new(),
                    },
                ],
                is_secret: false,
            }],
        })
    }

    use crate::acp::question::QuestionAnsweredItem;

    /// An `Ask` registers the question, parks, and — once the user answers —
    /// writes the `{ answers, declined }` envelope back over the same socket.
    #[tokio::test]
    async fn ask_registers_then_answer_resolves_response() {
        let questions = Arc::new(StubQuestion::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_question_listener(tokens, questions.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(&mut client, &ask_msg("tok")).await.unwrap();

        // The server must be parked until an answer arrives — no response yet.
        let early = tokio::time::timeout(Duration::from_millis(40), async {
            read_frame::<_, BrokerResponse>(&mut client).await
        })
        .await;
        assert!(early.is_err(), "ask must block until the user answers");

        // Wait for the stub to record the registration, then answer it.
        while questions.registered.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(questions.registered.lock().await[0].0, "parent-conn");
        questions
            .answer(
                "q-1",
                QuestionOutcome {
                    answers: vec![QuestionAnsweredItem {
                        question: "Which approach?".into(),
                        header: "Approach".into(),
                        multi_select: false,
                        selected: vec!["Incremental".into()],
                    }],
                    declined: false,
                },
            )
            .await;

        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["declined"], false);
        assert_eq!(resp.outcome["answers"][0]["selected"][0], "Incremental");
        assert_eq!(resp.outcome["answers"][0]["header"], "Approach");
    }

    /// A canceled tool call drops the request socket; the listener observes the
    /// peer-close, cancels the pending question, and returns without writing.
    #[tokio::test]
    async fn ask_peer_close_cancels_question() {
        let questions = Arc::new(StubQuestion::default());
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "parent-conn".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        let listener = make_question_listener(tokens, questions.clone());

        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move { listener.serve_one(&mut server).await });
        write_frame(&mut client, &ask_msg("tok")).await.unwrap();

        // Let the server park inside the wait.
        while questions.registered.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Companion cancels: drop the request socket.
        drop(client);

        let result = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("serve_one must return after peer close");
        result.unwrap().unwrap();
        assert_eq!(questions.canceled.lock().await.as_slice(), &["q-1".to_string()]);
    }

    /// An invalid token never registers a question and returns a `declined`
    /// outcome (the LLM proceeds with its own judgment).
    #[tokio::test]
    async fn ask_invalid_token_declined() {
        let questions = Arc::new(StubQuestion::default());
        let listener = make_question_listener(Arc::new(TokenRegistry::default()), questions.clone());
        let (mut client, mut server) = duplex(8 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(&mut client, &ask_msg("bad-token"))
            .await
            .unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(resp.outcome["declined"], true);
        assert!(questions.registered.lock().await.is_empty());
    }

    // -- browser tools ------------------------------------------------------

    async fn browser_tokens() -> Arc<TokenRegistry> {
        let tokens = Arc::new(TokenRegistry::default());
        tokens
            .register(
                "tok".into(),
                TokenEntry {
                    parent_connection_id: "conn-1".into(),
                    working_dir: PathBuf::from("/tmp"),
                },
            )
            .await;
        tokens
    }

    async fn browser_round_trip(
        listener: Arc<DelegationListener>,
        msg: BrokerMessage,
    ) -> BrokerResponse {
        let (mut client, mut server) = duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            listener.serve_one(&mut server).await.unwrap();
        });
        write_frame(&mut client, &msg).await.unwrap();
        let resp: BrokerResponse = read_frame(&mut client).await.unwrap();
        server_task.await.unwrap();
        resp
    }

    #[tokio::test]
    async fn browser_tabs_and_snapshot_reach_the_browser_with_their_arguments() {
        let browser = Arc::new(StubBrowser::default());
        let listener = make_browser_listener(browser_tokens().await, browser.clone());

        let listed = browser_round_trip(
            listener.clone(),
            BrokerMessage::BrowserTabs(BrokerBrowserTabsRequest { token: "tok".into() }),
        )
        .await;
        assert_eq!(listed.outcome["tabs"][0]["tabId"], "t1");
        assert_eq!(listed.outcome["tabs"][0]["level"], "read");

        let read = browser_round_trip(
            listener,
            BrokerMessage::BrowserSnapshot(BrokerBrowserSnapshotRequest {
                token: "tok".into(),
                tab_id: "t9".into(),
                max_chars: Some(1234),
            }),
        )
        .await;
        // A refusal travels as a value, not as a transport error: the agent
        // has something to do about it.
        assert_eq!(read.outcome["error"], ERROR_GRANT_REQUIRED);
        assert_eq!(read.outcome["tabId"], "t9");
        assert!(read.outcome["note"].as_str().unwrap().contains("t9"));

        assert_eq!(
            browser.calls.lock().await.as_slice(),
            &["list".to_string(), "snapshot t9 Some(1234)".to_string()]
        );
    }

    /// An action reaches the browser with the whole request — token checked,
    /// nothing reinterpreted on the way — and its refusal comes back as a
    /// value the companion can render.
    #[tokio::test]
    async fn an_action_reaches_the_browser_with_its_request() {
        use crate::browser::agent::{ActionKind, ActionRequest};
        let browser = Arc::new(StubBrowser::default());
        let listener = make_browser_listener(browser_tokens().await, browser.clone());

        let acted = browser_round_trip(
            listener,
            BrokerMessage::BrowserAct(BrokerBrowserActRequest {
                token: "tok".into(),
                tab_id: "t1".into(),
                request: ActionRequest {
                    generation: "g.4.2".into(),
                    target: Some("e5".into()),
                    action: ActionKind::Type {
                        text: "Ada".into(),
                        submit: true,
                    },
                },
            }),
        )
        .await;
        assert_eq!(
            acted.outcome["error"],
            crate::acp::browser_tools::ERROR_CONTROL_REQUIRED
        );
        assert_eq!(acted.outcome["tabId"], "t1");
        assert_eq!(
            browser.calls.lock().await.as_slice(),
            &[r#"act t1 g.4.2 Object {"kind": String("type"), "text": String("Ada"), "submit": Bool(true)}"#
                .to_string()]
        );
    }

    /// The console and screenshot arms carry their whole request through and
    /// bring a refusal back as a value; an invalid token is turned away before
    /// the browser hears of it, like every other browser arm.
    #[tokio::test]
    async fn console_and_capture_reach_the_browser_and_refuse_a_bad_token() {
        use crate::browser::capture::{CaptureFormat, CaptureRequest};
        use crate::browser::console::{ConsoleLevel, ConsoleQuery};
        let browser = Arc::new(StubBrowser::default());
        let listener = make_browser_listener(browser_tokens().await, browser.clone());

        let console = browser_round_trip(
            listener.clone(),
            BrokerMessage::BrowserConsole(BrokerBrowserConsoleRequest {
                token: "tok".into(),
                tab_id: "t2".into(),
                query: ConsoleQuery {
                    since: 7,
                    min_level: Some(ConsoleLevel::Warn),
                    limit: Some(20),
                },
            }),
        )
        .await;
        assert_eq!(console.outcome["error"], ERROR_GRANT_REQUIRED);
        assert_eq!(console.outcome["tabId"], "t2");

        let capture = browser_round_trip(
            listener.clone(),
            BrokerMessage::BrowserCapture(BrokerBrowserCaptureRequest {
                token: "tok".into(),
                tab_id: "t2".into(),
                request: CaptureRequest {
                    generation: Some("g.1.1".into()),
                    target: Some("e4".into()),
                    max_width: Some(800),
                    format: CaptureFormat::Jpeg,
                },
            }),
        )
        .await;
        assert_eq!(capture.outcome["error"], ERROR_GRANT_REQUIRED);
        assert_eq!(
            browser.calls.lock().await.as_slice(),
            &[
                "console t2 since=7 min=Some(Warn) limit=Some(20)".to_string(),
                "capture t2 Some((\"g.1.1\", \"e4\")) Some(800) Jpeg".to_string(),
            ]
        );

        browser.calls.lock().await.clear();
        for message in [
            BrokerMessage::BrowserConsole(BrokerBrowserConsoleRequest {
                token: "not-a-token".into(),
                tab_id: "t1".into(),
                query: ConsoleQuery::default(),
            }),
            BrokerMessage::BrowserCapture(BrokerBrowserCaptureRequest {
                token: "not-a-token".into(),
                tab_id: "t1".into(),
                request: CaptureRequest::default(),
            }),
        ] {
            let out = browser_round_trip(listener.clone(), message).await;
            assert_eq!(out.outcome["error"], ERROR_NO_SUCH_TAB);
        }
        assert!(browser.calls.lock().await.is_empty());
    }

    /// A capture the companion could not read back — over the frame cap —
    /// is turned into a refusal it can, at the last step before the frame.
    #[test]
    fn a_capture_over_the_frame_cap_becomes_a_refusal() {
        use crate::browser::capture::{CaptureOutcome, CaptureRegion};
        let huge = BrowserCaptureOutcome::image(
            "t1",
            CaptureOutcome {
                mime: "image/png".into(),
                data: "A".repeat(CAPTURE_RESPONSE_MAX_BYTES + 1),
                width: 1,
                height: 1,
                url: "http://x/".into(),
                region: CaptureRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                clipped: false,
            },
        );
        let response = browser_capture_response(huge).unwrap();
        assert_eq!(response.outcome["error"], crate::acp::browser_tools::ERROR_READ_FAILED);
        assert_eq!(response.outcome["tabId"], "t1");
        assert!(response.outcome.get("capture").is_none());
        assert!(serde_json::to_vec(&response.outcome).unwrap().len() < 4096);

        let small = BrowserCaptureOutcome::image(
            "t1",
            CaptureOutcome {
                mime: "image/png".into(),
                data: "AAAA".into(),
                width: 1,
                height: 1,
                url: "http://x/".into(),
                region: CaptureRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                clipped: false,
            },
        );
        assert_eq!(browser_capture_response(small).unwrap().outcome["capture"]["data"], "AAAA");
    }

    /// A caller who cannot prove it is a companion is told the same thing a
    /// user with no tabs open would be told, and the browser is never asked.
    /// Anything else would make the refusal codes a way to enumerate someone's
    /// open pages from outside the process.
    #[tokio::test]
    async fn an_invalid_token_learns_nothing_about_the_tabs() {
        let browser = Arc::new(StubBrowser::default());
        let listener = make_browser_listener(browser_tokens().await, browser.clone());

        let listed = browser_round_trip(
            listener.clone(),
            BrokerMessage::BrowserTabs(BrokerBrowserTabsRequest {
                token: "not-a-token".into(),
            }),
        )
        .await;
        assert_eq!(listed.outcome["tabs"].as_array().unwrap().len(), 0);
        assert!(listed.outcome.get("note").is_none());

        let read = browser_round_trip(
            listener,
            BrokerMessage::BrowserSnapshot(BrokerBrowserSnapshotRequest {
                token: "not-a-token".into(),
                tab_id: "t1".into(),
                max_chars: None,
            }),
        )
        .await;
        // `t1` really is open and really is shared — and the answer is the one
        // a nonexistent tab gets.
        assert_eq!(read.outcome["error"], ERROR_NO_SUCH_TAB);
        assert!(browser.calls.lock().await.is_empty());

        let acted = browser_round_trip(
            make_browser_listener(browser_tokens().await, browser.clone()),
            BrokerMessage::BrowserAct(BrokerBrowserActRequest {
                token: "not-a-token".into(),
                tab_id: "t1".into(),
                request: crate::browser::agent::ActionRequest {
                    generation: "g".into(),
                    target: Some("e1".into()),
                    action: crate::browser::agent::ActionKind::Hover,
                },
            }),
        )
        .await;
        assert_eq!(acted.outcome["error"], ERROR_NO_SUCH_TAB);
        assert!(browser.calls.lock().await.is_empty());
    }

    // -- socket path --------------------------------------------------------

    /// The fallback fires only when it has to. A temp directory short enough
    /// to hold a socket keeps the socket where the user's `TMPDIR` points,
    /// which is every ordinary desktop launch.
    #[cfg(unix)]
    #[test]
    fn a_short_temp_dir_is_used_directly() {
        let path = default_socket_path(Path::new("/tmp"));
        assert_eq!(path.parent(), Some(Path::new("/tmp")));
    }

    /// ...and an over-long one moves to the short directory rather than
    /// composing a path no `connect(2)` could name.
    #[cfg(unix)]
    #[test]
    fn an_over_long_temp_dir_falls_back_to_the_short_socket_dir() {
        let name = format!("codeg-delegation-{}.sock", std::process::id());
        // A macOS `/var/folders/…/T` with codeg's own per-session nesting under
        // it — the shape that actually produces this — padded so the composed
        // path lands exactly ONE byte past what `sun_path` can hold. Sized from
        // the cap rather than hard-coded, so it keeps straddling the boundary
        // if either side of it moves.
        let prefix = "/var/folders/hl/";
        let suffix = "/T/codeg-acp/12345-deadbeef";
        let pad = SUN_PATH_CAP - 1 - name.len() - prefix.len() - suffix.len();
        let ambient = PathBuf::from(format!("{prefix}{}{suffix}", "z".repeat(pad)));
        assert_eq!(ambient.as_os_str().len() + 1 + name.len(), SUN_PATH_CAP);
        assert!(!fits_sun_path(&ambient.join(&name)));

        let path = default_socket_path(&ambient);
        assert_eq!(path.parent(), Some(short_socket_dir().as_path()));
        assert_eq!(path.file_name(), Some(std::ffi::OsStr::new(&name)));
        assert!(
            fits_sun_path(&path),
            "the fallback must fit: {} is {} bytes",
            path.display(),
            path.as_os_str().len()
        );
    }

    /// The STAGED path is measured too, not assumed shorter than the real one.
    ///
    /// `.stg-<pid>-<8 hex>` is shorter than `codeg-delegation-<pid>.sock`, which
    /// is why production never noticed, but it is longer than a short name — so
    /// a short name in a deep directory fits `sun_path` while the path `bind`
    /// actually hands the kernel does not. Refusing both together is what makes
    /// the docstring's "a length limit cannot reject one and admit the other"
    /// true, and with it the case for having no unlink-then-bind fallback.
    ///
    /// Note what this must NOT lean on: the kernel refuses the over-long staged
    /// path by itself, with the same `InvalidInput` kind. Asserting only that
    /// `bind` errs would pass with the staged check deleted (verified by
    /// mutation). The discriminating observable is that NOTHING is touched —
    /// the un-created parent directory stays un-created.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_staged_path_too_long_for_sun_path_is_refused_too() {
        let holder = tempfile::tempdir_in("/tmp").unwrap();
        // Deep enough that `a.sock` still fits and `.stg-…` cannot. Left
        // UNCREATED on purpose — see above.
        let pad = SUN_PATH_CAP - 8 - holder.path().as_os_str().len() - 1;
        let dir = holder.path().join("x".repeat(pad));
        let socket = dir.join("a.sock");
        assert!(
            fits_sun_path(&socket),
            "the real path must fit, or this tests the wrong check"
        );
        assert!(!fits_sun_path(&DelegationListener::staging_socket_path(&socket)));

        let err = DelegationListener::bind(&socket).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("AF_UNIX"),
            "refused by us, not by the kernel after the fact: {err}"
        );
        assert!(
            !dir.exists(),
            "bind touched the filesystem before refusing: {} was created",
            dir.display()
        );
        assert!(!socket.exists());
    }

    /// The end-to-end claim, asserted against the KERNEL rather than against
    /// [`fits_sun_path`]'s own opinion of itself: what broke was a socket that
    /// could not be DIALED, so drive the whole path — choose, bind, connect.
    ///
    /// The ambient directory handed in here does not exist. That is deliberate:
    /// the fallback must not need it to.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_chosen_socket_path_can_actually_be_bound_and_dialed() {
        let ambient = PathBuf::from(format!(
            "/var/folders/hl/{}/T/codeg-acp/12345-deadbeef",
            "z".repeat(60)
        ));
        let path = default_socket_path(&ambient);

        let bound = DelegationListener::bind(&path).await.expect("bind");
        // No `accept` needed: a connect lands in the listener's backlog.
        let dialed = tokio::net::UnixStream::connect(&path).await;
        drop(bound);
        let _ = std::fs::remove_file(&path);

        assert!(
            dialed.is_ok(),
            "nothing could dial {} ({} bytes): {:?}",
            path.display(),
            path.as_os_str().len(),
            dialed.err()
        );
    }

    // ── computer use ───────────────────────────────────────────────────────

    #[derive(Default)]
    struct StubComputer {
        calls: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ComputerToolAccess for StubComputer {
        async fn list_apps(&self) -> ComputerAppsOutcome {
            self.calls.lock().await.push("apps".into());
            ComputerAppsOutcome::default()
        }
        async fn list_windows(&self, pid: Option<u32>) -> ComputerWindowsOutcome {
            self.calls.lock().await.push(format!("windows {pid:?}"));
            ComputerWindowsOutcome::default()
        }
        async fn capture(&self, target_id: &str, max: Option<u32>) -> ComputerCaptureOutcome {
            self.calls
                .lock()
                .await
                .push(format!("capture {target_id} {max:?}"));
            ComputerCaptureOutcome::refused(
                target_id,
                crate::acp::computer_tools::ERROR_GRANT_REQUIRED,
                crate::acp::computer_tools::grant_required_note(target_id),
            )
        }
        async fn snapshot(
            &self,
            target_id: &str,
            request: crate::acp::computer_tools::SnapshotRequest,
        ) -> ComputerSnapshotOutcome {
            self.calls
                .lock()
                .await
                .push(format!("snapshot {target_id} {:?}", request.max_chars));
            ComputerSnapshotOutcome::refused(
                target_id,
                crate::acp::computer_tools::ERROR_GRANT_REQUIRED,
                "stub",
            )
        }
        async fn verify(
            &self,
            target_id: &str,
            request: crate::computer::types::VerifyRequest,
        ) -> ComputerVerifyOutcome {
            self.calls
                .lock()
                .await
                .push(format!("verify {target_id} {}", request.expect.len()));
            ComputerVerifyOutcome::refused(
                target_id,
                crate::acp::computer_tools::ERROR_GRANT_REQUIRED,
                "stub",
            )
        }
        async fn act(
            &self,
            target_id: &str,
            request: crate::computer::types::ComputerActRequest,
            delivery: Option<crate::computer::types::ActDelivery>,
        ) -> ComputerActOutcome {
            let kind = serde_json::to_value(&request).unwrap()["kind"].clone();
            self.calls.lock().await.push(format!(
                "act {target_id} {} {delivery:?}",
                kind.as_str().unwrap_or("?")
            ));
            ComputerActOutcome::refused(
                target_id,
                crate::acp::computer_tools::ERROR_CONTROL_REQUIRED,
                "stub",
            )
        }

        async fn launch_app(
            &self,
            name: Option<String>,
            key: Option<String>,
        ) -> crate::acp::computer_tools::ComputerLaunchOutcome {
            self.calls
                .lock()
                .await
                .push(format!("launch {name:?} {key:?}"));
            crate::acp::computer_tools::ComputerLaunchOutcome::refused(
                crate::acp::computer_tools::ERROR_UNAVAILABLE,
                "stub",
            )
        }

        async fn clipboard(
            &self,
            op: crate::acp::computer_tools::ClipboardOp,
        ) -> crate::acp::computer_tools::ComputerClipboardOutcome {
            self.calls.lock().await.push(format!("clipboard {op:?}"));
            crate::acp::computer_tools::ComputerClipboardOutcome::refused(
                crate::acp::computer_tools::ERROR_UNAVAILABLE,
                "stub",
            )
        }
    }

    fn make_computer_listener(
        tokens: Arc<TokenRegistry>,
        computer: Arc<dyn ComputerToolAccess>,
    ) -> Arc<DelegationListener> {
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(AlwaysRootLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        DelegationListener::new(
            broker,
            tokens,
            Arc::new(StaticParentLookup(Some(1))),
            Arc::new(StubFeedback::default()),
            Arc::new(StubQuestion::default()),
            Arc::new(StubSessionInfo::default()),
            Arc::new(StubTaskTools),
            Arc::new(StubAuthoring::default()),
            Arc::new(NoBrowserTabs),
            computer,
        )
    }

    /// With a valid token every computer message reaches the access impl with
    /// its arguments intact, and a refusal comes back as a value.
    #[tokio::test]
    async fn computer_messages_reach_the_desktop_with_their_arguments() {
        let computer = Arc::new(StubComputer::default());
        let listener = make_computer_listener(browser_tokens().await, computer.clone());
        let token = || "tok".to_string();

        browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerApps(BrokerComputerAppsRequest { token: token() }),
        )
        .await;
        browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerWindows(BrokerComputerWindowsRequest {
                token: token(),
                pid: Some(42),
            }),
        )
        .await;
        let captured = browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerCapture(BrokerComputerCaptureRequest {
                token: token(),
                target_id: "w7".into(),
                max_dimension: Some(800),
            }),
        )
        .await;
        assert_eq!(captured.outcome["error"], "computer_grant_required");
        assert_eq!(captured.outcome["targetId"], "w7");
        browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerSnapshot(BrokerComputerSnapshotRequest {
                token: token(),
                target_id: "w7".into(),
                request: crate::acp::computer_tools::SnapshotRequest {
                    max_chars: Some(0),
                    ..Default::default()
                },
            }),
        )
        .await;
        browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerVerify(BrokerComputerVerifyRequest {
                token: token(),
                target_id: "w7".into(),
                request: crate::computer::types::VerifyRequest {
                    expect: vec![Default::default()],
                    ..Default::default()
                },
            }),
        )
        .await;
        let acted = browser_round_trip(
            listener,
            BrokerMessage::ComputerAct(BrokerComputerActRequest {
                token: token(),
                target_id: "w7".into(),
                request: crate::computer::types::ComputerActRequest::SetValue {
                    target: crate::computer::types::ElementTarget {
                        generation: "1.2".into(),
                        index: 4,
                    },
                    value: "x".into(),
                },
                // Passed on as asked; whether it may be is the service's call.
                delivery: Some(crate::computer::types::ActDelivery::Foreground),
            }),
        )
        .await;
        assert_eq!(acted.outcome["error"], "computer_control_required");
        assert_eq!(
            computer.calls.lock().await.as_slice(),
            &[
                "apps".to_string(),
                "windows Some(42)".to_string(),
                "capture w7 Some(800)".to_string(),
                "snapshot w7 Some(0)".to_string(),
                "verify w7 1".to_string(),
                "act w7 setValue Some(Foreground)".to_string(),
            ]
        );
    }

    /// A caller without a valid token learns nothing: the listings are empty,
    /// every window is "no such window", and the desktop is never asked.
    #[tokio::test]
    async fn an_invalid_token_learns_nothing_about_the_desktop() {
        let computer = Arc::new(StubComputer::default());
        let listener = make_computer_listener(browser_tokens().await, computer.clone());

        let apps = browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerApps(BrokerComputerAppsRequest {
                token: "forged".into(),
            }),
        )
        .await;
        assert_eq!(apps.outcome["apps"], serde_json::json!([]));
        assert!(apps.outcome.get("error").is_none());

        let read = browser_round_trip(
            listener.clone(),
            BrokerMessage::ComputerCapture(BrokerComputerCaptureRequest {
                token: "forged".into(),
                target_id: "w1".into(),
                max_dimension: None,
            }),
        )
        .await;
        assert_eq!(read.outcome["error"], "computer_no_such_target");
        let act = browser_round_trip(
            listener,
            BrokerMessage::ComputerAct(BrokerComputerActRequest {
                token: "forged".into(),
                target_id: "w1".into(),
                request: crate::computer::types::ComputerActRequest::Key {
                    target: None,
                    chord: crate::computer::keys::Chord {
                        key: crate::computer::keys::Key::Return,
                        modifiers: Default::default(),
                    },
                    repeat: 1,
                },
                delivery: None,
            }),
        )
        .await;
        assert_eq!(act.outcome["error"], "computer_no_such_target");
        assert!(computer.calls.lock().await.is_empty());
    }
