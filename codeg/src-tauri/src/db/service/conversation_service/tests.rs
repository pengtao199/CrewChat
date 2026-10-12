    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

    /// Build a parent + a delegation child for filter assertions.
    async fn seed_parent_with_child(conn: &DatabaseConnection, folder_id: i32) -> (i32, i32) {
        let parent = create(
            conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("P".into()),
            None,
        )
        .await
        .expect("parent");
        let link = DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-1".into(),
            delegation_call_id: "call-1".into(),
        };
        let child = create_with_delegation(
            conn,
            folder_id,
            AgentType::Codex,
            Some("C".into()),
            None,
            Some(link),
        )
        .await
        .expect("child");
        (parent.id, child.id)
    }

    #[tokio::test]
    async fn create_with_team_round_trips_and_stays_optional() {
        use crate::models::conversation::ConversationTeam;

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-team-roundtrip").await;

        let team = ConversationTeam {
            leader_role_id: "role-leader".into(),
            member_role_ids: vec!["role-a".into(), "role-b".into()],
        };
        let grouped = create_with_team(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("G".into()),
            None,
            team.clone(),
        )
        .await
        .expect("grouped");
        let loaded = get_by_id(&db.conn, grouped.id).await.expect("reload");
        assert_eq!(loaded.team.as_ref(), Some(&team), "team survives a reload");

        // A plain session keeps `None` — the column is optional and reading
        // it back must not invent a team.
        let plain = create(&db.conn, folder, AgentType::Codex, Some("P".into()), None)
            .await
            .expect("plain");
        let plain_loaded = get_by_id(&db.conn, plain.id).await.expect("reload");
        assert!(plain_loaded.team.is_none());
    }

    #[tokio::test]
    async fn corrupt_team_json_reads_as_no_team() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-team-corrupt").await;
        let row = create(&db.conn, folder, AgentType::Codex, Some("X".into()), None)
            .await
            .expect("row");
        let row_id = row.id;
        let mut active: conversation::ActiveModel = row.into();
        active.team = Set(Some("{not json".into()));
        active.update(&db.conn).await.expect("write corrupt");

        let loaded = get_by_id(&db.conn, row_id).await.expect("reload");
        assert!(loaded.team.is_none(), "a corrupt blob degrades to None");
    }

    #[tokio::test]
    async fn list_all_excludes_children_by_default() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-default").await;
        let (parent, _child) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert!(ids.contains(&parent), "parent must remain visible: {ids:?}");
        assert_eq!(
            rows.len(),
            1,
            "expected only the parent, got {} rows: {ids:?}",
            rows.len()
        );
    }

    #[tokio::test]
    async fn list_all_includes_children_when_requested() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-on").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_all(&db.conn, None, None, None, None, None, true)
            .await
            .expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&parent) && ids.contains(&child),
            "both parent + child must appear when include_children=true, got: {ids:?}",
        );
    }

    #[tokio::test]
    async fn list_children_returns_only_matching_parent() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-only").await;
        let (parent_a, child_a) = seed_parent_with_child(&db.conn, folder).await;
        let (_parent_b, _child_b) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_children(&db.conn, parent_a).await.expect("list");
        assert_eq!(
            rows.len(),
            1,
            "expected 1 child of parent_a, got {}",
            rows.len()
        );
        assert_eq!(rows[0].id, child_a);
        assert_eq!(rows[0].parent_id, Some(parent_a));
    }

    #[tokio::test]
    async fn list_children_orders_newest_first() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-order").await;
        let parent = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("P".into()),
            None,
        )
        .await
        .expect("parent");
        // Two children created oldest → newest under the same parent.
        let first = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("first".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("first child");
        let second = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("second".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-2".into(),
                delegation_call_id: "call-2".into(),
            }),
        )
        .await
        .expect("second child");

        // The sidebar shows sub-sessions newest-first so a freshly-spawned
        // sub-agent surfaces right under its parent.
        let rows = list_children(&db.conn, parent.id).await.expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![second.id, first.id],
            "newest child must come first"
        );
    }

    #[tokio::test]
    async fn child_count_reflects_direct_children() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-direct").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        // The root listing carries the parent's direct-child count so the
        // sidebar knows to show a chevron; the leaf child carries 0.
        let roots = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let parent_row = roots.iter().find(|r| r.id == parent).expect("parent row");
        assert_eq!(parent_row.child_count, 1, "parent has one delegation child");

        let children = list_children(&db.conn, parent).await.expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child);
        assert_eq!(children[0].child_count, 0, "leaf child has no children");
    }

    #[tokio::test]
    async fn child_count_counts_grandchildren_for_nested_chevron() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-nested").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        // Delegate a grandchild from the child so the child itself becomes
        // expandable one level down.
        let link = DelegationLink {
            parent_conversation_id: child,
            parent_tool_use_id: "tu-2".into(),
            delegation_call_id: "call-2".into(),
        };
        create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("G".into()),
            None,
            Some(link),
        )
        .await
        .expect("grandchild");

        // list_children(parent) must report the child's OWN child_count (1) so
        // the recursive chevron appears on the nested row.
        let children = list_children(&db.conn, parent).await.expect("children");
        let child_row = children.iter().find(|r| r.id == child).expect("child row");
        assert_eq!(child_row.child_count, 1, "child has one grandchild");
    }

    #[tokio::test]
    async fn child_count_excludes_soft_deleted_children() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-deleted").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        soft_delete(&db.conn, child)
            .await
            .expect("soft delete child");

        // A removed sub-session must not keep the parent's chevron alive: the
        // aggregate filters deleted_at IS NULL, matching list_children.
        let roots = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let parent_row = roots.iter().find(|r| r.id == parent).expect("parent row");
        assert_eq!(
            parent_row.child_count, 0,
            "soft-deleted child must not be counted"
        );
    }

    #[tokio::test]
    async fn seed_model_fills_an_empty_column_once_without_bumping_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-seed-model").await;
        let conv = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("c".into()),
            None,
        )
        .await
        .expect("create");

        // The gap this closes: a row created in-app carries no model at all,
        // which is why the sidebar could only ever show one for imported
        // sessions.
        let before = get_by_id(&db.conn, conv.id).await.expect("get before");
        assert!(before.model.is_none(), "a new row names no model");
        let updated_at_before = before.updated_at;

        assert!(
            seed_model_if_empty(&db.conn, conv.id, "  gpt-5-codex  ")
                .await
                .expect("seed"),
            "an empty column must be filled, and report that it was so the \
             caller knows to broadcast"
        );
        let seeded = get_by_id(&db.conn, conv.id).await.expect("get seeded");
        assert_eq!(seeded.model.as_deref(), Some("gpt-5-codex"), "trimmed");
        assert_eq!(
            seeded.updated_at, updated_at_before,
            "seeding must not bump updated_at: the sidebar sorts on it, and \
             merely opening a conversation must not float it to the top"
        );

        // First value wins. The detail view re-reads the transcript and stays
        // exact; the column is a projection for the list, not a second source
        // of truth that fights the parse.
        assert!(
            !seed_model_if_empty(&db.conn, conv.id, "gpt-5.2")
                .await
                .expect("second seed"),
            "a populated column must be left alone, and say nothing was written"
        );
        assert_eq!(
            get_by_id(&db.conn, conv.id)
                .await
                .expect("get after")
                .model
                .as_deref(),
            Some("gpt-5-codex")
        );

        // A transcript that names no model asks for no write at all.
        assert!(
            !seed_model_if_empty(&db.conn, conv.id, "   ")
                .await
                .expect("blank seed")
        );
    }

    #[tokio::test]
    async fn seed_model_skips_a_soft_deleted_row() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-seed-model-deleted").await;
        let conv = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("c".into()),
            None,
        )
        .await
        .expect("create");
        soft_delete(&db.conn, conv.id).await.expect("delete");

        assert!(
            !seed_model_if_empty(&db.conn, conv.id, "gpt-5-codex")
                .await
                .expect("seed"),
            "a deleted conversation is not something an open can resurrect a \
             column on"
        );
    }

    #[tokio::test]
    async fn update_pin_sets_and_clears_without_bumping_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-update-pin").await;
        let conv = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("c".into()),
            None,
        )
        .await
        .expect("create");

        // Freshly created rows are unpinned, and the summary projection carries
        // the field through (conv_to_summary mapping).
        let before = get_by_id(&db.conn, conv.id).await.expect("get before");
        assert!(
            before.pinned_at.is_none(),
            "new conversation must be unpinned"
        );
        let updated_at_before = before.updated_at;

        // Pin → pinned_at populated; updated_at must NOT move (pin is a view
        // preference, not activity).
        update_pin(&db.conn, conv.id, true).await.expect("pin");
        let pinned = get_by_id(&db.conn, conv.id).await.expect("get pinned");
        assert!(
            pinned.pinned_at.is_some(),
            "pinned_at must be set after pin"
        );
        assert_eq!(
            pinned.updated_at, updated_at_before,
            "pinning must not bump updated_at"
        );

        // Unpin → pinned_at cleared back to NULL; updated_at still unchanged.
        update_pin(&db.conn, conv.id, false).await.expect("unpin");
        let unpinned = get_by_id(&db.conn, conv.id).await.expect("get unpinned");
        assert!(
            unpinned.pinned_at.is_none(),
            "pinned_at must clear after unpin"
        );
        assert_eq!(
            unpinned.updated_at, updated_at_before,
            "unpinning must not bump updated_at"
        );
    }

    #[tokio::test]
    async fn list_children_excludes_soft_deleted() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-soft-del").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        soft_delete(&db.conn, child).await.expect("soft delete");

        let rows = list_children(&db.conn, parent).await.expect("list");
        assert!(
            rows.is_empty(),
            "soft-deleted child must not appear: {rows:?}"
        );
    }

    #[tokio::test]
    async fn create_leaves_title_unlocked() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-unlocked").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("hi".into()),
            None,
        )
        .await
        .expect("create");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert!(
            !summary.title_locked,
            "new conversation must start unlocked"
        );
    }

    #[tokio::test]
    async fn update_title_locks_the_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-lock").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "My name".into())
            .await
            .expect("rename");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("My name"));
        assert!(summary.title_locked, "manual rename must lock the title");
    }

    /// Read a row straight from the table — `get_by_id` returns a summary and
    /// filters deleted rows, but these tests assert on raw persisted columns.
    async fn raw_row(conn: &DatabaseConnection, id: i32) -> conversation::Model {
        conversation::Entity::find_by_id(id)
            .one(conn)
            .await
            .expect("query")
            .expect("row")
    }

    /// The single live row (if any) holding `external_id`, whatever its id.
    async fn rows_holding(conn: &DatabaseConnection, external_id: &str) -> Vec<conversation::Model> {
        conversation::Entity::find()
            .filter(conversation::Column::ExternalId.eq(external_id))
            .filter(conversation::Column::DeletedAt.is_null())
            .all(conn)
            .await
            .expect("query by external_id")
    }

    #[tokio::test]
    async fn bind_external_id_preserves_the_outgoing_session_on_a_new_row() {
        // codeg#500 in miniature: a row bound to S1 is handed S2 (a fresh
        // session that knows nothing about it). S1 must survive on a row of its
        // own, wearing the identity the user recognises, or it disappears from
        // a sidebar that is built purely from DB rows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-preserve").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("nouveauté de cette version".into()),
            Some("main".into()),
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");
        seed_model_if_empty(&db.conn, row.id, "gpt-5-codex")
            .await
            .expect("seed S1's model");
        let before = raw_row(&db.conn, row.id).await;

        let preserved_id = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("rebind")
            .expect("a session change with nothing holding S1 must preserve it");

        let current = raw_row(&db.conn, row.id).await;
        assert_eq!(
            current.external_id.as_deref(),
            Some("S2"),
            "the live row advances to the new session"
        );
        assert!(
            current.model.is_none(),
            "the model described S1; left behind it would name S1's model on a \
             row that is now S2, and `seed_model_if_empty` only fills an EMPTY \
             column, so nothing would ever correct it"
        );

        let preserved = raw_row(&db.conn, preserved_id).await;
        assert_eq!(preserved.external_id.as_deref(), Some("S1"));
        assert_eq!(
            preserved.title.as_deref(),
            Some("nouveauté de cette version"),
            "the preserved row must keep the name the user recognises"
        );
        assert_eq!(preserved.folder_id, before.folder_id);
        assert_eq!(preserved.agent_type, before.agent_type);
        assert_eq!(preserved.git_branch.as_deref(), Some("main"));
        assert_eq!(
            preserved.model.as_deref(),
            Some("gpt-5-codex"),
            "the model belongs to S1, and this row is what S1 becomes"
        );
        assert_eq!(
            preserved.created_at, before.created_at,
            "created_at is carried, not stamped now — the preserved row IS the \
             old conversation and must not read as freshly created"
        );
        assert_eq!(
            preserved.updated_at, before.updated_at,
            "updated_at is carried so the sidebar's activity ordering keeps the \
             conversation in place instead of floating it to the top. This also \
             pins the self-assigning claim write: bumping updated_at to claim \
             the writer lock would make the SELECT read the clobbered value."
        );
        assert!(preserved.deleted_at.is_none());
        assert!(
            preserved.pinned_at.is_none(),
            "pinning belongs to the row the user pinned, not to the history"
        );
    }

    #[tokio::test]
    async fn bind_external_id_advances_in_place_when_the_session_is_carried_forward() {
        // A custom agent that keeps sessions in memory forgets them on every
        // restart. codeg's answer is to open a fresh session and link the
        // transcripts (`continues_from`), and both the reader and the generic
        // parser then render the chain as ONE conversation — the parser even
        // hides the superseded ids, precisely so the conversation isn't listed
        // twice.
        //
        // So a bind whose incoming session carries the outgoing one forward
        // must NOT split. Splitting would clone the conversation in the sidebar
        // on every restart, with each copy rendering a longer prefix of the
        // same history (S1, S1+S2, S1+S2+S3...), and double-count its tokens.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-continues").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Custom("goose"),
            Some("Long-running chat".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        // Restart 1: S2 continues S1.
        let preserved = bind_external_id(&db.conn, row.id, "S2", &["S1".to_string()])
            .await
            .expect("continuation bind");
        assert_eq!(
            preserved, None,
            "a carried-forward session is the same conversation, not a new one"
        );

        // Restart 2: S3 continues S2 (and transitively S1).
        let preserved = bind_external_id(
            &db.conn,
            row.id,
            "S3",
            &["S2".to_string(), "S1".to_string()],
        )
        .await
        .expect("second continuation bind");
        assert_eq!(preserved, None);

        let rows = conversation::Entity::find()
            .filter(conversation::Column::DeletedAt.is_null())
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(
            rows.len(),
            1,
            "two restarts must leave ONE conversation, got {rows:?}"
        );
        assert_eq!(rows[0].external_id.as_deref(), Some("S3"));

        // The guard is not over-broad: an UNRELATED session on the same row is
        // still split off, which is the bug this whole primitive exists for.
        let preserved = bind_external_id(&db.conn, row.id, "UNRELATED", &[])
            .await
            .expect("unrelated bind")
            .expect("an unrelated session must still preserve the history");
        assert_eq!(
            raw_row(&db.conn, preserved).await.external_id.as_deref(),
            Some("S3")
        );
    }

    #[tokio::test]
    async fn bind_external_id_survives_the_unique_index() {
        // `idx_conversation_external_agent` is UNIQUE over
        // `(external_id, agent_type)`, so the preserving row can only be
        // inserted after the current row has released S1. Insert-first would
        // fail the constraint every single time — this test exists so a
        // refactor that "tidies" the two writes into the intuitive order gets
        // caught here rather than in production.
        //
        // The migrations run in `fresh_in_memory_db`, so the index is real.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-unique").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("the split must not trip the unique index")
            .expect("preserved");

        assert_eq!(
            rows_holding(&db.conn, "S1").await.len(),
            1,
            "exactly one row holds the old session"
        );
        assert_eq!(
            rows_holding(&db.conn, "S2").await.len(),
            1,
            "exactly one row holds the new session"
        );
    }

    #[tokio::test]
    async fn bind_external_id_refuses_a_session_another_row_already_holds() {
        // The incoming id is exposed to the unique index in BOTH branches, and
        // neither used to check for a holder, so each of these used to raise a
        // raw `UNIQUE constraint failed`.
        //
        // Two things are asserted together because they only make sense
        // together: the DB is left completely untouched, AND the caller is told
        // so. `Conflict` rather than `Ok(None)` is the load-bearing half —
        // `send_prompt_linked` reads any `Ok` as "the row is bound now" and
        // goes on to dispatch the prompt, which would then land in the HOLDER
        // row's transcript while every event named this row.
        for repointing in [false, true] {
            let db = fresh_in_memory_db().await;
            let folder = seed_folder(&db, "/tmp/codeg-bind-conflict").await;
            let holder = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("holder");
            bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
                .await
                .expect("seed holder");

            let row = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("create");
            // `false` exercises the plain-bind branch (a fresh row whose
            // connection resumed a session codeg already indexed); `true` the
            // split branch, which would otherwise fail on the release write
            // before ever reaching its INSERT.
            if repointing {
                bind_external_id(&db.conn, row.id, "S_OWN", &[])
                    .await
                    .expect("seed own session");
            }

            let err = bind_external_id(&db.conn, row.id, "S_SHARED", &[])
                .await
                .expect_err("a taken session id must be refused, not reported bound");

            assert!(
                matches!(err, DbError::Conflict(_)),
                "must be distinguishable from transient contention, got {err:?}"
            );
            assert!(
                err.to_string().contains(&holder.id.to_string()),
                "the message must name the row that actually holds the session, got {err}"
            );
            assert_eq!(
                raw_row(&db.conn, row.id).await.external_id.as_deref(),
                repointing.then_some("S_OWN"),
                "the refused row keeps exactly the session it had"
            );
            let holders = rows_holding(&db.conn, "S_SHARED").await;
            assert_eq!(holders.len(), 1, "the original holder is untouched");
            assert_eq!(holders[0].id, holder.id);
        }
    }

    #[tokio::test]
    async fn bind_external_id_refuses_a_session_a_soft_deleted_row_holds() {
        // The unique index carries no `deleted_at` predicate, so a soft-deleted
        // row still occupies its id — a lookup that filtered on `deleted_at IS
        // NULL` would miss it and walk straight into the constraint. Refused on
        // the same terms as a live holder rather than having its id taken,
        // matching `import_one` / `refresh_existing`, which skip such a row
        // instead of rewriting it.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-conflict-deleted").await;
        let holder = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("holder");
        bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
            .await
            .expect("seed holder");
        soft_delete(&db.conn, holder.id).await.expect("soft delete");

        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S_OWN", &[])
            .await
            .expect("seed own session");

        let err = bind_external_id(&db.conn, row.id, "S_SHARED", &[])
            .await
            .expect_err("a deleted row's id is still taken; refuse it");

        assert!(matches!(err, DbError::Conflict(_)), "got {err:?}");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("S_OWN")
        );
        let holder_after = raw_row(&db.conn, holder.id).await;
        assert_eq!(holder_after.external_id.as_deref(), Some("S_SHARED"));
        assert!(
            holder_after.deleted_at.is_some(),
            "a refused bind must not resurrect the deleted holder"
        );
    }

    #[tokio::test]
    async fn bind_external_id_is_a_plain_write_on_first_bind() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-first").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");

        let preserved = bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        assert_eq!(
            preserved, None,
            "a row with no session yet has nothing to preserve"
        );
        assert_eq!(raw_row(&db.conn, row.id).await.external_id.as_deref(), Some("S1"));
    }

    #[tokio::test]
    async fn bind_external_id_is_idempotent_for_the_same_session() {
        // A duplicate SessionStarted (replay, agent re-init, or simply both
        // subscribers handling the same event) must not fork the history.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-idem").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[]).await.expect("bind");

        let preserved = bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("rebind to the same id");

        assert_eq!(preserved, None, "rebinding the same id preserves nothing");
        assert_eq!(rows_holding(&db.conn, "S1").await.len(), 1);
    }

    #[tokio::test]
    async fn bind_external_id_adopts_a_row_that_already_holds_the_old_session() {
        // What fork establishes: a sibling already carries S1, so the invariant
        // holds before we start and re-preserving would both duplicate the row
        // and (given the unique index) fail outright. Either race order must
        // land on exactly two rows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-adopt").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[]).await.expect("bind");
        // Stand in for fork's sibling insert.
        let sibling = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("sibling");
        bind_external_id(&db.conn, sibling.id, "S1_HOLDER", &[])
            .await
            .expect("seed");
        let mut active: conversation::ActiveModel = raw_row(&db.conn, sibling.id).await.into();
        active.external_id = Set(Some("S1".into()));
        // Release S1 on the original first, exactly as the real fork does.
        let mut original: conversation::ActiveModel = raw_row(&db.conn, row.id).await.into();
        original.external_id = Set(Some("S2".into()));
        original.update(&db.conn).await.expect("release");
        active.update(&db.conn).await.expect("hand S1 to the sibling");

        // Now the late SessionStarted{S2} arrives for the original row.
        let preserved = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("late rebind");

        assert_eq!(
            preserved, None,
            "S1 already has a home; preserving again would duplicate it"
        );
        assert_eq!(rows_holding(&db.conn, "S1").await.len(), 1);
    }

    #[tokio::test]
    async fn bind_external_id_maps_in_progress_to_pending_review() {
        // The preserved row has no agent attached any more, so carrying
        // `InProgress` over would leave it spinning in the sidebar forever.
        // Settled states are carried as-is — they are user-visible and still
        // true of the history being preserved.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-status").await;

        for (original, expected) in [
            (
                conversation::ConversationStatus::InProgress,
                conversation::ConversationStatus::PendingReview,
            ),
            (
                conversation::ConversationStatus::Completed,
                conversation::ConversationStatus::Completed,
            ),
            (
                conversation::ConversationStatus::Cancelled,
                conversation::ConversationStatus::Cancelled,
            ),
        ] {
            let row = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("create");
            let seed = format!("S1-{original:?}");
            bind_external_id(&db.conn, row.id, &seed, &[]).await.expect("bind");
            update_status(&db.conn, row.id, original.clone())
                .await
                .expect("status");

            let preserved_id = bind_external_id(&db.conn, row.id, &format!("S2-{original:?}"), &[])
                .await
                .expect("rebind")
                .expect("preserved");

            assert_eq!(
                raw_row(&db.conn, preserved_id).await.status,
                expected,
                "{original:?} must be preserved as {expected:?}"
            );
        }
    }

    #[tokio::test]
    async fn bind_external_id_keeps_a_delegation_child_inside_its_parent() {
        // `kind == Delegate ⟺ parent_id IS NOT NULL` is a documented invariant,
        // and a preserved child that lost its parent would surface as a stray
        // root in the sidebar. The two near-unique delegation identity fields
        // are deliberately NOT carried — duplicating them would point the
        // parent's tool-call view at two children for one call.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-delegate").await;
        let (parent_id, child_id) = seed_parent_with_child(&db.conn, folder).await;
        bind_external_id(&db.conn, child_id, "S1", &[])
            .await
            .expect("bind child");

        let preserved_id = bind_external_id(&db.conn, child_id, "S2", &[])
            .await
            .expect("rebind child")
            .expect("preserved");

        let preserved = raw_row(&db.conn, preserved_id).await;
        assert_eq!(preserved.parent_id, Some(parent_id));
        assert_eq!(preserved.kind, ConversationKind::Delegate);
        assert_eq!(preserved.parent_tool_use_id, None);
        assert_eq!(preserved.delegation_call_id, None);
    }

    #[tokio::test]
    async fn bind_external_id_ignores_the_same_session_under_another_agent() {
        // The uniqueness the index enforces is per-AGENT, and so is the
        // "already preserved" lookup. A row belonging to a different agent that
        // happens to share the id string must not be mistaken for a home for
        // this agent's outgoing session.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-agent-scope").await;
        let other_agent = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("other agent row");
        bind_external_id(&db.conn, other_agent.id, "S1", &[])
            .await
            .expect("bind other agent");
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("same id, different agent is allowed by the index");

        let preserved = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("rebind")
            .expect("the other agent's row is not a home for OUR session");

        let preserved = raw_row(&db.conn, preserved).await;
        assert_eq!(preserved.agent_type, row.agent_type);
    }

    #[tokio::test]
    async fn renormalize_external_id_alias_is_a_compare_and_swap() {
        // The alias path rewrites one spelling of a session into another and
        // must NOT split history. Its expected-old guard exists so a
        // `SessionStarted` that rebound the row while the parse was running
        // cannot be clobbered by the stale write that follows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-alias").await;
        let row = create(&db.conn, folder, AgentType::Gemini, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "acp-uuid", &[])
            .await
            .expect("bind");

        renormalize_external_id_alias(&db.conn, row.id, Some("acp-uuid"), "branch-id".into())
            .await
            .expect("normalize");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("branch-id"),
            "the alias is normalized in place, with no second row"
        );
        assert_eq!(
            rows_holding(&db.conn, "acp-uuid").await.len(),
            0,
            "an alias rewrite must not leave a phantom conversation behind"
        );

        // Stale write: the row has since moved on. It must match nothing.
        renormalize_external_id_alias(&db.conn, row.id, Some("acp-uuid"), "stale".into())
            .await
            .expect("a no-match is a silent no-op, not an error");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("branch-id"),
            "a stale alias write must not clobber a newer binding"
        );
    }

    #[tokio::test]
    async fn bind_external_id_skips_soft_deleted_row() {
        // A late/stale `SessionStarted` write — e.g. a fork's SessionStarted{S2}
        // landing after the user deleted the conversation — must NOT mutate a
        // soft-deleted row. `bind_external_id` is guarded on `deleted_at IS
        // NULL`, so it is a silent no-op: the deleted row keeps its old
        // external_id and is never half-resurrected.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-extid-deleted").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Doomed".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-S1", &[])
            .await
            .expect("seed external_id");
        soft_delete(&db.conn, row.id).await.expect("soft delete");

        // The guarded write must no-op (Ok) without touching the deleted row.
        // Critically it must ALSO not preserve anything: a deleted row's
        // session is not being orphaned, so manufacturing a preserving row
        // here would resurrect a conversation the user just deleted.
        let preserved = bind_external_id(&db.conn, row.id, "session-S2", &[])
            .await
            .expect("a stale SessionStarted write must be a no-op, not an error");
        assert_eq!(
            preserved, None,
            "a soft-deleted row must not spawn a preserving row"
        );

        // Inspect the raw row directly — `get_by_id` filters deleted rows out.
        let raw = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row still exists (soft-deleted)");
        assert!(raw.deleted_at.is_some(), "row must remain soft-deleted");
        assert_eq!(
            raw.external_id.as_deref(),
            Some("session-S1"),
            "a stale external_id write must not re-point a soft-deleted row"
        );

        // Sanity: the guard is not over-broad — a LIVE row still updates.
        let live = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Live".into()),
            None,
        )
        .await
        .expect("create live");
        bind_external_id(&db.conn, live.id, "session-S9", &[])
            .await
            .expect("live update");
        let live_raw = conversation::Entity::find_by_id(live.id)
            .one(&db.conn)
            .await
            .expect("query live")
            .expect("live row");
        assert_eq!(
            live_raw.external_id.as_deref(),
            Some("session-S9"),
            "a live row must still receive its external_id"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_writes_when_unlocked_and_changed() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");

        let wrote = refresh_auto_title(&db.conn, row.id, "fresh".into())
            .await
            .expect("auto");
        assert!(wrote, "an unlocked, changed title must be written");

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("fresh"));
        assert!(
            !summary.title_locked,
            "auto refresh must NOT lock — the title stays eligible for future refreshes"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_skips_when_unchanged_or_empty() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto-skip").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("same".into()),
            None,
        )
        .await
        .expect("create");

        assert!(
            !refresh_auto_title(&db.conn, row.id, "same".into())
                .await
                .expect("auto-same"),
            "identical title must be a no-op"
        );
        assert!(
            !refresh_auto_title(&db.conn, row.id, String::new())
                .await
                .expect("auto-empty"),
            "empty title must be a no-op"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_never_clobbers_a_locked_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto-locked").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        let wrote = refresh_auto_title(&db.conn, row.id, "parser title".into())
            .await
            .expect("auto");
        assert!(!wrote, "a locked title must never be auto-overwritten");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
        assert!(summary.title_locked);
    }

    #[tokio::test]
    async fn refresh_auto_title_does_not_bump_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-no-bump").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;

        let wrote = refresh_auto_title(&db.conn, row.id, "fresh".into())
            .await
            .expect("auto");
        assert!(wrote);

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("fresh"));
        assert_eq!(
            summary.updated_at, before,
            "auto-title backfill is metadata, not activity — it must not bump updated_at"
        );
    }

    #[tokio::test]
    async fn seed_auto_title_if_empty_writes_only_when_untitled() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-seed").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let before = row.updated_at;

        assert!(
            seed_auto_title_if_empty(&db.conn, row.id, "  First prompt  ".into())
                .await
                .expect("seed"),
            "an empty unlocked title must be seeded"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("First prompt"));
        assert!(!summary.title_locked);
        assert_eq!(summary.updated_at, before, "seed must not bump updated_at");

        assert!(
            !seed_auto_title_if_empty(&db.conn, row.id, "Second prompt".into())
                .await
                .expect("seed-2"),
            "a later prompt must not replace the first-prompt seed"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get-2");
        assert_eq!(summary.title.as_deref(), Some("First prompt"));
    }

    #[tokio::test]
    async fn seed_auto_title_if_empty_skips_locked_and_empty() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-seed-skip").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        assert!(
            !seed_auto_title_if_empty(&db.conn, row.id, "First prompt".into())
                .await
                .expect("seed-locked"),
            "a locked title must not be seeded over"
        );
        assert!(
            !seed_auto_title_if_empty(&db.conn, row.id, String::new())
                .await
                .expect("seed-empty")
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
    }

    /// Neither auto-title primitive may write a soft-deleted row. Both are now
    /// driven from the live ACP path (a title can land while the user is
    /// deleting the conversation), and a late write to a deleted row is a
    /// resurrection the sidebar can never show — `emit_conversation_upsert`
    /// filters it out, so the row would silently diverge from every client.
    #[tokio::test]
    async fn auto_title_writes_skip_soft_deleted_rows() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-deleted").await;

        // Untitled + deleted: the first-prompt seed must not name it.
        let seeded = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        soft_delete(&db.conn, seeded.id).await.expect("soft delete");
        assert!(
            !seed_auto_title_if_empty(&db.conn, seeded.id, "First prompt".into())
                .await
                .expect("seed"),
            "a soft-deleted row must not be seeded"
        );

        // Titled + deleted: a live ACP title must not replace it either.
        let refreshed = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Old name".into()),
            None,
        )
        .await
        .expect("create");
        soft_delete(&db.conn, refreshed.id).await.expect("soft delete");
        assert!(
            !refresh_auto_title(&db.conn, refreshed.id, "Agent title".into())
                .await
                .expect("refresh"),
            "a soft-deleted row must not be auto-retitled"
        );

        for (id, expected) in [(seeded.id, None), (refreshed.id, Some("Old name"))] {
            let row = conversation::Entity::find_by_id(id)
                .one(&db.conn)
                .await
                .expect("query")
                .expect("row still present");
            assert_eq!(row.title.as_deref(), expected);
        }
    }

    /// The work-task / automation launch path: the seed IS the name, so locking
    /// must keep the title byte-identical, keep the row where it is in a
    /// recency-sorted sidebar, and make the next auto-title a no-op.
    #[tokio::test]
    async fn lock_title_freezes_the_seed_without_rewriting_or_bumping() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-lock-seed").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Fix the login flow".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;
        assert!(!row.title_locked, "a fresh row starts unlocked");

        lock_title(&db.conn, row.id).await.expect("lock");

        let locked = get_by_id(&db.conn, row.id).await.expect("get");
        assert!(locked.title_locked, "lock_title must set the flag");
        assert_eq!(
            locked.title.as_deref(),
            Some("Fix the login flow"),
            "lock_title must not rewrite the title"
        );
        assert_eq!(
            locked.updated_at, before,
            "locking is metadata, not activity — it must not bump updated_at"
        );

        // The whole point: the parsed session title can no longer take over.
        assert!(
            !refresh_auto_title(&db.conn, row.id, "项目：/Users/me/app".into())
                .await
                .expect("auto"),
            "a seeded-and-locked title must survive the per-turn backfill"
        );
        let after = get_by_id(&db.conn, row.id).await.expect("get again");
        assert_eq!(after.title.as_deref(), Some("Fix the login flow"));
    }

    /// A missing row must not be an error — the caller locks on a best-effort
    /// path where a failure would be worse than the drift it prevents.
    #[tokio::test]
    async fn lock_title_on_a_missing_row_is_a_no_op() {
        let db = fresh_in_memory_db().await;
        lock_title(&db.conn, 999_999).await.expect("no-op lock");
    }

    #[tokio::test]
    async fn retitle_if_unchanged_follows_the_owner_rename() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-follow").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;
        lock_title(&db.conn, row.id).await.expect("lock");

        let wrote = retitle_if_unchanged(&db.conn, row.id, "old", "new")
            .await
            .expect("retitle");
        assert!(wrote, "a title still in sync with its owner must follow it");

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("new"));
        assert!(summary.title_locked, "the row must stay locked");
        assert_eq!(
            summary.updated_at, before,
            "following an owner rename is metadata, not activity"
        );
    }

    /// The guard that makes the sync safe: once the user names the conversation
    /// themselves, the owner's rename must not reach it. `title_locked` is true
    /// in BOTH cases, so only the expected-value comparison can tell them apart.
    #[tokio::test]
    async fn retitle_if_unchanged_refuses_after_a_manual_rename() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-refuse").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        let wrote = retitle_if_unchanged(&db.conn, row.id, "old", "new")
            .await
            .expect("retitle");
        assert!(!wrote, "a hand-picked title must not follow the owner");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
    }

    #[tokio::test]
    async fn retitle_if_unchanged_skips_empty_and_identical() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-skip").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("same".into()),
            None,
        )
        .await
        .expect("create");

        assert!(
            !retitle_if_unchanged(&db.conn, row.id, "same", "same")
                .await
                .expect("identical"),
            "an unchanged owner name must be a no-op"
        );
        assert!(
            !retitle_if_unchanged(&db.conn, row.id, "same", "   ")
                .await
                .expect("blank"),
            "a blank new title must never erase the name"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("same"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_converges_without_bumping_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-title").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("Makefile 文件的作用".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "codex-session-1", &[])
            .await
            .expect("set external id");
        let before = get_by_id(&db.conn, row.id).await.expect("get before");
        let titles = HashMap::from([(
            "codex-session-1".to_string(),
            "解释 Makefile 文件作用".to_string(),
        )]);

        assert_eq!(
            refresh_codex_auto_titles(&db.conn, &titles).await,
            vec![row.id]
        );
        let refreshed = get_by_id(&db.conn, row.id).await.expect("get refreshed");
        assert_eq!(refreshed.title.as_deref(), Some("解释 Makefile 文件作用"));
        assert_eq!(
            refreshed.updated_at, before.updated_at,
            "index title refresh must not count as conversation activity"
        );

        assert_eq!(
            refresh_codex_auto_titles(&db.conn, &titles).await,
            Vec::<i32>::new(),
            "a converged title map must issue no UPDATE"
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_keeps_partial_successes_after_row_failure() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-partial-failure").await;
        let successful = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old success".into()),
            None,
        )
        .await
        .expect("create successful candidate");
        bind_external_id(&db.conn, successful.id, "session-success", &[])
            .await
            .expect("set successful external id");
        let failing = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old failure".into()),
            None,
        )
        .await
        .expect("create failing candidate");
        bind_external_id(&db.conn, failing.id, "session-failure", &[])
            .await
            .expect("set failing external id");
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    r#"CREATE TRIGGER fail_second_codex_title_sync
                       BEFORE UPDATE OF title ON conversation
                       WHEN OLD.id = {}
                       BEGIN
                         SELECT RAISE(FAIL, 'injected partial title sync failure');
                       END"#,
                    failing.id
                ),
            ))
            .await
            .expect("install title failure trigger");
        let titles = HashMap::from([
            ("session-success".to_string(), "new success".to_string()),
            ("session-failure".to_string(), "new failure".to_string()),
        ]);

        let refreshed = refresh_codex_auto_titles(&db.conn, &titles).await;

        assert_eq!(refreshed, vec![successful.id]);
        assert_eq!(
            get_by_id(&db.conn, successful.id)
                .await
                .expect("get successful row")
                .title
                .as_deref(),
            Some("new success")
        );
        assert_eq!(
            get_by_id(&db.conn, failing.id)
                .await
                .expect("get failing row")
                .title
                .as_deref(),
            Some("old failure")
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_external_id_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-before", &[])
            .await
            .expect("set initial external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        // In-place re-point: this test is about the CAS seeing a changed
        // `external_id`, not about session preservation, so it uses the alias
        // rewrite rather than `bind_external_id` (which would additionally
        // split `session-before` onto a preserving row and put a second
        // candidate-shaped row in front of the assertions below).
        renormalize_external_id_alias(
            &db.conn,
            row.id,
            Some("session-before"),
            "session-after".into(),
        )
        .await
        .expect("re-point conversation");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "title for session-before",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not update a re-pointed row");
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.external_id.as_deref(), Some("session-after"));
        assert_eq!(current.title.as_deref(), Some("old title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_original_title_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-title-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("candidate title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-title-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        refresh_auto_title(&db.conn, row.id, "newer automatic title".into())
            .await
            .expect("apply concurrent automatic title");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not overwrite a newer title");
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.title.as_deref(), Some("newer automatic title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_deleted_at_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-delete-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("candidate title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-delete-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        soft_delete(&db.conn, row.id)
            .await
            .expect("concurrently delete candidate");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not update a deleted row");
        let current = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("read current row")
            .expect("soft-deleted row remains persisted");
        assert!(current.deleted_at.is_some());
        assert_eq!(current.title.as_deref(), Some("candidate title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_title_lock_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-lock-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("same manual title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-lock-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        update_title(&db.conn, row.id, "same manual title".into())
            .await
            .expect("lock title without changing its value");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(
            !wrote,
            "a stale candidate must not overwrite a locked title"
        );
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert!(current.title_locked);
        assert_eq!(current.title.as_deref(), Some("same manual title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_folder_deletion_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-folder-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-folder-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        crate::db::service::folder_service::soft_delete_folder(&db.conn, folder)
            .await
            .expect("soft delete folder");
        let wrote =
            refresh_codex_auto_title_candidate(&db.conn, &stale_candidate, "index title").await;

        assert!(
            !wrote.expect("conditional refresh"),
            "a row whose folder was deleted mid-refresh must not be rewritten (and so must not be broadcast)"
        );
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.title.as_deref(), Some("old title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_adopts_a_title_over_null() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-null-title").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "session-null-title", &[])
            .await
            .expect("set external id");
        let candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");
        assert!(candidate.title.is_none(), "fixture must start titleless");

        assert!(
            refresh_codex_auto_title_candidate(&db.conn, &candidate, "first Codex title")
                .await
                .expect("conditional refresh"),
            "the IS NULL branch of the observed-title CAS must still write"
        );
        assert_eq!(
            get_by_id(&db.conn, row.id)
                .await
                .expect("read current row")
                .title
                .as_deref(),
            Some("first Codex title")
        );

        // ...and once a title exists, the same stale (title = NULL) candidate
        // must no longer match.
        assert!(
            !refresh_codex_auto_title_candidate(&db.conn, &candidate, "second Codex title")
                .await
                .expect("conditional refresh"),
            "a stale titleless candidate must not clobber the title it just wrote"
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_skips_rows_the_sidebar_list_hides() {
        let db = fresh_in_memory_db().await;
        let live_folder = seed_folder(&db, "/tmp/codeg-codex-index-visible").await;
        let dead_folder = seed_folder(&db, "/tmp/codeg-codex-index-hidden").await;

        let visible = create(
            &db.conn,
            live_folder,
            AgentType::Codex,
            Some("old visible".into()),
            None,
        )
        .await
        .expect("create visible");
        bind_external_id(&db.conn, visible.id, "session-visible", &[])
            .await
            .expect("set visible external id");

        let in_dead_folder = create(
            &db.conn,
            dead_folder,
            AgentType::Codex,
            Some("old hidden".into()),
            None,
        )
        .await
        .expect("create hidden");
        bind_external_id(&db.conn, in_dead_folder.id, "session-hidden", &[])
            .await
            .expect("set hidden external id");
        crate::db::service::folder_service::soft_delete_folder(&db.conn, dead_folder)
            .await
            .expect("soft delete folder");

        let loop_row = create(
            &db.conn,
            live_folder,
            AgentType::Codex,
            Some("old loop".into()),
            None,
        )
        .await
        .expect("create loop row");
        bind_external_id(&db.conn, loop_row.id, "session-loop", &[])
            .await
            .expect("set loop external id");
        // No public write path mints kind='loop' yet, so flip it directly.
        let mut active: conversation::ActiveModel = conversation::Entity::find_by_id(loop_row.id)
            .one(&db.conn)
            .await
            .expect("query loop row")
            .expect("loop row exists")
            .into();
        active.kind = Set(ConversationKind::Loop);
        active.update(&db.conn).await.expect("flip kind");

        let titles = HashMap::from([
            ("session-visible".to_string(), "new visible".to_string()),
            ("session-hidden".to_string(), "new hidden".to_string()),
            ("session-loop".to_string(), "new loop".to_string()),
        ]);

        let refreshed = refresh_codex_auto_titles(&db.conn, &titles).await;

        assert_eq!(
            refreshed,
            vec![visible.id],
            "only rows `list_all` would return may be refreshed — every refreshed id is broadcast as a sidebar upsert"
        );
        assert_eq!(
            get_by_id(&db.conn, in_dead_folder.id)
                .await
                .expect("read hidden row")
                .title
                .as_deref(),
            Some("old hidden")
        );
        assert_eq!(
            get_by_id(&db.conn, loop_row.id)
                .await
                .expect("read loop row")
                .title
                .as_deref(),
            Some("old loop")
        );
    }

    #[tokio::test]
    async fn refresh_external_activity_moves_forward_only() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-activity").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("kept".into()),
            None,
        )
        .await
        .expect("create");
        let created_at = row.created_at;
        let before = row.updated_at;

        // The session kept running in the agent's own CLI after import.
        let later = before + chrono::Duration::hours(2);
        assert!(
            refresh_external_activity(&db.conn, row.id, later, 7)
                .await
                .expect("newer"),
            "newer transcript activity must be adopted"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.updated_at, later);
        assert_eq!(summary.message_count, 7);
        assert_eq!(
            summary.created_at, created_at,
            "created_at is the import/creation time and must not move"
        );
        assert_eq!(summary.title.as_deref(), Some("kept"));

        // Re-scanning the same (or an older) transcript must not move the row
        // back or re-order a sidebar sorted by recency.
        for (at, label) in [(later, "identical"), (before, "older")] {
            assert!(
                !refresh_external_activity(&db.conn, row.id, at, 1)
                    .await
                    .expect("no-op"),
                "{label} activity must be a no-op"
            );
        }
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.updated_at, later);
        assert_eq!(summary.message_count, 7, "a no-op must not resync counts");
    }

    #[tokio::test]
    async fn refresh_external_activity_skips_deleted_and_child_rows() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-activity-guards").await;
        let parent = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("parent");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-activity".into(),
                delegation_call_id: "call-activity".into(),
            }),
        )
        .await
        .expect("child");
        let deleted = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("deleted");
        soft_delete(&db.conn, deleted.id)
            .await
            .expect("soft delete");

        let later = Utc::now() + chrono::Duration::hours(1);
        assert!(
            !refresh_external_activity(&db.conn, child.id, later, 9)
                .await
                .expect("child"),
            "a delegation child is not a sidebar row"
        );
        assert!(
            !refresh_external_activity(&db.conn, deleted.id, later, 9)
                .await
                .expect("deleted"),
            "a soft-deleted conversation must never be half-resurrected"
        );

        for id in [child.id, deleted.id] {
            let raw = conversation::Entity::find_by_id(id)
                .one(&db.conn)
                .await
                .expect("query")
                .expect("row present");
            assert_eq!(raw.message_count, 0, "row {id} untouched");
        }
    }

    #[tokio::test]
    async fn create_paths_write_expected_kinds() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/kinds").await;

        let regular = create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("regular");
        assert_eq!(regular.kind, ConversationKind::Regular);

        let chat = create_chat(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("chat");
        assert_eq!(chat.kind, ConversationKind::Chat);

        let child = create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: regular.id,
                parent_tool_use_id: "tu-kind".into(),
                delegation_call_id: "call-kind".into(),
            }),
        )
        .await
        .expect("delegate");
        assert_eq!(child.kind, ConversationKind::Delegate);
        assert_eq!(child.parent_id, Some(regular.id));
    }

    #[tokio::test]
    async fn list_all_excludes_loop_kind_rows() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/loop-filter").await;
        let keep = create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("keep".into()),
            None,
        )
        .await
        .expect("keep");
        let hide = create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("hide".into()),
            None,
        )
        .await
        .expect("hide");
        // No public write path mints kind='loop' yet (reserved for the loop
        // engine), so flip the row directly to exercise the filter.
        let mut active: conversation::ActiveModel = hide.into();
        active.kind = Set(ConversationKind::Loop);
        active.update(&db.conn).await.expect("flip kind");

        let rows = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        assert!(rows.iter().any(|r| r.id == keep.id), "regular row stays");
        assert!(
            !rows.iter().any(|r| r.title.as_deref() == Some("hide")),
            "loop row must be excluded"
        );
    }
