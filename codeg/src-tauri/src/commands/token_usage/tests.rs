    use super::*;
    use crate::models::conversation::{DbConversationSummary, SessionStats};
    use crate::models::message::{TurnRole, TurnUsage};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn row(occurred: &str, tokens: i64) -> UsageFactRow {
        UsageFactRow {
            conversation_id: 1,
            folder_id: 1,
            agent_type: "claude_code".into(),
            model: Some("claude-opus-5".into()),
            occurred_at: ts(occurred),
            input_tokens: tokens,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            total_tokens: tokens,
            duration_ms: 0,
        }
    }

    fn turn(id: &str, usage: Option<TurnUsage>, at: &str) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role: TurnRole::Assistant,
            blocks: vec![],
            timestamp: ts(at),
            usage,
            duration_ms: Some(1200),
            model: Some("  claude-opus-5  ".into()),
            completed_at: None,
        agent_message_id: None,
        }
    }

    fn usage(input: u64, output: u64, create: u64, read: u64) -> TurnUsage {
        TurnUsage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: create,
            cache_read_input_tokens: read,
        }
    }

    fn opts<'a>(
        labels: &'a HashMap<i32, String>,
        bucket: TokenUsageBucket,
        tz: i32,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> AggregateOptions<'a> {
        AggregateOptions {
            bucket,
            tz_offset_minutes: tz,
            range_start: start,
            range_end: end,
            folder_labels: labels,
            truncated: false,
            compare_previous: false,
        }
    }

    /// Same as [`opts`] but with the comparison window switched on.
    fn opts_comparing<'a>(labels: &'a HashMap<i32, String>) -> AggregateOptions<'a> {
        AggregateOptions {
            compare_previous: true,
            ..opts(labels, TokenUsageBucket::Day, 0, None, None)
        }
    }

    #[test]
    fn facts_skip_turns_without_usage_and_zero_turns() {
        let turns = vec![
            turn("a", None, "2026-08-01T10:00:00Z"),
            turn("b", Some(usage(0, 0, 0, 0)), "2026-08-01T11:00:00Z"),
            turn("c", Some(usage(10, 5, 0, 0)), "2026-08-01T12:00:00Z"),
        ];
        let facts = facts_from_turns(&turns, None);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].turn_key, "c");
        assert_eq!(facts[0].total_tokens(), 15);
        // Model strings are trimmed so "  x  " and "x" don't split the by-model
        // breakdown into two slices.
        assert_eq!(facts[0].model.as_deref(), Some("claude-opus-5"));
        assert_eq!(facts[0].duration_ms, 1200);
    }

    #[test]
    fn facts_land_when_the_usage_was_reported_not_when_the_turn_finished() {
        // `completed_at` is the last absorbed tool result, which a single
        // long-running command can push into the next calendar day. The tokens
        // were spent when the assistant spoke, so that is where they land.
        let mut t = turn("a", Some(usage(1, 1, 0, 0)), "2026-08-01T23:50:00Z");
        t.completed_at = Some(ts("2026-08-02T09:30:00Z"));
        let facts = facts_from_turns(&[t], None);
        assert_eq!(facts[0].occurred_at, ts("2026-08-01T23:50:00Z"));
    }

    #[test]
    fn facts_fall_back_to_positional_turn_key() {
        let t = turn("", Some(usage(1, 0, 0, 0)), "2026-08-01T10:00:00Z");
        let facts = facts_from_turns(&[t], None);
        assert_eq!(facts[0].turn_key, "turn-0");
    }

    #[test]
    fn facts_backfill_the_session_model_only_where_turns_have_none() {
        // The Codex shape: usage per turn, model only at the session level.
        let mut bare = turn("a", Some(usage(10, 5, 0, 0)), "2026-08-01T10:00:00Z");
        bare.model = None;
        let facts = facts_from_turns(&[bare], Some("gpt-5.5"));
        assert_eq!(facts[0].model.as_deref(), Some("gpt-5.5"));

        // A turn that names its own model keeps it — a recorded mid-session
        // switch must not be flattened to the session default.
        let named = turn("b", Some(usage(10, 5, 0, 0)), "2026-08-01T10:00:00Z");
        let facts = facts_from_turns(&[named], Some("gpt-5.5"));
        assert_eq!(facts[0].model.as_deref(), Some("claude-opus-5"));

        // A whitespace-only session model is no model at all.
        let mut blank = turn("c", Some(usage(1, 0, 0, 0)), "2026-08-01T10:00:00Z");
        blank.model = None;
        let facts = facts_from_turns(&[blank], Some("   "));
        assert_eq!(facts[0].model, None);
    }

    fn detail(turns: Vec<MessageTurn>, stats: Option<SessionStats>) -> DbConversationDetail {
        DbConversationDetail {
            summary: DbConversationSummary {
                id: 1,
                folder_id: 1,
                title: None,
                title_locked: false,
                agent_type: crate::models::AgentType::Hermes,
                status: "completed".into(),
                kind: crate::db::entities::conversation::ConversationKind::Regular,
                model: Some(" hermes-model ".into()),
                git_branch: None,
                external_id: None,
                message_count: turns.len() as u32,
                child_count: 0,
                created_at: ts("2026-08-01T09:00:00Z"),
                updated_at: ts("2026-08-01T12:00:00Z"),
                pinned_at: None,
                parent_id: None,
                parent_tool_use_id: None,
                delegation_call_id: None,
                origin_cwd: None,
                team: None,
                tag_ids: Vec::new(),
            },
            turns,
            session_stats: stats,
            transcript_watermark: None,
            in_flight_user_turn_id: None,
            turns_offset: None,
            turns_total: None,
            assistant_turns_before_offset: None,
            prefix_hash: None,
            uncovered_prefix_max_ts: None,
        }
    }

    fn session_stats(input: u64, output: u64) -> SessionStats {
        SessionStats {
            total_usage: Some(usage(input, output, 0, 0)),
            total_tokens: Some(input + output),
            total_duration_ms: 9_000,
            context_window_used_tokens: None,
            context_window_max_tokens: None,
            context_window_usage_percent: None,
        }
    }

    #[test]
    fn a_session_level_total_is_recorded_when_no_turn_reports_usage() {
        // Hermes' shape: every turn's usage is None, but the session row knows
        // the totals. Reporting zero would be a confident wrong number.
        let turns = vec![
            turn("a", None, "2026-08-01T10:00:00Z"),
            turn("b", None, "2026-08-01T11:00:00Z"),
        ];
        let facts = facts_from_detail(
            &detail(turns, Some(session_stats(500, 100))),
            ts("2026-08-01T12:00:00Z"),
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].turn_key, SESSION_TOTAL_TURN_KEY);
        assert_eq!(facts[0].total_tokens(), 600);
        // Attributed to the end of the conversation, not its start.
        assert_eq!(facts[0].occurred_at, ts("2026-08-01T11:00:00Z"));
        assert_eq!(facts[0].model.as_deref(), Some("hermes-model"));
        assert_eq!(facts[0].duration_ms, 9_000);
    }

    #[test]
    fn per_turn_usage_wins_over_the_session_total_so_nothing_double_counts() {
        let turns = vec![turn("a", Some(usage(10, 5, 0, 0)), "2026-08-01T10:00:00Z")];
        let facts = facts_from_detail(
            // A session total that disagrees must not be added on top.
            &detail(turns, Some(session_stats(999_999, 999_999))),
            ts("2026-08-01T12:00:00Z"),
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].turn_key, "a");
        assert_eq!(facts[0].total_tokens(), 15);
    }

    #[test]
    fn a_conversation_with_no_token_data_anywhere_contributes_nothing() {
        // Cursor's shape: no per-turn usage and no session totals either.
        let turns = vec![turn("a", None, "2026-08-01T10:00:00Z")];
        assert!(facts_from_detail(&detail(turns.clone(), None), ts("2026-08-01T12:00:00Z")).is_empty());
        // A stats block that exists but reports zeros is equally empty.
        assert!(
            facts_from_detail(&detail(turns, Some(session_stats(0, 0))), ts("2026-08-01T12:00:00Z"))
                .is_empty()
        );
    }

    fn candidate(synced_turns: Option<i32>) -> usage_service::SyncCandidate {
        usage_service::SyncCandidate {
            id: 1,
            title: None,
            updated_at: ts("2026-08-01T12:00:00Z"),
            synced_source_updated_at: Some(ts("2026-08-01T11:00:00Z")),
            synced_turn_count: synced_turns,
        }
    }

    #[test]
    fn a_full_rebuild_reparses_even_conversations_that_look_current() {
        let current = usage_service::SyncCandidate {
            synced_source_updated_at: Some(ts("2026-08-01T12:00:00Z")),
            ..candidate(Some(3))
        };
        assert!(!current.is_stale());
        assert!(!should_reparse(&current, TokenUsageSyncMode::Incremental));
        assert!(should_reparse(&current, TokenUsageSyncMode::Full));
    }

    #[test]
    fn a_full_rebuild_still_protects_an_unreadable_source() {
        // The composition that a wipe-first rebuild would have broken: `Full`
        // selects a conversation whose stamp is intact, so when its transcript
        // comes back empty the guard still has the turn count it needs and the
        // recorded facts survive.
        let current = usage_service::SyncCandidate {
            synced_source_updated_at: Some(ts("2026-08-01T12:00:00Z")),
            ..candidate(Some(5))
        };
        assert!(should_reparse(&current, TokenUsageSyncMode::Full));
        assert!(parse_lost_a_readable_source(&detail(vec![], None), &current));
    }

    #[test]
    fn a_re_import_leaves_the_guard_armed() {
        // `mark_stale_for_reparse` keeps the stamp row (and its turn count)
        // rather than deleting it, so a re-imported conversation whose
        // transcript then turns out to be unreachable keeps its facts.
        let reimported = usage_service::SyncCandidate {
            synced_source_updated_at: Some(usage_service::force_reparse_marker()),
            ..candidate(Some(5))
        };
        assert!(reimported.is_stale());
        assert!(should_reparse(
            &reimported,
            TokenUsageSyncMode::Incremental
        ));
        assert!(parse_lost_a_readable_source(
            &detail(vec![], None),
            &reimported
        ));
    }

    #[test]
    fn an_incremental_pass_reparses_only_what_moved() {
        let stale = candidate(Some(3));
        assert!(stale.is_stale());
        assert!(should_reparse(&stale, TokenUsageSyncMode::Incremental));
    }

    #[test]
    fn an_unreadable_transcript_never_erases_recorded_usage() {
        // `get_folder_conversation_core` reports a missing transcript as a
        // successful parse with zero turns; writing that through would delete
        // real history and stamp the conversation current.
        let empty = detail(vec![], None);
        assert!(parse_lost_a_readable_source(&empty, &candidate(Some(12))));
    }

    #[test]
    fn a_conversation_that_never_had_usage_is_still_stamped() {
        // Otherwise a genuinely usage-free conversation (Cursor) would be
        // re-parsed on every pass forever.
        let empty = detail(vec![], None);
        assert!(!parse_lost_a_readable_source(&empty, &candidate(Some(0))));
        assert!(!parse_lost_a_readable_source(&empty, &candidate(None)));
    }

    #[test]
    fn turns_that_parsed_fine_but_report_no_usage_are_not_treated_as_lost() {
        // Real turns came back — the source is readable, it just has no token
        // counts. That must be recorded, not retried forever.
        let parsed = detail(vec![turn("a", None, "2026-08-01T10:00:00Z")], None);
        assert!(!parse_lost_a_readable_source(&parsed, &candidate(Some(12))));

        // Session-level stats with no turns are also a real parse (Hermes).
        let hermes = detail(vec![], Some(session_stats(10, 5)));
        assert!(!parse_lost_a_readable_source(&hermes, &candidate(Some(12))));
    }

    #[test]
    fn a_session_total_with_no_turns_at_all_lands_on_the_rows_updated_at() {
        let facts = facts_from_detail(
            &detail(vec![], Some(session_stats(300, 0))),
            ts("2026-08-02T08:00:00Z"),
        );
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].occurred_at, ts("2026-08-02T08:00:00Z"));
    }

    #[test]
    fn buckets_use_the_viewers_local_day_not_utc() {
        let labels = HashMap::new();
        // 22:30 UTC on the 1st is 06:30 local on the 2nd at UTC+8.
        let rows = vec![row("2026-08-01T22:30:00Z", 100)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 480, None, None),
        );
        assert_eq!(report.series.len(), 1);
        assert_eq!(report.series[0].bucket_key, "2026-08-02");

        let utc_report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        assert_eq!(utc_report.series[0].bucket_key, "2026-08-01");
    }

    #[test]
    fn series_is_dense_across_the_requested_range() {
        let labels = HashMap::new();
        let rows = vec![row("2026-08-01T10:00:00Z", 100), row("2026-08-04T10:00:00Z", 50)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(
                &labels,
                TokenUsageBucket::Day,
                0,
                Some(ts("2026-08-01T00:00:00Z")),
                Some(ts("2026-08-05T00:00:00Z")),
            ),
        );
        let keys: Vec<&str> = report.series.iter().map(|p| p.bucket_key.as_str()).collect();
        assert_eq!(keys, ["2026-08-01", "2026-08-02", "2026-08-03", "2026-08-04"]);
        assert_eq!(report.series[1].total_tokens, 0);
        assert_eq!(report.series[3].total_tokens, 50);
    }

    #[test]
    fn week_buckets_start_on_monday() {
        let labels = HashMap::new();
        // 2026-08-01 is a Saturday; its ISO week starts Monday 2026-07-27.
        let rows = vec![row("2026-08-01T10:00:00Z", 10)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Week, 0, None, None),
        );
        assert_eq!(report.series[0].bucket_key, "2026-07-27");
    }

    #[test]
    fn month_buckets_roll_over_the_year() {
        let labels = HashMap::new();
        let rows = vec![
            row("2026-12-15T10:00:00Z", 10),
            row("2027-01-04T10:00:00Z", 20),
        ];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Month, 0, None, None),
        );
        let keys: Vec<&str> = report.series.iter().map(|p| p.bucket_key.as_str()).collect();
        assert_eq!(keys, ["2026-12", "2027-01"]);
    }

    #[test]
    fn range_end_on_a_bucket_boundary_does_not_add_an_empty_tail() {
        let labels = HashMap::new();
        let rows = vec![row("2026-08-01T10:00:00Z", 10)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(
                &labels,
                TokenUsageBucket::Day,
                0,
                Some(ts("2026-08-01T00:00:00Z")),
                Some(ts("2026-08-02T00:00:00Z")),
            ),
        );
        assert_eq!(report.series.len(), 1);
        assert_eq!(report.series[0].bucket_key, "2026-08-01");
    }

    #[test]
    fn breakdowns_split_by_folder_agent_and_model() {
        let mut labels = HashMap::new();
        labels.insert(1, "codeg".to_string());
        labels.insert(2, "other".to_string());
        let mut rows = vec![row("2026-08-01T10:00:00Z", 100), row("2026-08-01T11:00:00Z", 300)];
        rows[1].folder_id = 2;
        rows[1].conversation_id = 2;
        rows[1].agent_type = "codex".into();
        rows[1].model = None;

        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        assert_eq!(report.totals.total_tokens, 400);
        assert_eq!(report.totals.conversation_count, 2);
        // Sorted by tokens descending.
        assert_eq!(report.by_folder[0].label, "other");
        assert_eq!(report.by_folder[0].total_tokens, 300);
        assert_eq!(report.by_agent[0].key, "codex");
        // A turn with no model still counts, under the explicit sentinel.
        assert_eq!(report.by_model[0].key, UNKNOWN_MODEL);
    }

    #[test]
    fn heatmap_uses_local_weekday_and_hour() {
        let labels = HashMap::new();
        // Sunday 2026-08-02 23:00 UTC is Monday 07:00 at UTC+8.
        let rows = vec![row("2026-08-02T23:00:00Z", 10)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 480, None, None),
        );
        assert_eq!(report.heatmap.len(), 1);
        assert_eq!(report.heatmap[0].weekday, 0);
        assert_eq!(report.heatmap[0].hour, 7);
    }

    #[test]
    fn streak_measures_consecutive_local_days() {
        let labels = HashMap::new();
        let rows = vec![
            row("2026-08-01T10:00:00Z", 1),
            row("2026-08-02T10:00:00Z", 1),
            row("2026-08-03T10:00:00Z", 1),
            // Gap on the 4th.
            row("2026-08-05T10:00:00Z", 1),
        ];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        assert_eq!(report.totals.active_days, 4);
        assert_eq!(report.streak.longest_days, 3);
        assert_eq!(report.streak.current_days, 1);
        assert_eq!(report.streak.current_ends_on.as_deref(), Some("2026-08-05"));
    }

    #[test]
    fn previous_window_totals_are_reported_separately() {
        let labels = HashMap::new();
        let rows = vec![row("2026-08-08T10:00:00Z", 100)];
        let previous = vec![row("2026-08-01T10:00:00Z", 40)];
        let report = aggregate_report(&rows, &previous, &opts_comparing(&labels));
        assert_eq!(report.totals.total_tokens, 100);
        assert_eq!(
            report.previous_totals.as_ref().map(|t| t.total_tokens),
            Some(40)
        );
    }

    #[test]
    fn a_requested_but_empty_comparison_window_reports_zeros_not_absence() {
        // The distinction matters downstream: zeros render as "new", while
        // `None` renders as "no comparison available".
        let labels = HashMap::new();
        let rows = vec![row("2026-08-08T10:00:00Z", 100)];
        let report = aggregate_report(&rows, &[], &opts_comparing(&labels));
        assert_eq!(
            report.previous_totals.as_ref().map(|t| t.total_tokens),
            Some(0)
        );
    }

    #[test]
    fn no_comparison_requested_means_no_previous_totals() {
        let labels = HashMap::new();
        let rows = vec![row("2026-08-08T10:00:00Z", 100)];
        let report = aggregate_report(
            &rows,
            &[row("2026-08-01T10:00:00Z", 40)],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        assert!(report.previous_totals.is_none());
    }

    #[test]
    fn range_end_is_always_exclusive_even_when_derived_from_the_data() {
        let labels = HashMap::new();
        let rows = vec![row("2026-08-01T10:00:00Z", 10)];
        let report = aggregate_report(
            &rows,
            &[],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        // One tick past the last recorded turn, so "last day counted" is
        // `range_end - 1ms` no matter how the bound was resolved.
        assert_eq!(report.range_end, Some(ts("2026-08-01T10:00:00.001Z")));
        assert_eq!(report.last_activity_at, Some(ts("2026-08-01T10:00:00Z")));
    }

    #[test]
    fn empty_input_yields_an_empty_report_not_a_panic() {
        let labels = HashMap::new();
        let report = aggregate_report(
            &[],
            &[],
            &opts(&labels, TokenUsageBucket::Day, 0, None, None),
        );
        assert_eq!(report.totals.total_tokens, 0);
        assert!(report.series.is_empty());
        assert!(report.previous_totals.is_none());
        assert_eq!(report.streak.longest_days, 0);
    }

    #[test]
    fn tz_offset_is_clamped_to_real_zones() {
        assert_eq!(normalize_tz_offset(999_999), 14 * 60);
        assert_eq!(normalize_tz_offset(-999_999), -12 * 60);
        assert_eq!(normalize_tz_offset(-330), -330);
    }

    /// `token_usage_sync_core` serializes on a process-global `try_lock`, so two
    /// tests calling it at once would make one fail with "already running".
    static SYNC_TESTS_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_changed_fact_schema_reparses_conversations_a_stamp_calls_current() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};

        let _serial = SYNC_TESTS_SERIAL.lock().await;
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-schema").await;
        let conv = seed_conversation(&db, folder, crate::models::AgentType::ClaudeCode).await;

        // Stamp it exactly current, the state an incremental pass skips.
        let updated_at = crate::db::entities::conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;
        usage_service::replace_conversation_facts(&db.conn, conv, updated_at, &[])
            .await
            .expect("stamp");
        assert!(!usage_service::list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());

        // First pass: the recorded schema is absent, so the conversation is
        // re-parsed even though nothing about it moved. This is what carries a
        // counting fix to history the user already has — stale-tracking never
        // would, because the transcript did not change, the arithmetic did.
        let first = token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
            .await
            .expect("first sync");
        assert_eq!(first.scanned, 1);
        assert_eq!(first.skipped, 0, "a schema change must not skip anything");

        assert_eq!(
            app_metadata_service::get_value(&db.conn, FACT_SCHEMA_VERSION_KEY)
                .await
                .expect("read marker")
                .as_deref(),
            Some(FACT_SCHEMA_VERSION)
        );

        // Second pass: the marker matches, so incremental is incremental again.
        let second = token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
            .await
            .expect("second sync");
        assert_eq!(second.skipped, 1, "the marker must not be sticky");
    }

    #[tokio::test]
    async fn a_transcript_that_is_gone_keeps_its_facts_and_stops_being_retried() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};

        let _serial = SYNC_TESTS_SERIAL.lock().await;
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-schema-fail").await;
        let conv = seed_conversation(&db, folder, crate::models::AgentType::ClaudeCode).await;

        // Recorded usage, stamped exactly current: the shape only a rebuild
        // would revisit, and the shape whose transcript is unreadable now.
        let updated_at = crate::db::entities::conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;
        usage_service::replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[UsageFact {
                turn_key: "t1".into(),
                occurred_at: ts("2026-08-01T10:00:00Z"),
                model: Some("claude-opus-5".into()),
                input_tokens: 100,
                output_tokens: 20,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                duration_ms: 0,
            }],
        )
        .await
        .expect("seed facts");
        assert!(!usage_service::list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());

        // The schema change selects it, the parse comes back with nothing, and
        // the lost-source guard keeps the old rows rather than erasing them.
        let first =
            token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
                .await
                .expect("first sync");
        assert_eq!(first.lost, 1, "the empty parse must not be written through");
        assert_eq!(
            first.failed, 0,
            "a source that is simply gone is not a fault the user can act on"
        );
        assert_eq!(
            usage_service::fetch_facts(&db.conn, &FactQuery::default(), 100)
                .await
                .expect("facts")
                .len(),
            1,
            "its recorded usage survives"
        );

        // The regression this guards: holding the stamp open re-walked the
        // agent's whole transcript tree on every pass, failed identically, and
        // reported it — forever, because nothing here counts attempts. The
        // stamp settles instead, so the case closes after one look.
        let second =
            token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
                .await
                .expect("second sync");
        assert_eq!(second.skipped, 1, "a settled conversation is not revisited");
        assert_eq!(second.lost, 0);
        assert_eq!(second.failed, 0);

        // Settled is not sealed: a re-import (or any edit that moves
        // `updated_at`) puts it back in scope for exactly one more look, which
        // settles again. That is the escape hatch — without it, a conversation
        // whose transcript really did come back could never be re-read.
        usage_service::mark_stale_for_reparse(&db.conn, conv)
            .await
            .expect("re-import");
        let third =
            token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
                .await
                .expect("third sync");
        assert_eq!(third.lost, 1, "a re-import earns one more attempt");
        assert_eq!(
            usage_service::fetch_facts(&db.conn, &FactQuery::default(), 100)
                .await
                .expect("facts")
                .len(),
            1,
            "and still does not erase what it could not re-derive"
        );
    }

    #[tokio::test]
    async fn a_failed_lost_source_stamp_during_schema_rebuild_stays_retryable() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let _serial = SYNC_TESTS_SERIAL.lock().await;
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-schema-settle-fail").await;
        let conv = seed_conversation(&db, folder, crate::models::AgentType::ClaudeCode).await;
        let updated_at = crate::db::entities::conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;
        usage_service::replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[UsageFact {
                turn_key: "t1".into(),
                occurred_at: ts("2026-08-01T10:00:00Z"),
                model: Some("claude-opus-5".into()),
                input_tokens: 100,
                output_tokens: 20,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                duration_ms: 0,
            }],
        )
        .await
        .expect("seed facts");

        // A schema rebuild selects this otherwise-current row. Fail only the
        // no-op current-to-current update used to settle the missing source;
        // the recovery write to the epoch marker must remain available.
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                r#"CREATE TRIGGER fail_current_token_usage_stamp
                   BEFORE UPDATE OF source_updated_at ON token_usage_sync
                   WHEN NEW.source_updated_at = OLD.source_updated_at
                   BEGIN
                     SELECT RAISE(ABORT, 'simulated settle failure');
                   END"#
                    .to_owned(),
            ))
            .await
            .expect("install failure trigger");

        let first =
            token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
                .await
                .expect("schema rebuild");
        assert_eq!(first.failed, 1);
        assert_eq!(first.lost, 0);
        let after_failure = usage_service::list_sync_candidates(&db.conn)
            .await
            .expect("candidates");
        assert!(
            after_failure[0].is_stale(),
            "a failed settle must survive the advancing schema marker"
        );

        // The trigger allows epoch-to-current, proving the next incremental
        // pass actually revisits the row and can settle it normally.
        let second =
            token_usage_sync_core(&db.conn, &EventEmitter::Noop, TokenUsageSyncMode::Incremental)
                .await
                .expect("retry");
        assert_eq!(second.failed, 0);
        assert_eq!(second.lost, 1);
        assert!(!usage_service::list_sync_candidates(&db.conn)
            .await
            .expect("candidates")[0]
            .is_stale());
        assert_eq!(
            usage_service::fetch_facts(&db.conn, &FactQuery::default(), 100)
                .await
                .expect("facts")
                .len(),
            1,
            "the failed settle and its retry must both preserve recorded usage"
        );
    }
