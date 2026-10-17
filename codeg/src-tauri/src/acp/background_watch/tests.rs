    use super::*;
    use std::io::Write;

    fn temp_session(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("session-1.jsonl")
    }

    fn write_lines(path: &PathBuf, lines: &[&str]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    /// Append raw bytes WITHOUT a newline — simulates a mid-flush fragment.
    fn append_raw(path: &PathBuf, chunk: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        write!(f, "{chunk}").unwrap();
    }

    /// Real-shape async sub-agent launch ack (structured `toolUseResult`
    /// sibling as captured from a live transcript on 2026-07-07).
    fn agent_ack(agent_id: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:46:14.514Z","uuid":"u-ack-{agent_id}","message":{{"role":"user","content":[{{"tool_use_id":"toolu_01","type":"tool_result","content":[{{"type":"text","text":"Async agent launched successfully. agentId: {agent_id}"}}]}}]}},"toolUseResult":{{"isAsync":true,"status":"async_launched","agentId":"{agent_id}","description":"Run pnpm build"}}}}"#
        )
    }

    /// Real-shape background shell ack (`toolUseResult.backgroundTaskId`).
    fn bash_ack(task_id: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:46:15.000Z","uuid":"u-bash-{task_id}","message":{{"role":"user","content":[{{"tool_use_id":"toolu_02","type":"tool_result","content":"Command running in background with ID: {task_id}."}}]}},"toolUseResult":{{"stdout":"","stderr":"","interrupted":false,"backgroundTaskId":"{task_id}"}}}}"#
        )
    }

    /// Real-shape `<task-notification>` completion record (string content).
    fn notification(task_id: &str, status: &str) -> String {
        let inner = format!(
            "<task-notification>\\n<task-id>{task_id}</task-id>\\n<tool-use-id>toolu_01</tool-use-id>\\n<status>{status}</status>\\n<summary>Agent \\\"Run pnpm build\\\" finished</summary>\\n<result>Build OK</result>\\n</task-notification>"
        );
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:47:00.000Z","uuid":"u-note-{task_id}","isSidechain":false,"message":{{"role":"user","content":"{inner}"}}}}"#
        )
    }

    fn assistant_text(uuid: &str, text: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-07-07T03:47:08.000Z","uuid":"{uuid}","message":{{"role":"assistant","model":"claude-sonnet-5","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    /// Real-shape `TaskOutput` result: a tool-result user record whose
    /// structured `toolUseResult.task` carries `task_id` + `status` (shape
    /// captured from a live transcript on 2026-07-08).
    fn taskoutput_result(task_id: &str, status: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:47:30.000Z","uuid":"u-to-{task_id}-{status}","message":{{"role":"user","content":[{{"tool_use_id":"toolu_out_{task_id}","type":"tool_result","content":[{{"type":"text","text":"<task_id>{task_id}</task_id> <status>{status}</status>"}}]}}]}},"toolUseResult":{{"retrieval_status":"success","task":{{"task_id":"{task_id}","task_type":"local_bash","status":"{status}","exitCode":0}}}}}}"#
        )
    }

    /// Real-shape `TaskStop` call (assistant tool_use naming the task via
    /// `task_id`).
    fn taskstop(task_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-07-07T03:47:31.000Z","uuid":"a-stop-{task_id}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_stop_{task_id}","name":"TaskStop","input":{{"task_id":"{task_id}"}}}}]}}}}"#
        )
    }

    fn user_prompt_array(uuid: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:48:00.000Z","uuid":"{uuid}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    /// Real-shape cron-fired prompt: `isMeta:true` with bare STRING content.
    fn cron_prompt(text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-07-07T03:49:00.000Z","uuid":"u-cron","isMeta":true,"userType":"external","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    /// Real-shape generated-title record. Claude Code writes it with NO
    /// timestamp and no message body (captured from a live transcript).
    fn ai_title(title: &str) -> String {
        format!(r#"{{"type":"ai-title","aiTitle":"{title}","sessionId":"s1"}}"#)
    }

    /// Real-shape user-set title record (`/rename`, `claude -n`, a fork).
    fn custom_title(title: &str) -> String {
        format!(r#"{{"type":"custom-title","customTitle":"{title}","sessionId":"s1"}}"#)
    }

    fn tick_now(ws: &mut WatchState, ledger: &PromptLedger) -> Option<AcpEvent> {
        ws.tick(ledger, "/tmp", "conn-test", false, false)
    }

    /// Like `tick_now`, but with the connection snapshotted as `Prompting` —
    /// for tests of the held-turn suppression filter (§3 of the 0.59 upgrade
    /// plan), which engages while the connection is prompting and, with no
    /// time limit, for as long afterward as no new turn has started.
    fn tick_prompting(ws: &mut WatchState, ledger: &PromptLedger) -> Option<AcpEvent> {
        ws.tick(ledger, "/tmp", "conn-test", true, false)
    }

    /// Like `tick_now`, but reporting the just-ended turn as having stopped
    /// abnormally (cancelled/refused/etc) — for tests of the early-release
    /// path that lets a held turn's launched ids stop being suppression-
    /// eligible immediately instead of waiting for the next turn.
    fn tick_abnormal_end(ws: &mut WatchState, ledger: &PromptLedger) -> Option<AcpEvent> {
        ws.tick(ledger, "/tmp", "conn-test", false, true)
    }

    fn unpack(
        event: AcpEvent,
    ) -> (Vec<MessageTurn>, u32, Vec<BackgroundSettledInfo>, u64) {
        match event {
            AcpEvent::BackgroundActivity {
                turns,
                outstanding,
                settled,
                watermark,
                ..
            } => (turns, outstanding, settled, watermark),
            other => panic!("expected BackgroundActivity, got {other:?}"),
        }
    }

    #[test]
    fn force_rotates_a_single_giant_turn_and_bounds_the_episode() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        // One out-of-turn initiator, then one giant turn: FORCE + 3 assistant
        // records with NO further boundary. Written and read in a single tick
        // batch to also exercise the same-batch episode-base tie-break.
        let mut lines: Vec<String> = vec![cron_prompt("iterate forever")];
        for i in 0..(FORCE_ROTATE_MESSAGES + 3) {
            lines.push(assistant_text(&format!("a-{i}"), "chunk"));
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(&path, &refs);

        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("turns event"));

        // The episode was re-based mid-turn: what remains buffered is only the
        // post-rotation fragment, never the whole giant turn.
        let buffered = ws.episode.as_ref().map(|e| e.acc.messages.len()).unwrap();
        assert!(
            buffered < FORCE_ROTATE_MESSAGES,
            "episode must be bounded after force-rotation, still holds {buffered}"
        );
        // Both namespaces surfaced this tick, under distinct (non-colliding)
        // bases even though both episodes were created within one batch.
        let bases: std::collections::HashSet<&str> = turns
            .iter()
            .map(|t| t.id.rsplit_once('-').expect("bg id shape").0)
            .collect();
        assert!(
            bases.len() >= 2,
            "expected pre- and post-rotation id namespaces, got {bases:?}"
        );
        assert_eq!(
            turns.len(),
            turns
                .iter()
                .map(|t| t.id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "turn ids must be unique across the forced rotation"
        );
    }

    fn epoch(ts: &str) -> std::time::SystemTime {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&chrono::Utc)
            .into()
    }

    #[test]
    fn baseline_offset_since_finds_first_record_at_or_after_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let first = agent_ack("agent1"); // timestamp 03:46:14.514Z
        write_lines(&path, &[&first, &notification("agent1", "completed")]); // 03:47:00Z

        // Epoch between the two records: boundary at the second line.
        assert_eq!(
            baseline_offset_since(&path, epoch("2026-07-07T03:46:30.000Z")),
            Some(first.len() as u64 + 1)
        );
        // Epoch before everything: whole file is ours.
        assert_eq!(
            baseline_offset_since(&path, epoch("2020-01-01T00:00:00Z")),
            Some(0)
        );
        // Epoch after everything: pure history — baseline after the last
        // COMPLETE line (== EOF here, the file ends with a newline).
        let full = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            baseline_offset_since(&path, epoch("2030-01-01T00:00:00Z")),
            Some(full)
        );
        // A trailing partial flush is a record being written NOW: the
        // fallback baseline must sit BEFORE it so it reconstructs.
        append_raw(&path, r#"{"type":"user","half"#);
        assert_eq!(
            baseline_offset_since(&path, epoch("2030-01-01T00:00:00Z")),
            Some(full)
        );
    }

    /// FORK layout regression. A fork copies the parent transcript (records keep
    /// their ORIGINAL, pre-fork timestamps) and writes fresh `queue-operation`
    /// metadata at the FILE HEAD stamped at fork time. That head record is
    /// post-epoch by timestamp but sits BEFORE the copied history by byte
    /// offset, so a boundary keyed on "first record at/after epoch" of ANY type
    /// would land at offset 0 and pull the entire copied history into the
    /// out-of-turn overlay — duplicating what the detail fetch already renders.
    /// The boundary must skip non-conversation records and land on the first
    /// genuinely-new turn.
    #[test]
    fn baseline_skips_fork_head_metadata_and_lands_on_the_first_new_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        // Fork-time metadata at the head (post-epoch stamp), then the copied
        // history (original pre-fork stamps), then the genuinely-new prompt.
        let queue_op = r#"{"type":"queue-operation","timestamp":"2026-07-07T03:50:00.100Z","uuid":"q-1"}"#;
        let copied_user = r#"{"type":"user","timestamp":"2026-07-07T03:46:00.000Z","uuid":"u-hi","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        let copied_asst = r#"{"type":"assistant","timestamp":"2026-07-07T03:46:05.000Z","uuid":"a-hi","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}]}}"#;
        let new_user = r#"{"type":"user","timestamp":"2026-07-07T03:50:10.000Z","uuid":"u-hello","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#;
        write_lines(&path, &[queue_op, copied_user, copied_asst, new_user]);

        // Fork epoch sits after the copied history but before the new turn. The
        // boundary must be the new turn's byte offset (past the head metadata
        // AND the copied history), never offset 0.
        let new_turn_offset = (queue_op.len() + 1) as u64
            + (copied_user.len() + 1) as u64
            + (copied_asst.len() + 1) as u64;
        assert_eq!(
            baseline_offset_since(&path, epoch("2026-07-07T03:50:00.000Z")),
            Some(new_turn_offset),
            "fork-time head metadata must not drag the copied history past the baseline"
        );
    }

    /// A new session's file can be discovered mid-flush of its FIRST record:
    /// no complete post-epoch line exists yet, and an EOF fallback would
    /// baseline past the fragment — its completing suffix then reads as
    /// unparseable garbage and the record (a launch ack here) is lost.
    #[test]
    fn adopt_with_trailing_partial_line_keeps_it_ahead_of_the_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        // Complete pre-epoch history line, then half of an ack record.
        write_lines(&path, &[&notification("older", "completed")]);
        let ack = agent_ack("agentY");
        let (head, tail) = ack.split_at(ack.len() / 2);
        append_raw(&path, head);

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        ws.epoch = Some(epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        // First tick buffers the fragment (no complete line — no event).
        assert!(tick_now(&mut ws, &ledger).is_none());

        // The flush completes: the reconstructed ack must account.
        append_raw(&path, tail);
        append_raw(&path, "\n");
        let (_, outstanding, ..) =
            unpack(tick_now(&mut ws, &ledger).expect("ack event"));
        assert_eq!(outstanding, 1, "mid-flush ack must survive discovery");
    }

    /// Production fork timing: the frontend fires its follow-up prompt the
    /// moment the fork resolves, so post-fork records land in the forked
    /// transcript BEFORE the polling watcher's next tick notices the session
    /// change. The re-arm epoch is therefore the instant the session id
    /// CHANGED (stamped by SessionStarted in session state), not the tick
    /// time — records written in that gap must still process, while the
    /// fork-copied history (original, pre-fork timestamps) stays skipped.
    #[test]
    fn post_fork_records_written_before_the_watcher_notices_still_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agentX")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger);
        write_lines(&path, &[&notification("agentX", "completed")]);
        let _ = tick_now(&mut ws, &ledger);

        // Fork at 03:50 (all copied history predates it), and the resume
        // record (timestamp 03:52) lands BEFORE the watcher re-arms.
        let forked = dir.path().join("session-2.jsonl");
        std::fs::copy(&path, &forked).unwrap();
        let send = r#"{"type":"assistant","timestamp":"2026-07-07T03:52:00.000Z","uuid":"a-send3","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_05","name":"SendMessage","input":{"to":"agentX","summary":"continue","message":"go on"}}]}}"#;
        write_lines(&forked, &[send]);

        // The watcher's delayed tick finally observes the fork: epoch is the
        // session-change instant, so the already-written resume record is
        // ahead of the baseline and re-arms the accounting.
        ws.rearm("s2".into(), epoch("2026-07-07T03:50:00.000Z"));
        ws.adopt_file(forked.clone());
        let (_, outstanding, ..) =
            unpack(tick_now(&mut ws, &ledger).expect("resume event"));
        assert_eq!(
            outstanding, 1,
            "a resume written before the watcher noticed the fork must re-arm"
        );
    }

    /// `settled_ids` must survive a fork//re-resume re-arm: a post-fork
    /// `SendMessage(to: <id>)` resumes a sub-agent that settled BEFORE the
    /// fork, and missing the re-arm leaves outstanding at 0 — closing the
    /// tab could then kill the resumed background work.
    #[test]
    fn settled_ids_survive_rearm_so_post_fork_resume_rearms() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agentX")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger);
        write_lines(&path, &[&notification("agentX", "completed")]);
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).unwrap());
        assert_eq!(outstanding, 0);

        // Fork: new session id, new transcript file (history copied with
        // ORIGINAL timestamps — all before the re-arm epoch).
        let forked = dir.path().join("session-2.jsonl");
        std::fs::copy(&path, &forked).unwrap();
        ws.rearm("s2".into(), epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(forked.clone());
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "copied history must not re-account"
        );

        let send = r#"{"type":"assistant","timestamp":"2026-07-07T03:52:00.000Z","uuid":"a-send2","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_04","name":"SendMessage","input":{"to":"agentX","summary":"continue","message":"go on"}}]}}"#;
        write_lines(&forked, &[send]);
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).unwrap());
        assert_eq!(outstanding, 1, "post-fork resume must re-arm");

        write_lines(&forked, &[&notification("agentX", "completed")]);
        let (_, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).unwrap());
        assert_eq!(outstanding, 0);
        assert_eq!(settled.len(), 1);
    }

    fn clear_command(session_id: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","uuid":"u-clear","sessionId":"{session_id}","message":{{"role":"user","content":"<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"}}}}"#
        )
    }

    fn dated_user(session_id: &str, uuid: &str, ts: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","uuid":"{uuid}","sessionId":"{session_id}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn dated_assistant(session_id: &str, uuid: &str, ts: &str, text: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","uuid":"{uuid}","sessionId":"{session_id}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    /// `/clear` keeps the ACP session id but writes a NEW sibling jsonl.
    /// The watcher must leave the frozen file and tail the successor so
    /// post-clear out-of-turn records (and the reopen reader, via the
    /// pending transcript id) are not stranded on a file nobody writes to.
    #[test]
    fn clear_rollover_adopts_the_new_transcript_while_acp_id_stays() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old-sess.jsonl");
        let new = dir.path().join("new-sess.jsonl");
        write_lines(
            &old,
            &[
                &dated_user("old-sess", "u1", "2026-09-01T10:00:00Z", "hello before"),
                &dated_assistant("old-sess", "a1", "2026-09-01T10:00:05Z", "hi before"),
            ],
        );
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("old-sess", old.clone());
        let _ = tick_now(&mut ws, &ledger);

        write_lines(
            &new,
            &[
                &clear_command("new-sess", "2026-09-01T10:00:06Z"),
                &dated_user("new-sess", "u2", "2026-09-01T10:01:00Z", "hello after"),
                &cron_prompt("autonomous after clear"),
                &dated_assistant("new-sess", "a2", "2026-09-01T10:02:00Z", "hi after"),
            ],
        );

        // The rollover is only ours to take because THIS connection sent the
        // `/clear` that caused it.
        ledger.record_prompt_blocks(&[crate::acp::types::PromptInputBlock::Text {
            text: "/clear".to_string(),
        }]);
        let event = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.file.as_ref(),
            Some(&new),
            "watcher must follow the /clear successor file"
        );
        assert_eq!(
            ws.session_id.as_deref(),
            Some("old-sess"),
            "ACP session id is unchanged; overlay mapping still uses it"
        );
        assert_eq!(
            ws.pending_transcript_id.as_deref(),
            Some("new-sess"),
            "the new transcript uuid is what conversation.external_id must bind"
        );

        let (turns, ..) = unpack(event.expect("post-clear tail must produce activity"));
        let blob = serde_json::to_string(&turns).unwrap();
        // Both halves of the post-clear exchange, not `a || b`: either one
        // alone would also be satisfied by a watcher that adopted the file
        // and then read only part of it.
        assert!(
            blob.contains("hello after") && blob.contains("hi after"),
            "the whole post-clear tail must surface, not just one record: {blob}"
        );
        assert!(
            !blob.contains("hello before"),
            "pre-clear content stays on the abandoned file"
        );
    }

    /// Every conversation opened on one folder shares a project directory, so
    /// a `/clear` successor written by a DIFFERENT session sits right next to
    /// ours and looks identical on disk: fresh uuid, `/clear` head, starting
    /// when our own file happens to have gone quiet. Adopting it would tail a
    /// stranger's transcript and re-point this row's `external_id` at a
    /// session another row owns. The only thing that separates the two cases
    /// is whether WE asked for the clear.
    #[test]
    fn clear_rollover_ignores_a_sibling_session_we_did_not_clear() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old-sess.jsonl");
        write_lines(
            &old,
            &[
                &dated_user("old-sess", "u1", "2026-09-01T10:00:00Z", "hello before"),
                &dated_assistant("old-sess", "a1", "2026-09-01T10:00:05Z", "hi before"),
            ],
        );
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("old-sess", old.clone());
        let _ = tick_now(&mut ws, &ledger);

        // A neighbouring conversation clears six seconds later — well inside
        // every timing tolerance the detector has.
        write_lines(
            &dir.path().join("other-sess.jsonl"),
            &[
                &clear_command("other-sess", "2026-09-01T10:00:06Z"),
                &dated_user("other-sess", "u2", "2026-09-01T10:01:00Z", "not ours"),
            ],
        );

        let event = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.file.as_ref(),
            Some(&old),
            "a rollover this connection never asked for is not ours to adopt"
        );
        assert!(ws.pending_transcript_id.is_none(), "no re-point may be emitted");
        assert!(
            event.is_none(),
            "and none of the stranger's records may surface as our activity"
        );
    }

    /// One connection outlives a fork, and the watcher learns of the session
    /// switch up to a poll late — so at re-arm time an unconsumed `/clear`
    /// may belong to either side of it. The session's own change instant is
    /// what separates them: a `/clear` typed into the new session before the
    /// watcher noticed the switch still has a successor to adopt.
    #[test]
    fn clear_rollover_expectation_survives_a_rearm_it_postdates() {
        let ledger = PromptLedger::shared();

        let changed_a_second_ago = std::time::SystemTime::now() - Duration::from_secs(1);
        ledger.note_clear_for_test();
        ledger.expire_clear_request_before(Some(changed_a_second_ago));
        assert!(
            ledger.clear_rollover_expected(),
            "a /clear typed after the session changed belongs to the new session"
        );

        let changes_in_a_second = std::time::SystemTime::now() + Duration::from_secs(1);
        ledger.expire_clear_request_before(Some(changes_in_a_second));
        assert!(
            !ledger.clear_rollover_expected(),
            "one that predates the switch belonged to the session being left"
        );

        ledger.note_clear_for_test();
        ledger.expire_clear_request_before(None);
        assert!(
            !ledger.clear_rollover_expected(),
            "with nothing to compare against, a missed adoption beats a wrong one"
        );

        // The comparison crosses the wall clock, and a backward step under it
        // would otherwise make the OLD session's expectation look newer than
        // the switch. A gap no poll lag can explain resolves the safe way.
        ledger.note_clear_for_test();
        let changed_long_before = std::time::SystemTime::now() - REARM_CLEAR_GRACE * 2;
        ledger.expire_clear_request_before(Some(changed_long_before));
        assert!(
            !ledger.clear_rollover_expected(),
            "a gap wider than the watcher's poll lag is a moved clock, not a late /clear"
        );
    }

    /// The successor's `/clear` record and the predecessor's last record are
    /// written by one process milliseconds apart, and the CLI's timestamps are
    /// not ordered across the two files (measured on 2.1.270: the successor's
    /// caveat record is stamped 6ms BEFORE the command record that follows
    /// it). A detector that demands `successor >= predecessor` to the
    /// millisecond therefore drops real rollovers — and drops them for good,
    /// since every later tick re-compares the same two files.
    #[test]
    fn clear_rollover_tolerates_a_successor_stamped_just_before_us() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old-sess.jsonl");
        let new = dir.path().join("new-sess.jsonl");
        write_lines(
            &old,
            &[
                &dated_user("old-sess", "u1", "2026-09-01T10:00:00Z", "hello before"),
                &dated_assistant("old-sess", "a1", "2026-09-01T10:00:05.900Z", "hi before"),
            ],
        );
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("old-sess", old.clone());
        let _ = tick_now(&mut ws, &ledger);

        write_lines(
            &new,
            &[
                // 400ms BEFORE the last record of the file it replaces.
                &clear_command("new-sess", "2026-09-01T10:00:05.500Z"),
                &dated_user("new-sess", "u2", "2026-09-01T10:01:00Z", "hello after"),
            ],
        );
        ledger.note_clear_for_test();

        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.file.as_ref(),
            Some(&new),
            "a few hundred ms of clock skew between the two files is normal"
        );
    }

    /// A `_session/steering` injection reaches the agent outside
    /// `session/prompt`, but the CLI still writes it to the transcript — as a
    /// user record when it runs as a turn of its own, the shape here, and as an
    /// attachment when it is folded into the running turn (see
    /// `a_steer_folded_into_the_running_turn_classifies_foreground`) — which
    /// starts a new turn as far as `group_into_turns` is concerned. Since
    /// claude-agent-acp #958 the owning prompt stays in
    /// flight across the steered work, so every update of that turn is
    /// already streaming over the wire; surfacing it as overlay activity too
    /// double-renders it and shuffles the transcript as the upserts land
    /// (observed live before the `Steer` arm fingerprinted its text).
    #[test]
    fn steered_message_classifies_foreground_like_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        ledger.record_text("build a test page");

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        // The foreground prompt and its first reply.
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "build a test page"),
                &assistant_text("a1", "working"),
            ],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "the codeg-sent prompt classifies foreground"
        );

        // Mid-turn steer: the connection is STILL prompting, and the arm
        // fingerprinted the injected text when the adapter answered
        // `injected`.
        ledger.record_text("make it cyberpunk");
        write_lines(
            &path,
            &[
                &user_prompt_array("u2", "make it cyberpunk"),
                &assistant_text("a2", "restyling"),
            ],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "a steered turn is wire-rendered — it must not surface as overlay activity"
        );

        // The fingerprint was consumed exactly once: an autonomous re-fire of
        // the same text later is still background and must surface.
        write_lines(
            &path,
            &[
                &cron_prompt("make it cyberpunk"),
                &assistant_text("a3", "again"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("turns event"));
        assert!(
            !turns.is_empty(),
            "same-text refire must surface — the steer's entry was consumed, not left standing"
        );
    }

    /// Real-shape message folded into the running turn (CLI 2.1.293): a
    /// `queued_command` attachment rather than a user record, with no
    /// `promptId`.
    fn folded_prompt(uuid: &str, text: &str) -> String {
        format!(
            r#"{{"type":"attachment","timestamp":"2026-07-07T03:48:30.000Z","uuid":"{uuid}","renderedRole":"system","attachment":{{"type":"queued_command","prompt":[{{"type":"text","text":"{text}"}}],"source_uuid":"s-{uuid}","delivery_id":"d-{uuid}","commandMode":"prompt","origin":{{"kind":"human"}},"timestamp":"2026-07-07T03:48:30.000Z","humanTurn":true}}}}"#
        )
    }

    /// A steer that lands while a tool runs is folded into the running turn and
    /// recorded as an attachment. It matches the ledger like the user-record
    /// shape and consumes its fingerprint. Left standing, the entry took an
    /// autonomous re-fire of the same words within the ledger's lifetime for
    /// codeg's own, and that turn never surfaced.
    #[test]
    fn a_steer_folded_into_the_running_turn_classifies_foreground() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("build a test page");
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "build a test page"),
                &assistant_text("a1", "working"),
            ],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(event.is_none() || unpack(event.unwrap()).0.is_empty());

        ledger.record_text("make it cyberpunk");
        write_lines(
            &path,
            &[
                &folded_prompt("q1", "make it cyberpunk"),
                &assistant_text("a2", "restyling"),
            ],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "the steered work is wire-rendered — it must not surface as overlay activity"
        );

        write_lines(
            &path,
            &[
                &cron_prompt("make it cyberpunk"),
                &assistant_text("a3", "again"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("turns event"));
        assert!(
            !turns.is_empty(),
            "same-text refire must surface — the folded steer consumed its entry"
        );
    }

    /// #893 once claude-agent-acp answers a folded prompt (0.87.0+): a
    /// background task's notification starts a turn Claude Code runs on its
    /// own, which surfaces as overlay activity, and the user's prompt is folded
    /// into that turn. From there the wire renders the turn as the reply to the
    /// prompt, so the episode closes at the attachment and the reply stays off
    /// the overlay rather than rendering twice.
    #[test]
    fn a_prompt_folded_into_an_autonomous_turn_takes_the_rest_off_the_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(
            &path,
            &[
                &notification("bm1", "completed"),
                &assistant_text("a1", "checking the result"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("the autonomous turn surfaces"));
        assert!(serde_json::to_string(&turns)
            .unwrap()
            .contains("checking the result"));

        ledger.record_text("question B");
        write_lines(
            &path,
            &[
                &folded_prompt("q1", "question B"),
                &assistant_text("a2", "answer to B"),
            ],
        );
        let blob = tick_prompting(&mut ws, &ledger)
            .map(|event| serde_json::to_string(&unpack(event).0).unwrap())
            .unwrap_or_default();
        assert!(
            !blob.contains("question B") && !blob.contains("answer to B"),
            "the folded prompt and its reply are the wire's: {blob}"
        );
        assert!(matches!(ws.mode, Mode::Foreground));
    }

    /// A folded message carries no `promptId`, so matching one leaves no
    /// submission to group by. An initiator that lands before the reply — a
    /// cron prompt here, which carries no id either — is out-of-turn as usual,
    /// not swallowed as part of the folded message's submission.
    #[test]
    fn an_initiator_after_a_folded_prompt_is_not_taken_for_its_submission() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("question B");
        write_lines(
            &path,
            &[
                &folded_prompt("q1", "question B"),
                &cron_prompt("scheduled check"),
                &assistant_text("a1", "cron reply"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("the cron turn surfaces"));
        let blob = serde_json::to_string(&turns).unwrap();
        assert!(blob.contains("cron reply"), "{blob}");
        assert!(!blob.contains("question B"), "{blob}");
    }

    /// A folded message the ledger does not hold continues whatever turn is
    /// running. The steer arm records a steer only once the adapter answers
    /// `injected`, so the CLI can write the attachment first: inside a
    /// codeg-sent turn the rest of that turn stays the wire's instead of being
    /// drawn in the overlay too. Inside an overlay episode it is part of the
    /// episode, drawn as the user turn it is.
    #[test]
    fn a_folded_message_the_ledger_does_not_hold_continues_the_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("build a test page");
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "build a test page"),
                &assistant_text("a1", "working"),
                &folded_prompt("q1", "make it cyberpunk"),
                &assistant_text("a2", "restyling"),
            ],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "a steer the ledger got late must not pull the turn into the overlay"
        );
        assert!(matches!(ws.mode, Mode::Foreground));

        write_lines(
            &path,
            &[
                &notification("bm1", "completed"),
                &assistant_text("a3", "checking"),
                &folded_prompt("q2", "typed elsewhere"),
                &assistant_text("a4", "reply elsewhere"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("overlay activity"));
        let blob = serde_json::to_string(&turns).unwrap();
        assert!(
            blob.contains("checking")
                && blob.contains("typed elsewhere")
                && blob.contains("reply elsewhere"),
            "{blob}"
        );
    }

    /// Only a message the user typed has text to match: an image sent on its
    /// own has none (an image-only prompt records no fingerprint either), and
    /// the other `queued_command` attachments are not the user's. None of them
    /// initiates a turn.
    #[test]
    fn only_a_folded_message_the_user_typed_has_text_to_match() {
        let image_only: serde_json::Value = serde_json::from_str(
            r#"{"type":"attachment","uuid":"q1","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"humanTurn":true,"prompt":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"QUJD"}}]}}"#,
        )
        .unwrap();
        let blank: serde_json::Value = serde_json::from_str(
            r#"{"type":"attachment","uuid":"q2","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"  "}}"#,
        )
        .unwrap();
        let notification: serde_json::Value = serde_json::from_str(
            r#"{"type":"attachment","uuid":"q3","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>\n<task-id>b1</task-id>\n</task-notification>"}}"#,
        )
        .unwrap();
        let typed: serde_json::Value =
            serde_json::from_str(&folded_prompt("q4", "make it cyberpunk")).unwrap();
        for record in [&image_only, &blank, &notification] {
            assert_eq!(folded_prompt_text(record), None, "{record}");
        }
        assert_eq!(
            folded_prompt_text(&typed).as_deref(),
            Some("make it cyberpunk")
        );
        for record in [&image_only, &blank, &notification, &typed] {
            assert_eq!(turn_initiator_text(record), None, "{record}");
        }
    }

    /// A slash command sent from codeg writes MORE than its own record: the
    /// command, then `<local-command-stdout>`, then (for `/goal`) the `isMeta`
    /// STRING instruction Claude Code injects for the model — and only then the
    /// reply. Those side records are user records carrying text, so
    /// `turn_initiator_text` reads them as initiators and the ledger, already
    /// emptied by the command itself, has nothing to match them against. That
    /// flipped the watcher to Background mid-turn and the wire-rendered reply
    /// came back as an overlay copy — the reported `/goal` bug, where the whole
    /// answer rendered twice while streaming.
    #[test]
    fn a_commands_own_side_records_do_not_reopen_the_turn_as_background() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        ledger.record_text("/goal build a test page");

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        let command = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","promptId":"p1","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>build a test page</command-args>"}}"#;
        let stdout = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.100Z","uuid":"u-out","promptId":"p1","message":{"role":"user","content":"<local-command-stdout>Goal set: build a test page</local-command-stdout>"}}"#;
        let hook = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.200Z","uuid":"u-hook","promptId":"p1","isMeta":true,"userType":"external","message":{"role":"user","content":"A session-scoped Stop hook is now active with condition: build a test page."}}"#;
        write_lines(
            &path,
            &[command, stdout, hook, &assistant_text("a1", "On it.")],
        );
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "the wire renders this turn — its own submission records must not \
             surface an overlay copy of the reply"
        );

        // The window closes at the model's first record, not at the turn's end:
        // a genuinely autonomous initiator arriving after it still surfaces,
        // even though the connection is STILL prompting.
        write_lines(
            &path,
            &[
                &cron_prompt("keep going"),
                &assistant_text("a2", "resuming"),
            ],
        );
        let (turns, ..) =
            unpack(tick_prompting(&mut ws, &ledger).expect("turns event"));
        assert!(
            !turns.is_empty(),
            "an autonomous initiator after the reply is out-of-turn as before"
        );
    }

    /// The command record is the only one the ledger can match, and its
    /// initiator text is REBUILT from command tags — `slash_command_display`
    /// joins the name and the trimmed args with a single space, whatever the
    /// sender typed. The composer inserts a space after a command badge, so a
    /// sender who types their own lands two, and the rebuilt text no longer
    /// starts with the fingerprint. That miss leaves the submission window
    /// unarmed and every following side record classifies out-of-turn, which
    /// is the same duplicated `/goal` turn as above by a different route.
    #[test]
    fn a_command_matches_the_ledger_despite_a_rebuilt_separator() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        // As SENT: two spaces after the command badge.
        ledger.record_text("/goal  build a test page");

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        // As PERSISTED: the CLI trims the args, so the display form rebuilds
        // with one space.
        let command = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","promptId":"p1","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-args>build a test page</command-args>"}}"#;
        let hook = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.200Z","uuid":"u-hook","promptId":"p1","isMeta":true,"userType":"external","message":{"role":"user","content":"A session-scoped Stop hook is now active with condition: build a test page."}}"#;
        write_lines(&path, &[command, hook, &assistant_text("a1", "On it.")]);
        let event = tick_prompting(&mut ws, &ledger);
        assert!(
            event.is_none() || unpack(event.unwrap()).0.is_empty(),
            "the wire renders this turn — a rebuilt separator must not turn it \
             into an overlay copy"
        );
    }

    #[test]
    fn ledger_normalizes_only_a_reconstructed_command_separator() {
        let ledger = PromptLedger::shared();
        ledger.record_text("/goal  build  a test page");
        assert!(
            !ledger.consume_matching(&TurnInitiatorText::ReconstructedSlashCommand(
                "/goal build a test page".into()
            )),
            "whitespace inside the arguments remains significant"
        );
        assert!(
            ledger.consume_matching(&TurnInitiatorText::ReconstructedSlashCommand(
                "/goal build  a test page".into()
            ))
        );
        assert!(
            !ledger.consume_matching(&TurnInitiatorText::ReconstructedSlashCommand(
                "/goal build  a test page".into()
            )),
            "a reconstructed match consumes the entry exactly once"
        );

        let ledger = PromptLedger::shared();
        ledger.record_text("build  a test page");
        assert!(
            !ledger.consume_matching(&TurnInitiatorText::Verbatim("build a test page".into())),
            "ordinary prompt whitespace must remain byte-for-byte significant"
        );
        assert!(ledger.consume_matching(&TurnInitiatorText::Verbatim("build  a test page".into())));

        let ledger = PromptLedger::shared();
        ledger.record_text("/goal  build");
        assert!(
            !ledger.consume_matching(&TurnInitiatorText::ReconstructedSlashCommand(
                "/goal builder".into()
            )),
            "reconstructed command arguments do not use the verbatim prefix fallback"
        );
        assert!(
            ledger.consume_matching(&TurnInitiatorText::ReconstructedSlashCommand(
                "/goal build".into()
            ))
        );
    }

    /// The window is scoped by SUBMISSION, not by time: an autonomous prompt
    /// that lands in the same interval — after the ledger match, before the
    /// model's first record — carries a different `promptId` and must still
    /// open its episode. Without this the fix would trade a recoverable
    /// duplicate for a turn missing from the live view.
    #[test]
    fn a_foreign_submission_inside_the_window_still_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        ledger.record_text("/goal build a test page");

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        let command = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","promptId":"p1","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-args>build a test page</command-args>"}}"#;
        // Same shape as the hook injection above, foreign submission id.
        let cron = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.200Z","uuid":"u-cron2","promptId":"p2","isMeta":true,"userType":"external","message":{"role":"user","content":"iterate forever"}}"#;
        write_lines(
            &path,
            &[command, cron, &assistant_text("a1", "Working on it.")],
        );
        let (turns, ..) =
            unpack(tick_prompting(&mut ws, &ledger).expect("turns event"));
        assert!(
            !turns.is_empty(),
            "a different submission is not this one's side record"
        );
    }

    /// The other initiator the window may not swallow: an async sub-agent's
    /// `<task-notification>`. It settles on its own schedule and IS stamped with
    /// the in-flight submission's `promptId`, so only the explicit exemption
    /// keeps it out-of-turn. Its follow-up has no other live rendering path —
    /// the wire never carries it — so suppressing it would lose the turn until
    /// the next detail refetch.
    #[test]
    fn a_task_notification_inside_the_window_still_opens_an_episode() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agentA")]);
        let ledger = PromptLedger::shared();
        ledger.record_text("/goal build a test page");

        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        // The launch is observed while NOT prompting, so agentA is not eligible
        // for the held-turn suppression that would hide it for other reasons.
        let _ = tick_now(&mut ws, &ledger);

        let command = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","promptId":"p1","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-args>build a test page</command-args>"}}"#;
        let stdout = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.100Z","uuid":"u-out","promptId":"p1","message":{"role":"user","content":"<local-command-stdout>Goal set: build a test page</local-command-stdout>"}}"#;
        let note = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.300Z","uuid":"u-note-agentA","promptId":"p1","isSidechain":false,"message":{"role":"user","content":"<task-notification>\n<task-id>agentA</task-id>\n<tool-use-id>toolu_01</tool-use-id>\n<status>completed</status>\n<summary>Agent finished</summary>\n<result>Build OK</result>\n</task-notification>"}}"#;
        write_lines(
            &path,
            &[
                command,
                stdout,
                // Settles BEFORE the foreground turn has written anything.
                note,
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        let (turns, ..) =
            unpack(tick_prompting(&mut ws, &ledger).expect("settle event"));
        // Not just "an episode opened": the settlement's own follow-up is what
        // has nowhere else to render, so it must be IN the emitted turn.
        assert!(
            turns.iter().any(|t| t.blocks.iter().any(|b| matches!(
                b,
                crate::models::message::ContentBlock::Text { text } if text.contains("Build finished cleanly")
            ))),
            "the notification's follow-up must render in the overlay"
        );
    }

    /// The window must never outlive the turn that opened it. Both resets are
    /// asserted directly, because the `promptId` scoping makes their effect
    /// invisible from the outside in all but contrived transcripts: the model's
    /// first record closes it, and — for a turn that ends without ever writing
    /// one — any non-prompting tick does. The reset is level-triggered on
    /// purpose: an EDGE can be missed entirely (a whole turn can start and
    /// finish between two polls, and a tick's own lines re-arm the window AFTER
    /// the edge was handled), which would strand it set forever.
    #[test]
    fn submission_window_is_closed_by_the_reply_and_by_any_idle_tick() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        ledger.record_text("/goal build a test page");
        ledger.record_text("/goal try again");

        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));

        let command = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","promptId":"p1","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-args>build a test page</command-args>"}}"#;
        write_lines(&path, &[command, &assistant_text("a1", "On it.")]);
        let _ = tick_prompting(&mut ws, &ledger);
        assert!(
            !ws.foreground_awaiting_reply,
            "the model's first record closes the window"
        );

        // A second submission that never answers: armed while prompting…
        let retry = r#"{"type":"user","timestamp":"2026-07-07T03:51:00.000Z","uuid":"u-cmd2","promptId":"p2","message":{"role":"user","content":"<command-name>/goal</command-name>\n<command-args>try again</command-args>"}}"#;
        write_lines(&path, &[retry]);
        let _ = tick_prompting(&mut ws, &ledger);
        assert!(ws.foreground_awaiting_reply, "armed by the ledger match");
        // …and closed by the first tick that observes the connection idle,
        // whether or not that tick is the falling edge.
        let _ = tick_now(&mut ws, &ledger);
        assert!(
            !ws.foreground_awaiting_reply,
            "an idle tick closes a window no reply will ever close"
        );
    }

    /// The Critical arm-gap regression: a brand-new session's file (and its
    /// first prompt + launch ack) can exist BEFORE the watcher's first
    /// successful discovery — SessionStarted lags file creation by seconds.
    /// Those records must still be accounted and ledger-consumed; blindly
    /// baselining at EOF dropped them (outstanding never armed, and the
    /// unconsumed fingerprint could swallow a later same-text cron refire).
    #[test]
    fn pre_discovery_records_are_accounted_and_ledger_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        ledger.record_text("do the thing");

        // On disk before discovery: codeg's first prompt, the reply, an ack.
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "do the thing"),
                &assistant_text("a1", "launching"),
                &agent_ack("agentX"),
            ],
        );

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        // Spawn predates session creation for a new session.
        ws.epoch = Some(epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        let (turns, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("accounting event"));
        assert_eq!(outstanding, 1, "pre-discovery ack must register");
        assert!(settled.is_empty());
        assert!(
            turns.is_empty(),
            "the codeg-sent prompt classifies foreground — the wire renders it"
        );

        // Its fingerprint was consumed, so a same-text out-of-turn refire
        // (cron//loop) classifies as background and surfaces.
        write_lines(
            &path,
            &[&cron_prompt("do the thing"), &assistant_text("a2", "pass")],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("turns event"));
        assert!(
            !turns.is_empty(),
            "same-text refire must surface — a stale ledger entry would swallow it"
        );
    }

    #[test]
    fn resume_baselines_at_eof_and_skips_historical_acks() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let ledger = PromptLedger::shared();
        // Historical, never-settled ack from a previous run of this session.
        write_lines(&path, &[&agent_ack("stale-old")]);

        let mut ws = WatchState::new();
        ws.session_id = Some("s1".into());
        // Resume: the watch armed long after that history was written.
        ws.epoch = Some(epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "pure history yields no event"
        );

        // Only appended records are processed; the stale ack never registers.
        write_lines(
            &path,
            &[&cron_prompt("new pass"), &assistant_text("a9", "hi")],
        );
        let (turns, outstanding, ..) =
            unpack(tick_now(&mut ws, &ledger).expect("turns event"));
        assert_eq!(outstanding, 0, "historical ack must NOT register");
        assert_eq!(turns.len(), 1);
    }

    #[test]
    fn acks_register_outstanding_without_rendering_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&agent_ack("agent1"), &bash_ack("bash1")]);
        let (turns, outstanding, settled, watermark) =
            unpack(tick_now(&mut ws, &ledger).expect("accounting event"));
        assert!(turns.is_empty(), "acks are tool-result records, not turns");
        assert_eq!(outstanding, 2);
        assert!(settled.is_empty());
        assert!(watermark > 0);

        // Unchanged file → stat-gated, no event.
        assert!(tick_now(&mut ws, &ledger).is_none());
    }

    /// A background shell re-observed via a repeat `BashOutput`-style poll
    /// (the identical `backgroundTaskId` shape appearing again) must not
    /// reset its `started_at` — a blind `insert` would restart the max-age
    /// clock on every poll, letting an actively-polled-but-actually-finished
    /// shell pin the connection alive indefinitely. `entry().or_insert_with()`
    /// only sets `started_at` on the FIRST observation.
    #[test]
    fn repeat_shell_observation_does_not_reset_started_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&bash_ack("shellA")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger);
        let first_seen = ws.tasks.get("shellA").expect("registered").started_at;

        write_lines(&path, &[&bash_ack("shellA")]); // a repeat poll of the same shell
        let _ = tick_now(&mut ws, &ledger);
        let second_seen = ws.tasks.get("shellA").expect("still tracked").started_at;

        assert_eq!(
            first_seen, second_seen,
            "started_at must reflect first-seen (launch), not reset on a repeat observation"
        );
    }

    #[test]
    fn notification_settles_and_surfaces_the_response_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger); // consume the ack

        write_lines(
            &path,
            &[
                &notification("agent1", "completed"),
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert_eq!(outstanding, 0);
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].task_id, "agent1");
        assert_eq!(settled[0].status, "completed");
        assert_eq!(
            settled[0].summary.as_deref(),
            Some("Agent \"Run pnpm build\" finished")
        );
        // The notification record itself strips to nothing; the assistant
        // response is the rendered out-of-turn content.
        assert_eq!(turns.len(), 1);
        assert!(turns[0].id.starts_with("bg-"));
    }

    /// Real-shape notification of a task that settled while a turn ran (CLI
    /// 2.1.293): folded into that turn as a `queued_command` attachment, not a
    /// user record.
    fn folded_notification(task_id: &str) -> String {
        let inner = format!(
            "<task-notification>\\n<task-id>{task_id}</task-id>\\n<tool-use-id>toolu_01</tool-use-id>\\n<status>completed</status>\\n<summary>Agent \\\"Run pnpm build\\\" finished</summary>\\n<result>Build OK</result>\\n</task-notification>"
        );
        format!(
            r#"{{"type":"attachment","timestamp":"2026-07-07T03:47:00.000Z","uuid":"q-note-{task_id}","attachment":{{"type":"queued_command","prompt":"{inner}","commandMode":"task-notification","origin":{{"kind":"task-notification","producer":"session-task"}},"timestamp":"2026-07-07T03:47:00.000Z"}}}}"#
        )
    }

    /// Most tasks settle while a turn is running, and their notification is
    /// folded into it. That still settles the count — it used to keep the
    /// connection exempt from the idle sweeps until the keep-alive expired —
    /// but, like a `TaskOutput` collection, announces nothing: the agent takes
    /// the result inline. Nor does it start a turn.
    #[test]
    fn a_notification_folded_into_a_turn_settles_the_count_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1"), &bash_ack("bash1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).expect("launch event"));
        assert_eq!(outstanding, 2);

        write_lines(
            &path,
            &[
                &folded_notification("agent1"),
                &folded_notification("bash1"),
            ],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_prompting(&mut ws, &ledger).expect("settle event"));
        assert_eq!(outstanding, 0);
        assert!(settled.is_empty());
        assert!(turns.is_empty());
        assert!(matches!(ws.mode, Mode::Foreground));
        // A settled sub-agent can still be resumed and re-armed.
        assert!(ws.settled_ids.contains("agent1"));
    }

    /// A `<task-notification>`
    /// follow-up for an id THIS turn launched, arriving while the connection
    /// is still `Prompting` (claude-agent-acp v0.59.0's #870 holds the turn
    /// open for its own spawned sub-agents), is already rendering on the
    /// wire — so the OVERLAY turn for it must be suppressed. The `settled`
    /// entry, by contrast, MUST still flow: the frontend needs it to flip the
    /// launch card (which it does in-memory, so it can't double-render), and it
    /// carries the launching `tool_use_id` + `<result>` for exactly that.
    #[test]
    fn held_turn_followup_for_this_turns_launched_agent_is_suppressed() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        // Launched while prompting: agent1 enters this turn's launched set.
        let _ = tick_prompting(&mut ws, &ledger);

        write_lines(
            &path,
            &[
                &notification("agent1", "completed"),
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        // Still prompting: #870 is holding the turn open for agent1.
        let (turns, outstanding, settled, _) =
            unpack(tick_prompting(&mut ws, &ledger).expect("settle event"));
        assert!(
            turns.is_empty(),
            "held-turn overlay follow-up must be suppressed (already on the wire), got {turns:?}"
        );
        assert_eq!(outstanding, 0, "accounting must still reflect settlement");
        // The settle notification is NOT suppressed — it flips the launch card.
        assert_eq!(settled.len(), 1, "settle must flow to flip the card");
        assert_eq!(settled[0].task_id, "agent1");
        assert_eq!(
            settled[0].tool_use_id.as_deref(),
            Some("toolu_01"),
            "settle must carry the launching tool_use_id for the in-memory flip"
        );
        assert_eq!(settled[0].result.as_deref(), Some("Build OK"));
    }

    /// The exact real-world race that broke a naive "is_prompting right now"
    /// check: the turn settles (Prompting→Connected) BEFORE the watcher's own
    /// tick gets around to reading the follow-up's tail content — the content
    /// was genuinely wire-rendered a beat earlier, while still `Prompting`,
    /// but this tick observes `is_prompting == false`. There is no grace
    /// window anymore: `current_turn_launched_ids` simply isn't cleared until
    /// the NEXT turn starts (or an abnormal ending releases it early), so
    /// OVERLAY suppression tolerates an arbitrarily-delayed read — several idle
    /// ticks pass with no new content before the follow-up finally lands, and
    /// the overlay turn must still be suppressed. The `settled` entry still
    /// flows regardless (it flips the launch card).
    #[test]
    fn held_turn_followup_still_suppressed_when_settlement_races_ahead_of_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_prompting(&mut ws, &ledger); // agent1 launched while prompting

        // The turn settles with no new transcript content — several idle
        // ticks pass (simulating the watcher's own poll lag) with nothing
        // clearing the launched-set: no new turn has started.
        let _ = tick_now(&mut ws, &ledger);
        let _ = tick_now(&mut ws, &ledger);
        let _ = tick_now(&mut ws, &ledger);

        // The notification + follow-up land well after the falling edge —
        // is_prompting is `false` here, matching the real race exactly.
        write_lines(
            &path,
            &[
                &notification("agent1", "completed"),
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert!(
            turns.is_empty(),
            "must still suppress the overlay for an arbitrarily-delayed read, got {turns:?}"
        );
        assert_eq!(outstanding, 0);
        // Settle still flows (un-suppressed) so the card can flip, even though
        // this tick read it after the falling edge (the set isn't cleared until
        // the next rising edge).
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].tool_use_id.as_deref(), Some("toolu_01"));
    }

    /// A turn that ends ABNORMALLY (cancelled, refused, etc — the same
    /// `stop_reason != "end_turn"` bucket `connection.rs` already treats
    /// uniformly elsewhere) must release its launched ids immediately: that
    /// content never reached the wire (the ACP call was torn down before the
    /// real background work settled), so unlike a normal completion there is
    /// no live view left for a later notification to duplicate — the overlay
    /// is correctly the only place left to render it, and must not wait for
    /// the next turn to start.
    #[test]
    fn abnormal_turn_ending_releases_launched_ids_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_prompting(&mut ws, &ledger); // agent1 launched while prompting

        // The turn ends abnormally (e.g. cancelled) instead of a normal end_turn.
        let _ = tick_abnormal_end(&mut ws, &ledger);

        // The notification lands afterward, with no new turn having started —
        // under a NORMAL ending this would still be suppressed (see the
        // settlement-races test above), but the abnormal ending must have
        // already released it.
        write_lines(
            &path,
            &[
                &notification("agent1", "completed"),
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert_eq!(
            turns.len(),
            1,
            "an abandoned held turn's follow-up has nowhere else to render"
        );
        assert_eq!(outstanding, 0);
        assert_eq!(
            settled.len(),
            1,
            "the notification must fire — nothing else will tell the user"
        );
    }

    /// A background shell launched while `Prompting` must NOT enter
    /// `current_turn_launched_ids`: #870 never holds a turn open for a shell,
    /// so a shell's owning turn ends via an ordinary `end_turn` while the
    /// shell keeps running. If the shell's id were suppression-eligible, its
    /// eventual completion would be silently swallowed for the shell's entire
    /// realistic runtime — content that was never on the wire to begin with.
    #[test]
    fn background_shell_launched_while_prompting_is_never_suppression_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&bash_ack("shell1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        // Registered while prompting — same moment an async agent ack would
        // have entered `current_turn_launched_ids`.
        let _ = tick_prompting(&mut ws, &ledger);
        assert!(
            !ws.current_turn_launched_ids.contains("shell1"),
            "a background shell must never be suppression-eligible"
        );

        // The turn ends normally (a shell's owning turn always does, per
        // #870 never holding for shells) with no new turn since — under the
        // agent case this would still suppress (see the settlement-races
        // test above), but a shell's notification must always surface.
        let _ = tick_now(&mut ws, &ledger);
        write_lines(
            &path,
            &[&notification("shell1", "completed"), &assistant_text("a1", "Done.")],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert_eq!(turns.len(), 1, "a shell follow-up has nowhere else to render");
        assert_eq!(outstanding, 0);
        assert_eq!(
            settled.len(),
            1,
            "a shell's notification must never be suppressed"
        );
    }

    /// A `SendMessage`-resumed sub-agent must be suppression-eligible again if
    /// the RESUMING turn is itself held open by #870 for it — mirroring the
    /// launch-time insert. Without this, a resume-then-hold reproduces the
    /// same double-render the original launch-time tracking exists to
    /// prevent.
    #[test]
    fn resumed_agent_held_by_the_resuming_turn_is_suppressed() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agent1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_prompting(&mut ws, &ledger); // agent1 launched while prompting (turn A)

        write_lines(&path, &[&notification("agent1", "completed")]);
        let _ = tick_prompting(&mut ws, &ledger); // settles within turn A — already suppressed

        // Turn A ends normally; no new turn yet, so the set still holds
        // agent1 (unbounded persistence, per the new design).
        let _ = tick_now(&mut ws, &ledger);

        // Turn B starts and, in its very first tick, resumes agent1 via
        // SendMessage — itself held open by #870 for the resumed work. The
        // rising edge clears the set BEFORE this line is processed; the
        // resume must re-insert agent1 within the same tick.
        let resume = r#"{"type":"assistant","timestamp":"2026-07-07T03:53:00.000Z","uuid":"a-send-resume","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_09","name":"SendMessage","input":{"to":"agent1","summary":"continue","message":"go on"}}]}}"#;
        write_lines(&path, &[resume]);
        let _ = tick_prompting(&mut ws, &ledger);
        assert!(
            ws.current_turn_launched_ids.contains("agent1"),
            "a resume issued by a held-open turn must re-enter the launched set"
        );

        write_lines(
            &path,
            &[
                &notification("agent1", "completed"),
                &assistant_text("a2", "Continued and finished."),
            ],
        );
        let (turns, outstanding, settled, _) =
            unpack(tick_prompting(&mut ws, &ledger).expect("settle event"));
        assert!(
            turns.is_empty(),
            "the resumed agent's second overlay notification must be suppressed too, got {turns:?}"
        );
        assert_eq!(outstanding, 0);
        // The settle still flows to re-flip the card for the resumed run.
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].tool_use_id.as_deref(), Some("toolu_01"));
    }

    /// A cron//loop autonomous turn has no originating task id at all (its
    /// initiator is plain injected text, not a `<task-notification>`), so it
    /// must never be caught by the held-turn suppression filter — even if,
    /// coincidentally, some OTHER turn happens to be `Prompting` when it
    /// fires.
    #[test]
    fn cron_followup_is_never_suppressed_even_while_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(
            &path,
            &[
                &cron_prompt("iterate forever"),
                &assistant_text("a1", "Working on it."),
            ],
        );
        let (turns, ..) =
            unpack(tick_prompting(&mut ws, &ledger).expect("turns event"));
        assert_eq!(
            turns.len(),
            1,
            "a cron-originated turn has no task id to suppress on"
        );
    }

    /// A `<task-notification>` can name a task id that was launched (and
    /// settled) by a PAST, already-ended turn — not the turn currently
    /// `Prompting`. Only ids launched by the CURRENTLY active turn are
    /// suppression-eligible (`current_turn_launched_ids` clears on every
    /// rising edge), so this must render normally.
    #[test]
    fn notification_for_a_past_turns_task_is_not_suppressed_by_a_new_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agentA")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        // Turn A launches agentA while prompting...
        let _ = tick_prompting(&mut ws, &ledger);
        // ...then turn A ends (falls back to Connected) with no new lines.
        let _ = tick_now(&mut ws, &ledger);

        // Turn B starts (rising edge clears the launched-set) and, within its
        // own held-open window, agentA's late notification from turn A
        // arrives — it belongs to no id turn B itself launched.
        write_lines(
            &path,
            &[
                &notification("agentA", "completed"),
                &assistant_text("a1", "Build finished cleanly."),
            ],
        );
        let (turns, ..) =
            unpack(tick_prompting(&mut ws, &ledger).expect("settle event"));
        assert_eq!(
            turns.len(),
            1,
            "a foreign (past-turn) task id must not be suppressed by a different turn"
        );
    }

    /// The dominant real-world shell path: a background shell is launched, the
    /// agent awaits it with `TaskOutput{block:true}`, and the result's
    /// `task.status` goes terminal — with NO `<task-notification>` ever
    /// written. That collection must clear the outstanding count (the bug:
    /// only `<task-notification>` used to settle, so these stranded for the
    /// full keep-alive max-age). A non-terminal poll must NOT clear it.
    #[test]
    fn taskoutput_terminal_status_settles_background_shell() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&bash_ack("bash1")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).expect("ack event"));
        assert_eq!(outstanding, 1);

        // A non-blocking poll while still running must not touch the count.
        write_lines(&path, &[&taskoutput_result("bash1", "running")]);
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "a running TaskOutput poll must not change accounting"
        );

        // The collected completion settles it — no notification involved.
        write_lines(&path, &[&taskoutput_result("bash1", "completed")]);
        let (_, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert_eq!(outstanding, 0, "TaskOutput completion must clear the count");
        assert!(
            settled.is_empty(),
            "inline-awaited collection must not raise an OS notification"
        );
    }

    /// An explicit `TaskStop` settles the task immediately — the process is
    /// gone and no completion notification will follow.
    #[test]
    fn taskstop_settles_background_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&bash_ack("bash9")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).expect("ack event"));
        assert_eq!(outstanding, 1);

        write_lines(&path, &[&taskstop("bash9")]);
        let (_, outstanding, settled, _) =
            unpack(tick_now(&mut ws, &ledger).expect("settle event"));
        assert_eq!(outstanding, 0, "TaskStop must clear the count");
        assert!(settled.is_empty());
    }

    #[test]
    fn codeg_sent_prompt_is_foreground_and_not_surfaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("修复登录 bug");
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "修复登录 bug"),
                &assistant_text("a1", "On it."),
            ],
        );
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "foreground turn must not surface as overlay"
        );
    }

    #[test]
    fn same_text_refire_without_ledger_entry_is_background() {
        // The /loop case: codeg sent the text once (consumed), the scheduler
        // re-fires the SAME text later — second occurrence must surface.
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("查询武汉当前天气");
        write_lines(
            &path,
            &[
                &user_prompt_array("u1", "查询武汉当前天气"),
                &assistant_text("a1", "24°C 多云"),
            ],
        );
        assert!(tick_now(&mut ws, &ledger).is_none());

        write_lines(
            &path,
            &[
                &cron_prompt("查询武汉当前天气"),
                &assistant_text("a2", "25°C 晴"),
            ],
        );
        let (turns, ..) = unpack(tick_now(&mut ws, &ledger).expect("cron turn surfaces"));
        assert_eq!(turns.len(), 1, "cron assistant response renders as overlay");
    }

    #[test]
    fn meta_array_expansion_does_not_flip_mode() {
        // A slash-command expansion (isMeta + ARRAY content) belongs to the
        // foreground turn that issued the command.
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        ledger.record_text("/init");
        let command_record = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.000Z","uuid":"u-cmd","message":{"role":"user","content":"<command-name>/init</command-name><command-message>init</command-message><command-args></command-args>"}}"#;
        let expansion = r#"{"type":"user","timestamp":"2026-07-07T03:50:00.100Z","uuid":"u-exp","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Please analyze this codebase..."}]}}"#;
        write_lines(
            &path,
            &[
                command_record,
                expansion,
                &assistant_text("a1", "Analyzing..."),
            ],
        );
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "slash command turn is foreground end-to-end"
        );
    }

    #[test]
    fn growing_turn_reemits_with_same_id_and_partial_lines_carry() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&notification("t1", "completed")]);
        let _ = tick_now(&mut ws, &ledger); // settle-only event

        write_lines(&path, &[&assistant_text("a1", "step one")]);
        let (turns1, ..) = unpack(tick_now(&mut ws, &ledger).expect("first turn"));
        assert_eq!(turns1.len(), 1);
        let id1 = turns1[0].id.clone();

        // Append a PARTIAL line (no newline yet): nothing must surface.
        let more = assistant_text("a2", "step two");
        let (head, tail) = more.split_at(more.len() / 2);
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(head.as_bytes()).unwrap();
        }
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "partial line must not parse"
        );

        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(tail.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
        let (turns2, ..) = unpack(tick_now(&mut ws, &ledger).expect("completed line surfaces"));
        // Same episode: a NEW assistant message is a NEW turn (bg-…-1); the
        // first turn's content didn't change so it is not re-emitted.
        assert_eq!(turns2.len(), 1);
        assert_ne!(turns2[0].id, id1);
        assert!(turns2[0].id.starts_with("bg-"));
    }

    /// One large append (a resume replaying hundreds of records of history in
    /// a single write) must be read in memory linear in its size (#885).
    /// Splitting `carry` one line at a time left every returned line holding a
    /// buffer as large as everything still unread when it was split, about
    /// bytes × lines / 2 in all: a 15 MB append took the app past 13 GB. The
    /// rest of the reader's contract is pinned alongside: complete lines only,
    /// a non-UTF-8 line skipped but counted, the partial tail kept for later.
    #[test]
    fn a_large_append_reads_in_memory_linear_in_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        let record = assistant_text("a-replay", &"x".repeat(7_000));
        write_lines(&path, &[record.as_str(); 300]);
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"\xff\xfe not utf-8\n").unwrap();
        }
        write_lines(&path, &[&record]);
        let (head, tail) = record.split_at(record.len() / 2);
        append_raw(&path, head);

        let lines = ws.read_new_lines(&path).unwrap();
        assert_eq!(lines.len(), 301, "every complete UTF-8 line, nothing else");
        assert!(lines.iter().all(|line| *line == record));
        let bytes: usize = lines.iter().map(String::len).sum();
        let held: usize = lines.iter().map(String::capacity).sum();
        assert!(
            held <= 2 * bytes,
            "the lines must hold about their own bytes: {held} held for {bytes}"
        );
        // The watermark covers every complete line, the skipped one included.
        // Only the partial tail stays, and not in a buffer sized to the batch.
        let file_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(ws.committed, file_len - head.len() as u64);
        assert_eq!(ws.carry, head.as_bytes());
        assert!(
            ws.carry.capacity() < bytes / 4,
            "carry kept {} bytes of capacity after the batch",
            ws.carry.capacity()
        );

        // The tail completes on a later append and reads as one line.
        append_raw(&path, &format!("{tail}\n"));
        assert_eq!(ws.read_new_lines(&path).unwrap(), vec![record.clone()]);
        assert!(ws.carry.is_empty());
        assert_eq!(ws.committed, std::fs::metadata(&path).unwrap().len());
    }

    /// A read that fails partway leaves what it did read in `carry`. The next
    /// call must return the complete lines among those bytes even when the
    /// file has not grown since, rather than hold them until another append.
    #[test]
    fn lines_a_failed_read_left_in_carry_surface_without_a_new_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        let record = assistant_text("a1", "kept");
        write_lines(&path, &[&record]);
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        // What a read that failed after reaching EOF leaves behind.
        ws.carry = std::fs::read(&path).unwrap();

        assert_eq!(ws.read_new_lines(&path).unwrap(), vec![record]);
        assert!(ws.carry.is_empty());
        assert_eq!(ws.committed, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn truncation_rebaselines_without_stale_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&notification("t1", "completed")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger);

        std::fs::write(&path, b"").unwrap();
        // Shrink triggers re-baseline; no panic, no stale content.
        let event = tick_now(&mut ws, &ledger);
        if let Some(e) = event {
            let (turns, _, settled, watermark) = unpack(e);
            assert!(turns.is_empty());
            assert!(settled.is_empty());
            assert_eq!(watermark, 0);
        }
        write_lines(&path, &[&assistant_text("a9", "after rewrite")]);
        // Post-truncation content is foreground by default (no initiator seen).
        assert!(tick_now(&mut ws, &ledger).is_none());
    }

    #[test]
    fn send_message_rearms_settled_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&agent_ack("agentX")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let _ = tick_now(&mut ws, &ledger);

        write_lines(&path, &[&notification("agentX", "completed")]);
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).unwrap());
        assert_eq!(outstanding, 0);

        let send = r#"{"type":"assistant","timestamp":"2026-07-07T03:52:00.000Z","uuid":"a-send","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_03","name":"SendMessage","input":{"to":"agentX","summary":"continue","message":"go on"}}]}}"#;
        write_lines(&path, &[send]);
        let (_, outstanding, ..) = unpack(tick_now(&mut ws, &ledger).unwrap());
        assert_eq!(outstanding, 1, "resumed sub-agent re-arms the keep-alive");
    }

    /// A page the built-in browser handed over, as codeg sends it with nothing
    /// typed: the page's block as an embedded resource, its screenshot after.
    fn page_prompt(uri: &str) -> Vec<crate::acp::types::PromptInputBlock> {
        use crate::acp::types::PromptInputBlock;
        vec![
            PromptInputBlock::Resource {
                uri: uri.to_string(),
                mime_type: Some("text/markdown".into()),
                text: Some("Captured from a web page in the built-in browser.".into()),
                blob: None,
            },
            PromptInputBlock::Image {
                data: "iVBORw0KGgo=".into(),
                mime_type: "image/png".into(),
                uri: None,
            },
        ]
    }

    /// The records Claude Code writes for [`page_prompt`], shaped after a real
    /// transcript: the address where the badge was, the picture, the block at
    /// the end — then the picture's own `isMeta` record, then the reply.
    fn page_prompt_records(uri: &str) -> Vec<String> {
        vec![
            format!(
                r#"{{"type":"user","timestamp":"2026-09-24T00:56:17.303Z","uuid":"u-page","promptId":"p-page","message":{{"role":"user","content":[{{"type":"text","text":"{uri}"}},{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}}},{{"type":"text","text":"\n<context ref=\"{uri}\">\nCaptured from a web page in the built-in browser.\n</context>"}}]}}}}"#
            ),
            r#"{"type":"user","timestamp":"2026-09-24T00:56:17.303Z","uuid":"u-meta","promptId":"p-page","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"[Image: source: /private/tmp/claude-501/shot.png]"}]}}"#
                .to_string(),
            assistant_text("a-page", "That is the Google doodle."),
        ]
    }

    /// Sent with nothing typed, a page has no text block to fingerprint. Its
    /// own turn then classified as out-of-turn, and the overlay drew the
    /// prompt and the reply a second time under the live copy.
    #[test]
    fn a_page_sent_with_nothing_typed_stays_a_foreground_turn() {
        let uri = "https://www.google.com/";
        let records = page_prompt_records(uri);
        let refs: Vec<&str> = records.iter().map(String::as_str).collect();

        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        ledger.record_prompt_blocks(&page_prompt(uri));
        write_lines(&path, &refs);
        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "the prompt codeg sent must not come back as an overlay turn"
        );

        // The same records with nothing recorded are what the watcher treats
        // as out-of-turn — which is what the fingerprint above prevents.
        let other = tempfile::tempdir().unwrap();
        let other_path = temp_session(&other);
        write_lines(&other_path, &[]);
        let empty = PromptLedger::shared();
        let mut unrecorded = WatchState::with_file_for_test("s2", other_path.clone());
        write_lines(&other_path, &refs);
        let (turns, ..) = unpack(tick_now(&mut unrecorded, &empty).expect("surfaces"));
        assert!(!turns.is_empty());
    }

    #[test]
    fn a_prompt_is_fingerprinted_by_the_first_text_the_adapter_writes() {
        use crate::acp::types::PromptInputBlock;
        let ledger = PromptLedger::shared();
        let initiator = |text: &str| TurnInitiatorText::Verbatim(text.to_string());

        // An attachment first: its link, the way `formatUriAsLink` writes it.
        ledger.record_prompt_blocks(&page_prompt("https://example.com/a?b=1"));
        assert!(ledger.consume_matching(&initiator(
            "https://example.com/a?b=1\n\n<context ref=\"https://example.com/a?b=1\">"
        )));
        ledger.record_prompt_blocks(&[PromptInputBlock::ResourceLink {
            uri: "file:///repo/src/app.ts".into(),
            name: "whatever the composer called it".into(),
            mime_type: None,
            description: None,
        }]);
        assert!(ledger.consume_matching(&initiator("[@app.ts](file:///repo/src/app.ts)")));

        // Typed text first is still the text, whatever follows it.
        let mut typed = vec![PromptInputBlock::Text {
            text: "  what is this ".into(),
        }];
        typed.extend(page_prompt("https://example.com/"));
        ledger.record_prompt_blocks(&typed);
        assert!(ledger.consume_matching(&initiator("what is this\nhttps://example.com/")));

        // Nothing the adapter writes as text leaves nothing to record: an
        // image, and a blob resource it drops.
        ledger.record_prompt_blocks(&[
            PromptInputBlock::Image {
                data: "iVBORw0KGgo=".into(),
                mime_type: "image/png".into(),
                uri: None,
            },
            PromptInputBlock::Resource {
                uri: "clipboard://report.pdf-1".into(),
                mime_type: Some("application/pdf".into()),
                text: None,
                blob: Some("JVBERi0=".into()),
            },
        ]);
        assert!(ledger.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn claude_links_follow_format_uri_as_link() {
        assert_eq!(claude_uri_link("https://a.test/x"), "https://a.test/x");
        assert_eq!(
            claude_uri_link("file:///repo/a b.ts"),
            "[@a b.ts](file:///repo/a b.ts)"
        );
        // A trailing slash leaves no last segment; the whole path names it.
        assert_eq!(claude_uri_link("file:///repo/dir/"), "[@/repo/dir/](file:///repo/dir/)");
        assert_eq!(
            claude_uri_link("zed://agent/thread/42"),
            "[@42](zed://agent/thread/42)"
        );
        assert_eq!(
            claude_uri_link("clipboard://notes.md-1"),
            "clipboard://notes.md-1"
        );
    }

    #[test]
    fn ledger_prefix_matches_and_consumes_once() {
        let ledger = PromptLedger::shared();
        ledger.record_text("deploy the app");
        assert!(ledger.consume_matching(&TurnInitiatorText::Verbatim(
            "deploy the app\n<system-hint>extra</system-hint>".into()
        )));
        assert!(
            !ledger.consume_matching(&TurnInitiatorText::Verbatim("deploy the app".into())),
            "an entry is consumed exactly once"
        );
    }

    #[test]
    fn initiator_classification_ground_rules() {
        // tool-result-only user record: continues the turn.
        let ack: serde_json::Value = serde_json::from_str(&agent_ack("x")).unwrap();
        assert!(turn_initiator_text(&ack).is_none());

        // task-notification string record: initiates (raw text, no ledger hit).
        let note: serde_json::Value =
            serde_json::from_str(&notification("x", "completed")).unwrap();
        assert!(turn_initiator_text(&note)
            .unwrap()
            .as_str()
            .starts_with("<task-notification>"));

        // cron prompt (isMeta + string): initiates with the prompt text.
        let cron: serde_json::Value = serde_json::from_str(&cron_prompt("check weather")).unwrap();
        assert_eq!(
            turn_initiator_text(&cron).as_ref().map(|text| text.as_str()),
            Some("check weather")
        );

        // context-continuation summary: never a boundary.
        let cont = format!(
            r#"{{"type":"user","uuid":"u-cont","message":{{"role":"user","content":"{}..."}}}}"#,
            CONTEXT_CONTINUATION_PREFIX
        );
        let cont: serde_json::Value = serde_json::from_str(&cont).unwrap();
        assert!(turn_initiator_text(&cont).is_none());

        // slash command record matches via its display form.
        let cmd = r#"{"type":"user","uuid":"u-cmd","message":{"role":"user","content":"<command-name>/init</command-name><command-args>now</command-args>"}}"#;
        let cmd: serde_json::Value = serde_json::from_str(cmd).unwrap();
        assert_eq!(
            turn_initiator_text(&cmd),
            Some(TurnInitiatorText::ReconstructedSlashCommand(
                "/init now".into()
            ))
        );
    }

    /// The whole point of reading titles here: Claude Code's background
    /// summarizer writes `ai-title` AFTER the turn that triggered it has
    /// ended, and the ACP adapter only reads the name back at turn-end — so on
    /// a short session nothing ever publishes it. That tail is pure metadata:
    /// no turns, no settlements, no accounting change, so `tick` returns
    /// `None`. The title must still come out, which is why `run_watch` takes
    /// it independently of the activity event.
    #[test]
    fn a_title_only_tail_yields_no_activity_event_but_still_surfaces_the_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&ai_title("Find current vLLM stable release tag")]);

        assert!(
            tick_now(&mut ws, &ledger).is_none(),
            "a title record is not background ACTIVITY"
        );
        assert_eq!(
            ws.pending_title.take().as_deref(),
            Some("Find current vLLM stable release tag")
        );
    }

    /// Once taken, the same name must not be re-queued on every later tick —
    /// each publish walks the state write lock and a DB write.
    #[test]
    fn an_unchanged_title_is_queued_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&ai_title("Fix the login flow")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("Fix the login flow"));

        // Claude Code re-emits the record; the resolved name did not change.
        write_lines(&path, &[&ai_title("Fix the login flow")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title, None);

        // A genuinely new name is queued again.
        write_lines(&path, &[&ai_title("Fix the signup flow")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("Fix the signup flow"));
    }

    /// `reads_transcript` decides whether the adapter's held title may reach
    /// the row (`session_title::release_held_wire_title`): only while this
    /// watch has no transcript whose records title the session. A file it
    /// adopted counts; one that went away stops counting until the lookup
    /// finds a transcript again.
    #[test]
    fn reads_transcript_only_while_it_holds_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();

        assert!(!WatchState::new().reads_transcript(), "nothing located yet");
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        assert!(ws.reads_transcript());

        write_lines(&path, &[&ai_title("Fix the login flow")]);
        let _ = tick_now(&mut ws, &ledger);
        assert!(ws.reads_transcript());

        // The tick that finds the file gone drops it (and does not look it up
        // again before the next tick, so this test reads no real config dir).
        std::fs::remove_file(&path).unwrap();
        let _ = tick_now(&mut ws, &ledger);
        assert!(!ws.reads_transcript());
    }

    /// A transcript that exists but cannot be read titles nothing (the history
    /// parser cannot read it either), so it does not count as read and the
    /// held wire title stays the only title codeg has. The read is retried
    /// every tick, so the file counts again on the tick after it becomes
    /// readable, even when only its permissions changed (its length and mtime
    /// did not, so the stat gate alone would never read it again).
    #[cfg(unix)]
    #[test]
    fn a_transcript_that_cannot_be_read_is_not_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[&ai_title("Fix the login flow")]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&path).is_ok() {
            // Running as root: permissions do not stop the read.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }
        let _ = tick_now(&mut ws, &ledger);
        assert!(!ws.reads_transcript(), "a failed read is not a read");
        let _ = tick_now(&mut ws, &ledger);
        assert!(!ws.reads_transcript(), "nor is a failed retry");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = tick_now(&mut ws, &ledger);
        assert!(ws.reads_transcript());
        assert_eq!(
            ws.pending_title.take().as_deref(),
            Some("Fix the login flow"),
            "the retry read the file"
        );
    }

    fn bound_claude_state() -> Arc<RwLock<SessionState>> {
        let mut st = SessionState::new(
            "conn-test".to_string(),
            AgentType::ClaudeCode,
            None,
            "win".to_string(),
            None,
        );
        st.conversation_id = Some(7);
        Arc::new(RwLock::new(st))
    }

    /// Titles published on this connection so far, oldest first.
    async fn published_titles(state: &Arc<RwLock<SessionState>>) -> Vec<String> {
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

    /// A wake ends the wait for the next poll at once: here only the wake can
    /// end an hour's wait inside the test's five seconds.
    #[tokio::test]
    async fn a_wake_ends_the_wait_for_the_next_poll() {
        let wake = tokio::sync::Notify::new();
        wake.notify_one();
        let waited = tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_poll(Duration::from_secs(3600), &wake),
        )
        .await;
        assert!(waited.is_ok());
    }

    /// The real watch loop settles a held wire title on the wake that holding
    /// it sends, not on its poll. The clock is paused and this test never
    /// sleeps, so the clock cannot auto-advance and the loop's poll delay
    /// never elapses: only the wake can make it poll. The session id is one
    /// `find_session_file` refuses before it lists any directory, so the loop
    /// finds no transcript (and this test reads no real config dir): the held
    /// title is the only one, and the loop publishes it.
    #[tokio::test(start_paused = true)]
    async fn the_watch_loop_settles_a_held_title_on_its_wake() {
        let state = bound_claude_state();
        state.write().await.external_id = Some("../no-transcript".into());
        let guard = spawn_if_claude(
            "conn-test",
            AgentType::ClaudeCode,
            Arc::clone(&state),
            EventEmitter::Noop,
            "/tmp".into(),
            PromptLedger::shared(),
        )
        .expect("a Claude connection gets a watcher");

        crate::acp::session_title::accept_wire_title(
            &state,
            &EventEmitter::Noop,
            AgentType::ClaudeCode,
            "Fix the login flow".into(),
        )
        .await;

        // Real time (`std::time::Instant`), which the paused clock does not
        // stop: a bound for a loop that never polls, not a latency budget.
        let deadline = Instant::now() + Duration::from_secs(30);
        while published_titles(&state).await.is_empty() {
            assert!(Instant::now() < deadline, "the loop never published it");
            tokio::task::yield_now().await;
        }
        assert_eq!(
            published_titles(&state).await,
            vec!["Fix the login flow".to_string()]
        );
        assert!(state.read().await.held_wire_title.is_none());
        drop(guard);
    }

    /// The adapter's title for an untitled session (its newest `last-prompt`
    /// record), held by the notification loop, never reaches the row while
    /// this watch reads the transcript; the transcript's own title does.
    #[tokio::test]
    async fn a_tick_that_reads_the_transcript_drops_the_held_wire_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());
        let state = bound_claude_state();
        state.write().await.held_wire_title = Some("now update the tests".into());

        let _ = tick_now(&mut ws, &ledger);
        publish_tick_titles(&mut ws, &state, &EventEmitter::Noop).await;
        assert!(published_titles(&state).await.is_empty());
        assert!(state.read().await.held_wire_title.is_none());

        write_lines(&path, &[&ai_title("Fix the login flow")]);
        let _ = tick_now(&mut ws, &ledger);
        publish_tick_titles(&mut ws, &state, &EventEmitter::Noop).await;
        assert_eq!(
            published_titles(&state).await,
            vec!["Fix the login flow".to_string()]
        );
    }

    /// With no transcript to read, the held wire title is the only one codeg
    /// gets, so the watch publishes it.
    #[tokio::test]
    async fn a_tick_without_a_transcript_publishes_the_held_wire_title() {
        let mut ws = WatchState::new();
        let state = bound_claude_state();
        state.write().await.held_wire_title = Some("Fix the login flow".into());

        publish_tick_titles(&mut ws, &state, &EventEmitter::Noop).await;

        assert_eq!(
            published_titles(&state).await,
            vec!["Fix the login flow".to_string()]
        );
        assert!(state.read().await.held_wire_title.is_none());
    }

    /// `customTitle ?? aiTitle` — Claude Code's own precedence, and the one
    /// `parsers::claude` applies over the whole file. A generated title
    /// arriving after the user named the session must not take the name back.
    #[test]
    fn a_user_set_title_outranks_a_later_generated_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&custom_title("auth-refactor")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("auth-refactor"));

        write_lines(&path, &[&ai_title("Concise AI Summary")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.pending_title,
            None,
            "the generated title must not displace the user's own name"
        );
        assert_eq!(ws.resolved_title().as_deref(), Some("auth-refactor"));

        // A NEW user-set name still wins.
        write_lines(&path, &[&custom_title("auth-refactor-v2")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("auth-refactor-v2"));
    }

    /// Claude Code writes an empty `aiTitle` for trivial sessions. Publishing
    /// it would rename the conversation to nothing.
    #[test]
    fn a_blank_title_record_is_never_queued() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(&path, &[]);
        let ledger = PromptLedger::shared();
        let mut ws = WatchState::with_file_for_test("s1", path.clone());

        write_lines(&path, &[&ai_title("   ")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title, None);
        assert_eq!(ws.resolved_title(), None);
    }

    /// History before the arm baseline renders through the ordinary detail
    /// fetch, which resolves the title from the whole file. Re-publishing it
    /// from here would rename the conversation on every reconnect.
    #[test]
    fn a_title_in_pre_baseline_history_is_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(
            &path,
            &[
                &user_prompt_array("u-old", "old prompt"),
                &ai_title("Old Session Name"),
            ],
        );

        let ledger = PromptLedger::shared();
        let mut ws = WatchState::new();
        // Arm with an epoch after the existing records, exactly as `run_watch`
        // does for a resumed session, and take the real baseline.
        ws.rearm("s1".to_string(), epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title, None);

        // A title written from here on IS this watch's to surface.
        write_lines(&path, &[&ai_title("New Session Name")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("New Session Name"));
    }

    /// `customTitle ?? aiTitle` is a WHOLE-FILE rule, but the two records are
    /// appended independently — `/rename` writes a lone `custom-title`, the
    /// summarizer a lone `ai-title`. A session renamed BEFORE this watch armed
    /// keeps its `custom-title` in the skipped history, so resolving over the
    /// tail alone would let the very next `ai-title` (the CLI re-emits it
    /// constantly) publish over the user's own name. The arm seeds the slots
    /// from history to close that.
    #[test]
    fn a_pre_baseline_user_title_outranks_a_generated_one_read_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(
            &path,
            &[
                &user_prompt_array("u-old", "old prompt"),
                &custom_title("auth-refactor"),
            ],
        );

        let ledger = PromptLedger::shared();
        let mut ws = WatchState::new();
        ws.rearm("s1".to_string(), epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());
        assert_eq!(
            ws.resolved_title().as_deref(),
            Some("auth-refactor"),
            "the arm must read the name the user already set"
        );
        assert_eq!(
            ws.pending_title, None,
            "seeding is not a publication — history rides the detail fetch"
        );

        // Only the GENERATED title lands in this watch's tail.
        write_lines(&path, &[&ai_title("Concise AI Summary")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.pending_title, None,
            "a generated title must not take the name back from the user"
        );
        assert_eq!(ws.resolved_title().as_deref(), Some("auth-refactor"));

        // A new user-set name still publishes normally.
        write_lines(&path, &[&custom_title("auth-refactor-v2")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title.take().as_deref(), Some("auth-refactor-v2"));
    }

    /// The CLI re-emits the SAME `ai-title` record throughout a session (228
    /// identical copies in one observed transcript). On a resumed session the
    /// first one past the baseline is a repeat of what history — and therefore
    /// the detail fetch, and therefore the row — already holds, so seeding must
    /// swallow it rather than spend a lifecycle write on a no-op.
    #[test]
    fn a_generated_title_already_in_history_is_not_republished() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(
            &path,
            &[
                &user_prompt_array("u-old", "old prompt"),
                &ai_title("Old Session Name"),
            ],
        );

        let ledger = PromptLedger::shared();
        let mut ws = WatchState::new();
        ws.rearm("s1".to_string(), epoch("2030-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());

        write_lines(&path, &[&ai_title("Old Session Name")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(ws.pending_title, None);

        // A genuinely NEW generated name is still this watch's to surface.
        write_lines(&path, &[&ai_title("Renamed By The Summarizer")]);
        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.pending_title.take().as_deref(),
            Some("Renamed By The Summarizer")
        );
    }

    /// A brand-new session baselines at offset 0 (its file is created after the
    /// spawn epoch), so there is no history to seed and the seed must be a
    /// no-op — the path this PR actually targets stays untouched.
    #[test]
    fn seeding_is_a_noop_for_a_fresh_session_whose_whole_file_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_session(&dir);
        write_lines(
            &path,
            &[
                &user_prompt_array("u-new", "first prompt"),
                &ai_title("Find current vLLM stable release tag"),
            ],
        );

        let ledger = PromptLedger::shared();
        let mut ws = WatchState::new();
        ws.rearm("s1".to_string(), epoch("2020-01-01T00:00:00Z"));
        ws.adopt_file(path.clone());
        assert_eq!(ws.committed, 0, "the whole file belongs to this watch");
        assert_eq!(ws.resolved_title(), None, "nothing to seed from");

        let _ = tick_now(&mut ws, &ledger);
        assert_eq!(
            ws.pending_title.take().as_deref(),
            Some("Find current vLLM stable release tag")
        );
    }
