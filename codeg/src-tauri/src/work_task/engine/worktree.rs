use super::*;

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
pub(super) struct WorktreeRef {
    pub(super) folder_id: i32,
    pub(super) path: String,
}

/// Converge DB + clients once a task worktree is off disk. Split out of
/// [`TaskEngine::remove_worktree_locked`] so the ordering contract below is
/// unit-testable without a real git worktree.
///
/// Write order: conversations first (never orphaned under a vanishing folder),
/// then its tabs, then the folder row. Each step ends in a broadcast, because a
/// removal no client hears about is a removal no client shows: the sidebar keeps
/// the dead worktree until the next full `fetchFolders` (i.e. a reload).
pub(super) async fn converge_worktree_removal(
    db: &AppDatabase,
    emitter: &EventEmitter,
    task_id: i32,
    wt_id: i32,
    project_folder_id: i32,
    wt_path: &str,
) {
    let moved = conversation_service::reparent_folder_conversations(
        &db.conn,
        wt_id,
        project_folder_id,
        wt_path,
    )
    .await
    .unwrap_or(0);

    match tab_service::delete_folder_tabs_and_bump(&db.conn, wt_id).await {
        Ok(inv) => {
            if let Some(tabs) = inv.emit {
                emit_event(
                    emitter,
                    crate::web::event_bridge::TABS_CHANGED_EVENT,
                    crate::web::event_bridge::TabsChanged {
                        version: inv.version,
                        origin: "server".to_string(),
                        tabs,
                    },
                );
            }
        }
        Err(e) => tracing::warn!("[work_task] tab cleanup failed for folder {wt_id}: {e}"),
    }

    // Announce the folder drop only when the row really is gone, so a failed
    // write can't leave clients disagreeing with what a refetch would return.
    let folder_gone = match folder::Entity::find_by_id(wt_id).one(&db.conn).await {
        Ok(Some(row)) => {
            let now = chrono::Utc::now();
            let mut active = row.into_active_model();
            active.is_open = Set(false);
            active.deleted_at = Set(Some(now));
            active.updated_at = Set(now);
            match active.update(&db.conn).await {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!("[work_task] worktree folder {wt_id} soft-delete failed: {e}");
                    false
                }
            }
        }
        // Already soft-deleted / never there: a client still holding it is stale.
        Ok(None) => true,
        Err(e) => {
            tracing::warn!("[work_task] worktree folder {wt_id} lookup failed: {e}");
            false
        }
    };

    let _ = work_task_service::clear_worktree(&db.conn, task_id).await;
    let _ = work_task_service::record_event(
        &db.conn,
        task_id,
        "user_action",
        "engine",
        Some(serde_json::json!({ "action": "cleanup", "reparented": moved })),
    )
    .await;

    // Conversations first: this starts every client's refetch, so the moved
    // rows have the shortest possible window with no folder to render under.
    emit_event(
        emitter,
        crate::web::event_bridge::CONVERSATIONS_BULK_CHANGED_EVENT,
        crate::web::event_bridge::ConversationsBulkChanged {
            imported: 0,
            updated: moved as u32,
            folder_ids: vec![project_folder_id],
        },
    );
    if folder_gone {
        emit_folder_deleted(emitter, wt_id);
    }
}

/// The commit THIS TASK's own work sits on top of: the point its worktree was
/// checked out at, which is also the point an acceptance would deliver from.
/// The counters on the row and every "is there anything to land" probe measure
/// from here — deliberately not from the review baseline, which answers a
/// different question ("what is under review", pull request included).
///
/// For an ordinary task the two coincide: the branch was minted at the recorded
/// base, so "diff against the base" and "what the agent did" are one set. Two
/// things move the anchor off it:
///
/// - **A task triggered from a pull request starts ON that pull request's
///   head**, while its base is the MERGE BASE — because the review is meant to
///   show the pull request plus whatever the agent did (see
///   [`TaskEngine::pr_checkout_point`]). Measuring the task's own work from
///   there would credit it with every file the pull request itself changed: a
///   review-only task that committed nothing would report a full change set,
///   the board would offer to push it back (a delivery with nothing in it),
///   and "complete" — the acceptance that actually applies — would never
///   appear. The head comes from the task's source snapshot, which is what the
///   checkout used; it must still be a commit HERE, since a force-push can
///   leave the row naming one this repository never had.
/// - **The branch absorbing base content** (the agent merged the base branch
///   in, or rebased onto it) moves the merge base forward, and everything the
///   base contributed would otherwise read as this task's work. Taken only
///   when it moved FORWARD from the recorded base: a base branch that is
///   behind locally, or a same-name branch of unrelated history, must not drag
///   the anchor backwards and inflate the count it was meant to correct.
///
/// `wt_path` is the task's worktree — where every ref here is resolved.
pub(super) async fn own_work_anchor(
    wt_path: &str,
    task: &crate::db::entities::work_task::Model,
) -> Option<String> {
    if task.source_kind.as_deref() == Some(SOURCE_KIND_PR) {
        let head = task
            .source_meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<ForgeSourceMeta>(s).ok())
            .and_then(|m| m.head_sha)
            .filter(|s| !s.trim().is_empty());
        if let Some(head) = head {
            if task_git::commit_present(wt_path, &head).await.unwrap_or(false) {
                return Some(head);
            }
        }
    }
    let recorded = task.base_sha.clone().or_else(|| task.base_branch.clone());
    let Some(base_branch) = task.base_branch.as_deref() else {
        return recorded;
    };
    let Ok(Some(tip)) = task_git::local_branch_tip(wt_path, base_branch).await else {
        return recorded;
    };
    let Ok(merge_base) = task_git::merge_base(wt_path, &tip, "HEAD").await else {
        return recorded;
    };
    match task.base_sha.as_deref() {
        Some(sha) => task_git::is_ancestor(wt_path, sha, &merge_base)
            .await
            .unwrap_or(false)
            .then_some(merge_base)
            .or(recorded),
        // No recorded sha to move forward from — the merge base is strictly
        // better than the branch NAME this would otherwise diff against, which
        // measures against its tip and calls the base's own progress a change.
        None => Some(merge_base),
    }
}

/// `(files, additions, deletions)` of the worktree against `anchor`,
/// uncommitted work included — nothing forces the agent to commit before the
/// task lands in review, and a task whose new files are still untracked has
/// done work all the same.
pub(super) async fn diff_stats_from(wt_path: &str, anchor: &str) -> Option<(i32, i32, i32)> {
    let files = task_git::diff_numstat_with_untracked(wt_path, anchor)
        .await
        .ok()?;
    let adds: i32 = files.iter().map(|f| f.additions).sum();
    let dels: i32 = files.iter().map(|f| f.deletions).sum();
    Some((files.len() as i32, adds, dels))
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The project folder's own name — what a task worktree directory is named
/// after. `Path` semantics rather than a `/` split, so `C:\src\repo` yields
/// `repo` too.
///
/// The answer is always RELATIVE, empty included: a path with no final
/// component (a filesystem root, or one ending in `..`) must not fall back to
/// the path itself, because the name is JOINED onto the configured worktree
/// root — and an absolute one replaces that root instead of landing under it.
pub(super) fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// `name` placed next to the project folder — where task worktrees live when
/// the folder configures no root of its own. A project folder with no parent
/// (a drive or filesystem root) falls back to the bare name, which git then
/// resolves against the repository itself.
pub(super) fn sibling_path(root_path: &str, name: &str) -> String {
    match Path::new(root_path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join(name).to_string_lossy().into_owned()
        }
        _ => name.to_string(),
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Where the task worktree directory `name` goes for a project at
/// `root_path`. `worktree_root` is the folder setting: blank (or absent) keeps
/// the historical layout — right next to the project folder — and any other
/// value becomes the directory every new worktree of that folder is created
/// in. A typed path is taken at face value in the two ways a user expects: `~`
/// is the home directory, and something relative hangs off the project folder.
pub(super) fn worktree_path_in(root_path: &str, worktree_root: Option<&str>, name: &str) -> String {
    worktree_path_in_home(root_path, worktree_root, name, dirs::home_dir().as_deref())
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// [`worktree_path_in`] with the home directory injected — `dirs::home_dir()`
/// reads the real user's home even under a pinned `HOME`, so tests must hand
/// one in rather than set the environment.
pub(super) fn worktree_path_in_home(
    root_path: &str,
    worktree_root: Option<&str>,
    name: &str,
    home: Option<&Path>,
) -> String {
    let Some(configured) = worktree_root.map(str::trim).filter(|r| !r.is_empty()) else {
        return sibling_path(root_path, name);
    };
    let base = expand_home(configured, home);
    let base = if base.is_absolute() {
        base
    } else {
        Path::new(root_path).join(base)
    };
    base.join(name).to_string_lossy().into_owned()
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Expand a leading `~` against `home`. Left as-is when the home directory is
/// unknown, so the path still resolves as literally written instead of
/// silently becoming a different directory.
pub(super) fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
    {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}
