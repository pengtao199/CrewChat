    use super::*;
    use crate::acp::connection::AgentConnection;

    /// The probe applies the caller's model, so the session it reads holds that
    /// model; the delegation panel names its "Default" choice after the
    /// snapshot's current value. Model ids from a live opencode 2.0.24 probe:
    /// the agent opened on `exo-free` and the caller asked for
    /// `step-5-preview-free`. The agent's own effort differs from the one it
    /// answered after the switch, so the test can tell which one survives.
    #[test]
    fn the_probe_reports_the_agents_own_pick_for_the_options_it_applied() {
        use crate::acp::types::{
            SessionConfigBooleanInfo, SessionConfigKindInfo, SessionConfigOptionInfo,
            SessionConfigSelectInfo,
        };
        let option = |id: &str, kind: SessionConfigKindInfo| SessionConfigOptionInfo {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            category: None,
            kind,
            recommended_value: None,
        };
        let select = |current: &str| {
            SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current.to_string(),
                options: Vec::new(),
                groups: Vec::new(),
            })
        };
        let current = |options: &[SessionConfigOptionInfo], id: &str| {
            let option = options.iter().find(|o| o.id == id).expect("present");
            match &option.kind {
                SessionConfigKindInfo::Select(select) => select.current_value.clone(),
                SessionConfigKindInfo::Boolean(boolean) => boolean.current_value.to_string(),
            }
        };
        let mut options = vec![
            option("model", select("opencode/step-5-preview-free")),
            option("effort", select("default")),
            option("mode", select("plan")),
            option(
                "auto_approve",
                SessionConfigKindInfo::Boolean(SessionConfigBooleanInfo {
                    current_value: true,
                }),
            ),
        ];
        let agent_chosen: BTreeMap<String, String> = [
            ("model", "opencode/exo-free"),
            ("effort", "high"),
            ("mode", "build"),
            ("auto_approve", "false"),
        ]
        .into_iter()
        .map(|(id, value)| (id.to_string(), value.to_string()))
        .collect();
        // `provider` was applied but neither answered nor in the snapshot.
        let applied = ["model", "auto_approve", "provider"].map(String::from);

        report_agent_chosen_values(&mut options, &applied, &agent_chosen);

        assert_eq!(current(&options, "model"), "opencode/exo-free");
        assert_eq!(current(&options, "auto_approve"), "false");
        // Not applied: what the agent answered GIVEN the selection stands — the
        // default effort of the selected model, and a mode nobody asked for.
        assert_eq!(current(&options, "effort"), "default");
        assert_eq!(current(&options, "mode"), "plan");
    }

    /// An agent that has left the connection map but not yet exited can still
    /// be appending to the transcript a restore is about to unlink, so the
    /// gate has to see it. `disconnect` drops the map entry immediately, which
    /// is why the pid is parked separately rather than the entry being kept.
    #[tokio::test]
    async fn disconnect_keeps_a_live_child_visible_after_the_map_entry_is_gone() {
        use std::sync::atomic::Ordering::SeqCst;
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let pid = {
            let conns = mgr.connections.lock().await;
            conns.get("c1").unwrap().child_pid.clone()
        };
        pid.store(4242, SeqCst); // spawned

        mgr.disconnect("c1").await.unwrap();
        assert!(
            mgr.list_connections().await.is_empty(),
            "the map entry goes immediately — that is the whole problem"
        );
        assert_eq!(
            mgr.live_or_draining_agent_names().await,
            vec!["Claude Code".to_string()]
        );
    }

    /// Closing a window tears down its agents the same way, and leaves them
    /// exiting rather than exited.
    #[tokio::test]
    async fn closing_a_window_also_keeps_its_children_visible() {
        use std::sync::atomic::Ordering::SeqCst;
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        {
            let conns = mgr.connections.lock().await;
            conns.get("c1").unwrap().child_pid.store(777, SeqCst);
        }
        assert_eq!(mgr.disconnect_by_owner_window("test-window").await, 1);
        assert_eq!(
            mgr.live_or_draining_agent_names().await,
            vec!["Codex CLI".to_string()]
        );
    }

    /// The Antigravity sign-out ends that agent's connections and nobody
    /// else's. Precision is the point: their processes would write the cleared
    /// credential back on their next refresh, while an unrelated agent's
    /// session has nothing to do with it and must survive.
    #[tokio::test]
    async fn disconnecting_one_agent_type_leaves_the_others_running() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("agy1", AgentType::Antigravity, None, EventEmitter::Noop)
            .await;
        mgr.insert_test_connection("agy2", AgentType::Antigravity, None, EventEmitter::Noop)
            .await;
        mgr.insert_test_connection("cdx", AgentType::Codex, None, EventEmitter::Noop)
            .await;

        assert_eq!(
            mgr.disconnect_by_agent_type(AgentType::Antigravity).await,
            2
        );
        let left: Vec<String> = mgr
            .list_connections()
            .await
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(left, vec!["cdx".to_string()]);

        // And they leave exiting, not exited — the same handoff every other
        // teardown makes, so the shutdown backstop can still reach them. They
        // are out of the map yet still named here, alongside the live Codex.
        assert_eq!(
            mgr.live_or_draining_agent_names().await,
            vec!["Codex CLI".to_string(), "Google Antigravity".to_string()]
        );

        // Nothing of that type left: a second pass is a no-op rather than an
        // error, which is what makes the sign-out safe to retry.
        assert_eq!(
            mgr.disconnect_by_agent_type(AgentType::Antigravity).await,
            0
        );
    }

    /// A pid of zero means BOTH "reaped" and "not spawned yet", so a
    /// connection torn down mid-spawn — which can publish a live pid moments
    /// later — must not be dismissed. It ages out instead.
    #[tokio::test]
    async fn a_child_that_has_not_published_a_pid_is_still_assumed_live() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::Codex, None, EventEmitter::Noop)
            .await;
        mgr.disconnect("c1").await.unwrap();
        assert_eq!(
            mgr.live_or_draining_agent_names().await,
            vec!["Codex CLI".to_string()],
            "within the grace window an unpublished pid counts as running"
        );

        // Backdate past the grace: now zero can only mean finished.
        {
            let mut draining = mgr.draining.lock().await;
            for child in draining.iter_mut() {
                child.parked_at = std::time::Instant::now() - DRAIN_GRACE * 2;
            }
        }
        assert!(mgr.live_or_draining_agent_names().await.is_empty());
        assert_eq!(
            mgr.draining.lock().await.len(),
            0,
            "reading prunes, so the list cannot grow without bound"
        );
    }

    /// `spawn_agent` takes the read side of `external_restore_lock` as its
    /// very first statement, so holding the write side is what makes a backup
    /// restore's "are any agents running?" check a real gate rather than a
    /// check-then-use with a multi-minute gap.
    #[tokio::test]
    async fn lock_out_new_connections_blocks_connection_establishment() {
        let mgr = ConnectionManager::new();
        let guard = mgr.lock_out_new_connections().await;
        assert!(
            mgr.external_restore_lock.try_read().is_err(),
            "no connection may be established while the lockout is held"
        );
        drop(guard);
        assert!(mgr.external_restore_lock.try_read().is_ok());
    }
    // Test-only: the budget itself is enforced at the append in
    // `SessionState::apply_event`, so nothing in this module's production code
    // names it — only the tests that pin the bound do.
    use crate::acp::feedback::MAX_FEEDBACK_ATTACHMENT_BYTES_PER_TURN;
    use crate::acp::session_state::SessionState;
    use crate::acp::types::ConnectionStatus;
    use crate::web::event_bridge::{EventEmitter, WebEvent, WebEventBroadcaster};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::{broadcast, mpsc, RwLock};

    #[test]
    fn is_reserved_turn_id_matches_only_the_parser_namespace() {
        // Rejected: the parsers' `turn-<digits>` ids (an untrusted client id of
        // this shape would collide with a persisted transcript turn).
        assert!(is_reserved_turn_id("turn-0"));
        assert!(is_reserved_turn_id("turn-42"));
        // Accepted: anything else, including the real UI sender id shape and the
        // connection-scoped fallback shape.
        assert!(!is_reserved_turn_id("optimistic-9f3c1a2b"));
        assert!(!is_reserved_turn_id("user-conn-7"));
        assert!(!is_reserved_turn_id("turn-")); // no number
        assert!(!is_reserved_turn_id("turn-1a")); // not all digits
        assert!(!is_reserved_turn_id("turnabout-1"));
        assert!(!is_reserved_turn_id(""));
    }

    fn fake_connection(id: &str, conv_id: Option<i32>) -> AgentConnection {
        let (tx, _rx) = mpsc::channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = conv_id;
        state.status = ConnectionStatus::Connected;
        AgentConnection {
            id: id.to_string(),
            agent_type: crate::models::agent::AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Spawn a two-level process tree: `sh` (the stand-in for the agent CLI)
    /// backgrounds a `sleep` grandchild (the stand-in for the agent's own
    /// children — an MCP server, a forked `node`) and records its pid. The
    /// grandchild is what the backstop assertions are about: it is the process
    /// that gets reparented and lingers when only the direct child is killed.
    ///
    /// Returns the direct child — keep it alive, dropping a `Child` does NOT
    /// kill it — and the grandchild pid.
    #[cfg(unix)]
    async fn spawn_process_tree(pidfile: &std::path::Path) -> (std::process::Child, i32) {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 30 & echo $! > '{}'; wait", pidfile.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh");
        for _ in 0..150 {
            if let Ok(raw) = std::fs::read_to_string(pidfile) {
                if let Ok(pid) = raw.trim().parse::<i32>() {
                    return (child, pid);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Bail-out path still reaps the tree, so a failing test can't leave a
        // `sleep` behind for 30s.
        let _ = kill_tree::blocking::kill_tree(child.id());
        let _ = child.wait();
        panic!("grandchild never recorded its pid");
    }

    /// True once `pid` is gone. `kill(pid, 0)` sends no signal, it only probes
    /// existence; polling keeps the assertion from racing kernel teardown.
    #[cfg(unix)]
    async fn wait_until_dead(pid: i32) -> bool {
        for _ in 0..150 {
            // SAFETY: signal 0 only probes for existence; it sends no signal.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[cfg(unix)]
    fn is_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes for existence; it sends no signal.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Quit has to kill the agent's whole process TREE. Killing just the direct
    /// child leaves its children reparented and lingering — that orphan window
    /// is the entire reason the backstop exists.
    ///
    /// The test connection's command receiver is dropped, so the graceful path
    /// is unavailable and the kill can only come from the backstop.
    /// Unix-only (relies on `sh` / `kill(2)`).
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_kills_the_whole_agent_process_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-tree", None);
        conn.child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);
        mgr.connections
            .lock()
            .await
            .insert("conn-tree".to_string(), conn);

        assert_eq!(mgr.disconnect_all().await, 1);
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — the quit backstop did not kill the tree"
        );
        let _ = child.wait();
    }

    /// A connection still `Connecting` when quit begins publishes its pid AFTER
    /// the map is drained. Reading the pids up front would see `0` there and
    /// skip it, leaking exactly the orphan this exists to kill — so the load
    /// has to happen after the grace window, from the live cell.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_reaches_a_child_that_spawns_during_the_grace_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-late", None);
        // Still 0 at drain time, exactly like a connection whose agent process
        // hasn't launched yet.
        let cell = Arc::clone(&conn.child_pid);
        mgr.connections
            .lock()
            .await
            .insert("conn-late".to_string(), conn);

        let pid = child.id();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cell.store(pid, std::sync::atomic::Ordering::SeqCst);
        });

        assert_eq!(mgr.disconnect_all().await, 1);
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — a pid published during the grace window was missed"
        );
        let _ = child.wait();
    }

    /// The mirror image: once the agent process has been reaped, the `on_exit`
    /// callback zeroes the cell and the backstop must leave that pid alone.
    /// Without the clear, a quit fires `kill_tree` at a pid whose process is
    /// already dead and reaped — and if the OS recycled that number, the victim
    /// is an unrelated process tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_all_backstop_leaves_a_cleared_pid_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        let conn = fake_connection("conn-cleared", None);
        conn.child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);
        let cell = Arc::clone(&conn.child_pid);
        mgr.connections
            .lock()
            .await
            .insert("conn-cleared".to_string(), conn);

        // Stands in for the driver unwinding mid-window: the process is gone
        // and the guard has zeroed the cell.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cell.store(0, std::sync::atomic::Ordering::SeqCst);
        });

        assert_eq!(mgr.disconnect_all().await, 1);
        // Settle: a wrongly-issued SIGTERM would have landed by now.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            is_alive(gpid),
            "backstop killed a tree whose pid the driver had already cleared"
        );

        let _ = kill_tree::blocking::kill_tree(child.id());
        let _ = child.wait();
    }

    /// The Antigravity sign-out needs "no process of this agent is running",
    /// and a child an earlier `disconnect` left draining is exactly the one the
    /// map cannot see. It was asked to go — `disconnect` sends and returns —
    /// but nothing ever checked that it went, and while it lives it still holds
    /// the credential the sign-out is about to clear.
    ///
    /// The test connection's command receiver is dropped, so the graceful path
    /// is unavailable: this process can only die from the sweep.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_by_agent_type_sweeps_a_child_left_draining_by_an_earlier_teardown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_process_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("agy-old", AgentType::Antigravity, None, EventEmitter::Noop)
            .await;
        mgr.connections
            .lock()
            .await
            .get("agy-old")
            .unwrap()
            .child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);

        // Out of the map, into `draining` — a closed tab, say.
        mgr.disconnect("agy-old").await.unwrap();
        assert!(mgr.list_connections().await.is_empty());

        // Nothing live to end, so nothing to report — but its process is still
        // there, and that is what has to be gone.
        assert_eq!(
            mgr.disconnect_by_agent_type(AgentType::Antigravity).await,
            0
        );
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — a draining child of the same agent was never swept"
        );
        let _ = child.wait();
    }

    /// `kill_tree` sends `SIGTERM` and returns; `SIGTERM` can be caught or
    /// ignored. So "the kill call returned" is not "the agent is gone", and the
    /// sign-out's whole premise is that it IS gone before the credential goes.
    /// A process that ignores `SIGTERM` must still end up dead, via `SIGKILL`.
    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_by_agent_type_escalates_to_sigkill_for_a_child_that_ignores_sigterm() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut child, gpid) = spawn_sigterm_proof_tree(&dir.path().join("g.pid")).await;

        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("agy", AgentType::Antigravity, None, EventEmitter::Noop)
            .await;
        mgr.connections
            .lock()
            .await
            .get("agy")
            .unwrap()
            .child_pid
            .store(child.id(), std::sync::atomic::Ordering::SeqCst);

        assert_eq!(
            mgr.disconnect_by_agent_type(AgentType::Antigravity).await,
            1
        );

        // The target is this test's own child, so how it died is readable, and
        // signal 9 is the escalation: it ignores SIGTERM, so nothing softer did
        // this. `try_wait` rather than `wait` because it does not block — a
        // status from it means the process was dead by the time the sweep
        // returned, not merely that it died eventually. (Strictly it means
        // "dead a few microseconds after", which is as close to the ordering as
        // a caller outside the process can observe.)
        use std::os::unix::process::ExitStatusExt;
        let status = child
            .try_wait()
            .expect("read the target's status")
            .expect("target was still running when the sweep returned");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "target survived SIGTERM and was never SIGKILLed"
        );
        assert!(
            wait_until_dead(gpid).await,
            "grandchild {gpid} survived — the escalation reached the target but not its tree"
        );
    }

    /// Like [`spawn_process_tree`], but both levels ignore `SIGTERM`: the shell
    /// sets it to `SIG_IGN`, which `sleep` then inherits across `exec`. Stands
    /// in for an agent that traps it and takes its time — or never leaves.
    #[cfg(unix)]
    async fn spawn_sigterm_proof_tree(pidfile: &std::path::Path) -> (std::process::Child, i32) {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap '' TERM; sleep 30 & echo $! > '{}'; wait",
                pidfile.display()
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh");
        for _ in 0..150 {
            if let Ok(raw) = std::fs::read_to_string(pidfile) {
                if let Ok(pid) = raw.trim().parse::<i32>() {
                    return (child, pid);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // SIGTERM would bounce off, so the bail-out path has to use SIGKILL.
        let config = kill_tree::Config {
            signal: "SIGKILL".to_string(),
            ..Default::default()
        };
        let _ = kill_tree::blocking::kill_tree_with_config(child.id(), &config);
        let _ = child.wait();
        panic!("grandchild never recorded its pid");
    }

    /// Build a broadcaster + subscribed receiver. Subscribing here (not lazily
    /// inside the test) ensures events emitted between construction and the
    /// first `recv` are buffered rather than dropped.
    fn make_test_broadcaster() -> (Arc<WebEventBroadcaster>, broadcast::Receiver<WebEvent>) {
        let bcast = Arc::new(WebEventBroadcaster::new());
        let rx = bcast.subscribe();
        (bcast, rx)
    }

    /// Thin wrapper around `ConnectionManager::insert_test_connection` so the
    /// existing in-crate tests keep their `insert_fake_connection(mgr, ...)`
    /// call shape after the public test helper landed.
    async fn insert_fake_connection(
        mgr: &ConnectionManager,
        id: &str,
        agent_type: crate::models::agent::AgentType,
        working_dir: Option<PathBuf>,
        emitter: EventEmitter,
    ) {
        mgr.insert_test_connection(id, agent_type, working_dir, emitter)
            .await;
    }

    #[tokio::test]
    async fn refresh_connection_staleness_flags_only_drifted_running_sessions() {
        let mgr = ConnectionManager::new();
        // Test connections spawn with an empty fingerprint (insert_test_connection).
        insert_fake_connection(&mgr, "c1", AgentType::Codex, None, EventEmitter::Noop).await;
        // A different agent type that must stay untouched.
        insert_fake_connection(&mgr, "c2", AgentType::ClaudeCode, None, EventEmitter::Noop).await;

        // A real config change for Codex (fresh fp differs from the "" spawn fp).
        let mut fresh = HashMap::new();
        fresh.insert(AgentType::Codex, "codex-v2".to_string());
        let n = mgr
            .refresh_connection_staleness(&fresh, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n, 1, "only the Codex session is stale");
        assert!(
            mgr.get_state("c1").await.unwrap().read().await.config_stale,
            "Codex session flagged stale"
        );
        assert!(
            !mgr.get_state("c2").await.unwrap().read().await.config_stale,
            "ClaudeCode session untouched (agent not in the fresh set)"
        );

        // Re-running with the SAME fingerprint keeps it stale but is idempotent.
        let n2 = mgr
            .refresh_connection_staleness(&fresh, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n2, 1);

        // Reverting Codex back to its spawn fingerprint ("") clears staleness.
        let mut reverted = HashMap::new();
        reverted.insert(AgentType::Codex, String::new());
        let n3 = mgr
            .refresh_connection_staleness(&reverted, ConfigStaleKind::AgentConfig)
            .await;
        assert_eq!(n3, 0, "reverted config is no longer stale");
        assert!(
            !mgr.get_state("c1").await.unwrap().read().await.config_stale,
            "staleness cleared after revert"
        );
    }

    /// Subscribe directly to the per-connection event stream. Phase 4b
    /// removed the dual-broadcast through the global `WebEventBroadcaster`
    /// for ACP events; the per-connection stream is now the only delivery
    /// path tests can observe. Subscribe BEFORE triggering the producing
    /// call so events emitted between subscribe and recv buffer rather
    /// than drop.
    async fn subscribe_conn_stream(
        mgr: &ConnectionManager,
        conn_id: &str,
    ) -> broadcast::Receiver<std::sync::Arc<crate::acp::types::EventEnvelope>> {
        let state = mgr
            .get_state(conn_id)
            .await
            .expect("connection should be registered");
        let stream = state.read().await.event_stream();
        stream.subscribe()
    }

    /// Receive the first envelope from a per-connection stream. Times out
    /// after 200 ms to keep tests honest.
    async fn recv_first_acp_event(
        rx: &mut broadcast::Receiver<std::sync::Arc<crate::acp::types::EventEnvelope>>,
    ) -> crate::acp::types::EventEnvelope {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for acp event")
            .expect("per-connection stream closed");
        (*evt).clone()
    }

    /// Drain the connection's command receiver (non-blocking) and return one
    /// entry per enqueued `Prompt` command: its attached `user_message` payload
    /// (the cross-client broadcast the loop emits before the agent request),
    /// flattened to `(message_id, text blocks)`. The inner `Option` is `None`
    /// for a `Prompt` carrying no user message (delegation child / unbound).
    /// The vec length is the number of `Prompt` commands enqueued — useful for
    /// asserting the concurrency gate stopped a second one. Call after the
    /// producing await.
    fn drain_prompt_user_messages(
        cmd_rx: &mut mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
    ) -> Vec<Option<(String, Vec<String>)>> {
        let mut out = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let crate::acp::connection::ConnectionCommand::Prompt { user_message, .. } = cmd {
                out.push(user_message.map(|(id, blocks)| {
                    let texts = blocks
                        .iter()
                        .filter_map(|b| match b {
                            crate::acp::types::UserMessageBlock::Text { text } => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<String>>();
                    (id, texts)
                }));
            }
        }
        out
    }

    /// A minimal non-empty prompt for tests that exercise linking / status /
    /// caller-id behavior and don't care about the prompt content. (Empty
    /// prompts are now rejected before any side effects, so these tests must
    /// pass real content to reach the link path.)
    fn one_text_block() -> Vec<PromptInputBlock> {
        vec![PromptInputBlock::Text {
            text: "test prompt".into(),
        }]
    }

    /// Insert a connection with a LIVE command receiver so `send_prompt_inner`'s
    /// enqueue SUCCEEDS (the UserMessage broadcast is deferred until after a
    /// successful enqueue). Returns the receiver — keep it in scope for the
    /// test, otherwise the channel closes and the send fails.
    async fn insert_live_connection(
        mgr: &ConnectionManager,
        conn_id: &str,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
    ) -> tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand> {
        use crate::acp::connection::AgentConnection;
        use crate::acp::session_state::SessionState;
        let (tx, rx) = mpsc::channel::<crate::acp::connection::ConnectionCommand>(4);
        let mut state = SessionState::new(
            conn_id.to_string(),
            agent_type,
            working_dir,
            "test-window".to_string(),
            None,
        );
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: conn_id.to_string(),
            agent_type,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        mgr.connections
            .lock()
            .await
            .insert(conn_id.to_string(), conn);
        rx
    }

    /// Put a turn in flight on a connection inserted by
    /// [`insert_live_connection`] (which starts them `Connected`).
    async fn mark_prompting(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.expect("connection");
        state.write().await.status = ConnectionStatus::Prompting;
    }

    /// Mirror what a live `active` goal snapshot leaves on the state.
    async fn mark_goal_active(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.expect("connection");
        state.write().await.goal_active = true;
    }

    /// Stand in for the connection loop's `GoalControl` arm: take the command,
    /// assert the action, answer `landed`, and hand the receiver back so the
    /// test can see what (if anything) the manager enqueues next. Returns a
    /// JoinHandle because `goal_control` blocks on that answer.
    fn answer_goal_control(
        mut rx: tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
        expected: GoalControlAction,
        landed: bool,
    ) -> tokio::task::JoinHandle<tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>>
    {
        tokio::spawn(async move {
            match rx.recv().await.expect("goal control enqueued") {
                ConnectionCommand::GoalControl { action, reply } => {
                    assert_eq!(action, expected);
                    reply
                        .expect("an interrupting caller attaches a reply")
                        .send(landed)
                        .expect("manager is listening");
                }
                _ => panic!("expected a GoalControl command"),
            }
            rx
        })
    }

    #[tokio::test]
    async fn a_codex_goal_pause_mid_turn_interrupts_the_turn_after_the_goal_rpc() {
        // codex's pause is app-server metadata: it stops the goal from starting
        // ANOTHER turn but never touches the one that is running, so without
        // the interrupt the user clicks Pause and watches the agent keep
        // working. Order matters — the goal RPC has to LAND first, so the goal
        // is already non-active when the turn aborts and nothing auto-continues
        // on the way out.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-codex", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-codex").await;
        mark_goal_active(&mgr, "c-goal-codex").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Pause, true);

        mgr.goal_control(&db.conn, "c-goal-codex", GoalControlAction::Pause)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(matches!(rx.try_recv(), Ok(ConnectionCommand::Cancel)));
        assert!(rx.try_recv().is_err(), "nothing else is enqueued");
    }

    #[tokio::test]
    async fn a_rejected_goal_control_leaves_the_turn_alone() {
        // The agent refused the pause (bad params, unknown session, dead
        // process). The goal is still active, so killing the turn would destroy
        // in-flight work AND let codex resume the goal at the next idle point —
        // strictly worse than the error banner the loop already emitted.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-refused", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-refused").await;
        mark_goal_active(&mgr, "c-goal-refused").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Pause, false);

        mgr.goal_control(&db.conn, "c-goal-refused", GoalControlAction::Pause)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(rx.try_recv().is_err(), "a failed control cancels nothing");
    }

    #[tokio::test]
    async fn a_codex_goal_clear_interrupts_even_when_codeg_thinks_it_is_idle() {
        // Codeg's `Prompting` only covers turns IT started. A goal loop's
        // continuations are started by codex — detached turns no host request
        // owns — so the session reads "connected" while the agent is very much
        // working, which is precisely when the user hits Clear. Gating on our
        // own turn bookkeeping would skip the interrupt exactly there, so the
        // interrupt is not gated on liveness: with an ACTIVE goal, whatever is
        // running is the goal's work, and with nothing running the interrupt is
        // the same harmless no-op the Stop button is.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let rx = insert_live_connection(&mgr, "c-goal-idle", AgentType::Codex, None).await;
        mark_goal_active(&mgr, "c-goal-idle").await;
        let loop_stub = answer_goal_control(rx, GoalControlAction::Clear, true);

        mgr.goal_control(&db.conn, "c-goal-idle", GoalControlAction::Clear)
            .await
            .unwrap();

        let mut rx = loop_stub.await.unwrap();
        assert!(matches!(rx.try_recv(), Ok(ConnectionCommand::Cancel)));
        assert!(rx.try_recv().is_err(), "nothing else is enqueued");
    }

    #[tokio::test]
    async fn clearing_a_paused_goal_never_touches_the_users_own_turn() {
        // A paused goal drives nothing, so a turn running alongside it is
        // something the user started themselves. Dismissing the stale card is
        // housekeeping — killing that prompt (and draining its permissions, and
        // cascading to its delegations) would be destroying unrelated work.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx = insert_live_connection(&mgr, "c-goal-paused", AgentType::Codex, None).await;
        mark_prompting(&mgr, "c-goal-paused").await; // the user's own prompt
        // `goal_active` stays false: the last snapshot was `paused`.

        mgr.goal_control(&db.conn, "c-goal-paused", GoalControlAction::Clear)
            .await
            .unwrap();

        match rx.try_recv() {
            Ok(ConnectionCommand::GoalControl { action, reply }) => {
                assert_eq!(action, GoalControlAction::Clear);
                assert!(reply.is_none(), "no interrupt is intended, so none waits");
            }
            _ => panic!("expected a GoalControl command"),
        }
        assert!(rx.try_recv().is_err(), "the user's turn survives");
    }

    #[tokio::test]
    async fn a_claude_goal_clear_never_interrupts_the_turn_carrying_it() {
        // claude-agent-acp implements `_session/goal` by STEERING the text
        // "/goal clear" into the running turn (or prompting when idle).
        // Cancelling would kill the message that carries the clear and leave
        // the goal armed — worse than doing nothing. Same for every adapter
        // whose control channel we haven't verified. No interrupt is intended,
        // so no reply is even asked for and the call doesn't wait.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let mut rx =
            insert_live_connection(&mgr, "c-goal-claude", AgentType::ClaudeCode, None).await;
        mark_prompting(&mgr, "c-goal-claude").await;
        mark_goal_active(&mgr, "c-goal-claude").await;

        mgr.goal_control(&db.conn, "c-goal-claude", GoalControlAction::Clear)
            .await
            .unwrap();

        match rx.try_recv() {
            Ok(ConnectionCommand::GoalControl { action, reply }) => {
                assert_eq!(action, GoalControlAction::Clear);
                assert!(reply.is_none(), "nobody waits on an in-band control");
            }
            _ => panic!("expected a GoalControl command"),
        }
        assert!(rx.try_recv().is_err(), "the carrying turn survives");
    }

    #[tokio::test]
    async fn send_prompt_linked_attaches_user_message_to_prompt_for_root() {
        // A root send attaches the projected user-message payload to the
        // enqueued Prompt command (the connection loop emits the UserMessage
        // event itself, ordered before the agent request). With a live receiver
        // the enqueue succeeds and the payload is observable on the command.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-root").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-root";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-root")),
        )
        .await;

        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "hello viewers".into(),
                }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(
            result.is_ok(),
            "enqueue should succeed with a live receiver"
        );

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(prompts.len(), 1, "exactly one Prompt enqueued");
        let um = prompts[0]
            .as_ref()
            .expect("root Prompt carries a user_message");
        assert!(
            um.0.starts_with("user-"),
            "connection-scoped id fallback, got {:?}",
            um.0
        );
        assert!(
            um.1.iter().any(|t| t == "hello viewers"),
            "user_message must carry the prompt text, got {um:?}"
        );
    }

    #[tokio::test]
    async fn linked_ui_prompt_leaves_the_user_blocks_untouched() {
        use crate::db::test_helpers;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/agent-routes").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-agent-routes";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/agent-routes")),
        )
        .await;

        mgr.send_prompt_linked_with_message_id(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "ask [@Antigravity](codeg://agent/antigravity) to review".into(),
            }],
            Some(folder_id),
            None,
            None,
            Some("optimistic-route".into()),
        )
        .await
        .unwrap();

        let command = cmd_rx.try_recv().expect("one prompt command");
        let ConnectionCommand::Prompt {
            blocks,
            user_message,
        } = command
        else {
            panic!("expected prompt command");
        };
        // The routing frame is appended at the agent boundary in the connection
        // loop, never here: what the manager enqueues, persists and broadcasts
        // is exactly what the user typed.
        assert!(matches!(
            blocks.as_slice(),
            [PromptInputBlock::Text { text }]
                if text == "ask [@Antigravity](codeg://agent/antigravity) to review"
        ));
        let (message_id, user_blocks) = user_message.expect("root prompt is broadcast");
        assert_eq!(message_id, "optimistic-route");
        assert!(matches!(
            user_blocks.as_slice(),
            [crate::acp::types::UserMessageBlock::Text { text }]
                if text == "ask [@Antigravity](codeg://agent/antigravity) to review"
        ));
    }

    #[tokio::test]
    async fn reserved_route_separator_is_scrubbed_not_rejected() {
        // The separator is invisible and usually arrives inside content the user
        // did not author — an attached file's bytes land in `Resource.text`.
        // Rejecting made such a message permanently unsendable; the prompt must
        // go through with the character removed from EVERY copy.
        use crate::db::test_helpers;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/route-separator").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-route-separator";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/route-separator")),
        )
        .await;

        mgr.send_prompt_linked_with_message_id(
            &db,
            conn_id,
            vec![
                PromptInputBlock::Text {
                    text: "user\u{001e}frame".into(),
                },
                PromptInputBlock::Resource {
                    uri: "file:///tmp/records.dat".into(),
                    mime_type: Some("text/plain".into()),
                    text: Some("row-a\u{001e}row-b".into()),
                    blob: None,
                },
            ],
            Some(folder_id),
            None,
            None,
            Some("optimistic-reserved".into()),
        )
        .await
        .expect("an invisible control character must not block the send");

        let ConnectionCommand::Prompt {
            blocks,
            user_message,
            ..
        } = cmd_rx.try_recv().expect("the prompt still reaches the agent")
        else {
            panic!("expected prompt command");
        };
        assert!(matches!(
            blocks.as_slice(),
            [
                PromptInputBlock::Text { text },
                PromptInputBlock::Resource { text: Some(resource), .. },
            ] if text == "userframe" && resource == "row-arow-b"
        ));
        // The broadcast copy is projected from the SAME scrubbed blocks, so the
        // stored, displayed, and on-the-wire messages cannot drift apart.
        let (_, user_blocks) = user_message.expect("root prompt is broadcast");
        assert!(matches!(
            user_blocks.first(),
            Some(crate::acp::types::UserMessageBlock::Text { text }) if text == "userframe"
        ));
    }

    #[tokio::test]
    async fn send_prompt_linked_preserves_history_when_the_session_was_re_minted() {
        // codeg#500, end to end, in the exact shape the reporter described:
        // an existing completed conversation, then a new session started in the
        // same folder and pane.
        //
        // The connection was spawned with `session_id = None` (a reconnect that
        // lost the id, or an unclassified `session/load` failure that fell
        // through to `session/new`), so it holds S2 while the row the UI still
        // points at holds S1. Branch A adopts that row and persists the session
        // id onto it. Before the guard, S1 was left with no row at all — and
        // since the conversation list is built purely from DB rows, the user's
        // 49-minute conversation simply vanished from the sidebar.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/reminted").await;
        let existing = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("nouveauté de cette version".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, existing.id, "S1", &[])
            .await
            .unwrap();
        conversation_service::update_status(
            &db.conn,
            existing.id,
            ConversationStatus::PendingReview,
        )
        .await
        .unwrap();

        let mgr = ConnectionManager::new();
        let conn_id = "conn-reminted";
        let _cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/reminted")),
        )
        .await;
        // The connection came up on a session of its own — nobody told it about
        // S1, which is the whole problem.
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .external_id = Some("S2".into());

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "tu peux faire 2 couleur".into(),
            }],
            Some(folder_id),
            Some(existing.id),
            None,
        )
        .await
        .expect("the prompt itself must still go through");

        // The row the user has open follows the live session...
        let current = conversation_service::get_by_id(&db.conn, existing.id)
            .await
            .expect("current row");
        assert_eq!(current.external_id.as_deref(), Some("S2"));

        // ...and S1 is still indexed, under the title the user recognises, so
        // it remains visible and reopenable from the sidebar.
        let listed = crate::commands::conversations::list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            crate::commands::conversations::ListAllConversationsOptions::default(),
        )
        .await
        .expect("list");
        let preserved = listed
            .iter()
            .find(|c| c.external_id.as_deref() == Some("S1"))
            .expect("the previous session must remain in the conversation index");
        assert_eq!(
            preserved.title.as_deref(),
            Some("nouveauté de cette version")
        );
        assert_ne!(
            preserved.id, existing.id,
            "the preserved history lives on its own row"
        );
        assert_eq!(listed.len(), 2, "exactly two conversations, got {listed:?}");
    }

    #[tokio::test]
    async fn send_prompt_linked_refuses_a_session_owned_by_another_conversation() {
        // The connection holds a session that ALREADY belongs to a different
        // row, so no bind can make this row own it.
        //
        // The retry is the whole point of the test. `ConversationLinked` is
        // applied to `SessionState`, and `already_linked` is read from exactly
        // that field — so announcing the link before the bind meant the first
        // call failed with the link latched, and the SECOND call skipped the
        // entire link/bind block and dispatched the turn into a session this
        // row does not own. Both calls must fail identically, and neither may
        // enqueue a prompt.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/conflict").await;

        let holder = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("owns the session".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
            .await
            .unwrap();
        let target = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::Codex,
            Some("the row the user is looking at".into()),
            None,
        )
        .await
        .unwrap();

        let mgr = ConnectionManager::new();
        let conn_id = "conn-conflict";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/conflict")),
        )
        .await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .external_id = Some("S_SHARED".into());

        for attempt in 1..=2 {
            let err = mgr
                .send_prompt_linked(
                    &db,
                    conn_id,
                    vec![PromptInputBlock::Text {
                        text: "should never reach the agent".into(),
                    }],
                    Some(folder_id),
                    Some(target.id),
                    None,
                )
                .await
                .expect_err("a session owned by another row must be refused");
            assert!(
                err.to_string().contains("already bound"),
                "attempt {attempt} must name the conflict, got {err}"
            );
            assert!(
                mgr.get_state(conn_id)
                    .await
                    .unwrap()
                    .read()
                    .await
                    .conversation_id
                    .is_none(),
                "attempt {attempt} must leave the connection UNLINKED, or the next \
                 prompt takes the already_linked fast path and skips the bind"
            );
            assert!(
                drain_prompt_user_messages(&mut cmd_rx).is_empty(),
                "attempt {attempt} must not enqueue a prompt"
            );
        }

        // Neither row moved, and no split row was manufactured.
        assert_eq!(
            conversation_service::get_by_id(&db.conn, target.id)
                .await
                .expect("target")
                .external_id,
            None
        );
        assert_eq!(
            conversation_service::get_by_id(&db.conn, holder.id)
                .await
                .expect("holder")
                .external_id
                .as_deref(),
            Some("S_SHARED")
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_drops_the_row_it_minted_when_the_bind_is_refused() {
        // Branch B mints the row before the bind can be attempted. Since the
        // announce now happens AFTER the bind, a refused one leaves a row no
        // client was ever told about — invisible until the sidebar's next full
        // refetch surfaces it as an empty untitled conversation. Every retry
        // would add another.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/conflict-b").await;

        let holder =
            conversation_service::create(&db.conn, folder_id, AgentType::Codex, None, None)
                .await
                .unwrap();
        conversation_service::bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
            .await
            .unwrap();

        let mgr = ConnectionManager::new();
        let conn_id = "conn-conflict-b";
        let _cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/conflict-b")),
        )
        .await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .external_id = Some("S_SHARED".into());

        for _ in 0..2 {
            mgr.send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text { text: "hi".into() }],
                Some(folder_id),
                // No conversation_id — Branch B.
                None,
                None,
            )
            .await
            .expect_err("must be refused");
        }

        let listed = crate::commands::conversations::list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            crate::commands::conversations::ListAllConversationsOptions::default(),
        )
        .await
        .expect("list");
        assert_eq!(
            listed.len(),
            1,
            "only the pre-existing holder may remain, got {listed:?}"
        );
        assert_eq!(listed[0].id, holder.id);
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_second_prompt_while_turn_in_flight() {
        // Two clients co-controlling one connection can send near-
        // simultaneously. The first accepted prompt marks the turn in flight;
        // the second must be REJECTED with TurnInProgress (not enqueued behind
        // the active turn and silently dropped by the loop) so the frontend can
        // re-queue it. Only one Prompt reaches the connection loop.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-gate").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-gate";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-gate")),
        )
        .await;

        let first = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "first".into(),
                }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(first.is_ok(), "first prompt accepted");

        let second = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "second".into(),
                }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(
            matches!(second, Err(AcpError::TurnInProgress)),
            "second concurrent prompt must be rejected with TurnInProgress, got {second:?}"
        );

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(
            prompts.len(),
            1,
            "only the first prompt reaches the loop; the second is rejected, not queued"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_empty_prompt_without_wedging_gate() {
        // An empty prompt is rejected BEFORE any side effects: it must NOT
        // create/link a conversation row, must NOT set the concurrency gate
        // (which — with no TurnComplete to clear it — would 409 every future
        // send), and the connection must stay usable for a real prompt.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-empty").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-empty";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-empty")),
        )
        .await;

        let rows_before = count_conversation_rows(&db).await;
        let empty = mgr
            .send_prompt_linked(&db, conn_id, vec![], Some(folder_id), None, None)
            .await;
        assert!(empty.is_err(), "an empty prompt must be rejected");
        assert_eq!(
            count_conversation_rows(&db).await,
            rows_before,
            "a rejected empty prompt must NOT create/link a conversation row"
        );
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "a rejected empty prompt must NOT set the concurrency gate"
        );

        // The connection is not wedged: a real prompt afterwards is accepted.
        let ok = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text { text: "hi".into() }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(
            ok.is_ok(),
            "a real prompt after an empty one must still be accepted"
        );
        assert_eq!(
            drain_prompt_user_messages(&mut cmd_rx).len(),
            1,
            "exactly the one real prompt reached the loop"
        );
    }

    #[tokio::test]
    async fn send_prompt_returns_turn_in_progress_when_busy() {
        // The non-linked `send_prompt` path (used by the chat channel) must
        // surface `TurnInProgress` — NOT a connection-loss error — when a turn
        // is already in flight, so the chat channel treats it as a transient
        // busy rejection instead of tearing down the session.
        let mgr = ConnectionManager::new();
        let conn_id = "conn-busy";
        let _rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .turn_in_flight = true;

        let res = mgr
            .send_prompt(conn_id, vec![PromptInputBlock::Text { text: "hi".into() }])
            .await;
        assert!(
            matches!(res, Err(AcpError::TurnInProgress)),
            "send_prompt must return TurnInProgress when a turn is in flight, got {res:?}"
        );
    }

    #[tokio::test]
    async fn fork_session_rejects_when_turn_in_flight() {
        // A fork re-points the live session; it must not run while a turn is in
        // flight (a racing send would route to the wrong session, and the Fork
        // command would be dropped by the in-turn loop). It rejects with
        // TurnInProgress so the caller re-queues, WITHOUT enqueuing a Fork.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-fork-busy";
        let mut cmd_rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            let mut s = state.write().await;
            s.conversation_id = Some(7); // fork requires a linked row
            s.turn_in_flight = true; // a turn is already running
        }

        let res = mgr.fork_session(&db, conn_id, None, None, None).await;
        assert!(
            matches!(res, Err(AcpError::TurnInProgress)),
            "fork must reject with TurnInProgress while a turn is in flight, got {res:?}"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "a rejected fork must NOT enqueue a Fork command"
        );
    }

    #[tokio::test]
    async fn fork_session_failure_leaves_gate_clear_and_lock_free() {
        // A fork holds `prompt_lock` for its whole critical section and never
        // SETS `turn_in_flight`, so even when the fork FAILS (here: a dead
        // command receiver makes the `Fork` send error) the connection isn't
        // wedged — the gate stays clear and the prompt lock is released on the
        // error path. (A fork emits no TurnComplete, so a gate it had set would
        // have had nothing to clear it.)
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-fork-fail";
        // insert_fake_connection drops the cmd receiver → the Fork send fails.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        mgr.get_state(conn_id)
            .await
            .unwrap()
            .write()
            .await
            .conversation_id = Some(9);

        let res = mgr.fork_session(&db, conn_id, None, None, None).await;
        assert!(res.is_err(), "fork with a dead receiver must fail");
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "a failed fork must leave the gate clear"
        );
        let lock = mgr.clone_prompt_lock(conn_id).await.unwrap();
        assert!(
            lock.try_lock().is_ok(),
            "a failed fork must release prompt_lock so the connection stays usable"
        );
    }

    #[tokio::test]
    async fn fork_persists_despite_caller_cancellation() {
        // Cancellation-shield regression. Once `fork_session` enqueues the `Fork`
        // command, the connection loop re-points the live session to S2 and emits
        // `SessionStarted{S2}` REGARDLESS of caller liveness (it ignores a dead
        // reply channel). So the DB persistence that records the two-row layout
        // must NOT be tied to the caller's future — a dropped caller (HTTP client
        // disconnect) must not strand the live session on S2 with the pre-fork S1
        // history orphaned. We drop the caller mid-fork (reply withheld), then
        // release the reply and assert the detached task STILL persists the
        // current row (→ S2, `[Fork]` title) and the sibling (→ S1).
        use crate::acp::connection::ConnectionCommand;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-shield").await;
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();

        // Connection with a GATED fake fork reply: withheld until `go_tx` fires,
        // so we can drop the caller before the reply (and thus the persistence).
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
        let mut state = SessionState::new(
            "c-shield".to_string(),
            AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = Some(pre.id);
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: "c-shield".to_string(),
            agent_type: AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = ConnectionManager::new();
        mgr.connections
            .lock()
            .await
            .insert("c-shield".to_string(), conn);

        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let fake_loop = tokio::spawn(async move {
            if let Some(ConnectionCommand::Fork { reply, .. }) = rx.recv().await {
                go_rx.await.ok(); // withhold the reply until the test releases it
                let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                    forked_session_id: "session-S2".into(),
                    original_session_id: "session-S1".into(),
                }));
            }
            rx // keep the receiver alive
        });

        // Drive fork under a short timeout: it spawns the shielded task (which
        // enqueues `Fork` and blocks on the withheld reply), then the timeout
        // DROPS this caller future. The detached persistence task must survive.
        let timed = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mgr.fork_session(&db, "c-shield", None, None, None),
        )
        .await;
        assert!(
            timed.is_err(),
            "caller must be dropped before the withheld reply is delivered"
        );

        // Nothing persisted yet (reply still withheld) — the row is untouched.
        let mid = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            mid.external_id.as_deref(),
            Some("session-S1"),
            "fork must not persist before the protocol reply"
        );

        // Release the reply: the DETACHED task completes the persistence even
        // though the caller is long gone.
        go_tx.send(()).ok();
        let _ = fake_loop.await;

        // Poll (bounded) until the two-row layout appears.
        let mut persisted = false;
        for _ in 0..200 {
            let current = conversation_service::get_by_id(&db.conn, pre.id)
                .await
                .unwrap();
            let rows = conversation::Entity::find().all(&db.conn).await.unwrap();
            let has_sibling = rows
                .iter()
                .any(|r| r.id != pre.id && r.external_id.as_deref() == Some("session-S1"));
            if current.external_id.as_deref() == Some("session-S2")
                && current.title.as_deref() == Some("[Fork] Topic")
                && has_sibling
            {
                persisted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            persisted,
            "fork persistence must complete despite caller cancellation"
        );
    }

    #[tokio::test]
    async fn send_prompt_inner_does_not_set_gate_while_blocked_on_capacity() {
        // Cancellation-safety: the gate is set only AFTER reserving channel
        // capacity, with no await between the set and the send. If the future is
        // dropped while awaiting capacity (channel full), `turn_in_flight` must
        // remain false — otherwise a cancelled send would wedge the connection.
        let mgr = ConnectionManager::new();
        let conn_id = "conn-cancel";
        let _rx = insert_live_connection(&mgr, conn_id, AgentType::ClaudeCode, None).await;
        // Fill the command channel to capacity (4, per insert_live_connection)
        // by sending DIRECTLY on the cloned sender — bypassing the gate — so the
        // next reserve() blocks.
        let tx = mgr
            .connections
            .lock()
            .await
            .get(conn_id)
            .unwrap()
            .cmd_tx
            .clone();
        for _ in 0..4 {
            tx.send(crate::acp::connection::ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "filler".into(),
                }],
                user_message: None,
            })
            .await
            .unwrap();
        }

        // send_prompt_inner now blocks on reserve(); drop it via a short timeout.
        let fut = mgr.send_prompt_inner(
            conn_id,
            vec![PromptInputBlock::Text {
                text: "blocked".into(),
            }],
            None,
        );
        let res = tokio::time::timeout(std::time::Duration::from_millis(50), fut).await;
        assert!(
            res.is_err(),
            "send_prompt_inner should still be blocked on channel capacity"
        );
        assert!(
            !mgr.get_state(conn_id)
                .await
                .unwrap()
                .read()
                .await
                .turn_in_flight,
            "the gate must NOT be set while blocked on channel capacity (cancellation-safe)"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_uses_client_message_id_for_user_message() {
        // The UI threads its optimistic turn id as `client_message_id`; the
        // broadcast UserMessage must carry it verbatim so the sender dedups its
        // own echo by exact id (not a heuristic).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-cmid").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-cmid";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-cmid")),
        )
        .await;

        mgr.send_prompt_linked_with_message_id(
            &db,
            conn_id,
            vec![PromptInputBlock::Text { text: "hi".into() }],
            Some(folder_id),
            None,
            None,
            Some("optimistic-abc".to_string()),
        )
        .await
        .expect("send");

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(
            prompts
                .first()
                .and_then(|um| um.as_ref())
                .map(|(id, _)| id.as_str()),
            Some("optimistic-abc"),
            "Prompt's user_message must carry the client-supplied message_id verbatim"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_failed_reserve_leaves_gate_clear() {
        // A failed enqueue (dropped cmd receiver) fails at the channel
        // `reserve()` step — which is BEFORE the turn-in-flight gate is set — so
        // the gate is never set, not "rolled back". The connection must stay
        // usable (turn_in_flight false), and the row rolls back to Cancelled.
        // pending_user_message stays None (the loop, which never ran, owns it).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-fail").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-fail";
        // insert_fake_connection drops the cmd receiver → send_prompt_inner fails.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/um-fail")),
            EventEmitter::Noop,
        )
        .await;

        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "never enqueued".into(),
                }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(result.is_err(), "a dropped receiver must fail the enqueue");

        let state = mgr.get_state(conn_id).await.unwrap();
        let snap = state.read().await;
        assert!(
            !snap.turn_in_flight,
            "a failed enqueue must roll back turn_in_flight so the connection isn't wedged"
        );
        let pending = snap.pending_user_message.clone();
        assert!(
            pending.is_none(),
            "a failed enqueue must not strand pending_user_message"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_skips_user_message_for_delegation_child() {
        // Delegation children surface their kickoff prompt via a separate path;
        // send_prompt_linked must NOT broadcast a user_message (or capture
        // pending) for them, so the sub-agent viewer doesn't double-render.
        use crate::acp::delegation::spawner::DelegationLink;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/um-deleg").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("parent");
        let mgr = ConnectionManager::new();
        let conn_id = "conn-um-deleg";
        let mut cmd_rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::Codex,
            Some(PathBuf::from("/tmp/um-deleg")),
        )
        .await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "child kickoff".into(),
            }],
            Some(folder_id),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("delegation kickoff enqueues");

        let prompts = drain_prompt_user_messages(&mut cmd_rx);
        assert_eq!(prompts.len(), 1, "the kickoff prompt is enqueued");
        assert!(
            prompts[0].is_none(),
            "delegation child Prompt must carry NO user_message (kickoff is surfaced separately)"
        );
        let pending = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .pending_user_message
            .clone();
        assert!(
            pending.is_none(),
            "delegation child must not capture pending_user_message"
        );
    }

    #[test]
    fn user_prompt_text_preview_joins_and_trims_text_blocks() {
        let blocks = vec![
            PromptInputBlock::Text {
                text: "  hello  ".into(),
            },
            PromptInputBlock::Text {
                text: "world".into(),
            },
        ];
        assert_eq!(
            user_prompt_text_preview(&blocks).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn user_prompt_text_preview_is_none_for_empty_or_textless() {
        assert!(user_prompt_text_preview(&[]).is_none());
        assert!(
            user_prompt_text_preview(&[PromptInputBlock::Text { text: "   ".into() }]).is_none()
        );
        let img = vec![PromptInputBlock::Image {
            data: "x".into(),
            mime_type: "image/png".into(),
            uri: None,
        }];
        assert!(user_prompt_text_preview(&img).is_none());
    }

    #[test]
    fn user_prompt_text_preview_truncates_long_input() {
        let long = "a".repeat(USER_PROMPT_PREVIEW_MAX_CHARS + 50);
        let preview = user_prompt_text_preview(&[PromptInputBlock::Text { text: long }]).unwrap();
        // truncate_str keeps MAX chars then appends a 3-char "..." marker.
        assert_eq!(preview.chars().count(), USER_PROMPT_PREVIEW_MAX_CHARS + 3);
        assert!(preview.ends_with("..."));
    }

    /// The chat-channel handle is installed once at bootstrap on the ONE
    /// manager Tauri/axum owns, but every consumer (the web AppState at
    /// `web/mod.rs`, the lifecycle worker) holds a `clone_ref` of it. The
    /// handle therefore has to live behind the shared `Arc<OnceLock>` — if a
    /// clone ever got a fresh lock, live title writes would stop renaming
    /// bound Telegram topics on exactly the paths that do the writing, with
    /// no error anywhere.
    #[test]
    fn installed_chat_channel_survives_clone_ref() {
        let mgr = ConnectionManager::new();
        assert!(mgr.chat_channel().is_none(), "unset before bootstrap");

        // Install on a clone: the handle must become visible on the original
        // too (bootstrap order across `clone_ref` boundaries is not fixed).
        let clone = mgr.clone_ref();
        clone.install_chat_channel(crate::chat_channel::manager::ChatChannelManager::new());

        assert!(mgr.chat_channel().is_some());
        assert!(mgr.clone_ref().chat_channel().is_some());
    }

    #[test]
    fn delegation_child_title_seed_uses_parser_title_from_first_prompt() {
        // The delegating prompt is a single text block (the task) — the seed must
        // equal what the parser's `title_from_user_text` produces from it, so a
        // later `refresh_auto_title` over the same first turn is a no-op.
        let task = "Review the auth module for race conditions";
        let blocks = vec![PromptInputBlock::Text { text: task.into() }];
        assert_eq!(
            delegation_child_title_seed(&blocks),
            Some(crate::parsers::title_from_user_text(task))
        );
    }

    #[test]
    fn delegation_child_title_seed_is_none_for_textless_prompt() {
        // Empty / whitespace / image-only prompts seed no title (stays NULL,
        // backfilled on first detail load as before).
        assert!(delegation_child_title_seed(&[]).is_none());
        assert!(
            delegation_child_title_seed(&[PromptInputBlock::Text { text: "  \n ".into() }])
                .is_none()
        );
        let img = vec![PromptInputBlock::Image {
            data: "x".into(),
            mime_type: "image/png".into(),
            uri: None,
        }];
        assert!(delegation_child_title_seed(&img).is_none());
    }

    /// A message that is one dropped-in file and no prose. The row would read
    /// "Untitled" forever otherwise — for an agent with no store parser, ACP
    /// has no title channel and the history parse honestly reports none. The
    /// value is a SEED: it reaches `seed_auto_title_if_empty` / a fresh row's
    /// `title`, never `refresh_auto_title`, so a name the agent publishes over
    /// `session_info_update` still wins.
    #[test]
    fn delegation_child_title_seed_names_an_attachment_only_prompt() {
        let blocks = vec![
            PromptInputBlock::ResourceLink {
                uri: "file:///tmp/report.pdf".into(),
                name: "report.pdf".into(),
                mime_type: None,
                description: None,
            },
            PromptInputBlock::Resource {
                uri: "clipboard://my%20notes.txt-9f2".into(),
                mime_type: Some("text/plain".into()),
                text: Some("body".into()),
                blob: None,
            },
        ];
        assert_eq!(
            delegation_child_title_seed(&blocks).as_deref(),
            Some("report.pdf, my notes.txt-9f2")
        );

        // Prose still wins outright, attachments and all — and matches what
        // `refresh_auto_title` will later compute, so the row doesn't churn.
        let mut with_prose = blocks.clone();
        with_prose.insert(
            0,
            PromptInputBlock::Text {
                text: "Review these".into(),
            },
        );
        assert_eq!(
            delegation_child_title_seed(&with_prose),
            Some(crate::parsers::title_from_user_text("Review these"))
        );
    }

    #[test]
    fn delegation_child_title_seed_caps_long_task_text() {
        // Mirrors the parser cap (100 chars) so an over-long task doesn't store a
        // runaway title; `title_from_user_text` keeps 100 then appends "...".
        let long = "x".repeat(250);
        let seed = delegation_child_title_seed(&[PromptInputBlock::Text { text: long }]).unwrap();
        assert_eq!(seed.chars().count(), 103);
        assert!(seed.ends_with("..."));
    }

    /// A successful UI send (delegation = None, text present) emits
    /// `UserPromptSent` carrying the message preview, after the link + status
    /// events.
    #[tokio::test]
    async fn send_prompt_linked_emits_user_prompt_sent_on_success() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/ups").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-ups-1";
        let _rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/ups")),
        )
        .await;
        let mut stream = subscribe_conn_stream(&mgr, conn_id).await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "hello world".into(),
            }],
            Some(folder_id),
            None,
            None,
        )
        .await
        .expect("send should succeed with a live receiver");

        let mut found = None;
        for _ in 0..5 {
            let env = recv_first_acp_event(&mut stream).await;
            if let AcpEvent::UserPromptSent { text_preview } = env.payload {
                found = Some(text_preview);
                break;
            }
        }
        assert_eq!(found.as_deref(), Some("hello world"));
    }

    #[tokio::test]
    async fn send_prompt_linked_seeds_first_prompt_title_when_untitled() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/title-seed").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-title-seed";
        let _rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/title-seed")),
        )
        .await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Text {
                text: "hello world".into(),
            }],
            Some(folder_id),
            None,
            None,
        )
        .await
        .expect("send");

        let cid = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .conversation_id
            .expect("row linked");
        let row = conversation_service::get_by_id(&db.conn, cid)
            .await
            .expect("get");
        assert_eq!(row.title.as_deref(), Some("hello world"));
        assert!(
            !row.title_locked,
            "first-prompt seed must stay unlocked so a later ACP title can replace it"
        );
    }

    /// A textless prompt (image-only) succeeds but emits NO `UserPromptSent` —
    /// the notification fires for text messages only.
    #[tokio::test]
    async fn send_prompt_linked_skips_user_prompt_sent_for_textless_prompt() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/ups2").await;
        let mgr = ConnectionManager::new();
        let conn_id = "conn-ups-2";
        let _rx = insert_live_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/ups2")),
        )
        .await;
        let mut stream = subscribe_conn_stream(&mgr, conn_id).await;

        mgr.send_prompt_linked(
            &db,
            conn_id,
            vec![PromptInputBlock::Image {
                data: "deadbeef".into(),
                mime_type: "image/png".into(),
                uri: None,
            }],
            Some(folder_id),
            None,
            None,
        )
        .await
        .expect("send should succeed with a live receiver");

        let mut saw_user_prompt = false;
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), stream.recv()).await {
                Ok(Ok(env)) => {
                    if matches!(env.payload, AcpEvent::UserPromptSent { .. }) {
                        saw_user_prompt = true;
                    }
                }
                _ => break,
            }
        }
        assert!(
            !saw_user_prompt,
            "a textless (image-only) prompt must not emit UserPromptSent"
        );
    }

    #[tokio::test]
    async fn get_state_returns_arc_for_known_connection() {
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection("c1", None));
        }
        let state = mgr.get_state("c1").await.expect("state should be found");
        assert_eq!(state.read().await.connection_id, "c1");
    }

    #[tokio::test]
    async fn get_state_returns_none_for_unknown_connection() {
        let mgr = ConnectionManager::new();
        assert!(mgr.get_state("does-not-exist").await.is_none());
    }

    #[tokio::test]
    async fn find_connection_by_conversation_id_matches_when_bound() {
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection("c1", Some(42)));
            map.insert("c2".to_string(), fake_connection("c2", None));
        }
        let found = mgr
            .find_connection_by_conversation_id(42)
            .await
            .expect("should find c1");
        assert_eq!(found, "c1");
        assert!(mgr.find_connection_by_conversation_id(999).await.is_none());
    }

    #[tokio::test]
    async fn send_prompt_linked_creates_conversation_on_first_call_only() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test").await;

        let mgr = ConnectionManager::new();
        let conn_id = "c1";
        {
            let mut map = mgr.connections.lock().await;
            // Note: cmd_tx receiver is dropped, so send_prompt's mpsc.send will fail
            // with ProcessExited. That's fine — we only verify the linkage side
            // effect, not the actual prompt forwarding.
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }

        // First call: creates conversation row, sets state.conversation_id.
        // The mpsc send error after linking is expected and ignored here.
        let _ = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
            .await;
        let snap = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .to_snapshot();
        assert!(
            snap.conversation_id.is_some(),
            "conversation_id should be set"
        );
        assert_eq!(snap.folder_id, Some(folder_id));
        let first_id = snap.conversation_id.unwrap();

        // Second call: ignores folder_id, does NOT create another row.
        let _ = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
            .await;
        let snap2 = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .to_snapshot();
        assert_eq!(snap2.conversation_id, Some(first_id));
    }

    #[tokio::test]
    async fn send_prompt_linked_errors_when_no_folder_id() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let conn_id = "c1";
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }
        let result = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), None, None, None)
            .await;
        assert!(
            result.is_err(),
            "should error when folder_id is not provided for a new conversation row"
        );
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("folder_id"),
            "error should mention missing folder_id, got: {err_str}"
        );
    }

    /// Count of `conversation` rows (ignoring soft-delete) — used by the
    /// caller-supplied conversation_id tests to assert no new row was created.
    async fn count_conversation_rows(db: &crate::db::AppDatabase) -> usize {
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        conversation::Entity::find()
            .all(&db.conn)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn send_prompt_linked_uses_caller_conversation_id_when_provided() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/caller-id").await;
        // Pre-create a conversation row the caller will reference.
        let pre_existing =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-caller-id";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/caller-id")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // Count rows before
        let before = count_conversation_rows(&db).await;

        // Send with caller-supplied conversation_id + folder_id.
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre_existing.id),
                None,
            )
            .await;

        // No new conversation row was created.
        let after = count_conversation_rows(&db).await;
        assert_eq!(after, before, "no new row should be created");

        // State now has the caller-supplied conversation_id.
        let state = mgr.get_state(conn_id).await.unwrap();
        assert_eq!(state.read().await.conversation_id, Some(pre_existing.id));

        // ConversationLinked event was emitted with the caller's id.
        let env = recv_first_acp_event(&mut rx).await;
        match env.payload {
            AcpEvent::ConversationLinked {
                conversation_id,
                folder_id: emitted_folder,
                ..
            } => {
                assert_eq!(conversation_id, pre_existing.id);
                assert_eq!(emitted_folder, folder_id);
            }
            other => panic!("expected ConversationLinked, got {other:?}"),
        }
    }

    /// Drain the global broadcaster and report whether a `conversation://changed`
    /// upsert for `id` carrying `external_id` was emitted.
    fn drain_has_upsert_with_external_id(
        rx: &mut broadcast::Receiver<WebEvent>,
        id: i32,
        external_id: &str,
    ) -> bool {
        while let Ok(evt) = rx.try_recv() {
            if evt.channel != crate::web::event_bridge::CONVERSATION_CHANGED_EVENT {
                continue;
            }
            let p = &*evt.payload;
            if p["kind"] == "upsert"
                && p["summary"]["id"] == id
                && p["summary"]["external_id"] == external_id
            {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn send_prompt_linked_session_started_before_link_broadcasts_external_id_branch_b() {
        // SessionStarted-before-link: external_id is already on the live state
        // but no conversation_id yet, so the lifecycle subscriber skipped its
        // broadcast. The synchronous external_id persist inside
        // send_prompt_linked (backend-create Branch B) must itself emit a
        // corrective `conversation://changed` upsert so other clients converge.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/sess-pre-b").await;
        let mgr = ConnectionManager::new();
        let (broadcaster, mut rx) = make_test_broadcaster();
        let conn_id = "conn-sess-pre-b";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/sess-pre-b")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.external_id = Some("ext-pre".to_string());
        }

        // cmd_tx receiver is dropped → the prompt send fails after linking, but
        // the link + external_id persist + broadcast already happened.
        let _ = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
            .await;

        let cid = mgr
            .get_state(conn_id)
            .await
            .unwrap()
            .read()
            .await
            .conversation_id
            .expect("conversation should be linked");
        let row = conversation_service::get_by_id(&db.conn, cid)
            .await
            .unwrap();
        assert_eq!(row.external_id.as_deref(), Some("ext-pre"));
        assert!(
            drain_has_upsert_with_external_id(&mut rx, cid, "ext-pre"),
            "Branch B must broadcast a conversation://changed upsert carrying external_id"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_session_started_before_link_broadcasts_external_id_branch_a() {
        // Same precondition, caller-supplied conversation_id (adopt Branch A).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/sess-pre-a").await;
        let pre =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let mgr = ConnectionManager::new();
        let (broadcaster, mut rx) = make_test_broadcaster();
        let conn_id = "conn-sess-pre-a";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/sess-pre-a")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.external_id = Some("ext-pre-a".to_string());
        }

        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre.id),
                None,
            )
            .await;

        let row = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(row.external_id.as_deref(), Some("ext-pre-a"));
        assert!(
            drain_has_upsert_with_external_id(&mut rx, pre.id, "ext-pre-a"),
            "Branch A must broadcast a conversation://changed upsert carrying external_id"
        );
    }

    #[tokio::test]
    async fn send_prompt_linked_rejects_conversation_id_without_folder_id() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-bad-args";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/x")),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;

        let err = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), None, Some(42), None)
            .await
            .expect_err("should reject conversation_id without folder_id");
        assert!(matches!(err, AcpError::Protocol(_)));
    }

    #[tokio::test]
    async fn send_prompt_linked_caller_id_is_noop_when_already_linked() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/already").await;
        let pre =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-already";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/already")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        // Pre-link the connection state.
        {
            let state = mgr.get_state(conn_id).await.unwrap();
            state.write().await.conversation_id = Some(pre.id);
        }
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        let before = count_conversation_rows(&db).await;
        let _ = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                one_text_block(),
                Some(folder_id),
                Some(pre.id),
                None,
            )
            .await;
        let after = count_conversation_rows(&db).await;
        assert_eq!(after, before);

        // No ConversationLinked event was emitted (already linked). The
        // centralized status transition fires InProgress; then because the
        // dropped cmd_tx receiver makes `send_prompt_inner` return
        // ProcessExited, the rollback path fires Cancelled. Two events,
        // strictly ordered.
        let env_in_progress = recv_first_acp_event(&mut rx).await;
        match env_in_progress.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, pre.id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("first event must be ConversationStatusChanged(InProgress), got {other:?}")
            }
        }
        let env_cancelled = recv_first_acp_event(&mut rx).await;
        match env_cancelled.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, pre.id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => panic!(
                "second event must be ConversationStatusChanged(Cancelled) after send failure, got {other:?}"
            ),
        }
    }

    // ---------- Phase: status centralization ----------

    #[tokio::test]
    async fn send_prompt_linked_writes_in_progress_and_emits_event() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/status").await;

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-status-1";
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/status")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // First call: backend creates the conversation row and links it.
        // The cmd_tx receiver in `insert_fake_connection` has been dropped,
        // so `send_prompt_inner` returns ProcessExited — exercising the new
        // Cancelled-rollback path. We expect THREE events in order:
        //   1. ConversationLinked
        //   2. ConversationStatusChanged(InProgress)  [pre-send write]
        //   3. ConversationStatusChanged(Cancelled)   [rollback after send failure]
        let _ = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
            .await;

        let env1 = recv_first_acp_event(&mut rx).await;
        let conv_id = match env1.payload {
            AcpEvent::ConversationLinked {
                conversation_id,
                folder_id: emitted_folder,
                ..
            } => {
                assert_eq!(emitted_folder, folder_id);
                conversation_id
            }
            other => panic!("first event must be ConversationLinked, got {other:?}"),
        };
        let env2 = recv_first_acp_event(&mut rx).await;
        match env2.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("second event must be ConversationStatusChanged(InProgress), got {other:?}")
            }
        }
        let env3 = recv_first_acp_event(&mut rx).await;
        match env3.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => panic!(
                "third event must be ConversationStatusChanged(Cancelled) on send failure, got {other:?}"
            ),
        }
        // Ordering invariant: ConversationLinked < InProgress < Cancelled.
        assert!(
            env2.seq > env1.seq && env3.seq > env2.seq,
            "event seqs must be strictly monotonic: linked={} in_progress={} cancelled={}",
            env1.seq,
            env2.seq,
            env3.seq
        );

        // DB row settles at Cancelled (the rollback after send failure). The
        // intermediate InProgress write is observable only via the event,
        // not by the time the test reads the row.
        let row = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists");
        assert_eq!(row.status, ConversationStatus::Cancelled);

        // Second send: already-linked path also writes + emits InProgress
        // and then Cancelled (same send-failure rollback). Pre-flip the row
        // to PendingReview to observe the transition flip forward — mirrors
        // the "follow-up turn after a TurnComplete" scenario.
        conversation_service::update_status(&db.conn, conv_id, ConversationStatus::PendingReview)
            .await
            .unwrap();

        let _ = mgr
            .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
            .await;

        let env4 = recv_first_acp_event(&mut rx).await;
        match env4.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => panic!(
                "second send must re-emit ConversationStatusChanged(InProgress) first, got {other:?}"
            ),
        }
        let env5 = recv_first_acp_event(&mut rx).await;
        match env5.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(status, ConversationStatus::Cancelled);
            }
            other => {
                panic!("second send must rollback to Cancelled after send failure, got {other:?}")
            }
        }
        let row2 = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row2.status, ConversationStatus::Cancelled);
    }

    // ---------- Phase: connection dedup ----------

    #[tokio::test]
    async fn find_connection_for_reuse_returns_none_when_session_id_is_none() {
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        // Insert a connection that *would* match if session_id were Some.
        let id = "c1";
        insert_fake_connection(
            &mgr,
            id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/reuse")),
            EventEmitter::test_web_only(broadcaster),
        )
        .await;
        {
            let state = mgr.get_state(id).await.unwrap();
            state.write().await.external_id = Some("ext-1".into());
        }
        let found = mgr
            .find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&PathBuf::from("/tmp/reuse")),
                None,
            )
            .await;
        assert!(
            found.is_none(),
            "no session_id means we never dedup speculative connects"
        );
    }

    #[tokio::test]
    async fn spawn_agent_reuses_existing_connection_when_session_id_matches() {
        // Direct unit test for the lookup helper that spawn_agent calls
        // before its (process-spawning) block. We test the helper directly so
        // the test never tries to launch an agent process.
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let existing_id = "preexisting-conn";
        let working_dir = PathBuf::from("/tmp/reuse-match");
        insert_fake_connection(
            &mgr,
            existing_id,
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state(existing_id).await.unwrap();
            let mut s = state.write().await;
            s.external_id = Some("ext-1".into());
            s.status = ConnectionStatus::Connected;
        }

        // Same session_id + same agent + same working_dir -> reuse.
        let found = mgr
            .find_connection_for_reuse(AgentType::ClaudeCode, Some(&working_dir), Some("ext-1"))
            .await;
        assert_eq!(found.as_deref(), Some(existing_id));

        // Different session_id -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(AgentType::ClaudeCode, Some(&working_dir), Some("other-ext"))
            .await
            .is_none());

        // Different working_dir -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&PathBuf::from("/tmp/different")),
                Some("ext-1")
            )
            .await
            .is_none());

        // Different agent_type -> no reuse.
        assert!(mgr
            .find_connection_for_reuse(AgentType::Codex, Some(&working_dir), Some("ext-1"))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn find_connection_for_reuse_skips_disconnected_or_errored() {
        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let working_dir = PathBuf::from("/tmp/torn-down");
        insert_fake_connection(
            &mgr,
            "torn",
            AgentType::ClaudeCode,
            Some(working_dir.clone()),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        {
            let state = mgr.get_state("torn").await.unwrap();
            let mut s = state.write().await;
            s.external_id = Some("ext-1".into());
            s.status = ConnectionStatus::Disconnected;
        }
        assert!(
            mgr.find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&working_dir),
                Some("ext-1"),
            )
            .await
            .is_none(),
            "Disconnected connection must not be reused"
        );

        // Flip to Error — also excluded.
        {
            let state = mgr.get_state("torn").await.unwrap();
            state.write().await.status = ConnectionStatus::Error;
        }
        assert!(
            mgr.find_connection_for_reuse(
                AgentType::ClaudeCode,
                Some(&working_dir),
                Some("ext-1"),
            )
            .await
            .is_none(),
            "Errored connection must not be reused"
        );
    }

    /// Helper that backdates a connection's `last_activity_at` so the
    /// idle sweep sees it as having crossed its threshold.
    async fn backdate_last_activity(mgr: &ConnectionManager, conn_id: &str, secs_ago: i64) {
        let state = mgr.get_state(conn_id).await.expect("connection exists");
        let mut s = state.write().await;
        s.last_activity_at = chrono::Utc::now() - chrono::Duration::seconds(secs_ago);
    }

    #[tokio::test]
    async fn sweep_idle_disconnects_idle_connected_connections() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "stale",
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/stale")),
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "stale", 600).await;

        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1);
        assert!(
            mgr.connections.lock().await.get("stale").is_none(),
            "Idle connection must be removed after sweep"
        );
    }

    #[tokio::test]
    async fn sweep_idle_skips_recently_active_connection() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "fresh",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // last_activity_at defaults to "now" inside SessionState::new — no
        // backdating, so it should NOT be swept.
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 0);
        assert!(mgr.connections.lock().await.contains_key("fresh"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_prompting_connection() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "prompting",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "prompting", 600).await;
        // Override status to Prompting — a turn is in flight; never sweep.
        {
            let state = mgr.get_state("prompting").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 0);
        assert!(mgr.connections.lock().await.contains_key("prompting"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_pending_permission() {
        use crate::acp::session_state::PendingPermissionState;
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "permission",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "permission", 600).await;
        {
            let state = mgr.get_state("permission").await.unwrap();
            state.write().await.pending_permission = Some(PendingPermissionState {
                request_id: "req-1".into(),
                tool_call_id: "tc-1".into(),
                tool_call: serde_json::json!({ "toolCallId": "tc-1", "title": "test" }),
                options: vec![],
                created_at: chrono::Utc::now(),
                queued: 0,
            });
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(
            n, 0,
            "Connection with pending permission must not be swept (user is mid-decision)"
        );
        assert!(mgr.connections.lock().await.contains_key("permission"));
    }

    #[tokio::test]
    async fn sweep_idle_skips_active_background_work() {
        let mgr = ConnectionManager::new();
        insert_fake_connection(
            &mgr,
            "background",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        backdate_last_activity(&mgr, "background", 600).await;
        {
            let state = mgr.get_state("background").await.unwrap();
            let mut state = state.write().await;
            // Mirror what apply_event(BackgroundActivity) records: pending
            // work plus a recent watcher heartbeat.
            state.background_outstanding = 1;
            state.background_activity_at = Some(chrono::Utc::now());
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(
            n, 0,
            "Connection with unresolved background work must not be swept \
             (disconnecting kills the agent CLI and the background task with it)"
        );
        assert!(mgr.connections.lock().await.contains_key("background"));

        // Once the watcher settles the work (outstanding back to 0), the same
        // connection becomes sweepable again.
        {
            let state = mgr.get_state("background").await.unwrap();
            let mut state = state.write().await;
            state.background_outstanding = 0;
            state.last_activity_at = chrono::Utc::now() - chrono::Duration::seconds(600);
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1, "settled background work no longer exempts the sweep");
    }

    #[tokio::test]
    async fn sweep_idle_picks_only_qualifying_subset() {
        let mgr = ConnectionManager::new();
        for id in ["a", "b", "c"] {
            insert_fake_connection(&mgr, id, AgentType::ClaudeCode, None, EventEmitter::Noop).await;
        }
        // a: idle (sweep target), b: fresh (not idle), c: idle but Prompting (skipped).
        backdate_last_activity(&mgr, "a", 600).await;
        backdate_last_activity(&mgr, "c", 600).await;
        {
            let state = mgr.get_state("c").await.unwrap();
            state.write().await.status = ConnectionStatus::Prompting;
        }
        let n = mgr.sweep_idle(Duration::from_secs(300)).await;
        assert_eq!(n, 1);
        let map = mgr.connections.lock().await;
        assert!(!map.contains_key("a"));
        assert!(map.contains_key("b"));
        assert!(map.contains_key("c"));
    }

    /// When two `spawn_agent` calls race for the same logical session id,
    /// the per-key dedup mutex makes the second one observe the first's
    /// freshly-spawned connection and reuse it. Without the mutex, both
    /// would have missed dedup during the connecting window.
    ///
    /// Simulates the race by pre-inserting a "first call's connection" with
    /// `external_id` set; what's tested is that two concurrent
    /// `find_connection_for_reuse` calls under the same lock see consistent
    /// state. The `spawn_locks` map being shared via `clone_ref` is the
    /// invariant we need.
    #[tokio::test]
    async fn spawn_locks_are_shared_across_clone_ref() {
        let mgr = ConnectionManager::new();
        let cloned = mgr.clone_ref();
        // Both clones must reference the same map. Insert via one,
        // observe via the other.
        let key = SpawnDedupKey {
            agent_type: AgentType::ClaudeCode,
            working_dir: Some(PathBuf::from("/tmp/dedup-test")),
            session_id: "ext-shared".into(),
        };
        {
            let mut locks = mgr.spawn_locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
        }
        let cloned_locks = cloned.spawn_locks.lock().await;
        assert!(
            cloned_locks.contains_key(&key),
            "spawn_locks must be shared between original and clone_ref"
        );
    }

    /// Two concurrent `send_prompt_linked` calls on the SAME connection
    /// must serialize through the per-connection `prompt_lock` so the
    /// backend-creates branch can't fire twice and produce duplicate
    /// conversation rows. The second call observes `already_linked == true`
    /// (set by the first under the lock) and skips creation.
    #[tokio::test]
    async fn send_prompt_linked_serializes_concurrent_callers() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/race").await;

        let mgr = Arc::new(ConnectionManager::new());
        let conn_id = "race-conn";
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.into(), fake_connection(conn_id, None));
        }

        let before = count_conversation_rows(&db).await;
        // tokio::join! polls the two futures concurrently in the SAME
        // task — they can borrow `&db` and `mgr` without the 'static
        // requirement that `tokio::spawn` would impose.
        let mgr_ref = mgr.as_ref();
        tokio::join!(
            async {
                let _ = mgr_ref
                    .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
                    .await;
            },
            async {
                let _ = mgr_ref
                    .send_prompt_linked(&db, conn_id, one_text_block(), Some(folder_id), None, None)
                    .await;
            },
        );

        let after = count_conversation_rows(&db).await;
        assert_eq!(
            after - before,
            1,
            "exactly one new conversation row across two concurrent send_prompt_linked"
        );
    }

    // ---------- Phase: spawn handshake wait helper ----------

    #[tokio::test]
    async fn wait_for_session_started_returns_ready_when_sender_fires() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Fire immediately on a separate task so the wait future actually
        // gets to register.
        tokio::spawn(async move {
            let _ = tx.send(());
        });
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(500)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::Ready);
        assert!(
            elapsed < Duration::from_millis(500),
            "Ready outcome must resolve well before timeout, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn wait_for_session_started_returns_aborted_when_sender_drops() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Drop the sender — emulates "connection died before SessionStarted",
        // i.e. SessionState's tx was dropped during cleanup.
        drop(tx);
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(500)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::Aborted);
        assert!(
            elapsed < Duration::from_millis(500),
            "Aborted outcome must resolve well before timeout, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn wait_for_session_started_returns_timed_out_when_neither_happens() {
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Hold the sender alive but never fire and never drop. Tight
        // timeout so the test stays fast; production timeout is 60s.
        let (outcome, elapsed) = wait_for_session_started(rx, Duration::from_millis(40)).await;
        assert_eq!(outcome, HandshakeWaitOutcome::TimedOut);
        assert!(
            elapsed >= Duration::from_millis(40),
            "TimedOut must wait at least the full timeout, got {elapsed:?}"
        );
    }

    #[test]
    fn spawn_handshake_timeout_from_env_uses_default_when_unset() {
        // Snapshot env, mutate, restore. Single test owns this var to avoid
        // cross-test contention.
        let prev = std::env::var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS").ok();
        std::env::remove_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS");
        let default = spawn_handshake_timeout_from_env();
        assert_eq!(default, Duration::from_secs(SPAWN_HANDSHAKE_TIMEOUT_SECS));

        std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", "5");
        assert_eq!(spawn_handshake_timeout_from_env(), Duration::from_secs(5));

        std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", "garbage");
        assert_eq!(
            spawn_handshake_timeout_from_env(),
            Duration::from_secs(SPAWN_HANDSHAKE_TIMEOUT_SECS),
            "invalid value falls back to default"
        );

        // Restore.
        match prev {
            Some(v) => std::env::set_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS", v),
            None => std::env::remove_var("CODEG_ACP_SPAWN_HANDSHAKE_TIMEOUT_SECS"),
        }
    }

    #[test]
    fn with_spawn_handshake_timeout_overrides_default_for_tests() {
        let mgr = ConnectionManager::with_spawn_handshake_timeout(Duration::from_secs(7));
        assert_eq!(mgr.spawn_handshake_timeout, Duration::from_secs(7));
    }

    /// When `send_prompt_inner` fails (process gone, channel closed) the row
    /// must end up `Cancelled`, NOT stuck on `in_progress`. Without this
    /// rollback the lifecycle subscriber's TurnComplete write never fires
    /// (no turn ever started), so the only thing that could later un-stick
    /// the row is a follow-up prompt happening to succeed — fragile, and on
    /// the server-side / chat-channel paths there may be no follow-up at all.
    #[tokio::test]
    async fn send_prompt_linked_rolls_back_to_cancelled_on_send_failure() {
        use crate::db::entities::conversation;
        use crate::db::test_helpers;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/cancel-rollback").await;

        let mgr = ConnectionManager::new();
        let (broadcaster, _rx) = make_test_broadcaster();
        let conn_id = "conn-cancel";
        // insert_fake_connection drops the cmd_tx receiver, so send_prompt_inner
        // returns ProcessExited — exactly the failure mode this test targets.
        insert_fake_connection(
            &mgr,
            conn_id,
            AgentType::ClaudeCode,
            Some(PathBuf::from("/tmp/cancel-rollback")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;
        let mut rx = subscribe_conn_stream(&mgr, conn_id).await;

        // Non-empty blocks so the send reaches `reserve()` (which fails on the
        // dropped receiver → ProcessExited); an empty prompt would be rejected
        // earlier, before the gate, and never exercise this rollback path.
        let result = mgr
            .send_prompt_linked(
                &db,
                conn_id,
                vec![PromptInputBlock::Text {
                    text: "trigger send failure".into(),
                }],
                Some(folder_id),
                None,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(AcpError::ProcessExited)),
            "send_prompt_inner must propagate ProcessExited up to the caller; got {result:?}"
        );

        // Drain events: ConversationLinked → InProgress → Cancelled, in order.
        let env_linked = recv_first_acp_event(&mut rx).await;
        let conv_id = match env_linked.payload {
            AcpEvent::ConversationLinked {
                conversation_id, ..
            } => conversation_id,
            other => panic!("expected ConversationLinked first, got {other:?}"),
        };
        let env_in_progress = recv_first_acp_event(&mut rx).await;
        match env_in_progress.payload {
            AcpEvent::ConversationStatusChanged { status, .. } => {
                assert_eq!(status, ConversationStatus::InProgress);
            }
            other => {
                panic!("expected ConversationStatusChanged(InProgress) before send, got {other:?}")
            }
        }
        let env_cancelled = recv_first_acp_event(&mut rx).await;
        match env_cancelled.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, conv_id);
                assert_eq!(
                    status,
                    ConversationStatus::Cancelled,
                    "send_prompt failure must roll the row forward to Cancelled, not leave InProgress"
                );
            }
            other => panic!(
                "expected ConversationStatusChanged(Cancelled) on send failure, got {other:?}"
            ),
        }

        // Strict ordering: linked < in_progress < cancelled. The lifecycle
        // contract says the Cancelled emit cannot precede the InProgress one
        // — UIs that animate based on "previous → current" depend on this.
        assert!(
            env_in_progress.seq > env_linked.seq && env_cancelled.seq > env_in_progress.seq,
            "event seq must be strictly monotonic: linked={} in_progress={} cancelled={}",
            env_linked.seq,
            env_in_progress.seq,
            env_cancelled.seq,
        );

        // DB row settles at Cancelled — final ground truth read.
        let row = conversation::Entity::find_by_id(conv_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists");
        assert_eq!(row.status, ConversationStatus::Cancelled);
    }

    // ---------- fork_session ----------

    /// Build a connection whose cmd_rx is drained by a spawned task that
    /// fakes the protocol-level fork reply. Returns the manager so the test
    /// can call `fork_session`. The fake reply task lives until it processes
    /// one Fork command, then exits.
    async fn manager_with_fake_fork(
        conn_id: &str,
        conversation_id: i32,
        forked_session_id: &str,
        original_session_id: &str,
    ) -> (Arc<ConnectionManager>, tokio::task::JoinHandle<()>) {
        use crate::acp::connection::ConnectionCommand;
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
        let mut state = SessionState::new(
            conn_id.to_string(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = Some(conversation_id);
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: conn_id.to_string(),
            agent_type: crate::models::agent::AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = Arc::new(ConnectionManager::new());
        {
            let mut map = mgr.connections.lock().await;
            map.insert(conn_id.to_string(), conn);
        }

        let forked = forked_session_id.to_string();
        let original = original_session_id.to_string();
        let join = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let ConnectionCommand::Fork { reply, .. } = cmd {
                    let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                        forked_session_id: forked.clone(),
                        original_session_id: original.clone(),
                    }));
                    return;
                }
            }
        });
        (mgr, join)
    }

    #[tokio::test]
    async fn fork_session_writes_atomic_two_row_layout() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-happy").await;

        // Pre-existing row: stands in for the conversation about to be forked.
        // Title gets a `[Fork] ` prefix; sibling row inherits the clean title.
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Original Topic".into()),
            Some("feature/x".into()),
        )
        .await
        .unwrap();
        // External_id starts as S1 — manager.fork_session will swap to S2.
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-fork", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork", None, None, None)
            .await
            .expect("fork_session should succeed");
        let _ = join.await;

        assert_eq!(result.forked_session_id, "session-S2");
        assert_eq!(result.original_session_id, "session-S1");
        let sibling_id = result.sibling_conversation_id;
        assert_ne!(sibling_id, pre.id, "sibling row must be a fresh row");

        // Current row: external_id=S2, title prefixed.
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.external_id.as_deref(), Some("session-S2"));
        assert_eq!(current.title.as_deref(), Some("[Fork] Original Topic"));

        // Sibling row: external_id=S1, clean title, PendingReview, same folder/git_branch.
        let sibling = conversation_service::get_by_id(&db.conn, sibling_id)
            .await
            .unwrap();
        assert_eq!(sibling.external_id.as_deref(), Some("session-S1"));
        assert_eq!(sibling.title.as_deref(), Some("Original Topic"));
        assert_eq!(sibling.status, "pending_review");
        assert_eq!(sibling.folder_id, folder_id);
        assert_eq!(sibling.git_branch.as_deref(), Some("feature/x"));
    }

    #[tokio::test]
    async fn fork_session_sibling_keeps_the_conversations_tags() {
        use crate::db::service::conversation_tag_service;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-tags").await;
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Tagged".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();
        let global = conversation_tag_service::create_tag(&db.conn, None, "bug", "#d73a4a")
            .await
            .unwrap();
        let owned = conversation_tag_service::create_tag(&db.conn, Some(folder_id), "ui", "#0e8a16")
            .await
            .unwrap();
        conversation_tag_service::update_conversation_tags(
            &db.conn,
            pre.id,
            &[global.id, owned.id],
            &[],
        )
        .await
        .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-fork-tags", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork-tags", None, None, None)
            .await
            .expect("fork_session should succeed");
        let _ = join.await;

        let mut expected = vec![global.id, owned.id];
        expected.sort_unstable();
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.tag_ids, expected, "the pre-fork history keeps its tags");
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.tag_ids, expected, "the forked row keeps them too");
    }

    #[tokio::test]
    async fn fork_session_strips_existing_fork_prefix_without_stacking() {
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-restack").await;

        // Title already has `[Fork] ` — re-fork must not produce `[Fork] [Fork] ...`.
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("[Fork] Topic".into()),
            None,
        )
        .await
        .unwrap();
        let (mgr, join) =
            manager_with_fake_fork("c-restack", pre.id, "session-S2", "session-S1").await;
        let result = mgr.fork_session(&db, "c-restack", None, None, None).await.unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] Topic"),
            "should re-stack as single [Fork] prefix, not [Fork] [Fork] ..."
        );
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.title.as_deref(), Some("Topic"));
    }

    #[tokio::test]
    async fn fork_session_adopts_a_sibling_the_lifecycle_subscriber_already_made() {
        // Fork's reply reaches us BEFORE `handle_fork_or_exit` emits
        // `SessionStarted{S2}`, so this persistence and the lifecycle worker
        // (which retries with backoff) run concurrently. If the lifecycle side
        // lands first, its guarded `bind_external_id` has already preserved S1
        // on a row of its own.
        //
        // Inserting a second S1 row then is not a cosmetic duplicate:
        // `(external_id, agent_type)` is UNIQUE, so the INSERT fails and rolls
        // back the entire fork — the user's fork just breaks. Adopt the
        // existing row instead, and return ITS id, because the caller puts that
        // id straight into the fork result and the sidebar upsert.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-raced").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();
        // Lifecycle got there first: S2 landed on the row and S1 was preserved.
        let preserved_id = conversation_service::bind_external_id(&db.conn, pre.id, "session-S2", &[])
            .await
            .unwrap()
            .expect("the lifecycle bind preserves S1");

        let (mgr, join) =
            manager_with_fake_fork("c-raced", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-raced", None, None, None)
            .await
            .expect("fork must survive the lifecycle subscriber winning the race");
        let _ = join.await;

        assert_eq!(
            result.sibling_conversation_id, preserved_id,
            "fork must report the row that actually holds S1"
        );
        let listed = crate::commands::conversations::list_all_conversations_core(
            &db.conn,
            &EventEmitter::Noop,
            &crate::chat_channel::manager::ChatChannelManager::new(),
            crate::commands::conversations::ListAllConversationsOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            listed.len(),
            2,
            "exactly two rows regardless of who won the race, got {listed:?}"
        );
        assert_eq!(
            listed
                .iter()
                .filter(|c| c.external_id.as_deref() == Some("session-S1"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn fork_session_strips_no_space_fork_prefix() {
        // Defensive: a title produced outside the normal flow could lack the
        // space (e.g. external import). The frontend regex `/^\[Fork]\s*/g`
        // tolerated this; the backend strip must too, otherwise re-fork would
        // produce `[Fork] [Fork]xxx`.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-no-space").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("[Fork]NoSpaceTitle".into()),
            None,
        )
        .await
        .unwrap();
        let (mgr, join) =
            manager_with_fake_fork("c-nosp", pre.id, "session-S2", "session-S1").await;
        mgr.fork_session(&db, "c-nosp", None, None, None).await.unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] NoSpaceTitle"),
            "no-space prefix must be tolerantly stripped before re-stacking"
        );
    }

    #[tokio::test]
    async fn fork_session_reads_latest_committed_row_not_a_cached_snapshot() {
        // Regression guard for the write-first ordering in `persist_fork_outcome`.
        // The fork must derive its `[Fork] …` title and the sibling's preserved
        // title from the row's LATEST committed state, read under the write lock
        // the transaction takes with its opening statement — not from a value
        // captured earlier. If a future change reintroduces an early/cached read
        // (e.g. reading before the transaction, or threading a stale title in as
        // a param), a rename committed just before the fork would be clobbered.
        // Here we commit the rename first, then fork, and assert the fork
        // reflects the renamed title on both rows.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-latest").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Stale Original".into()),
            None,
        )
        .await
        .unwrap();
        // Commit a manual rename AFTER creation but BEFORE the fork runs. A
        // correct fork observes this; a stale-snapshot fork would emit
        // "[Fork] Stale Original" / "Stale Original" instead.
        conversation_service::update_title(&db.conn, pre.id, "Renamed By User".into())
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-latest", pre.id, "session-S2", "session-S1").await;
        let result = mgr.fork_session(&db, "c-latest", None, None, None).await.unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(
            current.title.as_deref(),
            Some("[Fork] Renamed By User"),
            "fork must prefix the LATEST committed title, not a stale snapshot"
        );
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(
            sibling.title.as_deref(),
            Some("Renamed By User"),
            "sibling must preserve the LATEST committed title, not a stale snapshot"
        );
    }

    #[tokio::test]
    async fn fork_session_sibling_inherits_the_manual_title_lock() {
        // A rename locks the row (`update_title` sets `title_locked`) precisely
        // so the per-turn auto-title backfill can never overwrite the user's
        // name. The sibling the fork inserts IS that conversation's pre-fork
        // history, so it must inherit the lock: without it the user's name
        // survives only until the sibling's first detail load, which re-parses
        // S1 and adopts the session-file title (rename → fork → the original
        // conversation silently reverts to its pre-rename name).
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-lock").await;

        // "XXX" stands in for the session-file (parser-derived) title.
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("XXX".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::update_title(&db.conn, pre.id, "YYY".into())
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-fork-lock", pre.id, "session-S2", "session-S1").await;
        let result = mgr.fork_session(&db, "c-fork-lock", None, None, None).await.unwrap();
        let _ = join.await;

        let sibling_id = result.sibling_conversation_id;
        let sibling = conversation_service::get_by_id(&db.conn, sibling_id)
            .await
            .unwrap();
        assert_eq!(sibling.title.as_deref(), Some("YYY"));
        assert!(
            sibling.title_locked,
            "the sibling carries the renamed conversation's history, so it must \
             inherit the manual title lock"
        );
        // The forked row keeps the lock it already had (it is the same row).
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.title.as_deref(), Some("[Fork] YYY"));
        assert!(current.title_locked);

        // End-to-end guard: this is exactly what the next detail load does
        // (`get_folder_conversation_with_live_core` → `refresh_auto_title` with
        // the title parsed out of the S1 transcript). It must be a no-op on
        // both rows.
        assert!(
            !conversation_service::refresh_auto_title(&db.conn, sibling_id, "XXX".into())
                .await
                .unwrap(),
            "the auto-title backfill must not revert the sibling's user-set name"
        );
        assert!(
            !conversation_service::refresh_auto_title(&db.conn, pre.id, "XXX".into())
                .await
                .unwrap()
        );
        assert_eq!(
            conversation_service::get_by_id(&db.conn, sibling_id)
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("YYY")
        );
    }

    #[tokio::test]
    async fn fork_session_locks_the_prefixed_title_it_writes() {
        // The `[Fork] ` marker exists nowhere in the transcript, so on an
        // unlocked row the very next detail load erases it: the auto-title
        // backfill adopts the parsed session-file title, leaving the forked row
        // and its sibling wearing the same name with nothing to tell them
        // apart. The fork therefore locks the title it writes.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-prefix-lock").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Auto Title".into()),
            None,
        )
        .await
        .unwrap();
        assert!(!pre.title_locked, "an auto-titled row starts unlocked");

        let (mgr, join) =
            manager_with_fake_fork("c-fork-prefix", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork-prefix", None, None, None)
            .await
            .unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.title.as_deref(), Some("[Fork] Auto Title"));
        assert!(
            current.title_locked,
            "the fork's own title must be protected from the auto-title backfill"
        );
        // The backfill the next detail load runs, with the title parsed out of
        // the forked transcript (a copy of S1, so the pre-fork title).
        assert!(
            !conversation_service::refresh_auto_title(&db.conn, pre.id, "Auto Title".into())
                .await
                .unwrap(),
            "the backfill must not strip the `[Fork] ` marker"
        );
        assert_eq!(
            conversation_service::get_by_id(&db.conn, pre.id)
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("[Fork] Auto Title")
        );
        // The sibling wears the parsed title itself, so it needs no such
        // protection — see `fork_session_sibling_stays_unlocked_for_an_auto_title`.
        assert!(
            !conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
                .await
                .unwrap()
                .title_locked
        );
    }

    #[tokio::test]
    async fn fork_session_leaves_a_titleless_row_unlocked() {
        // Nothing to prefix means nothing to protect: a row forked before it
        // ever got a title must stay unlocked, or the auto-title backfill could
        // never give it its first name and it would read "Untitled" forever.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-untitled").await;

        let pre =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-fork-untitled", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork-untitled", None, None, None)
            .await
            .unwrap();
        let _ = join.await;

        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.title, None, "no title to prefix");
        assert!(!current.title_locked, "an unwritten title must stay unlocked");
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.title, None);
        assert!(!sibling.title_locked);
        // Both rows can still be named by the backfill.
        assert!(
            conversation_service::refresh_auto_title(&db.conn, pre.id, "First Name".into())
                .await
                .unwrap()
        );
        assert!(
            conversation_service::refresh_auto_title(&db.conn, sibling.id, "First Name".into())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn fork_session_sibling_stays_unlocked_for_an_auto_title() {
        // Inheriting the lock must mean INHERITING it, not always setting it:
        // an auto-titled conversation's sibling stays eligible for the
        // auto-title backfill, so a title the agent regenerates later still
        // lands on the preserved history.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-unlocked").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Auto Title".into()),
            None,
        )
        .await
        .unwrap();
        assert!(!pre.title_locked, "a fresh row starts unlocked");

        let (mgr, join) =
            manager_with_fake_fork("c-fork-unlocked", pre.id, "session-S2", "session-S1").await;
        let result = mgr
            .fork_session(&db, "c-fork-unlocked", None, None, None)
            .await
            .unwrap();
        let _ = join.await;

        let sibling_id = result.sibling_conversation_id;
        assert!(
            !conversation_service::get_by_id(&db.conn, sibling_id)
                .await
                .unwrap()
                .title_locked,
            "an auto-derived title must not become locked by forking"
        );
        assert!(
            conversation_service::refresh_auto_title(&db.conn, sibling_id, "Newer Title".into())
                .await
                .unwrap(),
            "the backfill must still be able to update an unlocked sibling"
        );
    }

    #[tokio::test]
    async fn fork_session_errors_without_orphan_when_row_missing() {
        // The current-row write is the transaction's first statement and its
        // `rows_affected == 0` is the not-found signal. If the linked row has
        // vanished (hard-deleted out from under a live connection), the fork
        // must error and, because the sibling INSERT shares the transaction,
        // leave NO orphan sibling behind.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        // Seed a folder but NO conversation row; the connection points at an id
        // that does not exist in the DB.
        let _folder_id = test_helpers::seed_folder(&db, "/tmp/fork-missing").await;
        let missing_conversation_id = 99_999;

        let (mgr, join) = manager_with_fake_fork(
            "c-missing",
            missing_conversation_id,
            "session-S2",
            "session-S1",
        )
        .await;
        let err = mgr
            .fork_session(&db, "c-missing", None, None, None)
            .await
            .expect_err("fork against a missing row must error");
        let _ = join.await;
        assert!(
            err.to_string().contains("not found"),
            "error should mention the missing row, got: {err}"
        );

        // No orphan: the failed transaction rolled back, so the DB holds zero
        // conversation rows (the sibling INSERT must not have committed).
        let all = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert!(
            all.is_empty(),
            "a failed fork must not leave an orphan sibling row, found: {}",
            all.len()
        );
    }

    #[tokio::test]
    async fn fork_session_errors_without_orphan_when_row_soft_deleted() {
        // Forking a soft-deleted conversation must NOT resurrect it: the sibling
        // insert would set `deleted_at = None`, creating a fresh visible row from
        // deleted data. The write-first claim filters `deleted_at IS NULL`, so a
        // deleted row matches nothing → the fork aborts with a not-found error,
        // writes nothing, and leaves the original row soft-deleted and unchanged.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-deleted").await;

        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("Doomed Topic".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();
        conversation_service::soft_delete(&db.conn, pre.id)
            .await
            .unwrap();

        let (mgr, join) =
            manager_with_fake_fork("c-deleted", pre.id, "session-S2", "session-S1").await;
        let err = mgr
            .fork_session(&db, "c-deleted", None, None, None)
            .await
            .expect_err("fork against a soft-deleted row must error");
        let _ = join.await;
        assert!(
            err.to_string().contains("not found") || err.to_string().contains("deleted"),
            "error should mention the missing/deleted row, got: {err}"
        );

        // No resurrection: exactly the original row remains, still soft-deleted,
        // still bound to S1 — no visible sibling was inserted, and the current
        // row was neither re-pointed at S2 nor `[Fork]`-prefixed.
        let all = conversation::Entity::find().all(&db.conn).await.unwrap();
        assert_eq!(all.len(), 1, "no sibling row should have been inserted");
        let only = &all[0];
        assert_eq!(only.id, pre.id);
        assert!(
            only.deleted_at.is_some(),
            "the original row must stay soft-deleted"
        );
        assert_eq!(
            only.external_id.as_deref(),
            Some("session-S1"),
            "the deleted row must not be re-pointed at the forked session"
        );
        assert_eq!(
            only.title.as_deref(),
            Some("Doomed Topic"),
            "the deleted row must not gain a [Fork] prefix"
        );
    }

    #[tokio::test]
    async fn fork_session_rejects_unbound_connection() {
        // Without a linked conversation_id the sibling row would orphan S1
        // history (no row to point at it). fork_session must refuse early —
        // BEFORE sending the Fork command to the agent, so we don't burn an
        // ACP round-trip on a request we can't persist.
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c-unbound".into(), fake_connection("c-unbound", None));
        }
        let err = mgr
            .fork_session(&db, "c-unbound", None, None, None)
            .await
            .expect_err("unbound fork must error");
        assert!(
            err.to_string().contains("linked conversation row"),
            "error should mention missing linkage, got: {err}"
        );
    }

    #[tokio::test]
    async fn fork_session_links_unbound_row_from_caller_ids() {
        // Bug #2: a conversation opened from history resumes via `session_id`
        // but its row isn't bound to the connection until the first prompt
        // fires `ConversationLinked`. A fork-send forks BEFORE that prompt, so
        // fork_session must adopt the caller-supplied (conversation_id,
        // folder_id) and succeed — instead of rejecting as unlinked (which is
        // exactly what the user hit forking a conversation opened from history).
        use crate::acp::connection::ConnectionCommand;
        use crate::db::test_helpers;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/fork-relink").await;
        let pre = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("History".into()),
            None,
        )
        .await
        .unwrap();
        conversation_service::bind_external_id(&db.conn, pre.id, "session-S1", &[])
            .await
            .unwrap();

        // A connection with NO linked conversation_id — mirrors a fresh resume
        // of a historical conversation that hasn't sent a prompt yet.
        let (tx, mut rx) = mpsc::channel::<ConnectionCommand>(4);
        let mut state = SessionState::new(
            "c-relink".to_string(),
            AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = None;
        state.status = ConnectionStatus::Connected;
        let conn = AgentConnection {
            id: "c-relink".to_string(),
            agent_type: AgentType::ClaudeCode,
            status: ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_fingerprint: String::new(),
            last_observed_fingerprint: String::new(),
            child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c-relink".to_string(), conn);
        }
        let join = tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let ConnectionCommand::Fork { reply, .. } = cmd {
                    let _ = reply.send(Ok(crate::acp::types::ForkProtocolResult {
                        forked_session_id: "session-S2".to_string(),
                        original_session_id: "session-S1".to_string(),
                    }));
                    return;
                }
            }
        });

        let result = mgr
            .fork_session(&db, "c-relink", Some(pre.id), Some(folder_id), None)
            .await
            .expect("fork must link the unbound row from caller ids and succeed");
        let _ = join.await;

        assert_eq!(result.forked_session_id, "session-S2");
        // The connection is now linked to the row...
        let linked = mgr.get_state("c-relink").await.expect("connection exists");
        assert_eq!(linked.read().await.conversation_id, Some(pre.id));
        // ...the current row is re-pointed to S2 with a `[Fork]` title...
        let current = conversation_service::get_by_id(&db.conn, pre.id)
            .await
            .unwrap();
        assert_eq!(current.external_id.as_deref(), Some("session-S2"));
        assert_eq!(current.title.as_deref(), Some("[Fork] History"));
        // ...and a sibling preserves the pre-fork S1 history.
        let sibling = conversation_service::get_by_id(&db.conn, result.sibling_conversation_id)
            .await
            .unwrap();
        assert_eq!(sibling.external_id.as_deref(), Some("session-S1"));
    }

    // --- wait_for_session_options polling ----------------------------------
    //
    // These tests exercise the probe's wait loop directly by hand-seeding
    // `SessionState` on an injected connection. They avoid spawning a real
    // agent (which is what `probe_agent_options` itself would do) — the goal
    // is to lock in the three behaviors the public API depends on:
    //   1. ready+grace → Ok(snapshot) reflecting current state
    //   2. never-ready within timeout → Err(ProbeTimedOut), not Ok(empty)
    //   3. selectors_ready=true with empty options → Ok(empty snapshot)

    use crate::acp::types::{
        SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectInfo, SessionModeInfo,
        SessionModeStateInfo,
    };

    fn sample_modes() -> SessionModeStateInfo {
        SessionModeStateInfo {
            current_mode_id: "default".into(),
            available_modes: vec![
                SessionModeInfo {
                    id: "default".into(),
                    name: "Default".into(),
                    description: None,
                },
                SessionModeInfo {
                    id: "yolo".into(),
                    name: "YOLO".into(),
                    description: None,
                },
            ],
        }
    }

    fn sample_config_options() -> Vec<SessionConfigOptionInfo> {
        vec![SessionConfigOptionInfo {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: None,
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: "sonnet".into(),
                options: vec![],
                groups: vec![],
            }),
            recommended_value: None,
        }]
    }

    #[tokio::test]
    async fn wait_for_session_options_returns_snapshot_after_ready_plus_grace() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-1",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Seed the state the probe is waiting on. Done BEFORE the wait
        // starts so the very first poll already sees ready=true and only
        // the 500 ms grace period gates the return.
        {
            let state = mgr.get_state("probe-1").await.expect("state");
            let mut s = state.write().await;
            s.modes = Some(sample_modes());
            s.config_options = Some(sample_config_options());
            s.selectors_ready = true;
        }

        let start = std::time::Instant::now();
        let snapshot = mgr
            .wait_for_session_options("probe-1", Duration::from_secs(2))
            .await
            .expect("ready+grace path must return Ok");
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(450),
            "expected ~500ms grace, observed {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "should NOT wait the full 2s timeout, observed {elapsed:?}"
        );
        assert_eq!(snapshot.config_options.len(), 1);
        assert!(snapshot.modes.is_some());
    }

    #[tokio::test]
    async fn wait_for_session_options_times_out_when_selectors_never_ready() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-2",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Critical guarantee: even though `config_options` is `Some(...)`,
        // because `selectors_ready` is still false, the wait MUST timeout
        // and return Err — never Ok(empty) which would mislead the UI.
        {
            let state = mgr.get_state("probe-2").await.expect("state");
            let mut s = state.write().await;
            s.config_options = Some(vec![]);
            s.selectors_ready = false;
        }

        let err = mgr
            .wait_for_session_options("probe-2", Duration::from_millis(300))
            .await
            .expect_err("timeout path must return Err");
        assert!(
            matches!(err, AcpError::ProbeTimedOut),
            "expected ProbeTimedOut, got {err:?}"
        );
        assert_eq!(err.code(), Some("probe_timed_out"));
    }

    #[tokio::test]
    async fn wait_for_session_options_returns_empty_when_ready_with_no_options() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection(
            "probe-3",
            crate::models::agent::AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
        // Real outcome the UI renders as "agent has nothing to configure":
        // selectors_ready=true, modes=None, config_options=None. Must
        // succeed, not error — this is the path that distinguishes a
        // legitimately empty agent from an unresponsive one.
        {
            let state = mgr.get_state("probe-3").await.expect("state");
            let mut s = state.write().await;
            s.modes = None;
            s.config_options = None;
            s.selectors_ready = true;
        }

        let snapshot = mgr
            .wait_for_session_options("probe-3", Duration::from_secs(2))
            .await
            .expect("ready-empty path must return Ok, not Err");
        assert!(snapshot.modes.is_none());
        assert!(snapshot.config_options.is_empty());
    }

    #[tokio::test]
    async fn wait_for_session_options_unknown_connection_errors_immediately() {
        let mgr = ConnectionManager::new();
        let err = mgr
            .wait_for_session_options("does-not-exist", Duration::from_secs(5))
            .await
            .expect_err("missing connection must error");
        assert!(
            matches!(err, AcpError::ConnectionNotFound(_)),
            "expected ConnectionNotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn apply_event_error_populates_last_error_snapshot() {
        // Directly drives SessionState::apply_event to assert the Error
        // arm now writes `last_error` (rather than being a no-op as it
        // was before). The probe path reads this to surface the
        // agent's own error message after cleanup runs.
        use crate::acp::session_state::SessionState;
        let mut s = SessionState::new(
            "c1".into(),
            crate::models::agent::AgentType::ClaudeCode,
            None,
            "test-window".into(),
            None,
        );
        assert!(s.last_error.is_none(), "fresh state has no error");

        s.apply_event(&AcpEvent::Error {
            message: "agent exploded".into(),
            agent_type: "claude_code".into(),
            code: Some("sdk_not_installed".into()),
            details: None,
            terminal: true,
        });
        let captured = s.last_error.as_ref().expect("error must be captured");
        assert_eq!(captured.message, "agent exploded");
        assert_eq!(captured.code.as_deref(), Some("sdk_not_installed"));

        // A second Error event overwrites — `last_error` is "latest",
        // not "first". Keeps post-mortem reads aligned with what the
        // user most recently observed on the event channel.
        s.apply_event(&AcpEvent::Error {
            message: "second failure".into(),
            agent_type: "claude_code".into(),
            code: None,
            details: None,
            terminal: true,
        });
        let captured = s.last_error.as_ref().unwrap();
        assert_eq!(captured.message, "second failure");
        assert!(captured.code.is_none());
    }

    // --- live feedback: submit gate + consume drain --------------------

    /// Make a test connection feedback-capable AND mid-turn (the happy state).
    async fn mark_feedback_ready(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.unwrap();
        let mut s = state.write().await;
        s.feedback_tool_available = true;
        s.turn_in_flight = true;
    }

    async fn set_feedback_tool_available(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.unwrap();
        state.write().await.feedback_tool_available = true;
    }

    #[tokio::test]
    async fn submit_feedback_rejected_when_tool_unavailable() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        // feedback_tool_available defaults false: the agent never got the tool
        // (e.g. its session started before the feature was enabled), even mid-turn.
        let state = mgr.get_state("c1").await.unwrap();
        state.write().await.turn_in_flight = true;
        let err = mgr.submit_feedback("c1", "note".into(), None).await.unwrap_err();
        assert!(matches!(err, AcpError::FeedbackDisabled));
        assert!(state.read().await.feedback.is_empty());
    }

    #[tokio::test]
    async fn submit_feedback_rejected_when_no_turn_in_flight() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        // Tool available but no turn in flight → nothing to steer.
        set_feedback_tool_available(&mgr, "c1").await;
        let err = mgr.submit_feedback("c1", "note".into(), None).await.unwrap_err();
        assert!(matches!(err, AcpError::NoActiveTurn));
        // And nothing was appended.
        let state = mgr.get_state("c1").await.unwrap();
        assert!(state.read().await.feedback.is_empty());
    }

    #[tokio::test]
    async fn submit_feedback_missing_connection_errors() {
        let mgr = ConnectionManager::new();
        let err = mgr
            .submit_feedback("nope", "note".into(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, AcpError::ConnectionNotFound(_)));
    }

    #[tokio::test]
    async fn submit_feedback_appends_when_turn_in_flight() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        let item = mgr
            .submit_feedback("c1", "  use UserService  ".into(), None)
            .await
            .unwrap();
        assert_eq!(item.status, FeedbackStatus::Pending);
        // Stored text is trimmed.
        assert_eq!(item.text, "use UserService");
        let state = mgr.get_state("c1").await.unwrap();
        let s = state.read().await;
        assert_eq!(s.feedback.len(), 1);
        assert_eq!(s.feedback[0].text, "use UserService");
    }

    #[tokio::test]
    async fn submit_feedback_rejects_empty_and_oversized() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        // Empty / whitespace-only → rejected, nothing appended.
        for empty in ["", "   ", "\n\t "] {
            let err = mgr.submit_feedback("c1", empty.into(), None).await.unwrap_err();
            assert!(matches!(err, AcpError::InvalidFeedback(_)));
        }
        // Oversized → rejected.
        let huge = "x".repeat(MAX_FEEDBACK_CHARS + 1);
        let err = mgr.submit_feedback("c1", huge, None).await.unwrap_err();
        assert!(matches!(err, AcpError::InvalidFeedback(_)));
        // Exactly at the bound is accepted.
        let at_bound = "y".repeat(MAX_FEEDBACK_CHARS);
        assert!(mgr.submit_feedback("c1", at_bound, None).await.is_ok());
        let state = mgr.get_state("c1").await.unwrap();
        assert_eq!(state.read().await.feedback.len(), 1, "only the valid note stuck");
    }

    // --- native steering (push channel) ----------------------------------

    async fn mark_native_steering_ready(mgr: &ConnectionManager, conn_id: &str) {
        let state = mgr.get_state(conn_id).await.unwrap();
        let mut s = state.write().await;
        s.native_steering_available = true;
        s.turn_in_flight = true;
    }

    /// Play the connection loop's role: receive one `Steer` command and reply
    /// the given outcome. Returns the blocks the command carried.
    fn answer_steer(
        mut rx: tokio::sync::mpsc::Receiver<ConnectionCommand>,
        outcome: Result<SteerOutcome, AcpError>,
    ) -> tokio::task::JoinHandle<Vec<PromptInputBlock>> {
        tokio::spawn(async move {
            match rx.recv().await {
                Some(ConnectionCommand::Steer { blocks, reply }) => {
                    let _ = reply.send(outcome);
                    blocks
                }
                _ => panic!("expected a Steer command"),
            }
        })
    }

    /// [`answer_steer`] for a test that submits more than once: answers `n`
    /// Steer commands with the same outcome. The loop is what makes concurrent
    /// submits resolvable — each waits on its own reply channel, so a
    /// single-shot answerer would leave the second one hanging.
    /// Takes a bare [`SteerOutcome`] (which is `Copy`) rather than the
    /// `Result` [`answer_steer`] accepts, because `AcpError` is not `Clone` and
    /// a repeated answerer has to hand out the same value more than once. No
    /// caller needs a repeated FAILURE.
    fn answer_steer_n(
        mut rx: tokio::sync::mpsc::Receiver<ConnectionCommand>,
        outcome: SteerOutcome,
        n: usize,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            for _ in 0..n {
                match rx.recv().await {
                    Some(ConnectionCommand::Steer { reply, .. }) => {
                        let _ = reply.send(Ok(outcome));
                    }
                    _ => panic!("expected a Steer command"),
                }
            }
        })
    }

    /// The note's instant must precede the injection reaching the agent. The
    /// adapter hands the text to the agent BEFORE it answers `injected`, so the
    /// agent can write its own transcript copy of the message while this call
    /// is still awaiting that answer. A note stamped on the way back would
    /// postdate the copy it caused, and the frontend — which folds a persisted
    /// copy away only when it postdates the injection, so that the same words
    /// sent in an earlier round are never hidden — would show the message both
    /// as a transcript turn and as a live one.
    #[tokio::test]
    async fn native_submit_stamps_the_note_before_the_agent_can_see_it() {
        let mgr = ConnectionManager::new();
        let mut rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        // Stand in for the adapter: note when the injection reached it (the
        // earliest instant the agent could record the message), then dawdle
        // before answering, as a real round-trip does.
        let fake_loop = tokio::spawn(async move {
            match rx.recv().await {
                Some(ConnectionCommand::Steer { reply, .. }) => {
                    let seen_by_agent = chrono::Utc::now();
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    let _ = reply.send(Ok(SteerOutcome::Injected));
                    seen_by_agent
                }
                _ => panic!("expected a Steer command"),
            }
        });

        let item = mgr
            .submit_feedback("c1", "use the other API".into(), None)
            .await
            .unwrap();
        let seen_by_agent = fake_loop.await.unwrap();
        assert!(
            item.created_at <= seen_by_agent,
            "created_at ({}) must precede the injection reaching the agent ({})",
            item.created_at,
            seen_by_agent
        );
    }

    #[tokio::test]
    async fn native_submit_injected_records_delivered_and_pull_stays_empty() {
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        // The pull tool coexists by design — and must see NOTHING (the
        // no-double-delivery invariant: native notes are Delivered from birth,
        // `read_pending_feedback` only returns Pending).
        set_feedback_tool_available(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::Injected));

        let item = mgr.submit_feedback("c1", "  ship it  ".into(), None).await.unwrap();
        assert_eq!(item.status, FeedbackStatus::Delivered);
        assert!(item.delivered_at.is_some());
        assert_eq!(item.text, "ship it");
        // The wire carried the trimmed text as a single block (a blocks-less
        // submit stays byte-identical to the historical text-only steer).
        assert_eq!(
            fake_loop.await.unwrap(),
            vec![PromptInputBlock::Text {
                text: "ship it".into()
            }]
        );
        // ...and so does the NOTE: a text-only steer records no block list, so
        // every consumer keeps rendering it from `text` alone. This is the
        // half that keeps the attachment support below from changing the
        // historical path.
        assert_eq!(item.blocks, None);

        let state = mgr.get_state("c1").await.unwrap();
        {
            let s = state.read().await;
            assert_eq!(s.feedback.len(), 1);
            assert_eq!(s.feedback[0].status, FeedbackStatus::Delivered);
            assert!(s.native_steering_available, "injected must not downgrade");
        }
        assert!(
            mgr.read_pending_feedback("c1").await.is_empty(),
            "a check_user_feedback pull must never re-deliver a natively-injected note"
        );
    }

    #[tokio::test]
    async fn native_submit_with_blocks_carries_the_draft_and_records_the_text() {
        // A draft with an image attachment steers as its full block list (the
        // wire payload) while the recorded note's `text` stays the display
        // form. The note ALSO carries the projected blocks, because `text` is
        // what the composer collapsed the attachment into: without them the
        // live transcript renders a sentence about an image where the image
        // should be, and only a reload (which reads the agent's own copy) puts
        // it back. Projection matches an ordinary prompt's `user_message`.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::Injected));

        let draft = vec![
            PromptInputBlock::Text {
                text: "make it match this mock".into(),
            },
            PromptInputBlock::Image {
                data: "aGk=".into(),
                mime_type: "image/png".into(),
                uri: None,
            },
        ];
        let item = mgr
            .submit_feedback("c1", "make it match this mock".into(), Some(draft.clone()))
            .await
            .unwrap();
        assert_eq!(item.status, FeedbackStatus::Delivered);
        assert_eq!(item.text, "make it match this mock");
        // The note carries the image, so the live transcript can render it at
        // the point the user sent it rather than waiting for a reload.
        assert_eq!(
            item.blocks,
            Some(vec![
                crate::acp::types::UserMessageBlock::Text {
                    text: "make it match this mock".into()
                },
                crate::acp::types::UserMessageBlock::Image {
                    data: "aGk=".into(),
                    mime_type: "image/png".into(),
                },
            ])
        );
        // The wire carried the caller's blocks verbatim, attachment included.
        assert_eq!(fake_loop.await.unwrap(), draft);
    }

    #[tokio::test]
    async fn a_draft_that_projects_to_text_alone_records_no_block_list() {
        // `blocks` means "this note carried more than its text". A draft whose
        // projection is text-only says nothing `text` does not, so recording a
        // list for it would put one on a note the field's contract calls
        // text-only — and the frontend would key its dedup on that list.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::Injected));

        let item = mgr
            .submit_feedback(
                "c1",
                "just words".into(),
                Some(vec![PromptInputBlock::Text {
                    text: "just words".into(),
                }]),
            )
            .await
            .unwrap();

        assert_eq!(item.blocks, None);
        // The wire still carried what the caller sent — only the RECORD is
        // trimmed, so the agent's input is untouched.
        assert_eq!(
            fake_loop.await.unwrap(),
            vec![PromptInputBlock::Text {
                text: "just words".into()
            }]
        );
    }

    #[tokio::test]
    async fn steer_attachments_past_the_per_turn_budget_are_not_retained() {
        // The budget guards what OUTLIVES the event: `SessionState.feedback`,
        // which every snapshot is rebuilt from. So it is enforced at the append
        // — the single authorized writer, where the check and the push share
        // one critical section — not at the submit site, where a read, an agent
        // round-trip and a write would let two concurrent steers both pass it.
        //
        // Past the budget the note still DELIVERS and the event still carries
        // its blocks to whoever is attached right now; only the retained copy
        // drops them, so a reconnect renders it from `text` like any text-only
        // steer.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::Injected));

        let huge = "A".repeat(MAX_FEEDBACK_ATTACHMENT_BYTES_PER_TURN + 1);
        let item = mgr
            .submit_feedback(
                "c1",
                "one enormous screenshot".into(),
                Some(vec![PromptInputBlock::Image {
                    data: huge,
                    mime_type: "image/png".into(),
                    uri: None,
                }]),
            )
            .await
            .unwrap();

        assert_eq!(
            item.status,
            FeedbackStatus::Delivered,
            "over budget is not a rejection — the agent already has the content"
        );

        // The RETAINED note is the one that had to shed the attachment.
        let state = mgr.get_state("c1").await.unwrap();
        {
            let s = state.read().await;
            assert_eq!(s.feedback.len(), 1);
            assert_eq!(
                s.feedback[0].blocks, None,
                "an over-budget attachment must not be kept for the turn"
            );
            assert_eq!(s.feedback[0].text, "one enormous screenshot");
        }

        // And the wire was never trimmed: the budget governs what codeg KEEPS,
        // not what the agent receives.
        let sent = fake_loop.await.unwrap();
        assert!(matches!(
            sent.as_slice(),
            [PromptInputBlock::Image { data, .. }]
                if data.len() == MAX_FEEDBACK_ATTACHMENT_BYTES_PER_TURN + 1
        ));
    }

    #[tokio::test]
    async fn concurrent_over_budget_steers_cannot_both_be_retained() {
        // The interleaving a submit-site check could not stop: two steers whose
        // attachments each fit the budget alone but not together. The append is
        // serialized by the state write lock, so the second one sees the first
        // already retained and sheds its own blocks.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let fake_loop = answer_steer_n(rx, SteerOutcome::Injected, 2);

        // Two thirds of the budget each: either alone is admissible, the pair
        // is not.
        let half = "A".repeat((MAX_FEEDBACK_ATTACHMENT_BYTES_PER_TURN / 3) * 2);
        let shot = |text: &str| {
            let data = half.clone();
            let text = text.to_string();
            let mgr = &mgr;
            async move {
                mgr.submit_feedback(
                    "c1",
                    text,
                    Some(vec![PromptInputBlock::Image {
                        data,
                        mime_type: "image/png".into(),
                        uri: None,
                    }]),
                )
                .await
                .unwrap()
            }
        };
        let (a, b) = tokio::join!(shot("first"), shot("second"));
        assert_eq!(a.status, FeedbackStatus::Delivered);
        assert_eq!(b.status, FeedbackStatus::Delivered);
        fake_loop.await.unwrap();

        let state = mgr.get_state("c1").await.unwrap();
        let s = state.read().await;
        assert_eq!(s.feedback.len(), 2, "both notes are still recorded");
        let retained: usize = s
            .feedback
            .iter()
            .filter_map(|f| f.blocks.as_deref())
            .map(crate::acp::feedback::attachment_bytes)
            .sum();
        assert!(
            retained <= MAX_FEEDBACK_ATTACHMENT_BYTES_PER_TURN,
            "the aggregate bound holds across concurrent steers: retained={retained}"
        );
        assert_eq!(
            s.feedback.iter().filter(|f| f.blocks.is_some()).count(),
            1,
            "exactly one of the pair keeps its attachment"
        );
    }

    #[test]
    fn a_steer_admitted_against_one_turn_does_not_ride_the_next_one() {
        // The guard `submit_feedback_native` applies across attachment
        // hydration — the one await between admission and the enqueue. The
        // loop's idle arm covers "the turn ended"; only this covers "the next
        // turn started", which would otherwise have the loop inject the note
        // into a turn the user never aimed at.
        //
        // Same turn throughout — the overwhelmingly common case.
        assert!(!steered_turn_changed(3, true, 3));
        // The turn ended and a NEW one started: still in flight, so the flag
        // alone says nothing. This is the case nothing else catches.
        assert!(steered_turn_changed(3, true, 4));
        // The turn simply ended (the loop's idle arm would also catch this).
        assert!(steered_turn_changed(3, false, 4));
        // A repeat `TurnComplete` double-counts; only inequality is read, so
        // the verdict is the same.
        assert!(steered_turn_changed(3, true, 5));
        // First turn of a connection: the counter starts at zero and carries
        // identity from the very first turn, with no "unknown" window.
        assert!(!steered_turn_changed(0, true, 0));
        assert!(steered_turn_changed(0, true, 1));
    }

    #[tokio::test]
    async fn turn_complete_moves_the_turn_identity_the_steer_guard_reads() {
        // The guard above is only as good as the counter under it: prove
        // `TurnComplete` — the single production clear of `turn_in_flight` —
        // is what moves it, so "the turn I was admitted against is over" is
        // observable even once a NEXT turn has set the flag again.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let state = mgr.get_state("c1").await.unwrap();
        let admitted = {
            let mut s = state.write().await;
            s.turn_in_flight = true;
            s.turns_completed
        };
        state.write().await.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        });
        // A next turn re-sets the flag, exactly as `send_prompt_inner` does.
        state.write().await.turn_in_flight = true;

        let s = state.read().await;
        assert!(
            steered_turn_changed(admitted, s.turn_in_flight, s.turns_completed),
            "an in-flight flag that belongs to the NEXT turn must not read as the admitted one"
        );
    }

    #[tokio::test]
    async fn pull_submit_with_blocks_rejects_instead_of_dropping_attachments() {
        // The pull tool delivers plain text, so a blocks-bearing note on a
        // pull-only session (native downgraded mid-race) must reject with
        // NoActiveTurn — the caller's queue fallback re-routes the whole
        // draft — rather than deliver the text and silently drop the image.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        let draft = vec![PromptInputBlock::Image {
            data: "aGk=".into(),
            mime_type: "image/png".into(),
            uri: None,
        }];
        let err = mgr
            .submit_feedback("c1", "1 attachment".into(), Some(draft))
            .await
            .unwrap_err();
        assert!(matches!(err, AcpError::NoActiveTurn));
        // Nothing recorded: the content is still draft-owned.
        let state = mgr.get_state("c1").await.unwrap();
        assert!(state.read().await.feedback.is_empty());
        assert!(mgr.read_pending_feedback("c1").await.is_empty());
    }

    #[tokio::test]
    async fn native_submit_prompt_required_maps_to_no_active_turn_and_records_nothing() {
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::PromptRequired));

        let err = mgr.submit_feedback("c1", "note".into(), None).await.unwrap_err();
        assert!(matches!(err, AcpError::NoActiveTurn));
        let _ = fake_loop.await;

        let state = mgr.get_state("c1").await.unwrap();
        let s = state.read().await;
        // Content was NOT consumed (honored opt-in): nothing recorded, and the
        // channel stays native — the caller resubmits as a normal prompt.
        assert!(s.feedback.is_empty());
        assert!(s.native_steering_available);
    }

    #[tokio::test]
    async fn native_submit_started_new_turn_records_delivered_and_downgrades_to_pull() {
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        set_feedback_tool_available(&mgr, "c1").await;
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::StartedNewTurn));

        // The adapter ignored the opt-in: content consumed → recorded
        // Delivered (never resent), and the session downgrades to pull.
        let item = mgr.submit_feedback("c1", "note one".into(), None).await.unwrap();
        assert_eq!(item.status, FeedbackStatus::Delivered);
        let _ = fake_loop.await;
        let state = mgr.get_state("c1").await.unwrap();
        assert!(
            !state.read().await.native_steering_available,
            "startedNewTurn must downgrade native steering for the session"
        );

        // The NEXT note rides the pull path: lands Pending, no Steer command
        // (the loop receiver was consumed above — a native attempt would fail
        // on the dead channel, so an Ok(Pending) proves the pull branch ran).
        let second = mgr.submit_feedback("c1", "note two".into(), None).await.unwrap();
        assert_eq!(second.status, FeedbackStatus::Pending);
        let pending = mgr.read_pending_feedback("c1").await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "note two");
    }

    #[tokio::test]
    async fn native_submit_records_even_when_turn_settles_before_the_reply() {
        // The ungated-emit invariant: the adapter's `injected` reply is
        // authoritative even if `TurnComplete` lands first. A gated emit here
        // would misreport NoActiveTurn → the frontend would resend the same
        // text as a prompt → duplicate.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        let state = mgr.get_state("c1").await.unwrap();

        let state_for_loop = state.clone();
        let mut rx = rx;
        let fake_loop = tokio::spawn(async move {
            match rx.recv().await {
                Some(ConnectionCommand::Steer { reply, .. }) => {
                    // The turn settles BETWEEN the injection and the reply.
                    state_for_loop.write().await.turn_in_flight = false;
                    let _ = reply.send(Ok(SteerOutcome::Injected));
                }
                _ => panic!("expected a Steer command"),
            }
        });

        let item = mgr.submit_feedback("c1", "late note".into(), None).await.unwrap();
        assert_eq!(item.status, FeedbackStatus::Delivered);
        let _ = fake_loop.await;
        assert_eq!(state.read().await.feedback.len(), 1);

        // The next turn's UserMessage clears the (turn-scoped) note — the
        // post-turn residue is bounded and harmless.
        state.write().await.apply_event(&AcpEvent::UserMessage {
            message_id: "m-next".into(),
            blocks: Vec::new(),
        });
        assert!(state.read().await.feedback.is_empty());
    }

    #[tokio::test]
    async fn native_submit_records_despite_caller_cancellation() {
        // Cancellation-shield regression, mirroring
        // `fork_persists_despite_caller_cancellation`: once `Steer` is
        // enqueued the adapter may consume the text regardless of caller
        // liveness, so the Delivered record + broadcast must not be tied to
        // the caller's future.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;

        let (go_tx, go_rx) = tokio::sync::oneshot::channel::<()>();
        let mut rx = rx;
        let fake_loop = tokio::spawn(async move {
            if let Some(ConnectionCommand::Steer { reply, .. }) = rx.recv().await {
                go_rx.await.ok(); // withhold the outcome until the caller is gone
                let _ = reply.send(Ok(SteerOutcome::Injected));
            }
            rx // keep the receiver alive
        });

        // Drive the submit under a short timeout: the shielded task enqueues
        // `Steer` and blocks on the withheld reply; the timeout DROPS this
        // caller future.
        let timed = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            mgr.submit_feedback("c1", "shielded note".into(), None),
        )
        .await;
        assert!(
            timed.is_err(),
            "caller must be dropped before the withheld outcome is delivered"
        );

        // Release the outcome: the DETACHED task records the note anyway.
        go_tx.send(()).ok();
        let _ = fake_loop.await;
        let state = mgr.get_state("c1").await.unwrap();
        let mut recorded = false;
        for _ in 0..200 {
            {
                let s = state.read().await;
                if s.feedback.len() == 1 && s.feedback[0].status == FeedbackStatus::Delivered {
                    recorded = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            recorded,
            "native delivery must be recorded despite caller cancellation"
        );
    }

    #[tokio::test]
    async fn native_only_session_accepts_notes_without_the_pull_tool() {
        // The native channel needs no MCP companion (that's the point):
        // eligibility is channel-aware, not tool-only.
        let mgr = ConnectionManager::new();
        let rx = mgr
            .insert_test_connection_live("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_native_steering_ready(&mgr, "c1").await;
        // feedback_tool_available stays false.
        let fake_loop = answer_steer(rx, Ok(SteerOutcome::Injected));
        let item = mgr.submit_feedback("c1", "no tool needed".into(), None).await.unwrap();
        assert_eq!(item.status, FeedbackStatus::Delivered);
        let _ = fake_loop.await;
    }

    // --- ask_user_question: register / answer / cancel -------------------

    fn q_spec() -> Vec<QuestionSpec> {
        vec![crate::acp::question::QuestionSpec {
            id: "qa".into(),
            question: "Which approach?".into(),
            header: "Approach".into(),
            multi_select: false,
            options: vec![
                crate::acp::question::QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                },
                crate::acp::question::QuestionOption {
                    label: "B".into(),
                    description: String::new(),
                },
            ],
            is_secret: false,
        }]
    }

    #[tokio::test]
    async fn register_then_answer_question_resolves_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cq", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_question("cq", q_spec())
            .await
            .expect("registered");
        // SessionState reflects the pending question for snapshot recovery.
        assert!(mgr
            .get_state("cq")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());

        let answer = crate::acp::question::QuestionAnswer {
            answers: vec![crate::acp::question::QuestionAnswerItem {
                question_id: "qa".into(),
                labels: vec!["A".into()],
            }],
            declined: false,
        };
        mgr.answer_question("cq", &reg.question_id, answer)
            .await
            .unwrap();

        // The blocked listener's receiver resolves with the self-describing
        // outcome (question text joined in).
        let outcome = reg.answer_rx.await.expect("answer delivered");
        assert!(!outcome.declined);
        assert_eq!(outcome.answers.len(), 1);
        assert_eq!(outcome.answers[0].question, "Which approach?");
        assert_eq!(outcome.answers[0].selected, vec!["A".to_string()]);
        // pending_question cleared after resolve.
        assert!(mgr
            .get_state("cq")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());

        // Idempotent: answering an already-resolved id is a no-op success.
        mgr.answer_question("cq", &reg.question_id, Default::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_question_clears_and_drops_sender() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cqx", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg = mgr.register_question("cqx", q_spec()).await.unwrap();
        mgr.cancel_question("cqx", &reg.question_id).await;
        // Dropping the sender surfaces to the parked listener as a recv error
        // (which it renders as a declined outcome).
        assert!(reg.answer_rx.await.is_err());
        assert!(mgr
            .get_state("cqx")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());
    }

    #[tokio::test]
    async fn cancel_questions_by_parent_drops_only_matching_connection() {
        // The run_connection teardown guard sweeps a tearing-down connection's
        // parked ask without touching other connections' pending questions.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("ca", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mgr.insert_test_connection("cb", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let reg_a = mgr.register_question("ca", q_spec()).await.unwrap();
        let reg_b = mgr.register_question("cb", q_spec()).await.unwrap();

        // Tear down only connection "ca".
        mgr.cancel_questions_by_parent("ca").await;

        // ca's parked listener is unblocked (sender dropped → recv error) and its
        // card cleared; cb is untouched.
        assert!(reg_a.answer_rx.await.is_err());
        assert!(mgr
            .get_state("ca")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_none());
        assert!(mgr
            .get_state("cb")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some());

        // cb still resolves normally afterwards.
        mgr.answer_question("cb", &reg_b.question_id, Default::default())
            .await
            .unwrap();
        assert!(reg_b.answer_rx.await.is_ok());
    }

    #[tokio::test]
    async fn register_then_answer_plan_approval_resolves_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pa", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_plan_approval("pa", "call-1".into(), "# Plan\n- step".into())
            .await
            .expect("registered");
        // SessionState reflects the pending approval for snapshot recovery.
        {
            let state = mgr.get_state("pa").await.unwrap();
            let guard = state.read().await;
            let pending = guard.pending_plan_approval.as_ref().expect("pending set");
            assert_eq!(pending.plan_markdown, "# Plan\n- step");
            assert_eq!(pending.tool_call_id, "call-1");
        }

        mgr.answer_plan_approval(
            "pa",
            &reg.approval_id,
            crate::acp::plan_approval::PlanApprovalAnswer {
                decision: crate::acp::plan_approval::PlanApprovalDecision::RequestChanges,
                feedback: Some("use SSE".into()),
            },
        )
        .await
        .unwrap();

        // The blocked ext handler's receiver resolves with the user's decision.
        let got = reg.answer_rx.await.expect("answer delivered");
        assert_eq!(
            got.decision,
            crate::acp::plan_approval::PlanApprovalDecision::RequestChanges
        );
        assert_eq!(got.feedback.as_deref(), Some("use SSE"));
        // pending_plan_approval cleared after resolve.
        assert!(mgr
            .get_state("pa")
            .await
            .unwrap()
            .read()
            .await
            .pending_plan_approval
            .is_none());

        // Idempotent: answering an already-resolved id is a no-op success.
        mgr.answer_plan_approval(
            "pa",
            &reg.approval_id,
            crate::acp::plan_approval::PlanApprovalAnswer {
                decision: crate::acp::plan_approval::PlanApprovalDecision::Approve,
                feedback: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn register_plan_approval_refuses_second_pending_on_same_connection() {
        // At most one approval per connection (the agent is blocked in exit_plan_mode).
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pa2", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let _reg = mgr
            .register_plan_approval("pa2", "c1".into(), "plan".into())
            .await
            .expect("first registers");
        assert!(mgr
            .register_plan_approval("pa2", "c2".into(), "plan2".into())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn cancel_plan_approvals_by_parent_drops_sender_and_clears() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("pax", AgentType::Grok, None, EventEmitter::Noop)
            .await;
        let reg = mgr
            .register_plan_approval("pax", "c".into(), "plan".into())
            .await
            .unwrap();
        mgr.cancel_plan_approvals_by_parent("pax").await;
        // Dropping the sender surfaces to the parked handler as a recv error
        // (which it renders as a disconnect reply — plan mode stays active).
        assert!(reg.answer_rx.await.is_err());
        assert!(mgr
            .get_state("pax")
            .await
            .unwrap()
            .read()
            .await
            .pending_plan_approval
            .is_none());
    }

    #[tokio::test]
    async fn compensate_clears_card_when_entry_drained_before_request_emit() {
        // Regression for the teardown event-ordering race: register inserts, the
        // sweep drains the entry, THEN register's QuestionRequest emit lands. The
        // post-emit presence check must emit a compensating QuestionResolved so no
        // client keeps a card with no live backend waiter, and signal decline.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cc", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let (state, emitter) = mgr.get_state_and_emitter("cc").await.unwrap();

        // Simulate register's QuestionRequest emit for an entry that has already
        // been drained (never inserted here): the card shows, nothing is parked.
        emit_with_state(
            &state,
            &emitter,
            AcpEvent::QuestionRequest {
                question_id: "q1".into(),
                questions: q_spec(),
            },
        )
        .await;
        assert!(state.read().await.pending_question.is_some(), "card shown");

        // Missing entry → compensate clears the card and reports decline.
        assert!(
            mgr.compensate_if_question_drained("q1", &state, &emitter)
                .await,
            "missing entry is compensated"
        );
        assert!(
            state.read().await.pending_question.is_none(),
            "compensating QuestionResolved cleared the card"
        );

        // A genuinely-parked entry is left alone (no false compensation).
        let reg = mgr.register_question("cc", q_spec()).await.unwrap();
        assert!(
            !mgr.compensate_if_question_drained(&reg.question_id, &state, &emitter)
                .await,
            "present entry is not compensated"
        );
        assert!(state.read().await.pending_question.is_some());
    }

    #[tokio::test]
    async fn register_question_unknown_connection_is_none() {
        let mgr = ConnectionManager::new();
        assert!(mgr.register_question("nope", q_spec()).await.is_none());
    }

    #[tokio::test]
    async fn second_concurrent_ask_is_refused_and_first_stays_answerable() {
        // A parallel/misbehaving client could fire two asks on one connection
        // before the first resolves. The single-slot card/snapshot can't hold
        // two, so the second is refused (None → declined) and the FIRST stays
        // intact and answerable — never orphaned.
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("cc2", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        let first = mgr
            .register_question("cc2", q_spec())
            .await
            .expect("first registers");
        // Second concurrent ask on the same connection is refused.
        assert!(
            mgr.register_question("cc2", q_spec()).await.is_none(),
            "second concurrent ask must be refused"
        );
        // The first is still the pending one and still answerable.
        let state = mgr.get_state("cc2").await.unwrap();
        assert_eq!(
            state.read().await.pending_question.as_ref().map(|p| p.question_id.clone()),
            Some(first.question_id.clone())
        );
        mgr.answer_question(
            "cc2",
            &first.question_id,
            crate::acp::question::QuestionAnswer {
                answers: vec![crate::acp::question::QuestionAnswerItem {
                    question_id: "qa".into(),
                    labels: vec!["A".into()],
                }],
                declined: false,
            },
        )
        .await
        .unwrap();
        assert!(first.answer_rx.await.is_ok(), "first ask resolves");
        // After resolve, a new ask is accepted again.
        assert!(mgr.register_question("cc2", q_spec()).await.is_some());
    }

    #[tokio::test]
    async fn read_pending_is_readonly_commit_marks_delivered() {
        let mgr = ConnectionManager::new();
        mgr.insert_test_connection("c1", AgentType::ClaudeCode, None, EventEmitter::Noop)
            .await;
        mark_feedback_ready(&mgr, "c1").await;
        let a = mgr.submit_feedback("c1", "a".into(), None).await.unwrap();
        let b = mgr.submit_feedback("c1", "b".into(), None).await.unwrap();

        // READ returns both pending notes (insert order) WITHOUT mutating state.
        let pending = mgr.read_pending_feedback("c1").await;
        let texts: Vec<&str> = pending.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b"]);
        // A second read still returns them — read is non-destructive, so an
        // abandoned (peer-closed) call leaves the notes retryable.
        assert_eq!(
            mgr.read_pending_feedback("c1")
                .await
                .len(),
            2
        );
        {
            let state = mgr.get_state("c1").await.unwrap();
            assert!(state
                .read()
                .await
                .feedback
                .iter()
                .all(|f| f.status == FeedbackStatus::Pending));
        }

        // COMMIT marks the named notes delivered.
        mgr.commit_feedback_delivered("c1", vec![a.id.clone(), b.id.clone()])
            .await;
        // Now READ returns nothing (delivered notes are filtered out).
        assert!(mgr
            .read_pending_feedback("c1")
            .await
            .is_empty());
        let state = mgr.get_state("c1").await.unwrap();
        assert!(state
            .read()
            .await
            .feedback
            .iter()
            .all(|f| f.status == FeedbackStatus::Delivered));

        // COMMIT is idempotent — re-committing already-delivered ids is a no-op.
        mgr.commit_feedback_delivered("c1", vec![a.id, b.id]).await;
    }

    #[tokio::test]
    async fn read_pending_missing_connection_returns_empty() {
        let mgr = ConnectionManager::new();
        assert!(mgr
            .read_pending_feedback("nope")
            .await
            .is_empty());
        // Commit on a missing connection is a safe no-op.
        mgr.commit_feedback_delivered("nope", vec!["x".into()]).await;
    }

