    use super::*;

    /// Windows has no absolute path without a drive, and the resolver branches
    /// on `is_absolute` — so the fixtures need a prefix that makes them count
    /// as absolute on both platforms.
    #[cfg(windows)]
    const ABS_PREFIX: &str = "C:";
    #[cfg(not(windows))]
    const ABS_PREFIX: &str = "";

    /// The stderr shapes below are what git and the forges actually print. A
    /// push-back refused for being out of date used to come back advising the
    /// reader to turn on maintainer edits — a setting that, in the report that
    /// prompted this, was already on. So the two must never be confused.
    #[test]
    fn a_stale_push_back_is_not_read_as_a_permission_problem() {
        let out_of_date = "git push failed: ! [rejected]        task/57 -> feat/x \
             (fetch first)\nerror: failed to push some refs to \
             'https://github.com/owner/repo.git'\nhint: Updates were rejected because the \
             remote contains work that you do not have locally.";
        assert_eq!(classify_push_refusal(out_of_date), PushRefusal::BranchMoved);
        assert_eq!(
            classify_push_refusal("git push failed: ! [rejected] a -> b (non-fast-forward)"),
            PushRefusal::BranchMoved
        );
    }

    #[test]
    fn a_refused_fork_push_is_read_as_a_permission_problem() {
        let denied = "git push: authentication failed. Configure a GitHub account in \
             Settings → Version Control.: remote: Permission to author/repo.git denied to \
             maintainer.\nfatal: unable to access \
             'https://github.com/author/repo.git/': The requested URL returned error: 403";
        assert_eq!(classify_push_refusal(denied), PushRefusal::Permission);
        assert_eq!(
            classify_push_refusal(
                "remote: GitLab: You are not allowed to push code to \
                 protected branches on this project."
            ),
            PushRefusal::Permission
        );
        // A forge that refuses outright can also say "rejected"; permission
        // has to win, or the advice sends the reader to rebase for nothing.
        assert_eq!(
            classify_push_refusal(
                "! [remote rejected] a -> b (pre-receive hook declined)\nerror: failed to push"
            ),
            PushRefusal::Permission
        );
    }

    /// Anything unrecognised must stay `Unknown`, because `Unknown` is what
    /// prints no advice — the whole point of the split.
    #[test]
    fn an_unrecognised_push_failure_gets_no_advice() {
        assert_eq!(
            classify_push_refusal("git push failed: fatal: the remote end hung up unexpectedly"),
            PushRefusal::Unknown
        );
        assert_eq!(classify_push_refusal(""), PushRefusal::Unknown);
    }

    /// A status code has to be matched in the phrasing that carries it. Branch
    /// and repository names are part of every push error, and plenty of them
    /// are named after an issue number.
    #[test]
    fn a_number_in_a_branch_name_is_not_a_status_code() {
        let stale_on_an_issue_branch = "git push failed: ! [rejected] fix/403-page -> \
             fix/401-redirect (fetch first)\nerror: failed to push some refs to \
             'https://github.com/owner/repo-403.git'";
        assert_eq!(
            classify_push_refusal(stale_on_an_issue_branch),
            PushRefusal::BranchMoved,
            "a 403 in a ref name must not be read as a permission refusal"
        );
    }

    #[test]
    fn worktree_names_carry_ids() {
        assert_eq!(basename("/home/me/repo"), "repo");
        assert_eq!(basename("/home/me/repo/"), "repo");
        assert_eq!(
            sibling_path("/home/me/repo", "repo-task-7"),
            Path::new("/home/me")
                .join("repo-task-7")
                .to_string_lossy()
                .into_owned()
        );
        // No parent to be a sibling of: the bare name, which git resolves
        // against the repository it runs in.
        assert_eq!(sibling_path("repo", "repo-task-7"), "repo-task-7");
    }

    /// The derived directory name is joined ONTO the configured root, so it
    /// has to stay relative even for the paths that have no final component
    /// to borrow: a filesystem root, and anything ending in `..`. An absolute
    /// name would replace the configured root instead of landing under it —
    /// silently putting the worktree somewhere the user never chose.
    #[test]
    fn a_project_without_a_name_still_lands_under_the_configured_root() {
        let trees = format!("{ABS_PREFIX}/var/worktrees");
        for root in [format!("{ABS_PREFIX}/"), format!("{ABS_PREFIX}/home/me/..")] {
            let dir = format!("{}-task-7", basename(&root));
            assert!(
                !Path::new(&dir).is_absolute(),
                "the derived name must be relative, got {dir:?} for root {root:?}"
            );
            assert_eq!(
                worktree_path_in_home(&root, Some(&trees), &dir, None),
                Path::new(&trees).join(&dir).to_string_lossy().into_owned(),
                "root {root:?} must not escape the configured worktree directory"
            );
        }
    }

    /// Where a fresh task worktree lands. The default layout is load-bearing:
    /// every worktree already on disk was minted next to its project folder,
    /// so an unset (or blanked) setting has to keep producing exactly that
    /// path — the recreate-from-branch fallback looks for it by name.
    #[test]
    fn a_blank_worktree_root_keeps_worktrees_next_to_the_project() {
        let root = format!("{ABS_PREFIX}/home/me/repo");
        let expected = Path::new(&format!("{ABS_PREFIX}/home/me"))
            .join("repo-task-7")
            .to_string_lossy()
            .into_owned();
        for setting in [None, Some(""), Some("   ")] {
            assert_eq!(
                worktree_path_in_home(&root, setting, "repo-task-7", None),
                expected,
                "setting {setting:?} must not move the worktree"
            );
        }
    }

    /// A configured directory holds the worktree DIRECTORY, not the checkout
    /// itself — two tasks of the same folder have to be able to share it.
    #[test]
    fn a_configured_root_holds_one_directory_per_task() {
        let root = format!("{ABS_PREFIX}/home/me/repo");
        let trees = format!("{ABS_PREFIX}/var/worktrees");
        // Compared as paths, not strings: a separator the setting carried in
        // survives into the result — `Path::join` reuses a trailing one rather
        // than adding its own — and on Windows `/` and `\` are the same
        // separator. Only the directory the worktree lands in is pinned here,
        // never the spelling of the separators.
        let at = |name: &str| Path::new(&trees).join(name);
        let landed = |setting: &str, name: &str| {
            PathBuf::from(worktree_path_in_home(&root, Some(setting), name, None))
        };
        assert_eq!(landed(&trees, "repo-task-7"), at("repo-task-7"));
        assert_eq!(landed(&trees, "repo-task-8"), at("repo-task-8"));
        // Typed paths arrive with stray whitespace and trailing separators.
        assert_eq!(
            landed(&format!("  {trees}/  "), "repo-task-7"),
            at("repo-task-7")
        );
    }

    /// The two shorthands a typed path is expected to honour: `~` is the home
    /// directory, and a relative path hangs off the project folder. An unknown
    /// home leaves `~` alone rather than guessing a different directory.
    #[test]
    fn a_typed_root_expands_home_and_resolves_relatives() {
        let root = format!("{ABS_PREFIX}/home/me/repo");
        let home = PathBuf::from(format!("{ABS_PREFIX}/home/me"));
        assert_eq!(
            worktree_path_in_home(&root, Some("~/trees"), "repo-task-7", Some(&home)),
            home.join("trees")
                .join("repo-task-7")
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(
            worktree_path_in_home(&root, Some("~"), "repo-task-7", Some(&home)),
            home.join("repo-task-7").to_string_lossy().into_owned()
        );
        assert_eq!(
            worktree_path_in_home(&root, Some(".worktrees"), "repo-task-7", Some(&home)),
            Path::new(&root)
                .join(".worktrees")
                .join("repo-task-7")
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(
            worktree_path_in_home(&root, Some("~/trees"), "repo-task-7", None),
            Path::new(&root)
                .join("~/trees")
                .join("repo-task-7")
                .to_string_lossy()
                .into_owned(),
            "an unknown home must not silently relocate the worktree"
        );
    }

    /// Native Windows paths, where the naming and the join have to agree: a
    /// project folder whose "name" came back as the whole path (`C:\src\repo`)
    /// makes `<root>/<name>` an absolute join that REPLACES the configured
    /// root, so every setting would silently resolve back to the sibling
    /// layout. Nothing here is exotic — it is what the OS folder picker hands
    /// back on Windows.
    #[cfg(windows)]
    #[test]
    fn a_windows_project_path_still_honours_the_configured_root() {
        // Expectations are built with `Path::join` so they pin the DIRECTORY
        // the worktree lands in without also asserting separator spelling.
        let at = |dir: &str| {
            Path::new(dir)
                .join("repo-task-7")
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(basename("C:\\src\\repo"), "repo");
        assert_eq!(
            worktree_path_in_home("C:\\src\\repo", Some("D:\\worktrees"), "repo-task-7", None),
            at("D:\\worktrees"),
            "a configured root on another drive must not collapse to the sibling layout"
        );
        assert_eq!(
            worktree_path_in_home("C:\\src\\repo", Some(".worktrees"), "repo-task-7", None),
            at("C:\\src\\repo\\.worktrees")
        );
        assert_eq!(
            worktree_path_in_home("C:\\src\\repo", None, "repo-task-7", None),
            at("C:\\src"),
            "the default layout is unchanged on Windows"
        );
        assert_eq!(
            worktree_path_in_home(
                "C:\\src\\repo",
                Some("~\\trees"),
                "repo-task-7",
                Some(Path::new("C:\\Users\\me"))
            ),
            at("C:\\Users\\me\\trees")
        );
        // A UNC project folder keeps its share prefix.
        assert_eq!(
            sibling_path("\\\\server\\share\\repo", "repo-task-7"),
            at("\\\\server\\share")
        );
    }

    #[test]
    fn prompt_head_takes_the_first_text_block() {
        let blocks = vec![PromptInputBlock::Text {
            text: "  Fix the login flow and add tests.  ".to_string(),
        }];
        assert_eq!(prompt_head(&blocks), "Fix the login flow and add tests.");
        assert_eq!(prompt_head(&[]), "");
    }

    /// #649: the pump reads nothing but the conversation link to decide
    /// whether a launch continues or starts over, so a row that still points
    /// at an earlier run is dispatched as `Retry` no matter how it got back to
    /// the queue. `requeue_canceled` clears the link precisely so a task the
    /// user put back on the board comes through here as `Fresh`.
    #[test]
    fn launch_mode_follows_the_conversation_link() {
        let mut task = task_row();
        task.conversation_id = None;
        assert!(matches!(launch_mode_for(&task), LaunchMode::Fresh));

        task.conversation_id = Some(41);
        assert!(matches!(launch_mode_for(&task), LaunchMode::Retry));
    }

    /// The sweep's row-level gate: exactly the tasks whose merge button the
    /// board would show — and whose light is green — land unattended.
    #[test]
    fn auto_merge_candidate_mirrors_the_board() {
        let settings = WorkTaskFolderSettings::default();
        let mut task = task_row();
        task.status = WorkTaskStatus::Review;
        assert!(auto_merge_candidate(&task, &settings));

        // Unknown stats merge (the UI defaults to the merge button there); an
        // empty change set is the complete button's territory.
        task.files_changed = Some(3);
        assert!(auto_merge_candidate(&task, &settings));
        task.files_changed = Some(0);
        assert!(!auto_merge_candidate(&task, &settings));
        task.files_changed = None;

        // An earlier failed or refused merge waits for the user.
        task.last_error = Some("merge dispatch failed: conflict".into());
        assert!(!auto_merge_candidate(&task, &settings));
        task.last_error = None;

        // The forge red line: a task whose prompt embeds text authored by an
        // arbitrary external user (issue body) never lands unattended, no
        // matter what the folder's auto_merge says. Mutation check both ways —
        // this row is a candidate on every OTHER gate (asserted just above),
        // so flipping ONLY source_kind proves this guard is what blocks it,
        // and clearing it proves the guard doesn't leak onto normal tasks.
        task.source_kind = Some("forge_issue".into());
        assert!(
            !auto_merge_candidate(&task, &settings),
            "forge-sourced tasks must never be unattended-merge candidates"
        );
        task.source_kind = None;
        assert!(auto_merge_candidate(&task, &settings));

        // A merge the user queued belongs to the queue's own drain, with THEIR
        // commit message and worktree choice — the unattended dispatch has
        // neither, so it must not take this row (a drain that skipped it after
        // losing a race would otherwise hand it straight to the sweep).
        task.pending_merge = Some(
            serde_json::to_string(&WorkTaskQueuedMerge {
                message: Some("feat: land it".into()),
                delete_worktree: false,
                instructions: None,
                queued_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            })
            .unwrap(),
        );
        assert!(!auto_merge_candidate(&task, &settings));
        // Deliberately the same predicate `begin_merge`'s auto CAS uses —
        // "anything in the column" rather than "a parseable intent". A gate
        // looser than the CAS that finally decides would dispatch into a miss
        // on every sweep; a manual merge clears the column either way.
        task.pending_merge = Some(r#"{"legacy":true}"#.into());
        assert!(!auto_merge_candidate(&task, &settings));
        task.pending_merge = None;

        // Only review is mergeable.
        task.status = WorkTaskStatus::Running;
        assert!(!auto_merge_candidate(&task, &settings));
        task.status = WorkTaskStatus::Review;

        // A configured preflight gates on a green light — absent, running and
        // red all wait. A blank custom command is no gate; a legacy id-only
        // reference is one.
        let gated = WorkTaskFolderSettings {
            preflight_command: Some("pnpm test".into()),
            ..Default::default()
        };
        assert!(!auto_merge_candidate(&task, &gated));
        for status in ["running", "failed"] {
            task.preflight =
                Some(format!(r#"{{"status":"{status}","command":"pnpm test"}}"#));
            assert!(!auto_merge_candidate(&task, &gated), "{status} light");
        }
        task.preflight = Some(r#"{"status":"passed","command":"pnpm test"}"#.into());
        assert!(auto_merge_candidate(&task, &gated));
        assert!(!preflight_configured(&WorkTaskFolderSettings {
            preflight_command: Some("  ".into()),
            ..Default::default()
        }));
        assert!(preflight_configured(&WorkTaskFolderSettings {
            preflight_command_id: Some(4),
            ..Default::default()
        }));
    }

    /// The refusal wordings that mean "lost a race" are skipped silently by
    /// the sweep; the strings live in `merge_task`, and this pin keeps the
    /// match and the wording from drifting apart.
    #[test]
    fn benign_merge_races_are_recognized() {
        assert!(is_benign_merge_race("task is not in review"));
        assert!(is_benign_merge_race(
            "task left review before the merge began"
        ));
        // The queue's own CAS miss: the row moved on (a follow-up, a stop)
        // while its queued dispatch waited for the folder lock.
        assert!(is_benign_merge_race(
            "task left review before the merge was queued"
        ));
        // A withdrawal / edit under the pump is benign too, but it is the one
        // the drain must RE-READ for instead of stepping over: the user's new
        // word may sort first, and leaving the slot to the auto sweep would
        // land it with the unattended defaults.
        let superseded = missed_queue_cas(Some(&QueuedMergeClaim {
            raw: "{}".to_string(),
            queued_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        }));
        assert!(is_benign_merge_race(&superseded));
        assert!(is_queued_merge_superseded(&superseded));
        assert!(!is_queued_merge_superseded("task is not in review"));
        assert!(!is_queued_merge_superseded(&missed_queue_cas(None)));
        assert!(is_benign_merge_race(
            "another task of this project is already merging — wait for it"
        ));
        assert!(!is_benign_merge_race(
            "project folder is on 'feature', expected 'main' — switch it to 'main' to merge"
        ));
        assert!(!is_benign_merge_race(
            "the task worktree no longer exists on disk"
        ));
    }

    /// The merge queue is first-asked-first-served, with the task id breaking a
    /// tie — the same rule `mergeQueueRanks` draws on the cards, so the "第 2
    /// 位" a user reads is the order they actually land in.
    #[test]
    fn the_merge_queue_drains_oldest_request_first() {
        let intent = |secs: i64| WorkTaskQueuedMerge {
            message: None,
            delete_worktree: true,
            instructions: None,
            queued_at: chrono::DateTime::from_timestamp(1_800_000_000 + secs, 0)
                .expect("valid instant"),
        };
        let row = |id: i32| crate::db::entities::work_task::Model {
            id,
            ..task_row()
        };

        let mut queue = [
            (intent(2), row(1)),
            (intent(1), row(3)),
            // Same instant as task 3 — id decides, exactly as the client does.
            (intent(1), row(2)),
        ];
        queue.sort_by_key(|(intent, task)| queue_order(intent, task));
        assert_eq!(
            queue.iter().map(|(_, t)| t.id).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    /// Minimal task row for prompt composition (nothing here touches the DB).
    fn task_row() -> crate::db::entities::work_task::Model {
        let now = chrono::Utc::now();
        crate::db::entities::work_task::Model {
            id: 7,
            folder_id: 1,
            title: "Fix the login flow".to_string(),
            config: "{}".to_string(),
            status: WorkTaskStatus::Queued,
            failure_reason: None,
            last_error: None,
            run_seq: 1,
            sort_order: 0,
            worktree_folder_id: None,
            conversation_id: None,
            connection_id: None,
            base_branch: Some("main".to_string()),
            base_sha: None,
            work_branch: Some("task/7".to_string()),
            merge_state: None,
            pending_merge: None,
            cleanup_state: None,
            verdict: None,
            result_summary: None,
            files_changed: None,
            additions: None,
            deletions: None,
            merge_commit: None,
            completion_kind: None,
            preflight: None,
            archived_at: None,
            scheduled_at: None,
            source_kind: None,
            source_key: None,
            source_meta: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            settled_at: None,
            finished_at: None,
            deleted_at: None,
        }
    }

    fn task_config() -> WorkTaskConfig {
        WorkTaskConfig {
            prompt_blocks: vec![serde_json::json!({
                "type": "text",
                "text": "Fix the login flow and add tests."
            })],
            ..Default::default()
        }
    }

    fn settings_with(pairs: &[(&str, &str)]) -> WorkTaskFolderSettings {
        WorkTaskFolderSettings {
            stage_prompts: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn texts(blocks: &[PromptInputBlock]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                PromptInputBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn merge_mode() -> LaunchMode {
        LaunchMode::Merge {
            root_path: "/repo".to_string(),
            base_branch: "main".to_string(),
            work_branch: "task/7".to_string(),
            strategy: "squash".to_string(),
            message: None,
            instructions: None,
        }
    }

    fn return_mode(intent: FollowUpIntent) -> LaunchMode {
        LaunchMode::Return {
            intent,
            feedback: "please fix the copy".to_string(),
            attachments: Vec::new(),
        }
    }

    /// Insert a task row so events have something to hang off, then compose.
    async fn seeded_task(conn: &sea_orm::DatabaseConnection) -> i32 {
        use sea_orm::{ActiveModelTrait, Set};
        let now = chrono::Utc::now();
        let row = crate::db::entities::work_task::ActiveModel {
            folder_id: Set(1),
            title: Set("Fix the login flow".to_string()),
            config: Set("{}".to_string()),
            status: Set(WorkTaskStatus::Failed),
            run_seq: Set(1),
            sort_order: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        row.insert(conn).await.expect("insert task").id
    }

    async fn user_action(conn: &sea_orm::DatabaseConnection, id: i32, payload: serde_json::Value) {
        work_task_service::record_event(conn, id, "user_action", "user", Some(payload))
            .await
            .expect("record user action");
    }

    async fn settled_into_review(conn: &sea_orm::DatabaseConnection, id: i32) {
        work_task_service::record_event(
            conn,
            id,
            "status_changed",
            "engine",
            Some(serde_json::json!({ "to": "review" })),
        )
        .await
        .expect("record settle");
    }

    /// The replay half of [`instruction_scan`], for the cases that assert on
    /// the instruction alone and not on the licence it implies.
    async fn outstanding_instruction(
        conn: &sea_orm::DatabaseConnection,
        task_id: i32,
    ) -> Option<Outstanding> {
        instruction_scan(conn, task_id).await.outstanding
    }

    /// Each scenario reframes the SAME user text — that is the whole point of
    /// having scenarios, and `revise` must stay byte-identical to the wording
    /// the action had before they existed.
    #[tokio::test]
    async fn follow_up_scenarios_reframe_the_same_feedback() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let cases = [
            (
                FollowUpIntent::Revise,
                "The user reviewed your work on this task and returned it with the following \
                 feedback. Address it in this same worktree:\n\nplease fix the copy",
            ),
            (FollowUpIntent::Continue, "accepted it as it stands"),
            (FollowUpIntent::Question, "This is a question, not a work order"),
            (FollowUpIntent::Verify, "read the full diff of this worktree"),
        ];
        for (intent, expected) in cases {
            let blocks = compose_prompt(
                &task_config(),
                &task_row(),
                &return_mode(intent),
                &WorkTaskFolderSettings::default(),
                true,
                &db.conn,
            )
            .await
            .expect("compose");
            let joined = texts(&blocks).join("\n");
            assert!(
                joined.contains(expected),
                "{}: missing its own framing in {joined}",
                intent.as_str()
            );
            assert!(
                joined.contains("please fix the copy"),
                "{}: dropped the user's text",
                intent.as_str()
            );
        }
    }

    /// The standing guard licenses committing, and it is the LAST block the
    /// agent reads — so a question turn has to replace it, not merely say
    /// "don't edit" a few blocks earlier.
    #[tokio::test]
    async fn a_question_turn_withdraws_the_licence_to_commit() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &return_mode(FollowUpIntent::Question),
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        let guard = texts(&blocks)
            .into_iter()
            .find(|t| t.starts_with("—— Work task context ——"))
            .expect("guard block");
        assert!(!guard.contains("Commit to the current branch as you like"));
        assert!(guard.contains("do NOT create, edit, delete or commit any file"));
        // The base-branch rules survive the swap.
        assert!(guard.contains("push the base branch"));

        // …and a working scenario keeps the original licence.
        let working = compose_prompt(
            &task_config(),
            &task_row(),
            &return_mode(FollowUpIntent::Revise),
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(texts(&working)
            .iter()
            .any(|t| t.contains("Commit to the current branch as you like")));
    }

    /// A report-deliverable task (forge plan-first / review-only)
    /// swaps the commit licence on its ORIGINAL order — otherwise the guard's
    /// "commit as you like", being the last block read, would quietly undo the
    /// task's own "analysis only" instruction.
    #[tokio::test]
    async fn a_report_deliverable_order_swaps_the_commit_licence() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let report_cfg = WorkTaskConfig {
            deliverable: Some(DELIVERABLE_REPORT.to_string()),
            ..task_config()
        };
        let guard_of = |blocks: &[PromptInputBlock]| {
            texts(blocks)
                .into_iter()
                .find(|t| t.starts_with("—— Work task context ——"))
                .expect("guard block")
        };

        let fresh = compose_prompt(
            &report_cfg,
            &task_row(),
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let guard = guard_of(&fresh);
        assert!(!guard.contains("Commit to the current branch as you like"));
        assert!(guard.contains("This turn delivers a report"));
        // The base-branch rules survive the swap.
        assert!(guard.contains("push the base branch"));

        // A retry with nothing outstanding re-runs that same order.
        let retry = compose_prompt(
            &report_cfg,
            &task_row(),
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(guard_of(&retry).contains("This turn delivers a report"));

        // Returned for changes, the write licence comes BACK: "now apply the
        // fix" is precisely a change order, and it is how a report task's
        // loop is meant to close.
        let returned = compose_prompt(
            &report_cfg,
            &task_row(),
            &return_mode(FollowUpIntent::Revise),
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(guard_of(&returned).contains("Commit to the current branch as you like"));

        // Same for a retry that stands in for an interrupted review follow-up:
        // the outstanding feedback, not the original order, is the work.
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return",
                "intent": "revise",
                "feedback": "apply the fix you recommended",
            }),
        )
        .await;
        let mut row = task_row();
        row.id = id;
        let retry_review = compose_prompt(
            &report_cfg,
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(guard_of(&retry_review).contains("Commit to the current branch as you like"));

        // An unrecognized deliverable value reads as a normal task: a config
        // written by a newer build must still launch, not change meaning.
        let odd_cfg = WorkTaskConfig {
            deliverable: Some("something-newer".to_string()),
            ..task_config()
        };
        let odd = compose_prompt(
            &odd_cfg,
            &task_row(),
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(guard_of(&odd).contains("Commit to the current branch as you like"));
    }

    /// A retry stands in for the turn it interrupted, and a retry/requeue note
    /// refines that turn without changing its KIND — so the guard's licence
    /// follows the unsettled follow-up underneath the note, not the note.
    #[tokio::test]
    async fn a_retry_licence_follows_the_interrupted_turn_not_the_note() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let report_cfg = WorkTaskConfig {
            deliverable: Some(DELIVERABLE_REPORT.to_string()),
            ..task_config()
        };
        let guard_of = |blocks: &[PromptInputBlock]| {
            texts(blocks)
                .into_iter()
                .find(|t| t.starts_with("—— Work task context ——"))
                .expect("guard block")
        };

        // An unanswered question retried: the replay already says "do not
        // change any files for it", so the guard must withdraw the commit
        // grant as well — for every task, not only report ones.
        let questioned = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            questioned,
            serde_json::json!({
                "action": "return",
                "intent": "question",
                "feedback": "why is the cap 200?",
            }),
        )
        .await;
        let mut row = task_row();
        row.id = questioned;
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        let guard = guard_of(&blocks);
        assert!(guard.contains("do NOT create, edit, delete or commit any file"));
        assert!(!guard.contains("Commit to the current branch as you like"));

        // A note layered over a failed "apply the fix" return on a report
        // task: the turn underneath is a change order, so the write licence
        // survives — while the note is still the replayed instruction.
        let returned = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            returned,
            serde_json::json!({
                "action": "return",
                "intent": "revise",
                "feedback": "apply the fix you recommended",
            }),
        )
        .await;
        user_action(
            &db.conn,
            returned,
            serde_json::json!({
                "action": "retry",
                "note": "it failed on CI, go again",
                "blocks": [],
            }),
        )
        .await;
        row.id = returned;
        let blocks = compose_prompt(
            &report_cfg,
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(guard_of(&blocks).contains("Commit to the current branch as you like"));
        assert!(texts(&blocks).join("\n").contains("it failed on CI, go again"));

        // The same note on a run that never reached review: still the
        // original report order.
        let fresh_note = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            fresh_note,
            serde_json::json!({
                "action": "retry",
                "note": "network glitch, go again",
                "blocks": [],
            }),
        )
        .await;
        row.id = fresh_note;
        for (label, mode) in [("fresh", LaunchMode::Fresh), ("retry", LaunchMode::Retry)] {
            let blocks = compose_prompt(
                &report_cfg,
                &row,
                &mode,
                &WorkTaskFolderSettings::default(),
                true,
                &db.conn,
            )
            .await
            .expect("compose");
            assert!(
                guard_of(&blocks).contains("This turn delivers a report"),
                "restart note on the original order keeps the report licence ({label})"
            );
        }
    }

    /// The one scenario that stands alone without user text.
    #[tokio::test]
    async fn a_self_check_composes_without_any_user_text() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &LaunchMode::Return {
                intent: FollowUpIntent::Verify,
                feedback: String::new(),
                attachments: Vec::new(),
            },
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(joined.contains("read the full diff of this worktree"));
        assert!(!joined.contains("What they want you to pay attention to"));
    }

    /// A retry replays what the interrupted generation still owed — and an
    /// unanswered question must not come back as a work order.
    #[tokio::test]
    async fn a_retry_replays_an_unanswered_question_as_a_question() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return", "intent": "question", "feedback": "why the extra table?",
            }),
        )
        .await;

        let mut row = task_row();
        row.id = id;
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(joined.contains("never got an answer"));
        assert!(joined.contains("why the extra table?"));
        assert!(!joined.contains("Latest review feedback to address"));
    }

    /// The lookup is a barrier, not a filter: skipping past the newest
    /// instruction would resurrect an older one the agent already carried out.
    #[tokio::test]
    async fn a_newer_instruction_hides_an_older_one() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return", "intent": "revise", "feedback": "rename the column",
            }),
        )
        .await;
        settled_into_review(&db.conn, id).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return", "intent": "question", "feedback": "why the extra table?",
            }),
        )
        .await;

        let outstanding = outstanding_instruction(&db.conn, id)
            .await
            .expect("an outstanding instruction");
        assert_eq!(
            outstanding.kind,
            OutstandingKind::Review(FollowUpIntent::Question)
        );
        assert_eq!(outstanding.text, "why the extra table?");
    }

    /// A completed turn consumes the instruction it carried, so it stops being
    /// re-injected into every generation for the rest of the task's life.
    #[tokio::test]
    async fn settling_into_review_consumes_the_instruction() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "requeue", "note": "install deps first" }),
        )
        .await;
        assert!(outstanding_instruction(&db.conn, id).await.is_some());

        settled_into_review(&db.conn, id).await;
        assert!(outstanding_instruction(&db.conn, id).await.is_none());
    }

    /// A task canceled before it ever had a session comes back through `Fresh`,
    /// so the note the user attached has to reach that arm too.
    #[tokio::test]
    async fn a_restart_note_reaches_a_fresh_launch() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "requeue", "note": "target the v2 API this time" }),
        )
        .await;

        let mut row = task_row();
        row.id = id;
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(joined.contains("target the v2 API this time"));
        assert!(joined.contains("The user restarted this task"));
        // The task's own brief still opens the prompt, so the transcript's
        // phase divider keeps matching on it.
        assert_eq!(prompt_head(&blocks), "Fix the login flow and add tests.");
        // Nothing ever ran (`started_at` is unset), so nothing warns about a
        // worktree that cannot hold an earlier run's work.
        assert!(!joined.contains("running this task again from the top"));
    }

    /// A requeue drops the session link, so the run the user put back on the
    /// board comes through `Fresh` — in the SAME worktree, carrying whatever
    /// the stopped generation left there. The prompt has to say so: read as a
    /// first run it would have the agent redo work that is already committed on
    /// the branch.
    #[tokio::test]
    async fn a_requeued_run_says_its_worktree_is_not_empty() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;

        let mut row = task_row();
        row.id = id;
        row.started_at = Some(chrono::Utc::now());
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(
            joined.contains("Its worktree may still hold that run's work"),
            "a re-run must not read as a first run: {joined}"
        );
        // The brief itself still reaches the agent — the framing precedes it,
        // exactly as the retry / return arms do when their session is new.
        assert!(joined.contains("Fix the login flow and add tests."));
        // The original order is still an order: the licence stays the working
        // one (nothing was returned, so there is no follow-up underneath).
        assert!(joined.contains("Commit to the current branch as you like"));
    }

    /// #649's requeue lands on `Fresh`, and a task can be sitting on a
    /// follow-up when the user stops it: review → "rework this" → cancel →
    /// requeue. The feedback is the whole point of that generation, so the
    /// arm that replaces it must replay it — dropping it sends the agent back
    /// to the original order in a worktree where that order is already done.
    #[tokio::test]
    async fn a_requeued_run_replays_the_follow_up_it_interrupted() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return",
                "intent": "revise",
                "feedback": "the empty-state copy is still wrong",
            }),
        )
        .await;
        // A requeue with neither note nor attachment records no action at all
        // (`requeue_canceled` writes one only when the user wrote something),
        // so the returned feedback is still the newest instruction.

        let mut row = task_row();
        row.id = id;
        row.started_at = Some(chrono::Utc::now());
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(
            joined.contains("the empty-state copy is still wrong"),
            "the returned feedback must survive the requeue: {joined}"
        );
        assert!(joined.contains("Latest review feedback to address"));
    }

    /// And the licence follows that same turn. A question the user asked before
    /// stopping the run is still a question after the requeue: the guard's
    /// "commit as you like" is the last thing the agent reads, so it has to be
    /// withdrawn here just as the retry arm withdraws it.
    #[tokio::test]
    async fn a_requeued_question_keeps_its_read_only_licence() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return",
                "intent": "question",
                "feedback": "why did you pick a map here?",
            }),
        )
        .await;

        let mut row = task_row();
        row.id = id;
        row.started_at = Some(chrono::Utc::now());
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let guard = texts(&blocks)
            .into_iter()
            .find(|t| t.starts_with("—— Work task context ——"))
            .expect("guard block");
        assert!(!guard.contains("Commit to the current branch as you like"));
        assert!(guard.contains("do NOT create, edit, delete or commit any file"));
    }

    /// …and the note the user types INTO the requeue box must not bury it. The
    /// scan stops at the newest instruction, so a note takes the `outstanding`
    /// slot and only the returned turn's *intent* survives — fine for a resumed
    /// session, which still has the words, and useless for the session a
    /// requeue now starts, which would get a read-only licence for a question
    /// it was never shown, or "take this note into account" with no account.
    #[tokio::test]
    async fn a_requeue_note_does_not_bury_the_turn_it_refines() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return",
                "intent": "question",
                "feedback": "why did you pick a map here?",
            }),
        )
        .await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "requeue", "note": "check the logs first" }),
        )
        .await;

        let mut row = task_row();
        row.id = id;
        row.started_at = Some(chrono::Utc::now());
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&blocks).join("\n");
        assert!(
            joined.contains("why did you pick a map here?"),
            "the question the note refines has to reach a context-less session: {joined}"
        );
        assert!(joined.contains("check the logs first"));
        // Oldest first: the turn, then the note that refines it.
        let question = joined.find("why did you pick a map here?").expect("question");
        let note = joined.find("check the logs first").expect("note");
        assert!(question < note, "the note refines the turn, so it follows it");
        // And it is still a question, so the licence stays read-only.
        let guard = texts(&blocks)
            .into_iter()
            .find(|t| t.starts_with("—— Work task context ——"))
            .expect("guard block");
        assert!(guard.contains("do NOT create, edit, delete or commit any file"));
    }

    /// The mirror image, and the reason the extra replay is gated on `resumed`:
    /// a session that really did resume already holds the returned turn, and
    /// replaying it a second time would re-ask a question the transcript above
    /// already shows being asked.
    #[tokio::test]
    async fn a_resumed_retry_replays_only_the_newest_instruction() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "return", "intent": "revise", "feedback": "rename the column",
            }),
        )
        .await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "retry", "note": "install deps first" }),
        )
        .await;

        let mut row = task_row();
        row.id = id;
        let resumed = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&resumed).join("\n");
        assert!(joined.contains("install deps first"));
        assert!(
            !joined.contains("rename the column"),
            "the resumed transcript already carries it: {joined}"
        );

        // …while the fallback session that never resumed needs both.
        let fell_back = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let joined = texts(&fell_back).join("\n");
        assert!(joined.contains("rename the column"));
        assert!(joined.contains("install deps first"));
    }

    /// A screenshot pasted into the follow-up box has to reach the agent as an
    /// image block, right behind the sentence that framed it — dropping it
    /// would leave the framing pointing at nothing.
    #[tokio::test]
    async fn a_follow_up_carries_its_attachments_after_the_framing() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &LaunchMode::Return {
                intent: FollowUpIntent::Revise,
                feedback: "the header is wrong, see this".to_string(),
                attachments: vec![
                    serde_json::json!({
                        "type": "image", "data": "aGk=", "mime_type": "image/png", "uri": null,
                    }),
                    // A block that no longer deserializes is dropped, not fatal:
                    // one bad attachment must not stop the run carrying the rest.
                    serde_json::json!({ "type": "not_a_block" }),
                ],
            },
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        // The framing sentence, then the image it refers to, then the worktree
        // guard every prompt ends with.
        let framing = blocks
            .iter()
            .position(
                |b| matches!(b, PromptInputBlock::Text { text } if text.contains("the header is wrong, see this")),
            )
            .expect("the feedback is in there");
        assert!(
            matches!(
                blocks.get(framing + 1),
                Some(PromptInputBlock::Image { data, .. }) if data == "aGk="
            ),
            "the image follows the framing text, unparseable blocks aside: {blocks:?}"
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, PromptInputBlock::Image { .. }))
                .count(),
            1
        );
    }

    /// The same attachments have to survive the replay path: a run interrupted
    /// before it answered owes the user the screenshot as well as the sentence.
    #[tokio::test]
    async fn an_outstanding_instruction_replays_its_attachments() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({
                "action": "retry",
                "note": "it looked like this",
                "blocks": [
                    { "type": "image", "data": "aGk=", "mime_type": "image/png", "uri": null },
                ],
            }),
        )
        .await;

        let outstanding = outstanding_instruction(&db.conn, id)
            .await
            .expect("an outstanding instruction");
        assert_eq!(outstanding.attachments.len(), 1);

        let mut row = task_row();
        row.id = id;
        let blocks = compose_prompt(
            &task_config(),
            &row,
            &LaunchMode::Retry,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, PromptInputBlock::Image { data, .. } if data == "aGk=")),
            "the replayed instruction keeps its image: {blocks:?}"
        );
    }

    /// A task's image blocks are stored, so the encoding the composer chose can
    /// be stale by the time the run happens (a slow probe then, a different
    /// agent now). Dispatch re-encodes for whoever actually answered.
    #[test]
    fn images_are_reencoded_for_the_agent_that_answered() {
        let caps = |image, embedded_context| PromptCapabilitiesInfo {
            image,
            audio: false,
            embedded_context,
        };
        let image = || PromptInputBlock::Image {
            data: "aGk=".to_string(),
            mime_type: "image/png".to_string(),
            uri: None,
        };
        let embedded = || PromptInputBlock::Resource {
            uri: "file:///shot.png".to_string(),
            mime_type: Some("image/png".to_string()),
            text: None,
            blob: Some("aGk=".to_string()),
        };

        // Native image → embedded blob for an agent that takes only the latter,
        // with a stable synthetic uri for a path-less screenshot.
        let mut blocks = vec![PromptInputBlock::Text { text: "see".into() }, image()];
        reencode_images(&mut blocks, &caps(false, true));
        assert!(
            matches!(
                &blocks[1],
                PromptInputBlock::Resource { uri, mime_type, text: None, blob: Some(b) }
                    if uri == "clipboard://work-task-image-1"
                        && mime_type.as_deref() == Some("image/png")
                        && b == "aGk="
            ),
            "{:?}",
            blocks[1]
        );
        // The prose beside it is untouched.
        assert!(matches!(&blocks[0], PromptInputBlock::Text { text } if text == "see"));

        // …and back, for an agent that takes images but not embedded context.
        let mut blocks = vec![embedded()];
        reencode_images(&mut blocks, &caps(true, false));
        assert!(
            matches!(
                &blocks[0],
                PromptInputBlock::Image { data, mime_type, uri: Some(u) }
                    if data == "aGk=" && mime_type == "image/png" && u == "file:///shot.png"
            ),
            "{:?}",
            blocks[0]
        );

        // An agent that takes both, or neither, gets exactly what was stored:
        // there is no better shape to reach for in either case.
        for c in [caps(true, true), caps(false, false)] {
            let mut blocks = vec![image(), embedded()];
            reencode_images(&mut blocks, &c);
            assert!(matches!(&blocks[0], PromptInputBlock::Image { uri: None, .. }));
            assert!(matches!(&blocks[1], PromptInputBlock::Resource { blob: Some(_), .. }));
        }

        // A non-image embedded resource (a pasted text file) is never turned
        // into an image, whatever the agent accepts.
        let mut blocks = vec![PromptInputBlock::Resource {
            uri: "clipboard://notes".to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: None,
            blob: Some("aGk=".to_string()),
        }];
        reencode_images(&mut blocks, &caps(true, false));
        assert!(matches!(
            &blocks[0],
            PromptInputBlock::Resource { mime_type, .. }
                if mime_type.as_deref() == Some("text/markdown")
        ));
    }

    /// An event written before follow-ups could carry attachments has no
    /// `blocks` field at all — that has to read as "nothing was attached".
    #[tokio::test]
    async fn a_legacy_instruction_without_blocks_still_replays() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "return", "intent": "revise", "feedback": "redo it" }),
        )
        .await;

        let outstanding = outstanding_instruction(&db.conn, id)
            .await
            .expect("an outstanding instruction");
        assert_eq!(outstanding.text, "redo it");
        assert!(outstanding.attachments.is_empty());
    }

    /// Two ways a busy task could hide its own instruction: `list_events`
    /// returns the OLDEST rows within its limit, and an agent's progress
    /// milestones are unbounded in number, so scanning raw events would bury
    /// the instruction under them.
    #[tokio::test]
    async fn a_chatty_run_cannot_bury_the_latest_instruction() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        user_action(
            &db.conn,
            id,
            serde_json::json!({ "action": "retry", "note": "the DB was locked" }),
        )
        .await;
        for _ in 0..520 {
            work_task_service::record_event(&db.conn, id, "agent_progress", "agent", None)
                .await
                .expect("filler event");
        }

        let outstanding = outstanding_instruction(&db.conn, id)
            .await
            .expect("found under the filler");
        assert_eq!(outstanding.kind, OutstandingKind::Restart);
        assert_eq!(outstanding.text, "the DB was locked");
    }

    /// A follow-up recorded by `claim_for_run_with_action` must be readable the
    /// moment the claim commits — a pump can launch the generation right after.
    #[tokio::test]
    async fn a_claim_carries_its_instruction_atomically() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let id = seeded_task(&db.conn).await;
        let seq = work_task_service::claim_for_run_with_action(
            &db.conn,
            id,
            WorkTaskStatus::Failed,
            "user",
            Some(serde_json::json!({ "action": "retry", "note": "install deps first" })),
            false,
        )
        .await
        .expect("claim")
        .expect("claim won");
        assert_eq!(seq, 2);

        let outstanding = outstanding_instruction(&db.conn, id)
            .await
            .expect("instruction visible right after the claim");
        assert_eq!(outstanding.text, "install deps first");

        // A LOST claim must not leave an instruction behind for some later
        // generation to pick up.
        let lost = work_task_service::claim_for_run_with_action(
            &db.conn,
            id,
            WorkTaskStatus::Failed,
            "user",
            Some(serde_json::json!({ "action": "retry", "note": "orphan" })),
            false,
        )
        .await
        .expect("claim");
        assert!(lost.is_none());
        assert_eq!(
            outstanding_instruction(&db.conn, id).await.unwrap().text,
            "install deps first"
        );
    }

    #[tokio::test]
    async fn stage_prompts_land_after_the_built_in_guard() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = settings_with(&[("all", "Reply in Chinese."), ("work", "Run pnpm test.")]);
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &LaunchMode::Fresh,
            &settings,
            false,
            &db.conn,
        )
        .await
        .expect("compose");

        let texts = texts(&blocks);
        // …task blocks…, worktree guard, then the user's own instructions.
        assert!(texts[texts.len() - 2].starts_with("—— Work task context ——"));
        let last = texts.last().expect("trailing block");
        assert!(last.starts_with("—— Additional instructions ——"));
        // "all" first, then the stage's own text.
        let all_at = last.find("Reply in Chinese.").expect("all text");
        let work_at = last.find("Run pnpm test.").expect("stage text");
        assert!(all_at < work_at);
        // The round marker still keys off the task's own opening line, so the
        // transcript's phase dividers are unaffected.
        assert_eq!(prompt_head(&blocks), "Fix the login flow and add tests.");
    }

    #[tokio::test]
    async fn stage_prompts_select_only_their_own_stage() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = settings_with(&[
            ("all", "EVERY-STAGE"),
            ("work", "WORK-ONLY"),
            ("retry", "RETRY-ONLY"),
            ("return", "RETURN-ONLY"),
            ("merge", "MERGE-ONLY"),
        ]);
        let modes = [
            (LaunchMode::Fresh, "WORK-ONLY"),
            (LaunchMode::Retry, "RETRY-ONLY"),
            (return_mode(FollowUpIntent::Revise), "RETURN-ONLY"),
            // Every scenario shares the `return` stage, so the settings dialog
            // stays four stages wide however many scenarios exist.
            (return_mode(FollowUpIntent::Continue), "RETURN-ONLY"),
            (return_mode(FollowUpIntent::Question), "RETURN-ONLY"),
            (return_mode(FollowUpIntent::Verify), "RETURN-ONLY"),
            (merge_mode(), "MERGE-ONLY"),
        ];
        for (mode, expected) in modes {
            let blocks = compose_prompt(
                &task_config(),
                &task_row(),
                &mode,
                &settings,
                false,
                &db.conn,
            )
            .await
            .expect("compose");
            let joined = texts(&blocks).join("\n");
            assert!(joined.contains("EVERY-STAGE"), "{expected}: missing all-stage text");
            assert!(joined.contains(expected), "{expected}: missing own text");
            for other in ["WORK-ONLY", "RETRY-ONLY", "RETURN-ONLY", "MERGE-ONLY"] {
                if other != expected {
                    assert!(!joined.contains(other), "{expected}: leaked {other}");
                }
            }
        }
    }

    #[tokio::test]
    async fn merge_stage_keeps_its_extra_without_the_guard() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let settings = settings_with(&[("merge", "Write the commit message in Chinese.")]);
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &merge_mode(),
            &settings,
            true,
            &db.conn,
        )
        .await
        .expect("compose");

        let texts = texts(&blocks);
        // The merge generation replaces the guard (it forbids exactly what a
        // merge must do) — the user's extra still trails it.
        assert!(texts.iter().all(|t| !t.starts_with("—— Work task context ——")));
        assert!(texts[0].contains("land it onto the base branch"));
        assert!(texts
            .last()
            .expect("trailing block")
            .contains("Write the commit message in Chinese."));
    }

    /// What the user typed in the merge dialog has to reach the agent — and has
    /// to land BEFORE the closing rules, which are what keep "also do X" from
    /// being read as licence to push or to delete the worktree.
    #[tokio::test]
    async fn merge_prompt_carries_the_users_instructions_ahead_of_the_rules() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let LaunchMode::Merge {
            root_path,
            base_branch,
            work_branch,
            strategy,
            message,
            ..
        } = merge_mode()
        else {
            unreachable!("merge_mode is a merge")
        };
        let mode = LaunchMode::Merge {
            root_path,
            base_branch,
            work_branch,
            strategy,
            message,
            instructions: Some("Prefer this branch's side on any conflict.".to_string()),
        };
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &mode,
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");

        let recipe = &texts(&blocks)[0];
        let said = recipe
            .find("Prefer this branch's side on any conflict.")
            .expect("the user's own words reach the agent");
        let rules = recipe.find("Do NOT push").expect("the closing rules");
        assert!(
            said < rules,
            "the rules must stay last — they are what an over-eager reading of \
             the user's request would otherwise override"
        );
    }

    /// …and an empty box adds nothing: no dangling header, no stray blank
    /// section between the numbered steps and the rules.
    #[tokio::test]
    async fn merge_prompt_says_nothing_when_there_are_no_instructions() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let blocks = compose_prompt(
            &task_config(),
            &task_row(),
            &merge_mode(), // `instructions: None`
            &WorkTaskFolderSettings::default(),
            true,
            &db.conn,
        )
        .await
        .expect("compose");

        let recipe = &texts(&blocks)[0];
        assert!(
            !recipe.contains("instructions from the user"),
            "no header without a body"
        );
        assert!(recipe.contains("land it onto the base branch"));
    }

    #[tokio::test]
    async fn blank_stage_prompts_add_nothing() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let bare = compose_prompt(
            &task_config(),
            &task_row(),
            &LaunchMode::Fresh,
            &WorkTaskFolderSettings::default(),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        let blank = compose_prompt(
            &task_config(),
            &task_row(),
            &LaunchMode::Fresh,
            &settings_with(&[("all", "  \n "), ("work", "")]),
            false,
            &db.conn,
        )
        .await
        .expect("compose");
        assert_eq!(texts(&bare), texts(&blank));
    }

    /// The worktree is off disk; everything below is what the clients see.
    /// Without the `folder://changed` delete the removed worktree keeps
    /// rendering in every sidebar until the next full `fetchFolders`.
    #[tokio::test]
    async fn worktree_removal_announces_the_dropped_folder() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
        use crate::web::event_bridge::WebEventBroadcaster;

        let db = fresh_in_memory_db().await;
        let root_id = seed_folder(&db, "/tmp/repo").await;
        let wt = open_worktree_folder_core(&db, "/tmp/repo-task-1".to_string(), root_id)
            .await
            .expect("worktree folder");
        let conv = conversation_service::create(&db.conn, wt.id, AgentType::ClaudeCode, None, None)
            .await
            .expect("conversation");
        let task = work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id: root_id,
                title: "fix login".to_string(),
                config: serde_json::json!({
                    "display_text": "fix login",
                    "prompt_blocks": [{ "type": "text", "text": "fix login" }],
                }),
            },
        )
        .await
        .expect("task");
        work_task_service::attach_worktree(&db.conn, task.id, wt.id, "main", "abc", "task/1")
            .await
            .expect("attach");

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster.clone());

        converge_worktree_removal(&db, &emitter, task.id, wt.id, root_id, &wt.path).await;

        // The folder row is gone from every list a client can fetch…
        assert!(
            crate::db::service::folder_service::get_folder_by_id(&db.conn, wt.id)
                .await
                .expect("lookup")
                .is_none()
        );
        // …its conversation moved to the project folder (stamped with where it ran)…
        let moved = conversation_service::get_by_id(&db.conn, conv.id)
            .await
            .expect("conversation row");
        assert_eq!(moved.folder_id, root_id);
        assert_eq!(moved.origin_cwd.as_deref(), Some("/tmp/repo-task-1"));
        // …and the task no longer points at it.
        let after = work_task_service::get_model(&db.conn, task.id)
            .await
            .expect("task row");
        assert_eq!(after.worktree_folder_id, None);

        // Conversations first (that refetch is what re-places the moved rows),
        // then the folder drop.
        let bulk = rx.try_recv().expect("bulk change should broadcast");
        assert_eq!(
            bulk.channel,
            crate::web::event_bridge::CONVERSATIONS_BULK_CHANGED_EVENT
        );
        let dropped = rx.try_recv().expect("folder delete should broadcast");
        assert_eq!(
            dropped.channel,
            crate::web::event_bridge::FOLDER_CHANGED_EVENT
        );
        assert_eq!(dropped.payload["kind"], "deleted");
        assert_eq!(dropped.payload["id"], wt.id);
    }

    /// A retried cleanup finds the row already soft-deleted. It must still
    /// announce the drop — a client that missed the first broadcast is the
    /// reason the user retried.
    #[tokio::test]
    async fn worktree_removal_re_announces_an_already_deleted_folder() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
        use crate::web::event_bridge::WebEventBroadcaster;

        let db = fresh_in_memory_db().await;
        let root_id = seed_folder(&db, "/tmp/repo2").await;
        let task = work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id: root_id,
                title: "fix login".to_string(),
                config: serde_json::json!({
                    "display_text": "fix login",
                    "prompt_blocks": [{ "type": "text", "text": "fix login" }],
                }),
            },
        )
        .await
        .expect("task");

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster.clone());

        // Folder id 9999 never existed — the "already gone" branch.
        converge_worktree_removal(&db, &emitter, task.id, 9999, root_id, "/tmp/repo2-task-1").await;

        let _bulk = rx.try_recv().expect("bulk change should broadcast");
        let dropped = rx.try_recv().expect("folder delete should broadcast");
        assert_eq!(dropped.payload["kind"], "deleted");
        assert_eq!(dropped.payload["id"], 9999);
    }

    #[test]
    fn engine_lock_is_per_data_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        let guard = match acquire_engine_ownership(dir.path()) {
            Ownership::Exclusive(f) => f,
            _ => panic!("first acquisition should win"),
        };
        assert!(matches!(
            acquire_engine_ownership(dir.path()),
            Ownership::Taken
        ));
        // Independent of the automation engine's lock file in the same dir.
        let automation_lock = dir
            .path()
            .join(format!("{}.lock", crate::db::database_file_name()));
        assert!(!automation_lock.exists());
        drop(guard);
    }

    // -- blocking prompts raised by a delegation sub-agent (#447) -----------

    const PARENT_CONN: &str = "conn-task";
    const CHILD_CONN: &str = "conn-child";

    /// A task driven to `running` on `PARENT_CONN`, with the engine's live
    /// index seeded as the launch path would. Returns `(engine, task_id)`.
    async fn running_task() -> (Arc<TaskEngine>, i32) {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/task-deleg").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("conversation");
        let task = work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id,
                title: "fix login".to_string(),
                config: serde_json::json!({
                    "display_text": "fix login",
                    "prompt_blocks": [{ "type": "text", "text": "fix login" }],
                }),
            },
        )
        .await
        .expect("task");
        // Walk the real transition chain rather than writing the row directly,
        // so the run_seq the engine flips against is the one the CAS minted.
        let run_seq = work_task_service::claim_for_run(
            &db.conn,
            task.id,
            WorkTaskStatus::Todo,
            "test",
        )
        .await
        .expect("claim")
        .expect("claimed");
        assert!(work_task_service::begin_setup(&db.conn, task.id, run_seq)
            .await
            .expect("begin_setup"));
        assert!(
            work_task_service::mark_running(&db.conn, task.id, run_seq, conv.id, PARENT_CONN)
                .await
                .expect("mark_running")
        );

        let engine = test_engine(db);
        engine
            .index
            .lock()
            .await
            .insert(PARENT_CONN.into(), (task.id, run_seq));
        (engine, task.id)
    }

    async fn status_of(engine: &TaskEngine, task_id: i32) -> WorkTaskStatus {
        work_task_service::get_model(&engine.db.conn, task_id)
            .await
            .expect("task row")
            .status
    }

    #[tokio::test]
    async fn a_late_cancelled_event_does_not_cancel_a_task_in_review() {
        let (engine, task_id) = running_task().await;
        let run_seq = work_task_service::get_model(&engine.db.conn, task_id)
            .await
            .expect("task row")
            .run_seq;
        assert!(work_task_service::settle_review(
            &engine.db.conn,
            task_id,
            run_seq,
            None,
            None,
        )
        .await
        .expect("settle review"));

        engine.on_turn_complete(PARENT_CONN, "cancelled").await;

        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Review);
    }

    #[tokio::test]
    async fn a_current_cancelled_event_cancels_the_running_task() {
        let (engine, task_id) = running_task().await;

        engine.on_turn_complete(PARENT_CONN, "cancelled").await;

        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Canceled);
    }

    /// The other half of the CAS: the status alone would let a stale
    /// connection's cancel land on the RUN AFTER it, which is `running` again
    /// and so passes the status gate. Only `run_seq` tells the two apart.
    #[tokio::test]
    async fn a_previous_generation_cancelled_event_spares_the_current_run() {
        let (engine, task_id) = running_task().await;
        let row = work_task_service::get_model(&engine.db.conn, task_id)
            .await
            .expect("task row");
        let (stale_seq, conv_id) = (row.run_seq, row.conversation_id.expect("conversation"));
        // Walk the real chain into the next generation: settle, requeue, run.
        assert!(work_task_service::settle_review(
            &engine.db.conn,
            task_id,
            stale_seq,
            None,
            None,
        )
        .await
        .expect("settle review"));
        let next_seq = work_task_service::claim_for_run(
            &engine.db.conn,
            task_id,
            WorkTaskStatus::Review,
            "test",
        )
        .await
        .expect("claim")
        .expect("claimed");
        assert_ne!(next_seq, stale_seq);
        assert!(work_task_service::begin_setup(&engine.db.conn, task_id, next_seq)
            .await
            .expect("begin_setup"));
        assert!(work_task_service::mark_running(
            &engine.db.conn,
            task_id,
            next_seq,
            conv_id,
            "conn-next",
        )
        .await
        .expect("mark_running"));

        // The old connection's cancel finally arrives.
        engine.on_turn_complete(PARENT_CONN, "cancelled").await;

        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    /// Walk the row on to a fresh generation running on `conn_id`, WITHOUT
    /// retiring the previous one's `index` entry — the state a delayed
    /// `TurnComplete` from the old connection lands in. Returns the new run_seq.
    async fn relaunch_on(engine: &TaskEngine, task_id: i32, conn_id: &str) -> i32 {
        let row = work_task_service::get_model(&engine.db.conn, task_id)
            .await
            .expect("task row");
        let conv_id = row.conversation_id.expect("conversation");

        assert!(work_task_service::settle_review(
            &engine.db.conn,
            task_id,
            row.run_seq,
            None,
            None,
        )
        .await
        .expect("settle review"));
        let next_seq = work_task_service::claim_for_run(
            &engine.db.conn,
            task_id,
            WorkTaskStatus::Review,
            "test",
        )
        .await
        .expect("claim")
        .expect("claimed");
        assert!(work_task_service::begin_setup(&engine.db.conn, task_id, next_seq)
            .await
            .expect("begin_setup"));
        assert!(work_task_service::mark_running(
            &engine.db.conn,
            task_id,
            next_seq,
            conv_id,
            conn_id,
        )
        .await
        .expect("mark_running"));
        engine
            .index
            .lock()
            .await
            .insert(conn_id.into(), (task_id, next_seq));
        next_seq
    }

    #[tokio::test]
    async fn a_previous_generation_cancel_does_not_clear_current_waits() {
        let (engine, task_id) = running_task().await;
        relaunch_on(&engine, task_id, "conn-next").await;

        engine
            .track_request("conn-next", "permission-a".into(), true)
            .await;
        engine
            .track_request("conn-next", "permission-b".into(), true)
            .await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );

        engine.on_turn_complete(PARENT_CONN, "cancelled").await;
        engine
            .track_request("conn-next", "permission-a".into(), false)
            .await;

        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput,
            "the second request in the current generation is still outstanding"
        );

        engine
            .track_request("conn-next", "permission-b".into(), false)
            .await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    /// The mirror image of the case above, and the half sparing the set opens:
    /// a retired run's DELEGATION children are namespaced by connection id, so
    /// keeping the task's set keeps THEIR keys too — with nothing left that
    /// could ever answer them, because the same retirement just unmapped the
    /// chain (`track_request` and `forget_delegation_child` both resolve a
    /// detached child to nothing). One orphan pins the set non-empty until a
    /// wholesale clear comes along — a later retirement that IS the last one
    /// out, or a cancel — and until then every generation's first request
    /// misses the `len() == 1` edge: the board says `running` for a run parked
    /// on the user, which is bug #447 all over again. Unlike the transient
    /// over-clear, answering the request is not what gets you out of it.
    #[tokio::test]
    async fn retiring_a_run_takes_its_orphaned_delegation_requests_with_it() {
        let (engine, task_id) = running_task().await;
        engine
            .on_event(&delegation_started(PARENT_CONN, CHILD_CONN))
            .await;
        engine.on_event(&permission_request(CHILD_CONN, "r1")).await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );

        relaunch_on(&engine, task_id, "conn-next").await;
        engine.on_turn_complete(PARENT_CONN, "cancelled").await;

        assert!(
            engine.awaiting.lock().await.get(&task_id).is_none(),
            "a child's key cannot outlive the delegation chain that named it"
        );

        // ...and the surviving generation's own edge still swings both ways.
        engine
            .track_request("conn-next", "p:r9".into(), true)
            .await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );
        engine
            .track_request("conn-next", "p:r9".into(), false)
            .await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    fn env(conn_id: &str, payload: AcpEvent) -> EventEnvelope {
        EventEnvelope {
            seq: 0,
            connection_id: conn_id.to_string(),
            payload,
        }
    }

    fn delegation_started(parent: &str, child: &str) -> EventEnvelope {
        env(
            parent,
            AcpEvent::DelegationStarted {
                parent_connection_id: parent.into(),
                parent_tool_use_id: "tu-1".into(),
                child_connection_id: child.into(),
                child_conversation_id: 1,
                agent_type: AgentType::Codex,
                task_preview: "run the tests".into(),
                task_id: "task-1".into(),
            },
        )
    }

    fn delegation_completed(parent: &str, child: &str) -> EventEnvelope {
        env(
            parent,
            AcpEvent::DelegationCompleted {
                parent_connection_id: parent.into(),
                parent_tool_use_id: "tu-1".into(),
                child_connection_id: child.into(),
                child_conversation_id: 1,
                agent_type: AgentType::Codex,
                result: crate::acp::types::DelegationResultSummary::Ok {
                    duration_ms: 0,
                    text_preview: None,
                },
            },
        )
    }

    fn permission_request(conn: &str, request_id: &str) -> EventEnvelope {
        env(
            conn,
            AcpEvent::PermissionRequest {
                request_id: request_id.into(),
                tool_call: serde_json::json!({}),
                options: vec![],
                queued: 0,
            },
        )
    }

    #[tokio::test]
    async fn a_delegated_child_permission_flips_the_task_to_awaiting_input() {
        // #447: the prompt arrives on the CHILD's connection, which is not in
        // `index`. Before the parent lookup existed it was dropped outright and
        // the board kept saying "running" for a run that was parked on the user.
        let (engine, task_id) = running_task().await;
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        engine.on_event(&permission_request(CHILD_CONN, "r1")).await;

        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::AwaitingInput);

        engine
            .on_event(&env(
                CHILD_CONN,
                AcpEvent::PermissionResolved {
                    request_id: "r1".into(),
                },
            ))
            .await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    #[tokio::test]
    async fn a_child_torn_down_mid_permission_does_not_wedge_awaiting_input() {
        // The failure this guards: a sub-agent cancelled/crashed while its
        // permission was still pending leaves a key nothing can ever resolve,
        // pinning the row at awaiting_input for the rest of the run.
        let (engine, task_id) = running_task().await;
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        engine.on_event(&permission_request(CHILD_CONN, "r1")).await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::AwaitingInput);

        engine.on_event(&delegation_completed(PARENT_CONN, CHILD_CONN)).await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::Running,
            "an unanswerable request must not outlive its sub-agent"
        );
        assert!(engine.delegation_parents.lock().await.is_empty());
    }

    #[tokio::test]
    async fn a_child_permission_does_not_clear_the_parents_own_block() {
        // Both connections' keys live in one set, so they must be independently
        // namespaced — otherwise resolving one would flip the row back while
        // the other is still waiting.
        let (engine, task_id) = running_task().await;
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        engine.on_event(&permission_request(PARENT_CONN, "r1")).await;
        // Same request_id on the child: a plausible collision, since the two
        // agents mint ids independently.
        engine.on_event(&permission_request(CHILD_CONN, "r1")).await;

        engine
            .on_event(&env(
                CHILD_CONN,
                AcpEvent::PermissionResolved {
                    request_id: "r1".into(),
                },
            ))
            .await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput,
            "the parent's own permission is still outstanding"
        );

        engine
            .on_event(&env(
                PARENT_CONN,
                AcpEvent::PermissionResolved {
                    request_id: "r1".into(),
                },
            ))
            .await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    #[tokio::test]
    async fn a_permission_racing_ahead_of_delegation_started_is_recovered() {
        // The broker starts the child's turn BEFORE announcing the delegation
        // (`send_prompt_linked_for_delegation` precedes `emit_started_if_real`),
        // so a child that blocks immediately raises its prompt while the engine
        // still has no mapping for it — and drops it. Without the backfill the
        // board sits at `running` for a run already parked on the user, which is
        // the very bug #447 is about.
        use crate::acp::session_state::PendingPermissionState;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let (engine, task_id) = running_task().await;
        engine
            .manager
            .insert_test_connection(CHILD_CONN, AgentType::Codex, None, EventEmitter::Noop)
            .await;

        // Permission arrives first and is dropped (no mapping yet).
        engine.on_event(&permission_request(CHILD_CONN, "r1")).await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);

        // The child's live state carries it regardless — that is what the
        // backfill reads when the announcement finally lands.
        {
            let state = engine.manager.get_state(CHILD_CONN).await.expect("state");
            let mut s = state.write().await;
            s.pending_permission = Some(PendingPermissionState {
                request_id: "r1".into(),
                tool_call_id: "tc-1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
                created_at: chrono::Utc::now(),
                queued: 0,
            });
        }
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );

        // The recovered key matches what a normal resolve retracts — otherwise
        // the row would wedge at awaiting_input.
        engine
            .on_event(&env(
                CHILD_CONN,
                AcpEvent::PermissionResolved {
                    request_id: "r1".into(),
                },
            ))
            .await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
    }

    #[tokio::test]
    async fn a_nested_delegation_child_still_maps_to_the_run() {
        // A sub-agent that delegates further blocks the task just as much, so
        // the grandchild has to resolve through the chain rather than requiring
        // its parent to be the run's own connection.
        let (engine, task_id) = running_task().await;
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        engine.on_event(&delegation_started(CHILD_CONN, "conn-grandchild")).await;

        engine
            .on_event(&permission_request("conn-grandchild", "r1"))
            .await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );
    }

    #[tokio::test]
    async fn completing_a_child_retracts_its_whole_subtree() {
        // The middle child finishing takes its own delegations down with it, so
        // a grandchild's unanswered prompt must be retracted too — otherwise
        // the row wedges at awaiting_input on a key nothing can resolve, and
        // the grandchild's mapping is stranded in the map forever.
        let (engine, task_id) = running_task().await;
        engine.on_event(&delegation_started(PARENT_CONN, CHILD_CONN)).await;
        engine.on_event(&delegation_started(CHILD_CONN, "conn-grandchild")).await;
        engine
            .on_event(&permission_request("conn-grandchild", "r1"))
            .await;
        assert_eq!(
            status_of(&engine, task_id).await,
            WorkTaskStatus::AwaitingInput
        );

        engine.on_event(&delegation_completed(PARENT_CONN, CHILD_CONN)).await;
        assert_eq!(status_of(&engine, task_id).await, WorkTaskStatus::Running);
        assert!(
            engine.delegation_parents.lock().await.is_empty(),
            "the grandchild's mapping must not be stranded"
        );
    }

    // ── delivery (push + pull request) ──────────────────────────────────

    /// Forge write path under test control: no network, no keyring, and every
    /// step independently failable so each failure point can be exercised.
    #[derive(Default)]
    struct FakeForge {
        /// `(repository, work branch, remote branch)` of every push.
        pushes: Mutex<Vec<(String, String, String)>>,
        created: Mutex<Vec<(String, String, String, bool)>>,
        existing: Mutex<Vec<ForgePr>>,
        /// What the source repository's base branch points at. The fixture
        /// seeds it with the task's own base, i.e. "nothing unpushed".
        remote_base: Mutex<Option<String>>,
        /// `(issue number, comment body)` of every write-back attempt.
        comments: Mutex<Vec<(ForgeItemKind, i64, String)>>,
        push_error: Option<String>,
        find_error: Option<String>,
        create_error: Option<String>,
        comment_error: Option<String>,
        get_pull_error: Option<String>,
        /// What the pull request looks like once the push has happened — how
        /// a test says "someone closed / retargeted / merged it while codeg
        /// was pushing", which is the whole window the settle check guards.
        after_push: Mutex<Option<ForgePr>>,
        /// A file dropped into the worktree the moment the push runs. Opens
        /// the other window of the same kind: the worktree was clean when
        /// `deliver_pr` checked it and is dirty by the time the post-delivery
        /// cleanup would run `worktree remove --force` over it.
        dirty_on_push: Option<String>,
        /// A file COMMITTED onto the work branch the moment the push runs —
        /// the half `dirty_on_push` cannot reach. `git status` stays spotless,
        /// so only a check against the OID the delivery actually published can
        /// tell that the branch has outrun it.
        commit_on_push: Option<String>,
    }

    #[async_trait::async_trait]
    impl ForgeDeliveryApi for FakeForge {
        async fn push_branch(
            &self,
            _ctx: &DeliveryCtx<'_>,
            worktree_path: &str,
            repo: &str,
            work_branch: &str,
            remote_branch: &str,
        ) -> Result<(), String> {
            if let Some(e) = &self.push_error {
                return Err(e.clone());
            }
            if let Some(name) = &self.dirty_on_push {
                std::fs::write(std::path::Path::new(worktree_path).join(name), "scratch\n")
                    .expect("write");
            }
            if let Some(name) = &self.commit_on_push {
                let dir = std::path::Path::new(worktree_path);
                std::fs::write(dir.join(name), "landed after the push\n").expect("write");
                git_run(dir, &["add", "-A"]);
                git_run(dir, &["commit", "-qm", "never published"]);
            }
            self.pushes.lock().await.push((
                repo.to_string(),
                work_branch.to_string(),
                remote_branch.to_string(),
            ));
            if let Some(changed) = self.after_push.lock().await.clone() {
                *self.existing.lock().await = vec![changed];
            }
            Ok(())
        }

        async fn find_pulls(
            &self,
            _ctx: &DeliveryCtx<'_>,
            _head_branch: &str,
        ) -> Result<Vec<ForgePr>, String> {
            if let Some(e) = &self.find_error {
                return Err(e.clone());
            }
            Ok(self.existing.lock().await.clone())
        }

        async fn remote_base_tip(
            &self,
            _ctx: &DeliveryCtx<'_>,
            _worktree_path: &str,
            _base_branch: &str,
        ) -> Option<String> {
            self.remote_base.lock().await.clone()
        }

        /// The fixtures' "remote" is a local path on `origin`, so the fake
        /// fetches through it — what production adds on top is the pinned
        /// account's credentials and an explicit URL, neither of which a
        /// temporary directory has any use for.
        async fn fetch_ref(
            &self,
            _ctx: &DeliveryCtx<'_>,
            repo_path: &str,
            remote_ref: &str,
            local_ref: &str,
        ) -> Result<String, String> {
            task_git::fetch_into_ref(repo_path, "origin", remote_ref, local_ref)
                .await
                .map_err(|e| e.to_string())
        }

        async fn get_pull(&self, _ctx: &DeliveryCtx<'_>, number: i64) -> Result<ForgePr, String> {
            if let Some(e) = &self.get_pull_error {
                return Err(e.clone());
            }
            self.existing
                .lock()
                .await
                .iter()
                .find(|pr| pr.number == number)
                .cloned()
                .ok_or_else(|| format!("pull request #{number} not found"))
        }

        async fn comment_issue(
            &self,
            _ctx: &DeliveryCtx<'_>,
            kind: ForgeItemKind,
            number: i64,
            body: &str,
        ) -> Result<String, String> {
            if let Some(e) = &self.comment_error {
                return Err(e.clone());
            }
            self.comments.lock().await.push((kind, number, body.to_string()));
            Ok(format!("https://github.test/acme/app/issues/{number}#issuecomment-1"))
        }

        async fn create_pull(
            &self,
            _ctx: &DeliveryCtx<'_>,
            req: &NewPullRequest<'_>,
        ) -> Result<ForgePr, String> {
            if let Some(e) = &self.create_error {
                return Err(e.clone());
            }
            self.created.lock().await.push((
                req.title.to_string(),
                req.head.to_string(),
                req.base.to_string(),
                req.draft,
            ));
            Ok(ForgePr {
                number: 42,
                html_url: "https://github.test/acme/app/pull/42".to_string(),
                state: "open".to_string(),
                merged: false,
                head_sha: "unused-by-the-fake".to_string(),
                head_ref: req.head.to_string(),
                head_repo: "acme/app".to_string(),
                base_ref: req.base.to_string(),
            })
        }
    }

    fn git_run(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A fixture repository must not translate line endings, whoever's git
        // opens it. The env above neuters the global and system config for the
        // commands THIS helper runs — but the engine's git, spawned through
        // `crate::process`, inherits the real environment, and Git for Windows
        // ships `core.autocrlf=true` in its system config. The two then disagree
        // about one worktree: the engine checks a file out as CRLF while a
        // `git add` here stores those bytes verbatim, so a file nobody touched
        // reads as rewritten, and a diff measured from a commit credits the task
        // with work it never did. Repo-LOCAL config is the one layer both sides
        // read.
        match args.first() {
            // `init` runs with the repository as its cwd; `clone` names the
            // destination last.
            Some(&"init") => git_run(dir, &["config", "core.autocrlf", "false"]),
            Some(&"clone") => {
                let dest = std::path::PathBuf::from(args.last().expect("clone destination"));
                git_run(&dest, &["config", "core.autocrlf", "false"]);
            }
            _ => {}
        }
    }

    struct Delivery {
        engine: Arc<TaskEngine>,
        forge: Arc<FakeForge>,
        /// Everything the engine broadcast, in order — the ordering tests read
        /// it, and holding it also keeps the sender from reporting no
        /// receivers for every other test on this fixture.
        events: tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
        task_id: i32,
        head: String,
        /// The commit the task branched from — a real commit that is NOT a
        /// descendant of `head`, which is what a "merged something else" test
        /// needs to be about.
        base_sha: String,
        worktree: std::path::PathBuf,
        root: std::path::PathBuf,
        _root: tempfile::TempDir,
    }

    /// A real repository with a real task worktree holding one committed
    /// change, plus the board row that describes it — the exact state a task
    /// is in when its review column offers "deliver".
    async fn delivery_fixture(forge: FakeForge) -> Delivery {
        use sea_orm::{ActiveModelTrait, Set};

        let root = tempfile::tempdir().expect("tempdir");
        let root_path = root.path().to_path_buf();
        git_run(&root_path, &["init", "-q", "-b", "main"]);
        std::fs::write(root_path.join("a.txt"), "one\n").expect("write");
        git_run(&root_path, &["add", "-A"]);
        git_run(&root_path, &["commit", "-q", "-m", "base"]);
        let base_sha = task_git::rev_parse(root_path.to_str().unwrap(), "HEAD")
            .await
            .expect("base sha");
        // Its own `origin`: the fake forge fetches through it, so a test can
        // publish a real `refs/pull/7/head` and have git answer questions
        // about it exactly as it would against a server.
        git_run(&root_path, &["remote", "add", "origin", root_path.to_str().unwrap()]);

        let worktree = root_path.join("wt");
        git_run(
            &root_path,
            &["worktree", "add", "-q", "-b", "task/7", worktree.to_str().unwrap()],
        );
        std::fs::write(worktree.join("a.txt"), "one\ntwo\n").expect("write");
        git_run(&worktree, &["commit", "-qam", "the work"]);
        let head = task_git::rev_parse(worktree.to_str().unwrap(), "task/7")
            .await
            .expect("work head");

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id =
            crate::db::test_helpers::seed_folder(&db, root_path.to_str().unwrap()).await;
        let wt_folder_id =
            crate::db::test_helpers::seed_folder(&db, worktree.to_str().unwrap()).await;

        let meta = serde_json::json!({
            "provider": "github",
            "server_host": "github.com",
            "api_base": "https://api.github.com",
            "account_id": "acc-1",
            "owner_repo": "acme/app",
            "number": 7,
            "url": "https://github.com/acme/app/issues/7",
            "title": "Fix the login flow",
        });
        let now = chrono::Utc::now();
        let task = crate::db::entities::work_task::ActiveModel {
            folder_id: Set(folder_id),
            title: Set("#7 · Fix the login flow".to_string()),
            config: Set("{}".to_string()),
            status: Set(WorkTaskStatus::Review),
            run_seq: Set(1),
            sort_order: Set(0),
            worktree_folder_id: Set(Some(wt_folder_id)),
            base_branch: Set(Some("main".to_string())),
            base_sha: Set(Some(base_sha.clone())),
            work_branch: Set(Some("task/7".to_string())),
            source_kind: Set(Some(SOURCE_KIND_ISSUE.to_string())),
            source_key: Set(Some("github:github.com:acme/app:issue:7".to_string())),
            source_meta: Set(Some(meta.to_string())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("insert task");

        let forge = Arc::new(forge);
        // Default posture: the remote base is exactly where this task branched
        // from, so nothing unpushed can leak into the pull request.
        *forge.remote_base.lock().await = Some(base_sha.clone());
        let broadcaster = Arc::new(crate::web::event_bridge::WebEventBroadcaster::new());
        let events = broadcaster.subscribe();
        Delivery {
            engine: test_engine_full(
                db,
                forge.clone(),
                EventEmitter::test_web_only(broadcaster),
            ),
            forge,
            events,
            task_id: task.id,
            head,
            base_sha,
            worktree,
            root: root_path,
            _root: root,
        }
    }

    async fn row(engine: &Arc<TaskEngine>, id: i32) -> crate::db::entities::work_task::Model {
        work_task_service::get_model(&engine.db.conn, id)
            .await
            .expect("task row")
    }

    /// The happy path, end to end: the branch is pushed, a pull request is
    /// opened against the recorded base, and the task settles into `done` with
    /// the evidence of HOW it finished — plus the link, on the row the issue
    /// list reads back.
    #[tokio::test]
    async fn delivery_opens_a_pull_request_and_settles_the_task() {
        let f = delivery_fixture(FakeForge::default()).await;
        let url = f
            .engine
            .deliver_pr(f.task_id, Some("  Fix login  ".into()), false, false)
            .await
            .expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/42");

        assert_eq!(
            f.forge.pushes.lock().await.as_slice(),
            [("acme/app".to_string(), "task/7".to_string(), "task/7".to_string())]
        );
        let created = f.forge.created.lock().await.clone();
        assert_eq!(
            created,
            [("Fix login".to_string(), "task/7".to_string(), "main".to_string(), false)],
            "title trimmed, head/base taken from the task's own record"
        );

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert!(task.merge_state.is_none(), "the in-flight intent is spent");
        assert!(task.last_error.is_none());
        assert!(task.finished_at.is_some());
        let meta: serde_json::Value =
            serde_json::from_str(task.source_meta.as_deref().unwrap()).unwrap();
        assert_eq!(meta["result_pr"], "https://github.test/acme/app/pull/42");
        // The worktree is kept because this caller did not ask for it to go —
        // the pull request points at this branch and another round on it is a
        // normal next step.
        assert!(f.worktree.exists());
        // Nothing is left in the in-flight set for the reconcile tick to trip on.
        assert!(f.engine.merging.lock().await.is_empty());
    }

    /// The delivery's own version of the offer both other acceptances make:
    /// take the checkout along. Safe here in a way a bare `branch -D` is not —
    /// by the time this runs the commits it destroys locally are on the forge.
    #[tokio::test]
    async fn a_delivery_takes_the_worktree_along_when_asked() {
        let f = delivery_fixture(FakeForge::default()).await;
        let url = f.engine.deliver_pr(f.task_id, None, false, true).await.expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/42");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert!(!f.worktree.exists(), "the checkout is gone from disk");
        assert!(task.worktree_folder_id.is_none(), "and the row's pointer with it");
        assert!(task.cleanup_state.is_none(), "a removal that worked flags nothing");
        assert!(
            task_git::rev_parse(f.root.to_str().unwrap(), "refs/heads/task/7").await.is_err(),
            "the local branch goes too — the push that just ran is what makes that safe"
        );
        assert!(f.engine.merging.lock().await.is_empty());
    }

    /// The worktree turned dirty between `deliver_pr`'s pre-flight clean check
    /// and the removal. `worktree remove --force` would drop those files
    /// without a word and they are on no forge, so the checkout is KEPT and
    /// flagged for a retry from the card — while the delivery itself, which is
    /// what the button promised, still reports success.
    #[tokio::test]
    async fn a_worktree_that_turned_dirty_is_kept_rather_than_forced_away() {
        let f = delivery_fixture(FakeForge {
            dirty_on_push: Some("scratch.txt".into()),
            ..Default::default()
        })
        .await;
        let url = f.engine.deliver_pr(f.task_id, None, false, true).await.expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/42");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done, "the delivery still landed");
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert!(f.worktree.join("scratch.txt").exists(), "the stray file survives");
        assert!(task.worktree_folder_id.is_some(), "the checkout is still the task's");
        assert_eq!(
            task.cleanup_state.as_deref(),
            Some("failed"),
            "so the card can offer the cleanup again once the file is dealt with"
        );
    }

    /// The blind spot `git status` cannot cover: a COMMIT made in the same
    /// window leaves the worktree spotless while putting the branch ahead of
    /// everything the delivery published. `worktree remove --force` plus
    /// `branch -D` would take that commit with them, and it is on no forge —
    /// so the tip is checked against the OID that actually went out, and a
    /// branch that outran it keeps both its checkout and its commit.
    #[tokio::test]
    async fn a_branch_that_outran_the_delivery_keeps_its_unpublished_commit() {
        let f = delivery_fixture(FakeForge {
            commit_on_push: Some("later.txt".into()),
            ..Default::default()
        })
        .await;
        let url = f.engine.deliver_pr(f.task_id, None, false, true).await.expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/42");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done, "the delivery still landed");
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert!(f.worktree.exists(), "the checkout survives");
        assert!(task.worktree_folder_id.is_some(), "and stays the task's");
        assert_eq!(
            task.cleanup_state.as_deref(),
            Some("failed"),
            "retryable once the extra commit is delivered or moved somewhere safe"
        );
        // Spotless — which is exactly why the other probe could not have
        // caught this, and why the tip check has to exist.
        assert!(!task_git::has_changes(f.worktree.to_str().unwrap()).await.expect("status"));
        let tip = task_git::rev_parse(f.root.to_str().unwrap(), "refs/heads/task/7")
            .await
            .expect("the branch is still there");
        assert_ne!(tip, f.head, "and it holds the commit nobody pushed");
    }

    /// A pull request that already matches all four criteria is ADOPTED. This
    /// is what makes a retry after a settle-time failure safe: the second
    /// attempt must not open a second pull request for the same commit.
    #[tokio::test]
    async fn an_existing_matching_pull_request_is_adopted_not_duplicated() {
        let f = delivery_fixture(FakeForge::default()).await;
        f.forge.existing.lock().await.push(ForgePr {
            number: 11,
            html_url: "https://github.test/acme/app/pull/11".into(),
            state: "open".into(),
            merged: false,
            head_sha: f.head.clone(),
            head_ref: "task/7".into(),
            head_repo: "Acme/App".into(), // canonical casing, as the API sends
            base_ref: "main".into(),
        });

        let url = f.engine.deliver_pr(f.task_id, None, false, false).await.expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/11");
        assert!(
            f.forge.created.lock().await.is_empty(),
            "adoption must not create a duplicate"
        );
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Done);
    }

    /// A near-miss is never adopted: same commit, different base. Without the
    /// four-way match this would settle the task against the wrong pull
    /// request; with it, a new one is opened for the right base.
    #[tokio::test]
    async fn a_pull_request_for_another_base_is_not_adopted() {
        let f = delivery_fixture(FakeForge::default()).await;
        f.forge.existing.lock().await.push(ForgePr {
            number: 12,
            html_url: "https://github.test/acme/app/pull/12".into(),
            state: "open".into(),
            merged: false,
            head_sha: f.head.clone(),
            head_ref: "task/7".into(),
            head_repo: "acme/app".into(),
            base_ref: "release/1.x".into(), // ← the only difference
        });

        let url = f.engine.deliver_pr(f.task_id, None, false, false).await.expect("delivery");
        assert_eq!(url, "https://github.test/acme/app/pull/42", "a new one was opened");
        assert_eq!(f.forge.created.lock().await.len(), 1);
    }

    /// Every step after the review→merging CAS can fail, and every one of them
    /// has to leave the task in review with a readable reason — never stranded
    /// in `merging`, where only crash recovery could free it.
    #[tokio::test]
    async fn any_failed_step_returns_the_task_to_review() {
        let cases = [
            (
                FakeForge { push_error: Some("remote rejected".into()), ..Default::default() },
                "could not push",
            ),
            (
                FakeForge { find_error: Some("502 bad gateway".into()), ..Default::default() },
                "could not check for an existing pull request",
            ),
            (
                FakeForge { create_error: Some("422 no commits".into()), ..Default::default() },
                "could not open the pull request",
            ),
        ];
        for (forge, expected) in cases {
            let f = delivery_fixture(forge).await;
            let err = f
                .engine
                .deliver_pr(f.task_id, None, false, false)
                .await
                .expect_err("must fail");
            assert!(err.contains(expected), "got {err}");
            let task = row(&f.engine, f.task_id).await;
            assert_eq!(task.status, WorkTaskStatus::Review, "{expected}");
            assert!(task.last_error.is_some(), "the card must explain itself");
            assert!(task.merge_state.is_none());
            assert!(task.completion_kind.is_none());
            assert!(f.engine.merging.lock().await.is_empty());
        }
    }

    /// A closed-without-merge match is a human decision. The engine neither
    /// reopens it nor opens a duplicate — it hands the task back with the
    /// reason.
    #[tokio::test]
    async fn a_closed_pull_request_stops_the_delivery() {
        let f = delivery_fixture(FakeForge::default()).await;
        f.forge.existing.lock().await.push(ForgePr {
            number: 13,
            html_url: "https://github.test/acme/app/pull/13".into(),
            state: "closed".into(),
            merged: false,
            head_sha: f.head.clone(),
            head_ref: "task/7".into(),
            head_repo: "acme/app".into(),
            base_ref: "main".into(),
        });
        let err = f.engine.deliver_pr(f.task_id, None, false, false).await.expect_err("must stop");
        assert!(err.contains("closed without merging"), "got {err}");
        assert!(f.forge.created.lock().await.is_empty());
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Review);
    }

    /// Preconditions are checked BEFORE the CAS, so a refusal leaves the task
    /// untouched — same status, same run generation, and nothing pushed.
    #[tokio::test]
    async fn refusals_happen_before_anything_moves() {
        // Uncommitted work in the worktree: publishing it would push a branch
        // that does not match what the user reviewed.
        let dirty = delivery_fixture(FakeForge::default()).await;
        std::fs::write(dirty.worktree.join("scratch.txt"), "junk\n").expect("write");
        let err = dirty
            .engine
            .deliver_pr(dirty.task_id, None, false, false)
            .await
            .expect_err("dirty worktree");
        assert!(err.contains("uncommitted changes"), "got {err}");

        // Nothing to land: GitHub answers that with a 422, so refuse early and
        // point at the button that does apply.
        let empty = delivery_fixture(FakeForge::default()).await;
        git_run(&empty.worktree, &["reset", "-q", "--hard", "HEAD~1"]);
        let err = empty
            .engine
            .deliver_pr(empty.task_id, None, false, false)
            .await
            .expect_err("nothing to deliver");
        assert!(err.contains("nothing to deliver"), "got {err}");

        for f in [&dirty, &empty] {
            let task = row(&f.engine, f.task_id).await;
            assert_eq!(task.status, WorkTaskStatus::Review);
            assert_eq!(task.run_seq, 1, "no generation was spent");
            assert!(task.last_error.is_none(), "a refusal is not a card banner");
            assert!(f.forge.pushes.lock().await.is_empty());
        }
    }

    /// The gate lives in the engine, not the UI: this command is reachable by
    /// id from an old frontend or a direct web API call.
    ///
    /// Two ways to be undeliverable, and neither may push anything: a task with
    /// no forge source has no repository to deliver to at all, and a
    /// pull-request task whose branch information is missing (a row written
    /// before that was recorded) must be re-triggered rather than pushed at a
    /// branch name we would have to guess.
    #[tokio::test]
    async fn a_task_without_deliverable_provenance_is_refused() {
        use sea_orm::{ActiveModelTrait, Set};
        for (kind, needle) in [
            (None, "issue or pull request"),
            (Some(SOURCE_KIND_PR), "does not know which branch"),
        ] {
            let f = delivery_fixture(FakeForge::default()).await;
            let mut update: crate::db::entities::work_task::ActiveModel =
                row(&f.engine, f.task_id).await.into();
            update.source_kind = Set(kind.map(str::to_string));
            update.update(&f.engine.db.conn).await.expect("update kind");

            let err = f
                .engine
                .deliver_pr(f.task_id, None, false, false)
                .await
                .expect_err("must refuse");
            assert!(err.contains(needle), "got {err}");
            assert!(f.forge.pushes.lock().await.is_empty());
            assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Review);
        }
    }

    /// Crash recovery: the process died between the push and the settle. The
    /// forge is the only truth, and a full four-way match settles the task
    /// exactly as the live path would have.
    #[tokio::test]
    async fn recovery_settles_a_delivery_whose_pull_request_matches() {
        for merged in [false, true] {
            let f = delivery_fixture(FakeForge::default()).await;
            f.forge.existing.lock().await.push(ForgePr {
                number: 21,
                html_url: "https://github.test/acme/app/pull/21".into(),
                state: if merged { "closed".into() } else { "open".into() },
                merged,
                head_sha: f.head.clone(),
                head_ref: "task/7".into(),
                head_repo: "acme/app".into(),
                base_ref: "main".into(),
            });
            interrupt_delivery(&f, "task/7").await;

            f.engine.recover_merging(f.task_id).await;

            let task = row(&f.engine, f.task_id).await;
            assert_eq!(task.status, WorkTaskStatus::Done, "merged={merged}");
            assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
            assert!(f.forge.created.lock().await.is_empty(), "recovery never creates");
        }
    }

    /// Everything short of a full match goes back to a human. Adopting a
    /// near-miss would settle the task against a pull request that merely
    /// reused the branch name.
    #[tokio::test]
    async fn recovery_bounces_anything_it_cannot_prove() {
        let cases: Vec<(&str, Option<ForgePr>)> = vec![
            ("no pull request at all", None),
            (
                "someone else's branch of the same name",
                Some(ForgePr {
                    number: 22,
                    html_url: "https://github.test/acme/app/pull/22".into(),
                    state: "open".into(),
                    merged: false,
                    head_sha: "0000000000000000000000000000000000000000".into(),
                    head_ref: "task/7".into(),
                    head_repo: "acme/app".into(),
                    base_ref: "main".into(),
                }),
            ),
        ];
        for (label, existing) in cases {
            let f = delivery_fixture(FakeForge::default()).await;
            if let Some(pr) = existing {
                f.forge.existing.lock().await.push(pr);
            }
            interrupt_delivery(&f, "task/7").await;

            f.engine.recover_merging(f.task_id).await;

            let task = row(&f.engine, f.task_id).await;
            assert_eq!(task.status, WorkTaskStatus::Review, "{label}");
            assert!(task.last_error.is_some(), "{label}: needs a reason");
            assert!(task.completion_kind.is_none(), "{label}");
        }
    }

    /// A delivery running in THIS process is not orphaned. Recovery has no
    /// live connection to check (a delivery has no agent session), so the
    /// in-flight set is the only thing standing between a slow push and the
    /// reconcile tick tearing it down.
    #[tokio::test]
    async fn recovery_leaves_a_delivery_this_process_owns_alone() {
        let f = delivery_fixture(FakeForge::default()).await;
        interrupt_delivery(&f, "task/7").await;
        f.engine.claim_in_flight(f.task_id).await.expect("claim");

        f.engine.recover_merging(f.task_id).await;

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Merging, "left alone");
        assert!(task.last_error.is_none());
    }

    /// Park the task in `merging` with a delivery intent, exactly as a process
    /// that died mid-delivery would have left it.
    async fn interrupt_delivery(f: &Delivery, remote_branch: &str) {
        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::DeliverPr,
            remote_branch: Some(remote_branch.to_string()),
            expected_head: Some(f.head.clone()),
            pr_title: Some("Fix login".to_string()),
            ..Default::default()
        };
        let seq = row(&f.engine, f.task_id).await.run_seq;
        work_task_service::begin_delivery(&f.engine.db.conn, f.task_id, &state, seq)
            .await
            .expect("begin delivery")
            .expect("CAS");
    }

    /// The pull request would be diffed against the base branch AS THE REMOTE
    /// HAS IT. A local base that is ahead means the review showed the task's
    /// own commit while the pull request would carry the unpushed ones too —
    /// refuse rather than publish work nobody looked at.
    #[tokio::test]
    async fn a_local_base_ahead_of_the_remote_is_refused() {
        let f = delivery_fixture(FakeForge::default()).await;
        // One more commit on main, never pushed, and the task is recorded as
        // having branched from it.
        std::fs::write(f.root.join("b.txt"), "unpushed\n").expect("write");
        git_run(&f.root, &["add", "-A"]);
        git_run(&f.root, &["commit", "-q", "-m", "local only"]);
        let local_base = task_git::rev_parse(f.root.to_str().unwrap(), "HEAD")
            .await
            .expect("local base");
        let mut update: crate::db::entities::work_task::ActiveModel =
            row(&f.engine, f.task_id).await.into();
        update.base_sha = sea_orm::Set(Some(local_base));
        sea_orm::ActiveModelTrait::update(update, &f.engine.db.conn)
            .await
            .expect("record the local base");

        let err = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("must refuse");
        assert!(err.contains("not on the remote yet"), "got {err}");
        assert!(f.forge.pushes.lock().await.is_empty(), "refused before the push");
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Review);

        // And an unreadable remote base is NOT reassurance: a base branch that
        // does not exist on the remote fails exactly the same way, and the push
        // would still publish the branch. This gate fails closed.
        *f.forge.remote_base.lock().await = None;
        let err = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("an unreadable base must refuse");
        assert!(err.contains("could not read"), "got {err}");
        assert!(f.forge.pushes.lock().await.is_empty());
    }

    /// A merge that cannot take in-flight ownership must not dispatch either.
    /// The claim and the review→merging CAS are independent, so a claim-loser
    /// that carried on could win the CAS and then be left unowned when the
    /// claim-holder released the registry entry on its own way out.
    #[tokio::test]
    async fn a_merge_that_cannot_claim_ownership_does_not_dispatch() {
        let f = delivery_fixture(FakeForge::default()).await;
        let token = f
            .engine
            .claim_in_flight(f.task_id)
            .await
            .expect("delivery claims first");

        let err = f
            .engine
            .merge_task(f.task_id, None, false, None, false)
            .await
            .expect_err("must not dispatch");
        assert!(err.contains("already merging"), "got {err}");
        // Swallowed by the unattended sweep rather than bannered onto the card
        // — the condition clears itself in seconds.
        assert!(is_benign_merge_race(&err));

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Review, "no CAS was spent");
        assert!(task.merge_state.is_none());
        assert_eq!(
            f.engine.merging.lock().await.get(&f.task_id),
            Some(&token),
            "the refused merge must leave the claim it never held"
        );
    }

    /// Two clients clicking deliver at once. Only one may run the push, and —
    /// the part that bit us — the LOSER must not release the winner's
    /// ownership, or the reconcile sweep would bounce a live delivery.
    #[tokio::test]
    async fn a_second_delivery_neither_runs_nor_steals_the_claim() {
        let f = delivery_fixture(FakeForge::default()).await;
        let token = f
            .engine
            .claim_in_flight(f.task_id)
            .await
            .expect("first claim wins");

        let err = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("the second must not run");
        assert!(err.contains("already merging"), "got {err}");
        assert!(f.forge.pushes.lock().await.is_empty());
        assert_eq!(
            f.engine.merging.lock().await.get(&f.task_id),
            Some(&token),
            "the loser must leave the winner's claim in place"
        );

        // And releasing with a foreign token is a no-op for the same reason.
        f.engine.release_in_flight(f.task_id, token + 99).await;
        assert_eq!(f.engine.merging.lock().await.get(&f.task_id), Some(&token));
        f.engine.release_in_flight(f.task_id, token).await;
        assert!(f.engine.merging.lock().await.is_empty());
    }

    /// A recovery pass that spent time at the forge must not undo a delivery
    /// that started while it was deciding — the bounce is bound to the
    /// generation the pass actually read.
    #[tokio::test]
    async fn stale_recovery_cannot_bounce_a_newer_generation() {
        let f = delivery_fixture(FakeForge::default()).await;
        interrupt_delivery(&f, "task/7").await;
        let stale = row(&f.engine, f.task_id).await;

        // The row moves on (a bounce and a fresh delivery), so the snapshot the
        // recovery pass is holding is now a generation behind.
        f.engine.bounce_delivery(&stale, "first".into()).await;
        interrupt_delivery(&f, "task/7").await;
        let current = row(&f.engine, f.task_id).await;
        assert!(current.run_seq > stale.run_seq);

        f.engine.bounce_delivery(&stale, "stale bounce".into()).await;

        let after = row(&f.engine, f.task_id).await;
        assert_eq!(after.status, WorkTaskStatus::Merging, "still delivering");
        assert_eq!(after.run_seq, current.run_seq);
    }

    /// One branch may have several open pull requests as long as each targets
    /// its own base — but only ONE per head/base pair. So a pull request for
    /// another base is no obstacle (create), while one for THIS base headed by
    /// a different commit is (bounce, rather than earn a 422 from GitHub).
    #[tokio::test]
    async fn a_stale_head_on_this_base_bounces_instead_of_duplicating() {
        let f = delivery_fixture(FakeForge::default()).await;
        f.forge.existing.lock().await.push(ForgePr {
            number: 31,
            html_url: "https://github.test/acme/app/pull/31".into(),
            state: "open".into(),
            merged: false,
            head_sha: "0000000000000000000000000000000000000000".into(),
            head_ref: "task/7".into(),
            head_repo: "acme/app".into(),
            base_ref: "main".into(),
        });

        let err = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("must bounce");
        assert!(err.contains("different commit"), "got {err}");
        assert!(
            f.forge.created.lock().await.is_empty(),
            "GitHub would refuse a duplicate anyway — do not ask"
        );
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Review);
        assert!(task.last_error.is_some());
    }

    /// A delivery occupies the folder's `merging` slot, so a sibling task's
    /// unattended merge is refused while it runs. That refusal must stay a
    /// BENIGN race: `set_review_error` would latch auto-merge off for that card
    /// permanently, over a condition that clears itself in seconds.
    #[test]
    fn a_delivery_in_flight_does_not_latch_off_a_sibling_auto_merge() {
        assert!(is_benign_merge_race(
            "another task of this project is already merging — wait for it"
        ));
    }

    // ── write the outcome back to the forge ─────────────────────────────────

    /// Record the trigger dialog's write-back answer on the task's own source
    /// metadata — where the engine reads it. Call it AFTER any helper that
    /// rewrites `source_meta` wholesale (`as_pull_request_task`), or the
    /// rewrite drops the answer.
    async fn set_writeback(f: &Delivery, wanted: bool) {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let task = row(&f.engine, f.task_id).await;
        let mut meta: serde_json::Value =
            serde_json::from_str(task.source_meta.as_deref().expect("source meta"))
                .expect("source meta json");
        meta["writeback"] = serde_json::json!(wanted);
        let mut active = task.into_active_model();
        active.source_meta = Set(Some(meta.to_string()));
        active.update(&f.engine.db.conn).await.expect("record the write-back answer");
    }

    async fn enable_writeback(f: &Delivery) {
        set_writeback(f, true).await;
    }

    /// The write-back is spawned off the settlement path, so an assertion about
    /// it has to wait for it rather than read straight after the settle.
    async fn wait_for<F, Fut>(what: &str, probe: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..300 {
            if probe().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn events_of(engine: &Arc<TaskEngine>, task_id: i32) -> Vec<String> {
        work_task_service::list_events(&engine.db.conn, task_id, 200)
            .await
            .expect("events")
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    /// A task that came from a proposed change gets its comment on the CHANGE,
    /// not on an issue with the same number. GitHub serves both from one
    /// endpoint and would not notice; GitLab has two collections, where the
    /// wrong one lands on an unrelated issue or 404s.
    #[tokio::test]
    async fn a_pull_request_tasks_comment_targets_the_pull_request() {
        let f = delivery_fixture(FakeForge::default()).await;
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
        as_pull_request_task(&f, pr.clone()).await;
        enable_writeback(&f).await;
        f.forge.existing.lock().await.push(pr);

        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("push back");

        let forge = f.forge.clone();
        wait_for("the write-back", move || {
            let forge = forge.clone();
            async move { !forge.comments.lock().await.is_empty() }
        })
        .await;
        let comments = f.forge.comments.lock().await.clone();
        assert_eq!(
            (comments[0].0, comments[0].1),
            (ForgeItemKind::Change, 7),
            "the comment belongs on the pull request the task came from"
        );
    }

    /// Completing a pull-request task that pushed nothing — a review-only
    /// report, or a review that stopped at an unsound approach — must not tell
    /// the pull request's thread it was "accepted". That is codeg's word for
    /// signing off on the TASK; on a pull request it reads as approval of the
    /// change, which may be the opposite of what the review found. The thread
    /// hears what happened to its pull request: nothing was pushed to it.
    #[tokio::test]
    async fn completing_a_pull_request_task_tells_its_thread_nothing_was_pushed() {
        let f = delivery_fixture(FakeForge::default()).await;
        // The pull request's head IS the task branch's head: the task made no
        // changes of its own, which is when the board offers "complete".
        let pr = open_pull(&f.head, "feature", "acme/app");
        as_pull_request_task(&f, pr).await;
        enable_writeback(&f).await;

        f.engine
            .complete_task(f.task_id, false)
            .await
            .expect("completed with nothing to push");

        let forge = f.forge.clone();
        wait_for("the write-back", move || {
            let forge = forge.clone();
            async move { !forge.comments.lock().await.is_empty() }
        })
        .await;
        let comments = f.forge.comments.lock().await.clone();
        assert_eq!(comments.len(), 1, "one settle, one comment");
        let (kind, number, body) = &comments[0];
        assert_eq!((*kind, *number), (ForgeItemKind::Change, 7));
        assert!(body.contains("nothing was pushed to this pull request"), "{body}");
        assert!(!body.to_lowercase().contains("accept"), "reads as an approval: {body}");
    }

    /// The comment is a fact sheet: the link and the counters. Nothing the
    /// agent wrote may reach a thread other people are reading.
    #[tokio::test]
    async fn a_delivered_task_comments_the_link_and_the_numbers() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let f = delivery_fixture(FakeForge::default()).await;
        enable_writeback(&f).await;
        // The counters a settled run leaves on the row, plus the agent's own
        // words — which must NOT travel.
        let mut active = row(&f.engine, f.task_id).await.into_active_model();
        active.files_changed = Set(Some(3));
        active.additions = Set(Some(42));
        active.deletions = Set(Some(7));
        active.result_summary =
            Set(Some("I refactored the auth module and rewrote the tests".to_string()));
        active.update(&f.engine.db.conn).await.expect("record the run's result");

        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("delivery");

        let forge = f.forge.clone();
        wait_for("the write-back", move || {
            let forge = forge.clone();
            async move { !forge.comments.lock().await.is_empty() }
        })
        .await;
        let comments = f.forge.comments.lock().await.clone();
        assert_eq!(comments.len(), 1);
        let (kind, number, body) = &comments[0];
        assert_eq!(*number, 7, "the comment goes on the issue the task came from");
        assert_eq!(*kind, ForgeItemKind::Issue, "…in the issue's own thread");
        assert!(body.contains("https://github.test/acme/app/pull/42"), "{body}");
        assert!(body.contains("(3 files, +42/-7)"), "{body}");
        assert!(!body.contains("refactored"), "agent text leaked: {body}");
        assert!(
            events_of(&f.engine, f.task_id).await.contains(&"forge_writeback".to_string()),
            "the comment belongs on the timeline"
        );
    }

    /// A task whose trigger dialog left the box unchecked publishes nothing —
    /// this is the one thing a task does in a place other people watch.
    #[tokio::test]
    async fn a_task_that_declined_the_comment_writes_nothing() {
        let f = delivery_fixture(FakeForge::default()).await;
        set_writeback(&f, false).await;
        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("delivery");
        // The settle emitted; give a spawned write-back the same chance to run
        // as the enabled case gets before concluding it did not.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(f.forge.comments.lock().await.is_empty());
        assert!(!events_of(&f.engine, f.task_id).await.contains(&"forge_writeback".to_string()));
    }

    /// A row minted before the choice lived on the task carries no answer at
    /// all. "No recorded yes" is a no: an upgrade must not start commenting on
    /// threads for tasks whose author was never asked.
    #[tokio::test]
    async fn a_task_without_a_recorded_answer_writes_nothing() {
        let f = delivery_fixture(FakeForge::default()).await;
        assert!(
            !row(&f.engine, f.task_id)
                .await
                .source_meta
                .unwrap_or_default()
                .contains("writeback"),
            "the fixture stands in for a pre-choice row"
        );
        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("delivery");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(f.forge.comments.lock().await.is_empty());
        assert!(!events_of(&f.engine, f.task_id).await.contains(&"forge_writeback".to_string()));
    }

    /// Best-effort means best-effort: a comment that cannot be posted is a
    /// timeline entry, not a status change. The task is finished either way.
    #[tokio::test]
    async fn a_failed_write_back_leaves_the_finished_task_alone() {
        let f = delivery_fixture(FakeForge {
            comment_error: Some("403 forbidden".into()),
            ..Default::default()
        })
        .await;
        enable_writeback(&f).await;
        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("delivery");

        let engine = f.engine.clone();
        let task_id = f.task_id;
        wait_for("the failure event", move || {
            let engine = engine.clone();
            async move {
                events_of(&engine, task_id).await.contains(&"forge_writeback_failed".to_string())
            }
        })
        .await;
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert!(task.last_error.is_none(), "a failed comment is not a task failure");
    }

    /// The OTHER settlement path: a task that landed on the base branch
    /// locally. Its comment names the commit, not a pull request — and the
    /// write-back is spawned from inside the folder's git lock, which it must
    /// neither take nor be blocked by.
    #[tokio::test]
    async fn a_locally_merged_task_comments_the_commit() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let f = delivery_fixture(FakeForge::default()).await;
        enable_writeback(&f).await;

        // The state a crashed merge generation leaves behind: `merging`, with
        // the base HEAD it started from…
        let pre_merge_head = task_git::rev_parse(f.root.to_str().unwrap(), "HEAD")
            .await
            .expect("base head");
        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::Land,
            pre_merge_head,
            strategy: "merge".into(),
            ..Default::default()
        };
        let mut active = row(&f.engine, f.task_id).await.into_active_model();
        active.status = Set(WorkTaskStatus::Merging);
        active.merge_state = Set(Some(serde_json::to_string(&state).expect("state")));
        active.files_changed = Set(Some(1));
        active.additions = Set(Some(1));
        active.deletions = Set(Some(0));
        active.update(&f.engine.db.conn).await.expect("to merging");
        // …and the landing itself, which the dead process did complete.
        git_run(&f.root, &["merge", "--no-ff", "-q", "-m", "land", "task/7"]);
        let landed_commit = task_git::rev_parse(f.root.to_str().unwrap(), "HEAD")
            .await
            .expect("landed head");

        f.engine.recover_merging(f.task_id).await;
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Done);

        let forge = f.forge.clone();
        wait_for("the write-back", move || {
            let forge = forge.clone();
            async move { !forge.comments.lock().await.is_empty() }
        })
        .await;
        let comments = f.forge.comments.lock().await.clone();
        assert_eq!(comments.len(), 1, "one settle, one comment");
        let (kind, number, body) = &comments[0];
        assert_eq!((*kind, *number), (ForgeItemKind::Issue, 7));
        assert!(body.contains(&landed_commit[..7]), "the commit that landed: {body}");
        assert!(body.contains("`main`") && body.contains("(1 file, +1/-0)"), "{body}");

        // A second recovery pass finds a `done` row and settles nothing, so the
        // issue does not collect a comment per sweep.
        f.engine.recover_merging(f.task_id).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(f.forge.comments.lock().await.len(), 1);
    }

    // ── the dead merge generation's connection ──────────────────────────────

    /// Park the fixture's task exactly where a process that died mid-merge left
    /// it: `merging`, pointing at a connection that no longer exists, with that
    /// connection still in the engine's correlation index.
    async fn interrupt_merge(f: &Delivery, conn_id: &str, delete_worktree: bool) {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let state = WorkTaskMergeState {
            op: WorkTaskMergeOp::Land,
            pre_merge_head: task_git::rev_parse(f.root.to_str().unwrap(), "HEAD")
                .await
                .expect("base head"),
            strategy: "merge".into(),
            delete_worktree,
            ..Default::default()
        };
        let task = row(&f.engine, f.task_id).await;
        let run_seq = task.run_seq;
        let mut active = task.into_active_model();
        active.status = Set(WorkTaskStatus::Merging);
        active.merge_state = Set(Some(serde_json::to_string(&state).expect("state")));
        active.connection_id = Set(Some(conn_id.to_string()));
        active.update(&f.engine.db.conn).await.expect("to merging");
        // Registered at launch, and only a TurnComplete ever takes it out.
        f.engine
            .index
            .lock()
            .await
            .insert(conn_id.into(), (f.task_id, run_seq));
    }

    /// The dead generation's index entry has no other way out — `reconcile_once`
    /// only scans `running` / `awaiting_input`, and the TurnComplete that would
    /// have retired it is the very thing that went missing. Recovery inherits
    /// that teardown, and must do it before it acts: this same pass honors the
    /// "delete the worktree" the user ticked, and worktree removal reads that
    /// same map as "an agent is still working in there".
    #[tokio::test]
    async fn recovery_retires_the_dead_merge_generations_connection() {
        const DEAD_CONN: &str = "conn-of-a-dead-merge";
        let f = delivery_fixture(FakeForge::default()).await;
        interrupt_merge(&f, DEAD_CONN, true).await;
        // The landing itself, which the dead process did complete.
        git_run(&f.root, &["merge", "--no-ff", "-q", "-m", "land", "task/7"]);

        f.engine.recover_merging(f.task_id).await;

        assert!(
            !f.engine.index.lock().await.contains_key(DEAD_CONN),
            "the dead generation must not outlive its recovery"
        );
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(
            task.cleanup_state, None,
            "nothing is working in that worktree — the removal must not be refused"
        );
        assert_eq!(task.worktree_folder_id, None, "detached");
        assert!(!f.worktree.exists(), "and really gone from disk");
    }

    /// The other outcome, same teardown: a recovery that cannot prove the merge
    /// landed bounces the row to `review` — where a surviving entry would keep
    /// attributing the dead connection's late events to a task that has moved
    /// on, and would block the worktree cleanup offered right there on the card.
    #[tokio::test]
    async fn a_bounced_recovery_retires_the_dead_connection_too() {
        const DEAD_CONN: &str = "conn-of-a-lost-merge";
        let f = delivery_fixture(FakeForge::default()).await;
        // No merge commit: the process died before landing anything.
        interrupt_merge(&f, DEAD_CONN, false).await;

        f.engine.recover_merging(f.task_id).await;

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Review);
        assert!(task.last_error.is_some(), "bounced with a reason");
        assert!(
            !f.engine.index.lock().await.contains_key(DEAD_CONN),
            "a task back in review has no run to correlate events to"
        );
    }

    /// Retiring one generation must not disarm another. The outstanding-request
    /// set is keyed by task, so a stale pass that reaches a task someone else
    /// has already relaunched would otherwise drop the live generation's
    /// pending permissions — and the next resolution would empty a set that is
    /// already empty and put the card back to `running` while the agent is
    /// still parked on the request nobody answered.
    #[tokio::test]
    async fn retiring_a_connection_spares_the_requests_of_one_still_on_the_task() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let engine = test_engine(db);
        {
            let mut index = engine.index.lock().await;
            index.insert("conn-old".into(), (7, 1));
            index.insert("conn-new".into(), (7, 2));
        }
        engine
            .awaiting
            .lock()
            .await
            .insert(7, ["p:req-1".to_string()].into_iter().collect());

        engine.retire_connection("conn-old", 7).await;

        {
            let index = engine.index.lock().await;
            assert!(!index.contains_key("conn-old"), "the retired one is gone");
            assert!(index.contains_key("conn-new"), "and only that one");
        }
        assert_eq!(
            engine.awaiting.lock().await.get(&7).map(|s| s.len()),
            Some(1),
            "the live generation is still waiting on its permission"
        );

        // The last one out does clear it — a set inherited by the next
        // generation would never flip the row to `awaiting_input` again.
        engine.retire_connection("conn-new", 7).await;
        assert!(engine.awaiting.lock().await.get(&7).is_none());
    }

    /// `index` is a correlation table, not a liveness one. Whatever leaves an
    /// entry behind, the user's "remove worktree" must not be refused on behalf
    /// of a connection that is gone — the refusal sets a `failed` flag whose
    /// retry can never clear, because nothing would ever arrive to remove the
    /// entry. The manager is the truth, and what it says is gone is retired.
    #[tokio::test]
    async fn worktree_removal_is_not_refused_by_a_dead_connection() {
        const ZOMBIE: &str = "conn-nobody-holds";
        let f = delivery_fixture(FakeForge::default()).await;
        let run_seq = row(&f.engine, f.task_id).await.run_seq;
        f.engine
            .index
            .lock()
            .await
            .insert(ZOMBIE.into(), (f.task_id, run_seq));

        f.engine.cleanup_task(f.task_id).await.expect("cleanup");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.cleanup_state, None, "not a live agent");
        assert_eq!(task.worktree_folder_id, None, "detached");
        assert!(!f.worktree.exists(), "and really gone from disk");
        assert!(!f.engine.index.lock().await.contains_key(ZOMBIE), "retired");
    }

    /// Git for Windows can finish the destructive part of `worktree remove`
    /// (registration and checkout contents) but fail to remove the now-empty
    /// directory. That first pass leaves the task flagged for cleanup; its retry
    /// must recognize the harmless shell, finish the branch/DB cleanup, and not
    /// report files that do not exist.
    #[tokio::test]
    async fn worktree_cleanup_retry_converges_an_empty_detached_shell() {
        let f = delivery_fixture(FakeForge::default()).await;
        let worktree = f.worktree.to_str().expect("utf-8 worktree");
        git_run(&f.root, &["worktree", "remove", "--force", worktree]);
        std::fs::create_dir(&f.worktree).expect("empty shell");
        work_task_service::set_cleanup_state(
            &f.engine.db.conn,
            f.task_id,
            true,
            Some("the first removal stopped after git detached it".into()),
        )
        .await
        .expect("flag failed cleanup");

        f.engine
            .cleanup_task(f.task_id)
            .await
            .expect("retry cleanup");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.cleanup_state, None, "the retry is fully settled");
        assert_eq!(
            task.worktree_folder_id, None,
            "folder bookkeeping converged"
        );
        assert!(!f.worktree.exists(), "the empty shell is gone");
        assert!(
            task_git::rev_parse(f.root.to_str().unwrap(), "refs/heads/task/7")
                .await
                .is_err(),
            "the requested work branch goes too"
        );
    }

    /// A missing `.git` file alone is not proof that the directory is a harmless
    /// post-removal shell. Files may have appeared after the partial teardown,
    /// and none of them is recoverable through git, so the retry must preserve
    /// both the sentinel and the branch.
    #[tokio::test]
    async fn worktree_cleanup_retry_preserves_a_file_in_a_detached_shell() {
        let f = delivery_fixture(FakeForge::default()).await;
        let worktree = f.worktree.to_str().expect("utf-8 worktree");
        git_run(&f.root, &["worktree", "remove", "--force", worktree]);
        std::fs::create_dir(&f.worktree).expect("empty shell");
        std::fs::write(f.worktree.join("sentinel.txt"), "not in git\n").expect("sentinel");

        let err = f
            .engine
            .cleanup_task(f.task_id)
            .await
            .expect_err("a non-empty shell is not removable");

        assert!(err.holds_work, "the retry is refused as a data-safety gate");
        assert_eq!(
            std::fs::read_to_string(f.worktree.join("sentinel.txt")).expect("read sentinel"),
            "not in git\n"
        );
        let task = row(&f.engine, f.task_id).await;
        assert!(
            task.worktree_folder_id.is_some(),
            "the retry remains available"
        );
        assert!(
            task_git::rev_parse(f.root.to_str().unwrap(), "refs/heads/task/7")
                .await
                .is_ok(),
            "the branch survives with the sentinel"
        );
    }

    /// A repository with `layout` applied to it, for the probe tests below:
    /// returns `(tempdir, repo path, worktree path)`. The two tests that pass
    /// `"sibling"` reproduce the DEFAULT worktree layout — beside the project,
    /// with no repository of the fixture's own above it, which is the layout in
    /// which a shell has no enclosing repository for `git status` to answer
    /// about instead. (Nothing here can rule out a repository ABOVE the system
    /// temp directory; in that environment the sibling pair still asserts the
    /// right answers, but it is `an_empty_shell_inside_a_dirty_project_reads_as_clean`
    /// — which builds its own enclosing repository — that pins the regression
    /// unconditionally.)
    fn probe_repo(layout: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        git_run(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("write");
        git_run(&repo, &["add", "-A"]);
        git_run(&repo, &["commit", "-q", "-m", "base"]);
        let worktree = match layout {
            "sibling" => dir.path().join("wt"),
            _ => repo.join("wt"),
        };
        (dir, repo, worktree)
    }

    /// THE probe behind issue #642's dead-end retry, exercised where the bug
    /// actually happens. `has_changes` runs `git status` from the path itself,
    /// so a shell in the default layout — beside the project, no repository
    /// above it — makes git error, and reading that error as "dirty" is what
    /// made the retry offer the same refusal forever. An empty shell has, by
    /// definition, nothing a `--force` removal could take.
    #[tokio::test]
    async fn an_empty_shell_beside_a_project_reads_as_clean() {
        let (_dir, _repo, worktree) = probe_repo("sibling");
        std::fs::create_dir(&worktree).expect("empty shell");

        assert!(
            !path_holds_uncommitted(worktree.to_str().expect("utf-8")).await,
            "an empty post-removal shell holds nothing"
        );
    }

    /// The other half of the same answer: outside a repository git errors on a
    /// shell whatever is in it, so emptiness is the ONLY thing separating a
    /// harmless leftover from files nothing can give back.
    #[tokio::test]
    async fn a_file_in_a_shell_beside_a_project_still_reads_as_dirty() {
        let (_dir, _repo, worktree) = probe_repo("sibling");
        std::fs::create_dir(&worktree).expect("shell");
        std::fs::write(worktree.join("sentinel.txt"), "not in git\n").expect("sentinel");

        assert!(
            path_holds_uncommitted(worktree.to_str().expect("utf-8")).await,
            "a shell with an entry in it is not removable"
        );
    }

    /// A worktree root configured INSIDE the project (a relative
    /// `worktree_root`, which the setting takes at face value) puts the shell
    /// under the project's own `.git`. `git status` then answers happily — about
    /// the PROJECT — so a project with a stray file of its own would answer
    /// "dirty" for a shell holding nothing, and #642's retry would still never
    /// converge. Without a `.git` marker the path is not a checkout git speaks
    /// for, and its answer must not be read as this path's.
    ///
    /// The project's dirt is an edit to a TRACKED file, not a stray untracked
    /// one: `has_changes` spawns git through `crate::process`, which inherits
    /// the real environment, so an untracked fixture is only as visible as the
    /// developer's global `core.excludesFile` lets it be — and a fixture git
    /// silently ignores would make this test pass against the very bug it
    /// exists to pin. No ignore rule can hide a modified tracked file.
    #[tokio::test]
    async fn an_empty_shell_inside_a_dirty_project_reads_as_clean() {
        let (_dir, repo, worktree) = probe_repo("nested");
        std::fs::write(repo.join("a.txt"), "the project's own mess\n").expect("dirty project");
        std::fs::create_dir(&worktree).expect("empty shell");

        assert!(
            !path_holds_uncommitted(worktree.to_str().expect("utf-8")).await,
            "the project's dirt is not the shell's"
        );
    }

    /// And the marker check must not short-circuit the case it is guarding: a
    /// checkout that still HAS its `.git` is exactly what `git status` is for,
    /// uncommitted work included. Tracked and modified for the reason above —
    /// an untracked fixture would read as clean under a global ignore rule that
    /// happens to match it, and fail this assertion on that developer's machine
    /// alone.
    #[tokio::test]
    async fn an_intact_worktree_is_still_measured_by_git() {
        let (_dir, repo, worktree) = probe_repo("sibling");
        let worktree_path = worktree.to_str().expect("utf-8");
        git_run(
            &repo,
            &["worktree", "add", "-q", "-b", "task/probe", worktree_path],
        );
        assert!(
            !path_holds_uncommitted(worktree_path).await,
            "a fresh checkout holds nothing"
        );

        std::fs::write(worktree.join("a.txt"), "unstaged\n").expect("edit");

        assert!(
            path_holds_uncommitted(worktree_path).await,
            "an uncommitted edit in a live checkout is work"
        );
    }

    /// The removal underneath is `worktree remove --force`, and a stop pressed
    /// while the agent is editing is exactly when a checkout is dirty. Nothing
    /// the user clicked named those files — the cancel dialog's checkbox offers
    /// "its worktree and work branch" — so this has to refuse, the same line
    /// `complete_task` draws and the branch dropdown re-asks about.
    #[tokio::test]
    async fn worktree_removal_refuses_to_take_uncommitted_work() {
        let f = delivery_fixture(FakeForge::default()).await;
        // Both shapes `git status` reports: an edit to a tracked file, and a
        // file git has never seen. Either one on its own is unrecoverable.
        std::fs::write(f.worktree.join("a.txt"), "one\ntwo\nthree\n").expect("write");
        std::fs::write(f.worktree.join("notes.md"), "half a thought\n").expect("write");

        let err = f
            .engine
            .cleanup_task(f.task_id)
            .await
            .expect_err("must not take uncommitted work");
        assert!(err.holds_work, "declined to protect work, not a failure");
        assert!(
            err.message.contains("uncommitted files"),
            "and says why: {}",
            err.message
        );

        assert!(f.worktree.exists(), "the checkout survives");
        assert_eq!(
            std::fs::read_to_string(f.worktree.join("notes.md")).expect("read"),
            "half a thought\n",
            "and so does the work inside it"
        );
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(
            task.cleanup_state.as_deref(),
            Some("failed"),
            "flagged, because the cancel and delete paths swallow the error and \
             the card is the only place left to say it"
        );
        assert!(
            task.worktree_folder_id.is_some(),
            "still attached, so the retry has something to remove"
        );
    }

    /// The probe fails CLOSED. `git status` can stop answering for reasons that
    /// have nothing to do with the checkout being gone — a corrupt index, a
    /// permission error — and the operation waiting on that answer is
    /// `worktree remove --force`. Reading "could not ask" as "clean" would
    /// trade a recoverable stall for unrecoverable files.
    #[tokio::test]
    async fn a_worktree_git_cannot_read_is_not_assumed_clean() {
        let f = delivery_fixture(FakeForge::default()).await;
        // A linked worktree's `.git` is a FILE pointing back at the real gitdir.
        // Pointing it nowhere makes every git command in here fail while the
        // directory — and whatever the user left in it — stays on disk.
        std::fs::write(f.worktree.join("unsaved.txt"), "not committed\n").expect("write");
        std::fs::write(f.worktree.join(".git"), "gitdir: /nowhere/at/all\n").expect("write");

        let err = f
            .engine
            .cleanup_task(f.task_id)
            .await
            .expect_err("must not force-remove a checkout it could not read");
        assert!(err.holds_work, "unprovable is treated as unsafe");

        assert!(f.worktree.exists(), "the checkout survives");
        assert_eq!(
            std::fs::read_to_string(f.worktree.join("unsaved.txt")).expect("read"),
            "not committed\n"
        );
    }

    /// The removal has to serialize against a LAUNCH, and the folder git lock
    /// cannot do that: `launch` holds the TASK lock across its whole setup —
    /// `ensure_worktree` included — and takes no folder lock at all.
    ///
    /// The sequence that gets there is ordinary board use: a cancel that was
    /// asked to take the worktree along leaves the row `canceled`, the user
    /// requeues it (a pure DB write that waits on no lock), and the pump
    /// launches it — all while the removal sits between its status read and its
    /// `git worktree remove`. Nothing about the requeue is wrong; the fresh run
    /// would mint its own checkout. Deleting the directory out from under
    /// `ensure_worktree` is.
    ///
    /// Holding the task lock here stands in for that launch: without the lock
    /// on the removal's side, the worktree is gone before the guard is dropped.
    #[tokio::test]
    async fn worktree_removal_waits_for_the_task_lock() {
        let f = delivery_fixture(FakeForge::default()).await;
        let engine = f.engine.clone();
        let task_id = f.task_id;

        // Stand in for a launch that has the row and is inside its setup.
        let held = engine.task_lock(task_id).await;
        let guard = held.lock().await;

        let removal = tokio::spawn({
            let engine = engine.clone();
            async move { engine.cleanup_task(task_id).await }
        });

        // Long enough that an unserialized removal — a few DB reads and a
        // `git worktree remove` — would have finished several times over.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            f.worktree.exists(),
            "the checkout must survive while the launch holds the task lock"
        );
        assert!(!removal.is_finished(), "and the removal must still be parked");

        drop(guard);
        removal.await.expect("join").expect("cleanup");
        assert!(!f.worktree.exists(), "and go once the lock is free");
        assert_eq!(row(&engine, task_id).await.worktree_folder_id, None);
    }

    /// Drain everything the fixture's engine has broadcast so far.
    fn drained(f: &mut Delivery) -> Vec<crate::web::event_bridge::WebEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = f.events.try_recv() {
            out.push(ev);
        }
        out
    }

    /// An acceptance that removes the worktree has to say so on the TASK
    /// channel, not just the folder one. Both acceptance paths broadcast the
    /// row BEFORE the cleanup runs (`settle_merge_generation` and this one), so
    /// a removal that stays silent leaves every open board holding the snapshot
    /// taken while the worktree still existed: the card flips to `done` but its
    /// "worktree removed" badge — keyed on `worktree_folder_id` going null —
    /// never appears until someone refetches the whole table.
    #[tokio::test]
    async fn removing_the_worktree_announces_the_task_after_the_folder() {
        let mut f = delivery_fixture(FakeForge::default()).await;
        interrupt_merge(&f, "conn-of-a-dead-merge", true).await;
        git_run(&f.root, &["merge", "--no-ff", "-q", "-m", "land", "task/7"]);
        let _ = drained(&mut f); // everything up to the merge is setup noise

        f.engine.recover_merging(f.task_id).await;

        assert_eq!(
            row(&f.engine, f.task_id).await.worktree_folder_id,
            None,
            "precondition: this run really did detach the worktree"
        );
        let events = drained(&mut f);
        let folder_gone = events
            .iter()
            .position(|e| {
                e.channel == crate::web::event_bridge::FOLDER_CHANGED_EVENT
                    && e.payload["kind"] == "deleted"
            })
            .expect("the worktree folder drop is broadcast");
        assert!(
            events.iter().skip(folder_gone).any(|e| {
                e.channel == WORK_TASK_CHANGED_EVENT
                    && e.payload["kind"] == "upsert"
                    && e.payload["id"] == f.task_id
            }),
            "the task must be re-announced AFTER the detach — an upsert from \
             before it carries the worktree the client is being told to drop; \
             saw {:?}",
            events.iter().map(|e| &e.channel).collect::<Vec<_>>()
        );
    }

    /// A task nobody triggered from a forge has no thread to comment on — the
    /// write-back must never invent one from the folder's remote.
    #[tokio::test]
    async fn a_task_without_a_forge_source_is_never_commented() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let f = delivery_fixture(FakeForge::default()).await;
        enable_writeback(&f).await;
        let mut active = row(&f.engine, f.task_id).await.into_active_model();
        active.source_kind = Set(None);
        active.source_meta = Set(None);
        active.update(&f.engine.db.conn).await.expect("clear source");

        f.engine
            .forge_writeback(f.task_id, WritebackOutcome::Merged("abc1234".into()))
            .await;
        assert!(f.forge.comments.lock().await.is_empty());
    }

    /// The engine has to be listening to the event bus BEFORE it starts its
    /// boot work, not after: the bus drops what it broadcasts while nobody is
    /// subscribed, so a task the user starts during boot would emit its
    /// `TurnComplete` into nothing and sit `running` forever — the reconcile
    /// sweep deliberately leaves rows whose connection is still live to
    /// `on_event`.
    ///
    /// Pinned without timing games. A `merging` row parks the boot pass on the
    /// folder's git lock, which this test holds, so the engine is provably
    /// still inside boot work when the event goes out; and the queued row
    /// flipping to `failed` is the boot pass announcing it got that far.
    #[tokio::test]
    async fn boot_work_does_not_swallow_the_events_published_under_it() {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
        use sea_orm::{ActiveModelTrait, Set};

        let dir = tempfile::tempdir().expect("tempdir");
        git_run(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").expect("write");
        git_run(dir.path(), &["add", "-A"]);
        git_run(dir.path(), &["commit", "-q", "-m", "base"]);

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, dir.path().to_str().expect("utf-8 path")).await;
        let draft = |title: &str| crate::models::WorkTaskDraft {
            folder_id,
            title: title.to_string(),
            config: serde_json::json!({ "display_text": title }),
        };

        // Interrupted by the restart — the boot pass fails it, which is how
        // this test learns the pass has begun.
        let interrupted = work_task_service::create(&db.conn, draft("interrupted"))
            .await
            .expect("task");
        work_task_service::claim_for_run(&db.conn, interrupted.id, WorkTaskStatus::Todo, "test")
            .await
            .expect("claim")
            .expect("claimed");

        // The row that parks the boot pass on the folder's git lock.
        let now = chrono::Utc::now();
        let merging = crate::db::entities::work_task::ActiveModel {
            folder_id: Set(folder_id),
            title: Set("mid-merge".to_string()),
            config: Set("{}".to_string()),
            status: Set(WorkTaskStatus::Merging),
            run_seq: Set(1),
            sort_order: Set(0),
            base_branch: Set(Some("main".to_string())),
            work_branch: Set(Some("task/1".to_string())),
            merge_state: Set(Some(
                serde_json::to_string(&WorkTaskMergeState {
                    op: WorkTaskMergeOp::Land,
                    strategy: "merge".into(),
                    ..Default::default()
                })
                .expect("state"),
            )),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("merging row");

        let engine = test_engine(db);
        let lock = engine.folder_lock(folder_id).await;
        let guard = lock.lock().await;

        let driver = engine.clone();
        tokio::spawn(async move { run_task_engine(driver).await });

        let probe = engine.clone();
        wait_for("the boot pass to start", move || {
            let probe = probe.clone();
            async move { row(&probe, interrupted.id).await.status == WorkTaskStatus::Failed }
        })
        .await;

        // A run the user starts while boot is still going. Created HERE, after
        // the boot pass swept the interrupted rows, so nothing but the event
        // below can settle it.
        let live = work_task_service::create(&engine.db.conn, draft("started during boot"))
            .await
            .expect("task");
        let conv = conversation_service::create(
            &engine.db.conn,
            folder_id,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .expect("conversation");
        let run_seq =
            work_task_service::claim_for_run(&engine.db.conn, live.id, WorkTaskStatus::Todo, "test")
                .await
                .expect("claim")
                .expect("claimed");
        assert!(work_task_service::begin_setup(&engine.db.conn, live.id, run_seq)
            .await
            .expect("begin_setup"));
        assert!(work_task_service::mark_running(
            &engine.db.conn,
            live.id,
            run_seq,
            conv.id,
            PARENT_CONN
        )
        .await
        .expect("mark_running"));
        engine
            .index
            .lock()
            .await
            .insert(PARENT_CONN.into(), (live.id, run_seq));
        // A LIVE connection, so the reconcile sweep keeps its hands off the row
        // — which is exactly the production shape that makes a lost
        // `TurnComplete` permanent.
        engine
            .manager
            .insert_test_connection(
                PARENT_CONN,
                AgentType::ClaudeCode,
                None,
                crate::web::event_bridge::EventEmitter::Noop,
            )
            .await;

        engine.bus.send(std::sync::Arc::new(env(
            PARENT_CONN,
            AcpEvent::TurnComplete {
                session_id: "s-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        )));

        // Boot can finish now; the event has to survive it.
        drop(guard);
        let probe = engine.clone();
        wait_for("the settle from an event published during boot", move || {
            let probe = probe.clone();
            async move { row(&probe, live.id).await.status == WorkTaskStatus::Review }
        })
        .await;
        assert_eq!(
            row(&engine, merging.id).await.status,
            WorkTaskStatus::Review,
            "the parked boot pass still ran to completion"
        );
    }

    /// Nothing makes an agent commit before its task lands in review — the
    /// merge generation's prompt starts by committing whatever is left over —
    /// so a task whose new files are still untracked has done work all the
    /// same. Reading it as "changed nothing" would put "complete" on the card
    /// and let the user close a task whose work never lands.
    #[tokio::test]
    async fn work_the_agent_never_committed_still_counts() {
        let f = delivery_fixture(FakeForge::default()).await;
        // Undo the fixture's commit, leaving its content in the worktree as an
        // uncommitted edit, and add a new file nobody ran `git add` on.
        git_run(&f.worktree, &["reset", "-q", "--mixed", "HEAD~1"]);
        std::fs::write(f.worktree.join("fresh.txt"), "a\nb\n").expect("write");

        let stats = f.engine.snapshot_diff_stats(f.task_id).await.expect("stats");
        assert_eq!(stats, (2, 3, 0), "the edit and the new file both count");
        let task = row(&f.engine, f.task_id).await;
        assert!(
            f.engine
                .has_landable_changes(&task, f.worktree.to_str().unwrap())
                .await
                .expect("landable probe"),
            "completing this task would strand real work"
        );
    }

    /// Base content the branch ABSORBED is not this task's work. An agent that
    /// merges the base branch in — or rebases onto it — brings every commit the
    /// base gained along, and measuring from the commit the task branched off
    /// would count all of it: a task that added one line would report the whole
    /// week's work, and the counters decide which acceptance the board offers.
    #[tokio::test]
    async fn base_content_the_branch_absorbed_is_not_the_tasks_work() {
        let f = delivery_fixture(FakeForge::default()).await;
        // The task's own contribution, as the fixture left it.
        assert_eq!(
            f.engine.snapshot_diff_stats(f.task_id).await,
            Some((1, 1, 0))
        );

        // The base branch moves on, and the worktree takes it in.
        git_run(&f.root, &["checkout", "-q", "main"]);
        std::fs::write(f.root.join("later.txt"), "one\ntwo\nthree\n").expect("write");
        git_run(&f.root, &["add", "-A"]);
        git_run(&f.root, &["commit", "-qm", "the base moves on"]);
        git_run(&f.worktree, &["merge", "-q", "--no-edit", "main"]);

        assert_eq!(
            f.engine.snapshot_diff_stats(f.task_id).await,
            Some((1, 1, 0)),
            "the merged-in base branch is not this task's change set"
        );
        let task = row(&f.engine, f.task_id).await;
        assert!(
            f.engine
                .has_landable_changes(&task, f.worktree.to_str().unwrap())
                .await
                .expect("landable probe"),
            "…and its own commit is still there to land"
        );
    }

    // ── tasks that ARE a pull request ───────────────────────────────────────

    /// Re-stamp the delivery fixture's task as one triggered from a pull
    /// request: same repository and worktree, different provenance — and a
    /// head branch (`feature`) that is deliberately NOT the task's own branch,
    /// so a push to the wrong one is visible.
    async fn as_pull_request_task(f: &Delivery, pr: ForgePr) {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let meta = serde_json::json!({
            "provider": "github",
            "server_host": "github.com",
            "api_base": "https://api.github.com",
            "account_id": "acc-1",
            "owner_repo": "acme/app",
            "number": 7,
            "url": "https://github.com/acme/app/pull/7",
            "title": "Fix the login flow",
            "base_ref": "main",
            "head_ref": pr.head_ref,
            "head_sha": pr.head_sha,
            // Canonical casing, as GitHub answers it — the fork test is a
            // comparison, and an exact one would call this a fork.
            "head_repo": pr.head_repo,
        });
        let mut active = row(&f.engine, f.task_id).await.into_active_model();
        active.source_kind = Set(Some(SOURCE_KIND_PR.to_string()));
        active.source_key = Set(Some("github:github.com:acme/app:pr:7".to_string()));
        active.source_meta = Set(Some(meta.to_string()));
        active.update(&f.engine.db.conn).await.expect("as a pull request task");
    }

    fn open_pull(head_sha: &str, head_ref: &str, head_repo: &str) -> ForgePr {
        ForgePr {
            number: 7,
            html_url: "https://github.test/acme/app/pull/7".into(),
            state: "open".into(),
            merged: false,
            head_sha: head_sha.into(),
            head_ref: head_ref.into(),
            head_repo: head_repo.into(),
            base_ref: "main".into(),
        }
    }

    /// A repository on `main` that also carries a `feature` branch the project
    /// checkout is NOT on. Returns `(engine, task id, home, feature tip, main
    /// tip)`; the task's config is whatever the caller passes.
    async fn branch_choice_fixture(
        config: serde_json::Value,
    ) -> (Arc<TaskEngine>, i32, tempfile::TempDir, String, String) {
        use sea_orm::{ActiveModelTrait, Set};

        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        git_run(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git_run(&root, &["add", "-A"]);
        git_run(&root, &["commit", "-q", "-m", "base"]);
        // The branch the user wants this task to work on…
        git_run(&root, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("feature.txt"), "the feature\n").expect("write");
        git_run(&root, &["add", "-A"]);
        git_run(&root, &["commit", "-q", "-m", "the feature so far"]);
        let feature_tip = task_git::rev_parse(root.to_str().unwrap(), "HEAD")
            .await
            .expect("feature tip");
        // …and the branch the project folder happens to be sitting on.
        git_run(&root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("b.txt"), "meanwhile\n").expect("write");
        git_run(&root, &["add", "-A"]);
        git_run(&root, &["commit", "-q", "-m", "main moves on"]);
        let main_tip = task_git::rev_parse(root.to_str().unwrap(), "HEAD")
            .await
            .expect("main tip");

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, root.to_str().unwrap()).await;
        let now = chrono::Utc::now();
        let task = crate::db::entities::work_task::ActiveModel {
            folder_id: Set(folder_id),
            title: Set("Polish the feature".to_string()),
            config: Set(config.to_string()),
            status: Set(WorkTaskStatus::Todo),
            run_seq: Set(1),
            sort_order: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("insert task");

        (test_engine(db), task.id, home, feature_tip, main_tip)
    }

    /// The task says which branch it is for, and that is where it starts and
    /// what it lands back onto — whatever the project folder happens to be
    /// checked out on when the task is claimed.
    #[tokio::test]
    async fn a_task_starts_on_the_base_branch_it_was_created_for() {
        let (engine, task_id, home, feature_tip, _main_tip) =
            branch_choice_fixture(serde_json::json!({ "base_branch": "feature" })).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        let wt = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        assert_eq!(
            task_git::rev_parse(&wt.path, "HEAD").await.expect("head"),
            feature_tip,
            "the worktree starts at the chosen branch's tip"
        );
        let after = row(&engine, task_id).await;
        assert_eq!(
            after.base_branch.as_deref(),
            Some("feature"),
            "the merge lands back onto the branch the task was created for"
        );
        assert_eq!(after.base_sha.as_deref(), Some(feature_tip.as_str()));
        drop(home);
    }

    /// No choice recorded — every task created before this existed, and every
    /// task of a user who does not care — keeps branching from the project
    /// folder's current checkout.
    #[tokio::test]
    async fn a_task_without_a_chosen_branch_still_follows_the_checkout() {
        let (engine, task_id, home, _feature_tip, main_tip) =
            branch_choice_fixture(serde_json::json!({})).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        let after = row(&engine, task_id).await;
        assert_eq!(after.base_branch.as_deref(), Some("main"));
        assert_eq!(after.base_sha.as_deref(), Some(main_tip.as_str()));
        drop(home);
    }

    /// A branch that is gone by the time the task is claimed must not quietly
    /// fall back to the checkout: the whole point of the choice is that the
    /// work lands somewhere specific, and starting from `main` instead would
    /// be a wrong answer delivered silently.
    #[tokio::test]
    async fn a_chosen_branch_that_disappeared_refuses_instead_of_falling_back() {
        let (engine, task_id, home, _feature_tip, _main_tip) =
            branch_choice_fixture(serde_json::json!({ "base_branch": "gone" })).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        let err = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .err()
            .expect("must refuse");
        assert!(err.contains("gone"), "{err}");
        assert!(row(&engine, task_id).await.worktree_folder_id.is_none());
        drop(home);
    }

    /// A repository that HAS a proposed change: `origin` carries `main` (moved
    /// on since), the contributor's `feature` branch, and the server-side head
    /// ref the forge publishes for it — `refs/pull/7/head` on GitHub,
    /// `refs/merge_requests/7/head` on GitLab. Parameterized by forge because
    /// that ref name is the ONE thing setup cannot guess: the wrong spelling
    /// is simply a ref that does not exist.
    ///
    /// `root` is a clone — the project folder. Returns `(engine, task id, root
    /// path, head commit, merge base)`.
    async fn pull_checkout_fixture(
        head_sha_recorded: Option<&str>,
        provider: crate::forge::ForgeProvider,
    ) -> (Arc<TaskEngine>, i32, tempfile::TempDir, String, String) {
        use sea_orm::{ActiveModelTrait, Set};

        let home = tempfile::tempdir().expect("tempdir");
        let origin = home.path().join("origin");
        std::fs::create_dir_all(&origin).expect("mkdir");
        git_run(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("a.txt"), "one\n").expect("write");
        git_run(&origin, &["add", "-A"]);
        git_run(&origin, &["commit", "-q", "-m", "base"]);
        let branch_point = task_git::rev_parse(origin.to_str().unwrap(), "HEAD")
            .await
            .expect("branch point");
        // The contributor's work…
        git_run(&origin, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(origin.join("feature.txt"), "the contribution\n").expect("write");
        git_run(&origin, &["add", "-A"]);
        git_run(&origin, &["commit", "-q", "-m", "the pull request"]);
        let pull_head = task_git::rev_parse(origin.to_str().unwrap(), "HEAD")
            .await
            .expect("pull head");
        git_run(&origin, &["update-ref", &provider.change_head_ref(7), &pull_head]);
        // …and the base branch moving on underneath it, which is what makes
        // "merge base" and "base tip" different commits.
        git_run(&origin, &["checkout", "-q", "main"]);
        std::fs::write(origin.join("b.txt"), "meanwhile\n").expect("write");
        git_run(&origin, &["add", "-A"]);
        git_run(&origin, &["commit", "-q", "-m", "main moves on"]);

        let root = home.path().join("root");
        git_run(
            home.path(),
            &["clone", "-q", origin.to_str().unwrap(), root.to_str().unwrap()],
        );

        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, root.to_str().unwrap()).await;
        let meta = serde_json::json!({
            "provider": provider.as_str(),
            "server_host": "github.com",
            "api_base": "https://api.github.com",
            "account_id": "acc-1",
            "owner_repo": "acme/app",
            "number": 7,
            "url": "https://github.com/acme/app/pull/7",
            "title": "The contribution",
            "base_ref": "main",
            "head_ref": "feature",
            "head_sha": head_sha_recorded.unwrap_or(&pull_head),
            "head_repo": "acme/app",
        });
        let now = chrono::Utc::now();
        let task = crate::db::entities::work_task::ActiveModel {
            folder_id: Set(folder_id),
            title: Set("#7 · The contribution".to_string()),
            config: Set("{}".to_string()),
            status: Set(WorkTaskStatus::Todo),
            run_seq: Set(1),
            sort_order: Set(0),
            source_kind: Set(Some(SOURCE_KIND_PR.to_string())),
            source_key: Set(Some(format!("{}:github.com:acme/app:pr:7", provider.as_str()))),
            source_meta: Set(Some(meta.to_string())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("insert task");
        // The fake forge, because setup now fetches through the pinned account
        // rather than the folder's `origin` — and resolving that account for
        // real would read the OS keyring, which a unit test must never do. The
        // fake fetches from this fixture's local `origin` instead.
        (
            test_engine_with_forge(db, Arc::new(FakeForge::default())),
            task.id,
            home,
            pull_head,
            branch_point,
        )
    }

    /// A pull-request task starts ON the pull request, and its review diff is
    /// measured from the MERGE BASE — so the pull request's own changes are
    /// part of what gets reviewed, and the base branch's unrelated progress is
    /// not.
    #[tokio::test]
    async fn a_pull_request_task_checks_out_the_pull_request_head() {
        let (engine, task_id, home, pull_head, branch_point) =
            pull_checkout_fixture(None, crate::forge::ForgeProvider::GitHub).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        let wt = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        assert_eq!(
            task_git::rev_parse(&wt.path, "HEAD").await.expect("head"),
            pull_head,
            "the worktree starts at the pull request's head"
        );
        let after = row(&engine, task_id).await;
        assert_eq!(after.base_branch.as_deref(), Some("main"));
        assert_eq!(
            after.base_sha.as_deref(),
            Some(branch_point.as_str()),
            "the diff baseline is where the pull request branched off"
        );
        let changed = task_git::diff_numstat(&wt.path, &branch_point)
            .await
            .expect("diff");
        assert!(
            changed.iter().any(|f| f.file == "feature.txt"),
            "the pull request's own change must be inside the review diff: {changed:?}"
        );
        assert!(
            !changed.iter().any(|f| f.file == "b.txt"),
            "the base branch's later commits are not this task's work: {changed:?}"
        );
        drop(home);
    }

    /// The counters on the row are the TASK's own work, which on a
    /// pull-request task is NOT the review diff: the worktree starts on the
    /// pull request, so the whole contribution is already there before the
    /// agent does anything. A review-only run — findings reported, nothing
    /// committed — has to settle at zero, or the board hides "complete" behind
    /// a delivery that would push an empty change.
    #[tokio::test]
    async fn a_pull_request_task_counts_only_what_the_agent_added() {
        let (engine, task_id, home, _pull_head, branch_point) =
            pull_checkout_fixture(None, crate::forge::ForgeProvider::GitHub).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");
        let wt = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");
        let task = row(&engine, task_id).await;

        // What the user reviews still holds the pull request itself…
        let reviewed = task_git::diff_numstat(&wt.path, &branch_point)
            .await
            .expect("review diff");
        assert!(
            reviewed.iter().any(|f| f.file == "feature.txt"),
            "the review diff is unchanged: {reviewed:?}"
        );
        // …while the task's own contribution is empty, on both the recorded
        // stat and the live probe the acceptances re-ask.
        assert_eq!(
            engine.snapshot_diff_stats(task_id).await,
            Some((0, 0, 0)),
            "a review-only run changed nothing"
        );
        assert!(!engine
            .has_landable_changes(&task, &wt.path)
            .await
            .expect("landable probe"));

        // A round that does commit is counted — and only its own file.
        std::fs::write(Path::new(&wt.path).join("fix.txt"), "the agent's work\n").expect("write");
        git_run(Path::new(&wt.path), &["add", "-A"]);
        git_run(Path::new(&wt.path), &["commit", "-qm", "the agent's fix"]);
        assert_eq!(
            engine.snapshot_diff_stats(task_id).await,
            Some((1, 1, 0)),
            "the agent's file, not the pull request's"
        );
        assert!(engine
            .has_landable_changes(&task, &wt.path)
            .await
            .expect("landable probe"));
        drop(home);
    }

    /// The same rule on the path that made it matter: a pull-request task whose
    /// agent wrote a file and never added it. The counters have to say the task
    /// did something, the drawer has to list the same file the counters count,
    /// and completing has to refuse — closing it here would leave work that
    /// nothing is going to land (delivery cannot push an uncommitted tree
    /// either; it says so and asks for another round). The ignore rules are
    /// pinned where they are applied, in
    /// `git::tests::the_acceptance_measure_sees_work_that_was_never_committed`.
    #[tokio::test]
    async fn an_untracked_file_keeps_a_pull_request_task_out_of_complete() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let (engine, task_id, home, _pull_head, _branch_point) =
            pull_checkout_fixture(None, crate::forge::ForgeProvider::GitHub).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");
        let wt = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        std::fs::write(Path::new(&wt.path).join("findings.md"), "one\ntwo\n").expect("write");

        assert_eq!(
            engine.snapshot_diff_stats(task_id).await,
            Some((1, 2, 0)),
            "an uncommitted new file is still this task's work"
        );
        let listed = crate::commands::work_task::work_task_changed_files_core(&engine.db, task_id)
            .await
            .expect("changed files");
        assert!(
            listed.iter().any(|f| f.file == "findings.md"),
            "the drawer must list what the card counts: {listed:?}"
        );

        let mut active = row(&engine, task_id).await.into_active_model();
        active.status = Set(WorkTaskStatus::Review);
        active.update(&engine.db.conn).await.expect("into review");
        let refused = engine
            .complete_task(task_id, false)
            .await
            .expect_err("the work is still sitting there");
        assert!(refused.contains("changed files after all"), "{refused}");
        assert_eq!(
            row(&engine, task_id).await.status,
            WorkTaskStatus::Review,
            "a refused completion leaves the task where it was"
        );
        drop(home);
    }

    /// The counters are written once, when the run settles, and every board
    /// decision reads them afterwards — so a row can outlive what it describes
    /// (a worktree committed to since, or a pull-request task settled by a
    /// build that credited it with the pull request's own files). The boot pass
    /// re-measures whatever is still sitting in review.
    #[tokio::test]
    async fn the_boot_pass_re_measures_a_review_row() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
        let (engine, task_id, home, _pull_head, _branch_point) =
            pull_checkout_fixture(None, crate::forge::ForgeProvider::GitHub).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");
        engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        // The row an older build left behind: in review, carrying the pull
        // request's change set as if the task had written it.
        let mut active = row(&engine, task_id).await.into_active_model();
        active.status = Set(WorkTaskStatus::Review);
        active.files_changed = Set(Some(1));
        active.additions = Set(Some(1));
        active.deletions = Set(Some(0));
        active.update(&engine.db.conn).await.expect("stale review row");

        engine.refresh_review_diff_stats().await;

        let after = row(&engine, task_id).await;
        assert_eq!(
            (after.files_changed, after.additions, after.deletions),
            (Some(0), Some(0), Some(0)),
            "the corrected counters are what the board reads"
        );
        assert_eq!(after.status, WorkTaskStatus::Review, "nothing else moved");
        drop(home);
    }

    /// The same setup against GitLab. Everything downstream — the merge-base
    /// baseline, the pinned OID, the review diff — is shared code; what is NOT
    /// shared is the ref the head is published under, and fetching GitHub's
    /// spelling from a GitLab server finds nothing at all.
    #[tokio::test]
    async fn a_merge_request_task_checks_out_the_gitlab_head_ref() {
        let (engine, task_id, home, mr_head, branch_point) =
            pull_checkout_fixture(None, crate::forge::ForgeProvider::GitLab).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        let wt = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .expect("worktree");

        assert_eq!(
            task_git::rev_parse(&wt.path, "HEAD").await.expect("head"),
            mr_head,
            "the worktree starts at the merge request's head"
        );
        assert_eq!(
            row(&engine, task_id).await.base_sha.as_deref(),
            Some(branch_point.as_str()),
            "the diff baseline is where the merge request branched off"
        );
        drop(home);
    }

    /// A pull request force-pushed since the task was created no longer has
    /// the commit the user triggered on. Checking out the NEW head would run
    /// the task against code nobody chose, so setup fails and says so.
    #[tokio::test]
    async fn a_force_pushed_pull_request_refuses_instead_of_switching_commits() {
        let gone = "0123456789012345678901234567890123456789";
        let (engine, task_id, home, _pull_head, _base) =
            pull_checkout_fixture(Some(gone), crate::forge::ForgeProvider::GitHub).await;
        let task = row(&engine, task_id).await;
        let root = get_folder_core(&engine.db, task.folder_id).await.expect("root");

        let err = engine
            .ensure_worktree(&task, &root, &WorkTaskFolderSettings::default())
            .await
            .err()
            .expect("must refuse");
        assert!(err.contains("force-pushed"), "{err}");
        assert!(row(&engine, task_id).await.worktree_folder_id.is_none());
        drop(home);
    }

    /// The window between "we checked" and "we pushed" is real, and what the
    /// pull request looks like AFTER the push is what the card will claim. A
    /// review that was closed, or retargeted at another base, is not a
    /// delivery — the task says so instead of recording `done`.
    #[tokio::test]
    async fn a_push_back_does_not_settle_against_a_review_that_moved() {
        for (label, mutate, expect) in [
            (
                "closed while we pushed",
                Box::new(|pr: &mut ForgePr| pr.state = "closed".into()) as Box<dyn Fn(&mut ForgePr)>,
                "closed without merging",
            ),
            (
                "retargeted while we pushed",
                Box::new(|pr: &mut ForgePr| pr.base_ref = "release/1.x".into()),
                "now targets 'release/1.x'",
            ),
        ] {
            let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
            let mut moved = pr.clone();
            mutate(&mut moved);
            let f = delivery_fixture(FakeForge {
                after_push: Mutex::new(Some(moved)),
                ..Default::default()
            })
            .await;
            as_pull_request_task(&f, pr.clone()).await;
            f.forge.existing.lock().await.push(pr);

            let err = f
                .engine
                .deliver_pr(f.task_id, None, false, false)
                .await
                .err()
                .unwrap_or_else(|| panic!("{label}: must not settle"));
            assert!(err.contains(expect), "{label}: {err}");
            // The push itself DID happen — the work is safe on the branch, and
            // the message says where it went. What is refused is calling it a
            // finished delivery.
            assert_eq!(f.forge.pushes.lock().await.len(), 1, "{label}");
            let task = row(&f.engine, f.task_id).await;
            assert_eq!(task.status, WorkTaskStatus::Review, "{label}");
        }
    }

    /// Merged in that same window is the opposite answer — but ONLY when what
    /// merged is what we pushed. The losing order is real: someone merges the
    /// old head, we push on top, and the reread says "merged" about a merge
    /// our commits are not in.
    #[tokio::test]
    async fn a_push_back_settles_on_a_merge_only_when_it_contains_our_commit() {
        // Merged AT the commit this delivery pushed: the work landed.
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
        let mut merged_ours = pr.clone();
        merged_ours.state = "closed".into();
        merged_ours.merged = true;
        let f = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr.clone());
        // `f.head` is the commit the task branch points at — what the push
        // publishes, and therefore what a merge containing our work reports.
        merged_ours.head_sha = f.head.clone();
        *f.forge.after_push.lock().await = Some(merged_ours);
        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("merged at our commit is a delivery");
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Done);

        // Merged at a DESCENDANT of what we pushed: someone fast-forwarded on
        // top of this task's work and merged that. Our commits are in it, so
        // it IS the delivery — equality alone would have rejected this.
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
        let f = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr.clone());
        // A real child commit of the task's head, published where the forge
        // publishes a change's head.
        git_run(&f.worktree, &["commit", "-q", "--allow-empty", "-m", "on top of the task"]);
        let descendant = task_git::rev_parse(f.worktree.to_str().unwrap(), "HEAD")
            .await
            .expect("descendant");
        git_run(&f.root, &["update-ref", "refs/pull/7/head", &descendant]);
        // …and the task branch put back, so what the delivery pushes is still
        // this task's own head.
        git_run(&f.worktree, &["reset", "-q", "--hard", &f.head]);
        let mut merged_on_top = pr.clone();
        merged_on_top.state = "closed".into();
        merged_on_top.merged = true;
        merged_on_top.head_sha = descendant;
        *f.forge.after_push.lock().await = Some(merged_on_top);
        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("a merge that contains our commit is a delivery");
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Done);
        assert!(
            probe_refs(&f.root).await.is_empty(),
            "the ancestry probe cleans up after itself"
        );

        // Merged at SOMETHING ELSE while we were pushing: our commits are on
        // the branch and not in that merge, and the card must not say done.
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
        let f = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr.clone());
        // The published head is the base commit — a real commit, and NOT a
        // descendant of this task's work.
        git_run(&f.root, &["update-ref", "refs/pull/7/head", &f.base_sha]);
        let mut merged_theirs = pr;
        merged_theirs.state = "closed".into();
        merged_theirs.merged = true;
        merged_theirs.head_sha = f.base_sha.clone();
        *f.forge.after_push.lock().await = Some(merged_theirs);
        let err = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("a merge without our commit is not a delivery");
        assert!(err.contains("does not contain it"), "{err}");
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Review);
        // The scratch ref is scoped to the task (siblings in one folder do not
        // share a lock) and is gone whichever way the answer went — on this
        // failing path as much as on the settling one above.
        assert!(
            probe_refs(&f.root).await.is_empty(),
            "no scratch ref may survive a refusal either"
        );
    }

    /// Every `refs/codeg/*` ref left in a repository — the engine's scratch
    /// namespace, which nothing outside it may ever find.
    async fn probe_refs(root: &std::path::Path) -> Vec<String> {
        let out = crate::process::tokio_command("git")
            .args(["for-each-ref", "--format=%(refname)", "refs/codeg/"])
            .current_dir(root)
            .output()
            .await
            .expect("for-each-ref");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The delivery of a pull-request task is a push back onto that pull
    /// request's OWN branch. Nothing is created — the review the user is
    /// already looking at is where the work belongs.
    #[tokio::test]
    async fn a_pull_request_task_is_pushed_back_to_its_own_branch() {
        let f = delivery_fixture(FakeForge::default()).await;
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Acme/App");
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr);

        let url = f
            .engine
            .deliver_pr(f.task_id, Some("ignored".into()), false, false)
            .await
            .expect("push back");
        assert_eq!(url, "https://github.test/acme/app/pull/7");
        assert_eq!(
            f.forge.pushes.lock().await.as_slice(),
            [("acme/app".to_string(), "task/7".to_string(), "feature".to_string())],
            "the work branch goes to the pull request's head branch"
        );
        assert!(
            f.forge.created.lock().await.is_empty(),
            "a pull request that exists is not opened a second time"
        );
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
        assert_eq!(
            task.source_meta
                .as_deref()
                .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
                .and_then(|m| m.result_pr)
                .as_deref(),
            Some("https://github.test/acme/app/pull/7")
        );
    }

    /// A pull request from a FORK pushes back to the fork — that is where its
    /// branch lives, and where its author and reviewers are looking. The
    /// repository in the push is the fork's; every API call stays on the
    /// source repository.
    #[tokio::test]
    async fn a_fork_pull_request_task_pushes_back_to_the_fork() {
        let f = delivery_fixture(FakeForge::default()).await;
        // Canonical casing, as the API answers it — the push URL normalizes.
        let pr = open_pull("whatever-the-branch-points-at", "feature", "Contributor/App");
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr);

        f.engine.deliver_pr(f.task_id, None, false, false).await.expect("fork push back");
        assert_eq!(
            f.forge.pushes.lock().await.as_slice(),
            [("contributor/app".to_string(), "task/7".to_string(), "feature".to_string())],
            "the push lands in the fork, not in the source repository"
        );
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
    }

    /// A push back with nothing new is REFUSED, and the acceptance that fits
    /// takes over. The task's branch is the pull request's own head — a review
    /// turn that committed nothing — so a delivery would push nothing, leave
    /// the pull request exactly as it was, and still record the task as
    /// delivered to it. The board offers "complete" for this row (nothing of
    /// its own to land); the backend has to agree, or the two disagree about
    /// the same task.
    ///
    /// The review-only task on a fork this account cannot write to is still
    /// accepted — completing needs no write access at all, and the fake here
    /// would fail any push.
    #[tokio::test]
    async fn a_push_back_with_nothing_new_is_refused_for_completing() {
        let forge = FakeForge {
            push_error: Some("403: permission denied".into()),
            ..FakeForge::default()
        };
        let f = delivery_fixture(forge).await;
        // The pull request's head IS the task branch's head: nothing to push.
        let pr = open_pull(&f.head, "feature", "contributor/app");
        as_pull_request_task(&f, pr.clone()).await;
        f.forge.existing.lock().await.push(pr);

        let refused = f
            .engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect_err("a delivery with nothing in it");
        assert!(refused.contains("complete it instead"), "{refused}");
        assert!(f.forge.pushes.lock().await.is_empty(), "no push may run");
        assert_eq!(
            row(&f.engine, f.task_id).await.status,
            WorkTaskStatus::Review,
            "a refused delivery leaves the task exactly as it was"
        );

        f.engine
            .complete_task(f.task_id, false)
            .await
            .expect("accepted without touching the fork");
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(
            task.completion_kind.as_deref(),
            Some("accepted_without_merge"),
            "…and recorded as what it was, not as a delivery"
        );
    }

    /// The OTHER way a delivery finds nothing to push: its push already landed
    /// and something interrupted the settle. The retry must recognize the pull
    /// request as carrying this task's commit and finish — without pushing
    /// again, which is what lets it complete on a fork this account cannot
    /// write to (the fake here fails any push). Distinguished from the refused
    /// case above by the task actually having work: it was triggered on the
    /// pull request's earlier head and committed on top of it.
    #[tokio::test]
    async fn a_delivery_whose_push_already_landed_settles_without_pushing_again() {
        let forge = FakeForge {
            push_error: Some("403: permission denied".into()),
            ..FakeForge::default()
        };
        let f = delivery_fixture(forge).await;
        as_pull_request_task(&f, open_pull(&f.base_sha, "feature", "contributor/app")).await;
        // What the forge answers today: the task's own commit is already there.
        f.forge
            .existing
            .lock()
            .await
            .push(open_pull(&f.head, "feature", "contributor/app"));

        f.engine
            .deliver_pr(f.task_id, None, false, false)
            .await
            .expect("settled without a push");
        assert!(f.forge.pushes.lock().await.is_empty(), "no push may run");
        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Done);
        assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
    }

    /// A row recorded before the head repository was part of the meta (builds
    /// that refused forks at trigger, so same-repo by construction) pushes
    /// where it always did: the source repository.
    #[tokio::test]
    async fn a_push_back_without_a_recorded_head_repo_pushes_to_the_source_repo() {
        let f = delivery_fixture(FakeForge::default()).await;
        let pr = open_pull("whatever-the-branch-points-at", "feature", "");
        as_pull_request_task(&f, pr.clone()).await;
        // What the forge answers TODAY still names the repository.
        f.forge.existing.lock().await.push(open_pull(
            "whatever-the-branch-points-at",
            "feature",
            "acme/app",
        ));

        f.engine.deliver_pr(f.task_id, None, false, false).await.expect("push back");
        assert_eq!(
            f.forge.pushes.lock().await.as_slice(),
            [("acme/app".to_string(), "task/7".to_string(), "feature".to_string())]
        );
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Done);
    }

    /// Everything that would make the push land somewhere it does not belong
    /// is refused BEFORE the push, with the task untouched in review.
    #[tokio::test]
    async fn a_push_back_refuses_before_it_pushes_anywhere_wrong() {
        // A closed pull request: the commits would sit on a branch nobody is
        // reviewing any more.
        let closed = delivery_fixture(FakeForge::default()).await;
        let mut pr = open_pull("x", "feature", "acme/app");
        pr.state = "closed".into();
        as_pull_request_task(&closed, pr.clone()).await;
        closed.forge.existing.lock().await.push(pr);
        let err = closed
            .engine
            .deliver_pr(closed.task_id, None, false, false)
            .await
            .expect_err("closed");
        assert!(err.contains("no longer open"), "{err}");
        assert!(closed.forge.pushes.lock().await.is_empty());
        assert_eq!(row(&closed.engine, closed.task_id).await.status, WorkTaskStatus::Review);

        // A fork codeg cannot NAME — GitLab's unresolved `project-{id}`
        // placeholder — has no push URL, ever. Refused before the CAS.
        let fork = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&fork, open_pull("x", "feature", "project-4711")).await;
        let err = fork
            .engine
            .deliver_pr(fork.task_id, None, false, false)
            .await
            .expect_err("unnameable fork");
        assert!(err.contains("cannot see"), "{err}");
        assert!(fork.forge.pushes.lock().await.is_empty());
        assert_eq!(row(&fork.engine, fork.task_id).await.status, WorkTaskStatus::Review);

        // Retargeted under us: the pull request now tracks another branch, so
        // pushing to the recorded one delivers into nothing.
        let moved = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&moved, open_pull("x", "feature", "acme/app")).await;
        moved
            .forge
            .existing
            .lock()
            .await
            .push(open_pull("x", "someone-elses-branch", "acme/app"));
        let err = moved
            .engine
            .deliver_pr(moved.task_id, None, false, false)
            .await
            .expect_err("retargeted");
        assert!(err.contains("now tracks branch"), "{err}");
        assert!(moved.forge.pushes.lock().await.is_empty());
        assert_eq!(row(&moved.engine, moved.task_id).await.status, WorkTaskStatus::Review);
    }

    /// Landing a pull request's work on the local base would take its changes
    /// in behind the author's back and leave the review open. The refusal is
    /// in the engine, where an old frontend or a direct API call still hits it.
    #[tokio::test]
    async fn a_pull_request_task_is_never_merged_locally() {
        let f = delivery_fixture(FakeForge::default()).await;
        as_pull_request_task(&f, open_pull("x", "feature", "acme/app")).await;
        let err = f
            .engine
            .merge_task(f.task_id, None, false, None, false)
            .await
            .expect_err("must refuse");
        assert!(err.contains("deliver it back"), "{err}");
        assert_eq!(row(&f.engine, f.task_id).await.status, WorkTaskStatus::Review);
    }

    /// The merge dispatch runs its launch off-thread so the click is not held
    /// for the run (a resumed session may compact first — an unbounded turn).
    /// What must NOT change with it: a launch that never gets an agent up still
    /// reports its reason to the caller, and leaves the row settled and unowned
    /// by the time it does. Anything less and the dialog would close on a merge
    /// that silently never started.
    #[tokio::test]
    async fn a_merge_whose_launch_never_goes_live_still_reports_and_settles() {
        let f = delivery_fixture(FakeForge::default()).await;
        // Neither the task nor its folder names an agent, so the launch fails
        // where every launch checks first — before any connection exists.
        let err = f
            .engine
            .merge_task(f.task_id, None, false, None, false)
            .await
            .expect_err("a launch that cannot start must not read as dispatched");
        assert!(err.contains("no agent configured"), "{err}");

        let task = row(&f.engine, f.task_id).await;
        assert_eq!(task.status, WorkTaskStatus::Review, "settled before we returned");
        assert!(
            task.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("merge dispatch failed"),
            "{:?}",
            task.last_error
        );
        assert!(
            f.engine.merging.lock().await.is_empty(),
            "the generation must own nothing once its failure is reported"
        );
    }

    /// Fire-once, from either side. The launch reports itself live; the task
    /// driving it reports every way of failing before that point. A second
    /// report — a launch that fails AFTER going live — must not overwrite the
    /// first, or a dispatch the user was told succeeded would come back as an
    /// error into a dialog that has already closed.
    #[tokio::test]
    async fn the_dispatch_signal_reports_exactly_once() {
        let (signal, live) = DispatchSignal::new();
        signal.fire(Ok(()));
        signal.fire(Err("the round's prompt was refused".to_string()));
        assert_eq!(live.await.expect("sender fired"), Ok(()));

        // …and the other way round: nothing went live, so the failure is what
        // the caller gets.
        let (signal, live) = DispatchSignal::new();
        signal.fire(Err("no agent configured".to_string()));
        signal.fire(Ok(()));
        assert_eq!(
            live.await.expect("sender fired"),
            Err("no agent configured".to_string())
        );

        // A sender dropped without firing is the launch returning early without
        // dispatching (a cancel gate, a lost CAS) — the caller has to notice
        // rather than hang.
        let (signal, live) = DispatchSignal::new();
        drop(signal);
        assert!(live.await.is_err());
    }

    /// Recovery of an interrupted push-back settles on ONE piece of evidence:
    /// the pull request's head IS the commit this delivery was pushing.
    #[tokio::test]
    async fn recovery_settles_a_push_back_only_on_the_commit_it_pushed() {
        for (head_sha, expect_done) in [("<the task head>", true), ("0000000", false)] {
            let f = delivery_fixture(FakeForge::default()).await;
            let sha = if expect_done { f.head.clone() } else { head_sha.to_string() };
            as_pull_request_task(&f, open_pull(&sha, "feature", "acme/app")).await;
            f.forge.existing.lock().await.push(open_pull(&sha, "feature", "acme/app"));
            interrupt_delivery(&f, "feature").await;

            f.engine.recover_merging(f.task_id).await;
            let task = row(&f.engine, f.task_id).await;
            if expect_done {
                assert_eq!(task.status, WorkTaskStatus::Done);
                assert_eq!(task.completion_kind.as_deref(), Some("delivered_pr"));
            } else {
                assert_eq!(task.status, WorkTaskStatus::Review);
                assert!(
                    task.last_error.as_deref().unwrap_or_default().contains("does not show"),
                    "{:?}",
                    task.last_error
                );
            }
        }
    }

    /// Merge states written before deliveries existed carry no `op`. They are
    /// all lands, and reading one as a delivery would send crash recovery to
    /// the forge for a task that never had one.
    #[test]
    fn merge_state_without_an_op_decodes_as_a_land() {
        let legacy = r#"{"pre_merge_head":"abc123","message":"m","strategy":"squash",
                         "delete_worktree":true,"auto_message":false}"#;
        let state: WorkTaskMergeState = serde_json::from_str(legacy).expect("legacy state");
        assert_eq!(state.op, WorkTaskMergeOp::Land);
        assert_eq!(state.pre_merge_head, "abc123");
        assert!(state.delete_worktree);
        assert!(state.expected_head.is_none());
    }

    #[tokio::test]
    async fn delegations_from_a_non_task_connection_are_not_tracked() {
        // A delegation from an ordinary chat tab has no board row to flip; the
        // map must not grow for it.
        let (engine, _task_id) = running_task().await;
        engine.on_event(&delegation_started("conn-chat", "conn-other-child")).await;
        assert!(engine.delegation_parents.lock().await.is_empty());
    }

    // ── pre-prompt context compaction ───────────────────────────────────────

    const COMPACT_CONN: &str = "conn-compact";

    /// A generation parked exactly where `compact_before_prompt` runs: claimed,
    /// setup begun (`preparing`), agent connection live, and — as in the real
    /// launch at this point — nothing in `index` yet.
    struct CompactFixture {
        engine: Arc<TaskEngine>,
        task_id: i32,
        run_seq: i32,
        folder_id: i32,
        conversation_id: i32,
        /// The connection's command channel. Held for the test's duration: a
        /// dropped receiver makes every send fail before it reaches the gate.
        rx: tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>,
    }

    async fn compact_fixture(usage: Option<(u64, u64)>) -> CompactFixture {
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/task-compact").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .expect("conversation");
        let task = work_task_service::create(
            &db.conn,
            crate::models::WorkTaskDraft {
                folder_id,
                title: "fix login".to_string(),
                config: serde_json::json!({
                    "display_text": "fix login",
                    "prompt_blocks": [{ "type": "text", "text": "fix login" }],
                }),
            },
        )
        .await
        .expect("task");
        let run_seq =
            work_task_service::claim_for_run(&db.conn, task.id, WorkTaskStatus::Todo, "test")
                .await
                .expect("claim")
                .expect("claimed");
        assert!(work_task_service::begin_setup(&db.conn, task.id, run_seq)
            .await
            .expect("begin_setup"));

        let engine = test_engine(db);
        let rx = engine
            .manager
            .insert_test_connection_live(
                COMPACT_CONN,
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        if let Some((used, size)) = usage {
            let state = engine.manager.get_state(COMPACT_CONN).await.expect("state");
            state.write().await.usage =
                Some(crate::acp::session_state::UsageInfo { used, size });
        }
        CompactFixture {
            engine,
            task_id: task.id,
            run_seq,
            folder_id,
            conversation_id: conv.id,
            rx,
        }
    }

    fn compact_settings(percent: i32, command: Option<&str>) -> WorkTaskFolderSettings {
        WorkTaskFolderSettings {
            auto_compact_percent: percent,
            compact_command: command.map(str::to_string),
            ..Default::default()
        }
    }

    /// Drive `compact_before_prompt` to completion, answering the compaction
    /// turn with `stop_reason` once its prompt has actually been enqueued.
    /// Returns the text of the prompt it sent, or `None` when it sent none.
    async fn run_compaction(
        f: &mut CompactFixture,
        settings: WorkTaskFolderSettings,
        stop_reason: &str,
    ) -> Option<String> {
        let engine = f.engine.clone();
        let (task_id, run_seq, folder_id, conversation_id) =
            (f.task_id, f.run_seq, f.folder_id, f.conversation_id);
        let handle = tokio::spawn(async move {
            engine
                .compact_before_prompt(CompactRequest {
                    task_id,
                    run_seq,
                    conn_id: COMPACT_CONN,
                    agent_type: AgentType::ClaudeCode,
                    settings: &settings,
                    folder_id,
                    conversation_id,
                    in_flight: WorkTaskStatus::Preparing,
                })
                .await;
        });

        // Either a prompt shows up (compaction ran) or the call returns having
        // sent nothing. Waiting on the two together is what keeps the "no
        // compaction" assertions from depending on a sleep.
        let sent = tokio::select! {
            cmd = f.rx.recv() => match cmd.expect("connection alive") {
                crate::acp::connection::ConnectionCommand::Prompt { blocks, .. } => blocks
                    .iter()
                    .find_map(|b| match b {
                        PromptInputBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    }),
                _ => panic!("the compaction must arrive as a prompt"),
            },
            _ = &mut Box::pin(async {
                while !handle.is_finished() {
                    tokio::task::yield_now().await;
                }
            }) => None,
        };
        if sent.is_some() {
            // The waiter subscribed before it sent, so the bus has a receiver.
            f.engine.bus.send(Arc::new(EventEnvelope {
                seq: 1,
                connection_id: COMPACT_CONN.to_string(),
                payload: AcpEvent::TurnComplete {
                    session_id: "sess".to_string(),
                    stop_reason: stop_reason.to_string(),
                    agent_type: AgentType::ClaudeCode.to_string(),
                },
            }));
        }
        handle.await.expect("compaction finished");
        sent
    }

    async fn compact_events(engine: &TaskEngine, task_id: i32) -> Vec<serde_json::Value> {
        work_task_service::list_events(&engine.db.conn, task_id, 500)
            .await
            .expect("events")
            .into_iter()
            .filter(|e| e.kind == "context_compact")
            .map(|e| e.payload.unwrap_or(serde_json::Value::Null))
            .collect()
    }

    /// The feature's whole point: a nearly full session is compacted, and the
    /// round's own prompt only goes out after that turn has landed.
    #[tokio::test]
    async fn a_full_context_is_compacted_before_the_round_prompt() {
        let mut f = compact_fixture(Some((90_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(80, None), "end_turn").await;

        assert_eq!(sent.as_deref(), Some("/compact"));
        let events = compact_events(&f.engine, f.task_id).await;
        // A pair: the turn is announced when it goes out (the wait after it is
        // unbounded, and a card with nothing on its timeline is how a compaction
        // that never returns used to look) and again when it lands.
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0]["status"], "started");
        assert_eq!(events[0]["before_percent"], 90.0);
        assert_eq!(events[0]["command"], "/compact");
        assert_eq!(events[1]["status"], "ok");
        assert_eq!(events[1]["before_percent"], 90.0);
        assert_eq!(events[1]["before_source"], "live");
        assert_eq!(events[1]["command"], "/compact");
        // The slot the canceller reaches through must not outlive the turn.
        assert!(f.engine.compacting.lock().await.is_empty());
    }

    /// A compaction the agent REFUSED is a failure, not an "ok" with a
    /// footnote. `rejected` reaches the `TurnComplete` arm at all only because
    /// a prompt rejection stopped tearing the connection down (issue #797); it
    /// used to arrive as the terminal `Error` the waiter settles as `failed`,
    /// and the catch-all below it would otherwise have promoted a compaction
    /// that never ran to a successful one.
    #[tokio::test]
    async fn a_rejected_compaction_prompt_is_recorded_as_a_failure() {
        let mut f = compact_fixture(Some((90_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(80, None), "rejected").await;

        assert_eq!(sent.as_deref(), Some("/compact"));
        let events = compact_events(&f.engine, f.task_id).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[1]["status"], "failed");
        assert_eq!(
            events[1]["detail"], "the agent rejected the compaction prompt",
            "{events:?}"
        );
    }

    /// Below the threshold nothing is sent and nothing is written — the
    /// ordinary case has to stay free of both an extra turn and timeline noise.
    #[tokio::test]
    async fn a_session_below_the_threshold_is_left_alone() {
        let mut f = compact_fixture(Some((50_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(80, None), "end_turn").await;

        assert_eq!(sent, None);
        assert!(compact_events(&f.engine, f.task_id).await.is_empty());
    }

    /// The off switch. A full context with the setting at 0 is exactly today's
    /// behaviour — no reading, no turn, no event.
    #[tokio::test]
    async fn a_zero_threshold_disables_the_check() {
        let mut f = compact_fixture(Some((99_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(0, None), "end_turn").await;

        assert_eq!(sent, None);
        assert!(compact_events(&f.engine, f.task_id).await.is_empty());
    }

    /// The explicit setting is what gets sent, verbatim — it exists precisely
    /// for the agents codeg has no default for.
    #[tokio::test]
    async fn the_configured_command_is_sent_verbatim() {
        let mut f = compact_fixture(Some((90_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(80, Some("/summarize")), "end_turn")
            .await;

        assert_eq!(sent.as_deref(), Some("/summarize"));
    }

    /// A session that reports no occupancy can't be measured against a
    /// threshold. It must not be compacted on a guess — and the skip is
    /// recorded, because with the setting switched on "nothing happened" needs
    /// an explanation.
    #[tokio::test]
    async fn an_unmeasurable_session_is_not_compacted_but_is_recorded() {
        let mut f = compact_fixture(None).await;

        let sent = run_compaction(&mut f, compact_settings(80, None), "end_turn").await;

        assert_eq!(sent, None);
        let events = compact_events(&f.engine, f.task_id).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["status"], "skipped");
        assert_eq!(events[0]["reason"], "usage_unknown");
    }

    /// A cancel that lands while the compaction turn is in flight ends the
    /// wait through the same `TurnComplete{cancelled}` the manager emits — the
    /// launch must not sit on it, and the round is recorded as canceled rather
    /// than compacted.
    #[tokio::test]
    async fn a_canceled_compaction_turn_releases_the_launch() {
        let mut f = compact_fixture(Some((90_000, 100_000))).await;

        let sent = run_compaction(&mut f, compact_settings(80, None), "cancelled").await;

        assert_eq!(sent.as_deref(), Some("/compact"));
        let events = compact_events(&f.engine, f.task_id).await;
        assert_eq!(events[0]["status"], "started");
        assert_eq!(events[1]["status"], "canceled");
        assert!(f.engine.compacting.lock().await.is_empty());
    }

    fn turn_complete_envelope(conn_id: &str, seq: u64, stop_reason: &str) -> EventEnvelope {
        EventEnvelope {
            seq,
            connection_id: conn_id.to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "sess".to_string(),
                stop_reason: stop_reason.to_string(),
                agent_type: AgentType::ClaudeCode.to_string(),
            },
        }
    }

    /// The race the `compaction_turns` marker exists for. The bus is a
    /// broadcast: the launch's waiter reacts to the compaction's `TurnComplete`
    /// at once, but the engine's own receiver awaits DB work per event and can
    /// dequeue that same envelope only after the launch has indexed the
    /// connection and sent the round's real prompt. Handled naively, the
    /// compaction's completion would settle the round it preceded — and
    /// disconnect the agent mid-work.
    #[tokio::test]
    async fn a_late_compaction_completion_does_not_settle_the_round() {
        let mut f = compact_fixture(Some((90_000, 100_000))).await;
        let sent = run_compaction(&mut f, compact_settings(80, None), "end_turn").await;
        assert_eq!(sent.as_deref(), Some("/compact"));

        // The launch goes on to bind the connection to the run and prompt it.
        assert!(work_task_service::mark_running(
            &f.engine.db.conn,
            f.task_id,
            f.run_seq,
            f.conversation_id,
            COMPACT_CONN,
        )
        .await
        .expect("mark_running"));
        f.engine
            .index
            .lock()
            .await
            .insert(COMPACT_CONN.into(), (f.task_id, f.run_seq));

        // Only NOW does the engine's own loop get to the compaction's envelope
        // (seq 1 — the one `run_compaction` published).
        f.engine
            .on_event(&turn_complete_envelope(COMPACT_CONN, 1, "end_turn"))
            .await;

        assert_eq!(
            status_of(&f.engine, f.task_id).await,
            WorkTaskStatus::Running,
            "the compaction's completion must not settle the round it preceded"
        );

        // The other direction: the ROUND's own completion, a later seq on the
        // same connection, still settles normally.
        f.engine
            .on_event(&turn_complete_envelope(COMPACT_CONN, 2, "end_turn"))
            .await;
        assert_eq!(
            status_of(&f.engine, f.task_id).await,
            WorkTaskStatus::Review
        );
    }

    /// The fence covers everything up to its watermark — the wait can end
    /// without ever reading the completion, and must still be able to fence one
    /// it never saw — but nothing above it. A higher seq can only be the round's
    /// own turn, so it passes AND retires the fence.
    #[tokio::test]
    async fn the_compaction_fence_stops_at_the_rounds_own_turn() {
        let f = compact_fixture(Some((90_000, 100_000))).await;
        f.engine.mark_compaction_turn(COMPACT_CONN, 4).await;

        assert!(f.engine.claim_compaction_turn(COMPACT_CONN, 4).await);
        assert!(
            f.engine.claim_compaction_turn(COMPACT_CONN, 2).await,
            "a completion emitted before the watermark is the launch's too"
        );

        assert!(!f.engine.claim_compaction_turn(COMPACT_CONN, 9).await);
        assert!(
            f.engine.compaction_turns.lock().await.is_empty(),
            "the round's own turn retires the fence"
        );
        // Another connection's turns are never fenced by this one's mark.
        f.engine.mark_compaction_turn(COMPACT_CONN, 4).await;
        assert!(!f.engine.claim_compaction_turn("conn-other", 1).await);
    }

    /// Ending the wait on liveness rather than on the event itself is exactly
    /// when the completion is most likely to still be sitting in the engine's
    /// receiver: `apply_event` clears `turn_in_flight` BEFORE the envelope is
    /// broadcast. The probe has to fence it on the way out, or the fix for the
    /// settle race has a hole the size of the tick.
    #[tokio::test]
    async fn ending_the_wait_on_liveness_still_fences_the_completion() {
        let f = compact_fixture(Some((90_000, 100_000))).await;
        let state = f
            .engine
            .manager
            .get_state(COMPACT_CONN)
            .await
            .expect("state");
        {
            let mut s = state.write().await;
            s.turn_in_flight = false;
            s.event_seq = 7;
        }

        let outcome = f
            .engine
            .compaction_still_running(COMPACT_CONN)
            .await
            .expect("the wait ends");
        assert_eq!(outcome.status, "ok");
        assert!(
            f.engine.claim_compaction_turn(COMPACT_CONN, 7).await,
            "a completion the wait never read must still be fenced"
        );

        // A connection that vanished cannot report a seq, so its whole id is
        // fenced — ids are per-launch UUIDs, never reused.
        let outcome = f
            .engine
            .compaction_still_running("conn-vanished")
            .await
            .expect("the wait ends");
        assert_eq!(outcome.status, "failed");
        assert!(f.engine.claim_compaction_turn("conn-vanished", 99).await);
    }

    /// The fence must not outlive the launch that set it. A generation torn
    /// down before it ever went live (lost CAS, failed prompt, cancel) is
    /// retired by `forget_connection`, not by `retire_connection`.
    #[tokio::test]
    async fn a_torn_down_launch_takes_its_fence_with_it() {
        let f = compact_fixture(Some((90_000, 100_000))).await;
        f.engine.mark_compaction_turn(COMPACT_CONN, 4).await;
        f.engine
            .index
            .lock()
            .await
            .insert(COMPACT_CONN.into(), (f.task_id, f.run_seq));

        f.engine.forget_connection(COMPACT_CONN).await;

        assert!(f.engine.compaction_turns.lock().await.is_empty());
        assert!(f.engine.index.lock().await.is_empty());
    }

    /// `apply_event` clears `turn_in_flight` on `TurnComplete` but not on a
    /// terminal error or a disconnect — so a receiver that lagged one of those
    /// away would block on `recv()` for a connection that is never going to
    /// speak again, holding the task lock and taking any later cancel with it.
    /// The liveness tick is what ends those waits.
    #[tokio::test]
    async fn a_wait_with_no_events_still_ends_on_liveness() {
        let f = compact_fixture(Some((90_000, 100_000))).await;
        // Paused only now, and never around DB work: the sqlite pool's acquire
        // timeout runs on the same clock, so a test that freezes it before the
        // fixture exists dies with `PoolTimedOut`. With the runtime otherwise
        // idle, the tick's sleep auto-advances, so the wait costs no wall time.
        tokio::time::pause();

        // The connection is gone and no event is coming.
        let mut rx = f.engine.bus.subscribe();
        let outcome = f
            .engine
            .await_compaction_turn(&mut rx, "conn-never-registered")
            .await;
        assert_eq!(outcome.status, "failed");

        // Alive, but the turn already ended and its event was missed.
        let state = f
            .engine
            .manager
            .get_state(COMPACT_CONN)
            .await
            .expect("state");
        state.write().await.turn_in_flight = false;
        let outcome = f.engine.await_compaction_turn(&mut rx, COMPACT_CONN).await;
        assert_eq!(outcome.status, "ok");
    }

    /// The kill slot is generation-scoped like every other one on this engine:
    /// a cancel of an older run must not abort the compaction of a newer one.
    #[tokio::test]
    async fn a_stale_cancel_does_not_abort_a_newer_compaction() {
        let f = compact_fixture(Some((90_000, 100_000))).await;
        f.engine.compacting.lock().await.insert(
            f.task_id,
            CompactRun {
                run_seq: f.run_seq,
                conn_id: COMPACT_CONN.to_string(),
            },
        );

        f.engine.release_compact_slot(f.task_id, f.run_seq - 1).await;
        assert!(
            f.engine.compacting.lock().await.contains_key(&f.task_id),
            "an older generation must not release the live slot"
        );

        f.engine.release_compact_slot(f.task_id, f.run_seq).await;
        assert!(f.engine.compacting.lock().await.is_empty());
    }
