    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::AgentType;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid timestamp")
            .with_timezone(&Utc)
    }

    fn fact(key: &str, occurred: &str, input: i64, output: i64) -> UsageFact {
        UsageFact {
            turn_key: key.to_string(),
            occurred_at: at(occurred),
            model: Some("claude-opus-5".into()),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            duration_ms: 100,
        }
    }

    #[tokio::test]
    async fn replace_is_idempotent_and_never_accumulates_duplicates() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-a").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;

        let facts = vec![
            fact("t1", "2026-08-01T10:00:00Z", 100, 20),
            fact("t2", "2026-08-01T11:00:00Z", 200, 40),
        ];
        let (turns, tokens) =
            replace_conversation_facts(&db.conn, conv, at("2026-08-01T12:00:00Z"), &facts)
                .await
                .expect("first write");
        assert_eq!(turns, 2);
        assert_eq!(tokens, 360);

        // Re-syncing the same conversation replaces its rows rather than
        // appending — the fold would otherwise double-count every turn.
        replace_conversation_facts(&db.conn, conv, at("2026-08-01T12:00:00Z"), &facts)
            .await
            .expect("second write");
        let rows = fetch_facts(&db.conn, &FactQuery::default(), 1000)
            .await
            .expect("fetch");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(|r| r.total_tokens).sum::<i64>(), 360);
    }

    #[tokio::test]
    async fn a_shrinking_reparse_drops_the_rows_that_went_away() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-b").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;

        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T12:00:00Z"),
            &[
                fact("t1", "2026-08-01T10:00:00Z", 100, 0),
                fact("t2", "2026-08-01T11:00:00Z", 100, 0),
            ],
        )
        .await
        .expect("write");
        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T13:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("rewrite");

        let rows = fetch_facts(&db.conn, &FactQuery::default(), 1000)
            .await
            .expect("fetch");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn writing_zero_facts_still_stamps_the_conversation_as_synced() {
        // Otherwise a conversation whose transcript has no usage would be
        // re-parsed on every single pass, forever.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-c").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        let updated_at = conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;

        replace_conversation_facts(&db.conn, conv, updated_at, &[])
            .await
            .expect("write");

        let candidates = list_sync_candidates(&db.conn).await.expect("candidates");
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].is_stale());
    }

    #[tokio::test]
    async fn staleness_tracks_the_conversation_row_exactly() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-d").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;

        // Never parsed ⇒ stale.
        let candidates = list_sync_candidates(&db.conn).await.expect("candidates");
        assert!(candidates[0].is_stale());
        let row_updated_at = candidates[0].updated_at;

        replace_conversation_facts(&db.conn, conv, row_updated_at, &[])
            .await
            .expect("write");
        assert!(!list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());

        // Any movement of the row — forward OR backward — makes it stale again.
        crate::db::service::conversation_service::update_status(
            &db.conn,
            conv,
            conversation::ConversationStatus::Completed,
        )
        .await
        .expect("status");
        assert!(list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());
    }

    #[tokio::test]
    async fn candidates_carry_the_turn_count_the_last_parse_wrote() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-n").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;

        assert_eq!(
            list_sync_candidates(&db.conn).await.expect("c")[0].synced_turn_count,
            None
        );

        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T12:00:00Z"),
            &[
                fact("t1", "2026-08-01T10:00:00Z", 1, 0),
                fact("t2", "2026-08-01T11:00:00Z", 1, 0),
            ],
        )
        .await
        .expect("write");

        assert_eq!(
            list_sync_candidates(&db.conn).await.expect("c")[0].synced_turn_count,
            Some(2)
        );
    }

    #[tokio::test]
    async fn marking_stale_keeps_the_facts_and_the_turn_count() {
        // The import path's hook. Three things must survive a re-import:
        //  * the facts, so the dashboard doesn't blank between "this may have
        //    grown" and the fresh parse landing;
        //  * the turn count, which is what stops a subsequently-unreachable
        //    transcript from erasing those facts (see
        //    `parse_lost_a_readable_source`);
        //  * staleness itself, so the re-parse actually happens.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-o").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        let updated_at = conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;
        replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[
                fact("t1", "2026-08-01T10:00:00Z", 100, 0),
                fact("t2", "2026-08-01T11:00:00Z", 50, 0),
            ],
        )
        .await
        .expect("write");
        assert!(!list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());

        mark_stale_for_reparse(&db.conn, conv).await.expect("mark");

        let candidate = list_sync_candidates(&db.conn).await.expect("c")[0].clone();
        assert!(candidate.is_stale());
        // The regression this guards: deleting the stamp instead of marking it
        // would drop `synced_turn_count` to None, and the unreadable-source
        // guard would then let an empty parse wipe the retained facts.
        assert_eq!(candidate.synced_turn_count, Some(2));
        assert_eq!(
            fetch_facts(&db.conn, &FactQuery::default(), 1000)
                .await
                .expect("fetch")
                .len(),
            2
        );

        // Marking an unstamped conversation is a no-op, not an error.
        let other = seed_conversation(&db, folder, AgentType::Codex).await;
        mark_stale_for_reparse(&db.conn, other).await.expect("again");
    }

    #[tokio::test]
    async fn a_re_parse_after_marking_settles_back_to_current() {
        // The marker must not be sticky: once the fresh parse writes a real
        // `source_updated_at`, the conversation stops being re-read every pass.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-p").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        let updated_at = conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;
        replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("write");
        mark_stale_for_reparse(&db.conn, conv).await.expect("mark");
        assert!(list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());

        replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[fact("t1", "2026-08-01T10:00:00Z", 140, 0)],
        )
        .await
        .expect("re-parse");

        assert!(!list_sync_candidates(&db.conn).await.expect("c")[0].is_stale());
    }

    #[tokio::test]
    async fn soft_deleted_conversations_vanish_from_every_read() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-e").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T12:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("write");
        assert_eq!(
            fetch_facts(&db.conn, &FactQuery::default(), 1000)
                .await
                .expect("fetch")
                .len(),
            1
        );

        crate::db::service::conversation_service::soft_delete(&db.conn, conv)
            .await
            .expect("delete");

        // No re-sync needed: the join filters on the live conversation row, so
        // the deletion takes effect immediately.
        assert!(fetch_facts(&db.conn, &FactQuery::default(), 1000)
            .await
            .expect("fetch")
            .is_empty());
        assert!(facet_folders(&db.conn).await.expect("facets").is_empty());
        assert!(list_sync_candidates(&db.conn)
            .await
            .expect("candidates")
            .is_empty());
    }

    #[tokio::test]
    async fn filters_narrow_by_time_folder_agent_and_model() {
        let db = fresh_in_memory_db().await;
        let folder_a = seed_folder(&db, "/tmp/tu-f1").await;
        let folder_b = seed_folder(&db, "/tmp/tu-f2").await;
        let conv_a = seed_conversation(&db, folder_a, AgentType::ClaudeCode).await;
        let conv_b = seed_conversation(&db, folder_b, AgentType::Codex).await;

        replace_conversation_facts(
            &db.conn,
            conv_a,
            at("2026-08-05T00:00:00Z"),
            &[
                fact("t1", "2026-08-01T10:00:00Z", 100, 0),
                fact("t2", "2026-08-03T10:00:00Z", 200, 0),
            ],
        )
        .await
        .expect("write a");
        let mut b = fact("t1", "2026-08-02T10:00:00Z", 500, 0);
        b.model = Some("gpt-5".into());
        replace_conversation_facts(&db.conn, conv_b, at("2026-08-05T00:00:00Z"), &[b])
            .await
            .expect("write b");

        let by_time = fetch_facts(
            &db.conn,
            &FactQuery {
                start: Some(at("2026-08-02T00:00:00Z")),
                end: Some(at("2026-08-03T00:00:00Z")),
                ..Default::default()
            },
            1000,
        )
        .await
        .expect("time");
        assert_eq!(by_time.len(), 1);
        assert_eq!(by_time[0].total_tokens, 500);

        let by_folder = fetch_facts(
            &db.conn,
            &FactQuery {
                folder_ids: Some(vec![folder_a]),
                ..Default::default()
            },
            1000,
        )
        .await
        .expect("folder");
        assert_eq!(by_folder.len(), 2);

        let by_agent = fetch_facts(
            &db.conn,
            &FactQuery {
                agent_types: Some(vec!["codex".into()]),
                ..Default::default()
            },
            1000,
        )
        .await
        .expect("agent");
        assert_eq!(by_agent.len(), 1);
        assert_eq!(by_agent[0].folder_id, folder_b);

        let by_model = fetch_facts(
            &db.conn,
            &FactQuery {
                models: Some(vec!["gpt-5".into()]),
                ..Default::default()
            },
            1000,
        )
        .await
        .expect("model");
        assert_eq!(by_model.len(), 1);
    }

    #[tokio::test]
    async fn an_explicitly_empty_selection_matches_nothing_without_hitting_the_db() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-g").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T12:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("write");

        let q = FactQuery {
            folder_ids: Some(vec![]),
            ..Default::default()
        };
        assert!(q.is_empty_selection());
        assert!(fetch_facts(&db.conn, &q, 1000)
            .await
            .expect("fetch")
            .is_empty());
    }

    #[tokio::test]
    async fn the_newest_rows_survive_the_scan_cap() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-h").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-05T00:00:00Z"),
            &[
                fact("t1", "2026-08-01T10:00:00Z", 1, 0),
                fact("t2", "2026-08-02T10:00:00Z", 2, 0),
                fact("t3", "2026-08-03T10:00:00Z", 3, 0),
            ],
        )
        .await
        .expect("write");

        let capped = fetch_facts(&db.conn, &FactQuery::default(), 2)
            .await
            .expect("fetch");
        assert_eq!(capped.len(), 2);
        // Newest first: a truncated scan keeps the recent history, not the tail.
        assert_eq!(capped[0].occurred_at, at("2026-08-03T10:00:00Z"));
        assert_eq!(capped[1].occurred_at, at("2026-08-02T10:00:00Z"));
    }

    #[tokio::test]
    async fn facets_only_offer_dimensions_that_actually_have_data() {
        let db = fresh_in_memory_db().await;
        let folder_a = seed_folder(&db, "/tmp/tu-i1").await;
        // A folder with a conversation but no recorded usage must not appear.
        let folder_b = seed_folder(&db, "/tmp/tu-i2").await;
        let conv_a = seed_conversation(&db, folder_a, AgentType::ClaudeCode).await;
        let _conv_b = seed_conversation(&db, folder_b, AgentType::Codex).await;

        let mut untagged = fact("t2", "2026-08-02T10:00:00Z", 50, 0);
        untagged.model = None;
        replace_conversation_facts(
            &db.conn,
            conv_a,
            at("2026-08-05T00:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0), untagged],
        )
        .await
        .expect("write");

        let folders = facet_folders(&db.conn).await.expect("folders");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].0.id, folder_a);

        assert_eq!(
            facet_agents(&db.conn).await.expect("agents"),
            vec!["claude_code".to_string()]
        );
        // A NULL model is not offered as a filter option — there is nothing to
        // type into the query.
        assert_eq!(
            facet_models(&db.conn).await.expect("models"),
            vec!["claude-opus-5".to_string()]
        );

        let (start, end) = facet_extent(&db.conn).await.expect("extent");
        assert_eq!(start, Some(at("2026-08-01T10:00:00Z")));
        assert_eq!(end, Some(at("2026-08-02T10:00:00Z")));
    }

    #[tokio::test]
    async fn expanding_a_folder_id_includes_its_worktree_children() {
        let db = fresh_in_memory_db().await;
        let root = seed_folder(&db, "/tmp/tu-j").await;
        let child = seed_folder(&db, "/tmp/tu-j-wt").await;
        folder::Entity::update_many()
            .col_expr(
                folder::Column::ParentId,
                sea_orm::sea_query::Expr::value(root),
            )
            .filter(folder::Column::Id.eq(child))
            .exec(&db.conn)
            .await
            .expect("reparent");

        let expanded = expand_folder_ids(&db.conn, &[root]).await.expect("expand");
        assert_eq!(expanded, vec![root, child]);
        // A leaf expands to itself, not to its siblings.
        assert_eq!(
            expand_folder_ids(&db.conn, &[child]).await.expect("expand"),
            vec![child]
        );
    }

    #[tokio::test]
    async fn pruning_drops_facts_whose_conversation_is_gone_for_good() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-k").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            conv,
            at("2026-08-01T12:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("write");

        // A soft delete is NOT a prune target: the row still exists.
        crate::db::service::conversation_service::soft_delete(&db.conn, conv)
            .await
            .expect("soft delete");
        assert_eq!(prune_orphaned_facts(&db.conn).await.expect("prune"), 0);

        // A hard delete is.
        conversation::Entity::delete_by_id(conv)
            .exec(&db.conn)
            .await
            .expect("hard delete");
        assert_eq!(prune_orphaned_facts(&db.conn).await.expect("prune"), 1);
        assert_eq!(
            token_usage_turn::Entity::find()
                .count(&db.conn)
                .await
                .expect("count"),
            0
        );
        assert_eq!(prune_orphaned_facts(&db.conn).await.expect("prune"), 0);
    }

    #[tokio::test]
    async fn sync_counts_report_what_is_recorded_and_what_is_behind() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-l").await;
        let conv = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        let updated_at = conversation::Entity::find_by_id(conv)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .updated_at;

        let before = sync_counts(&db.conn).await.expect("counts");
        assert_eq!(before.total_conversations, 1);
        assert_eq!(before.synced_conversations, 0);
        assert_eq!(before.stale_conversations, 1);
        assert!(before.last_synced_at.is_none());

        replace_conversation_facts(
            &db.conn,
            conv,
            updated_at,
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 0)],
        )
        .await
        .expect("write");

        let after = sync_counts(&db.conn).await.expect("counts");
        assert_eq!(after.fact_rows, 1);
        assert_eq!(after.synced_conversations, 1);
        assert_eq!(after.stale_conversations, 0);
        assert!(after.last_synced_at.is_some());
    }

    #[tokio::test]
    async fn folder_label_prefers_the_user_alias() {
        let db = fresh_in_memory_db().await;
        let id = seed_folder(&db, "/tmp/tu-m").await;
        let row = folder::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        let name = row.name.clone();
        assert_eq!(folder_display_label(&row), name);

        let mut aliased = row.clone();
        aliased.alias = Some("  ".into());
        // A whitespace-only alias is not a name.
        assert_eq!(folder_display_label(&aliased), name);

        aliased.alias = Some("Codeg".into());
        assert_eq!(folder_display_label(&aliased), "Codeg");
    }

    /// Pin a conversation's `updated_at` so the factless branch's activity
    /// window is deterministic (creation stamps it "now").
    async fn set_updated_at(db: &crate::db::AppDatabase, id: i32, when: DateTime<Utc>) {
        use sea_orm::{ActiveModelTrait, IntoActiveModel};
        let row = conversation::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        let mut active = row.into_active_model();
        active.updated_at = Set(when);
        active.update(&db.conn).await.expect("update");
    }

    #[tokio::test]
    async fn workspace_count_reconciles_with_the_session_list() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-n").await;

        // A session with usage, and an empty one the fact table never sees.
        let with_facts = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            with_facts,
            at("2026-08-01T12:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 20)],
        )
        .await
        .expect("write facts");
        let empty = seed_conversation(&db, folder, AgentType::Codex).await;
        set_updated_at(&db, empty, at("2026-07-10T09:00:00Z")).await;

        // A delegation child with usage: its tokens count, the session does
        // not — the sidebar list it reconciles against hides children.
        let child = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        let mut active = conversation::Entity::find_by_id(child)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .into_active_model();
        active.parent_id = Set(Some(with_facts));
        active.kind = Set(conversation::ConversationKind::Delegate);
        active.update(&db.conn).await.expect("mark child");
        replace_conversation_facts(
            &db.conn,
            child,
            at("2026-08-01T12:00:00Z"),
            &[fact("t2", "2026-08-01T11:00:00Z", 50, 10)],
        )
        .await
        .expect("write child facts");

        // Unbounded window: the workspace list's two top-level sessions.
        let all = workspace_conversation_count(&db.conn, &FactQuery::default())
            .await
            .expect("count");
        assert_eq!(all, 2);

        // A window holding only the usage turn: the empty session's activity
        // stamp (July) is outside it.
        let august = FactQuery {
            start: Some(at("2026-08-01T00:00:00Z")),
            end: Some(at("2026-08-02T00:00:00Z")),
            ..Default::default()
        };
        assert_eq!(
            workspace_conversation_count(&db.conn, &august)
                .await
                .expect("count"),
            1
        );

        // A window holding only the empty session's activity.
        let july = FactQuery {
            start: Some(at("2026-07-01T00:00:00Z")),
            end: Some(at("2026-08-01T00:00:00Z")),
            ..Default::default()
        };
        assert_eq!(
            workspace_conversation_count(&db.conn, &july)
                .await
                .expect("count"),
            1
        );

        // A model filter can only speak for recorded usage, so the factless
        // branch drops out entirely.
        let by_model = FactQuery {
            models: Some(vec!["claude-opus-5".into()]),
            ..Default::default()
        };
        assert_eq!(
            workspace_conversation_count(&db.conn, &by_model)
                .await
                .expect("count"),
            1
        );
    }

    #[tokio::test]
    async fn workspace_count_ignores_soft_deleted_and_filters_by_agent() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/tu-o").await;

        let kept = seed_conversation(&db, folder, AgentType::ClaudeCode).await;
        replace_conversation_facts(
            &db.conn,
            kept,
            at("2026-08-01T12:00:00Z"),
            &[fact("t1", "2026-08-01T10:00:00Z", 100, 20)],
        )
        .await
        .expect("write facts");

        // Soft-deleted sessions keep their facts (tokens were spent) but are
        // not sessions the workspace shows.
        let deleted = seed_conversation(&db, folder, AgentType::Codex).await;
        let mut active = conversation::Entity::find_by_id(deleted)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row")
            .into_active_model();
        active.deleted_at = Set(Some(at("2026-08-02T00:00:00Z")));
        active.update(&db.conn).await.expect("soft delete");

        assert_eq!(
            workspace_conversation_count(&db.conn, &FactQuery::default())
                .await
                .expect("count"),
            1
        );

        // Agent filter applies to both branches.
        let codex_only = FactQuery {
            agent_types: Some(vec!["codex".into()]),
            ..Default::default()
        };
        assert_eq!(
            workspace_conversation_count(&db.conn, &codex_only)
                .await
                .expect("count"),
            0
        );
    }
