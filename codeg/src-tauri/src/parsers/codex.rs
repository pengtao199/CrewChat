use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use walkdir::WalkDir;

use crate::acp::agent_mentions::{
    contains_only_internal_agent_routes, strip_internal_agent_routes,
};
use crate::acp::js_text::is_js_whitespace;
use crate::models::*;
use crate::parsers::codex_code_mode::{
    extract_chunk_ids, extract_shell_session_ids, is_code_mode_call, parse_code_mode_script,
    script_card_input,
    split_code_mode_output, with_note, CodeModeCall, CodeModeOutput, CodeModeScript, ScriptStatus,
    Separator, CODEX_SCRIPT_TOOL_NAME,
};
use crate::parsers::{
    codex_desktop_attachments, folder_name_from_path, title_from_user_text, truncate_str,
    AgentParser, ParseError,
};

pub struct CodexParser {
    base_dir: PathBuf,
}

/// How far into a rollout to look for the `session_meta` carrying its history
/// pointer. It is line 0 in every file on disk; the slack is for a future
/// preamble, and the bound is what keeps this off the cost of a full parse for
/// the overwhelming majority of rollouts, which inherit no history.
const FORK_HEADER_SCAN_LINES: usize = 4;

/// What a canonical rollout filename encodes.
///
/// codex names a thread's rollout `rollout-<YYYY-MM-DDTHH-MM-SS>-<thread>.jsonl`.
/// `thread/revert` then moves the thread onto a NEW immutable file,
/// `rollout-<ts>-<thread>_<rollout>.jsonl`, and leaves the old one in place: the
/// thread id is the conversation, the rollout id names one file — and is what a
/// `history_base` points at. An ordinary rollout's rollout id is its thread id.
#[derive(Clone, Copy)]
struct RolloutFileName<'a> {
    timestamp: &'a str,
    thread_id: &'a str,
    rollout_id: &'a str,
}

impl<'a> RolloutFileName<'a> {
    fn parse(path: &'a Path) -> Option<Self> {
        let core = path
            .file_name()?
            .to_str()?
            .strip_prefix("rollout-")?
            .strip_suffix(".jsonl")?;
        let timestamp = core.get(..19)?;
        chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S").ok()?;
        let ids = core.get(19..)?.strip_prefix('-')?;
        let (thread_id, rollout_id) = ids.split_once('_').unwrap_or((ids, ids));
        (!thread_id.is_empty() && !rollout_id.is_empty()).then_some(Self {
            timestamp,
            thread_id,
            rollout_id,
        })
    }

    /// Orders one thread's rollouts the way codex does when it has no SQLite
    /// pointer to ask: by the filename's timestamp, then — for files created in
    /// the same second — by the rollout id, a time-ordered UUIDv7.
    fn recency(&self) -> (&'a str, &'a str) {
        (self.timestamp, self.rollout_id)
    }
}

/// Where a rollout's inherited history lives: the index of its header, the
/// rollout id it continues, and the first ordinal NOT taken from that rollout.
/// `None` for a rollout that holds its whole history itself.
///
/// Only the rollout's OWN header counts — the first `session_meta`. Anything
/// after it is content, such as a parent header a legacy fork replayed inline.
///
/// `history_base` is the physical pointer, and the only one codex follows to
/// read a thread. Forks write it, and so does `thread/revert`, which is why it
/// names a ROLLOUT: a revert keeps the thread id and swaps the file, so the
/// pointer can name an earlier file of this very thread. A fork's
/// `forked_from_id` + `forked_from_ordinal_exclusive` is its logical lineage —
/// codex keeps it when the fork is reverted and `history_base` moves to the
/// fork's own earlier rollout — so it is read only when `history_base` is
/// absent. Its id is a thread id, which is also the rollout id of that thread's
/// original file.
fn inherited_history_pointer(lines: &[String]) -> Option<(usize, String, u64)> {
    let (header_idx, header) = lines
        .iter()
        .take(FORK_HEADER_SCAN_LINES)
        .enumerate()
        .find_map(|(idx, line)| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            (value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta"))
                .then_some((idx, value))
        })?;
    let payload = header.get("payload")?;
    let pointer = |id: Option<&serde_json::Value>, cut: Option<&serde_json::Value>| {
        let id = id?.as_str().map(str::trim).filter(|id| !id.is_empty())?;
        Some((header_idx, id.to_string(), cut?.as_u64()?))
    };
    let history_base = payload.get("history_base");
    pointer(
        history_base.and_then(|base| base.get("thread_id")),
        history_base.and_then(|base| base.get("end_ordinal_exclusive")),
    )
    .or_else(|| {
        pointer(
            payload.get("forked_from_id"),
            payload.get("forked_from_ordinal_exclusive"),
        )
    })
}

/// A rollout line's `ordinal`, the position codex assigns within a thread's
/// stream. `None` for older rollouts, which predate the field.
fn codex_line_ordinal(line: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("ordinal")?
        .as_u64()
}

/// Drop the parent history a sub-agent rollout opens with, keeping the child's
/// own header and everything it did itself.
///
/// A codex sub-agent runs as a full rollout of its own, but codex seeds the file
/// with however much of the parent's thread the spawn asked to carry
/// (`fork_turns`). Those records are the PARENT's, and at the record level they
/// are indistinguishable from the child's — reading the file whole is what puts
/// somebody else's conversation at the top of the child's transcript.
///
/// `subagent_history_start_ordinal` is codex's own declaration of where the seed
/// ends, so the cut is exact rather than inferred. It is REQUIRED: rollouts
/// without it (codex ≤ 0.147, `history_mode: "legacy"`) are returned untouched.
/// Guessing a boundary there would be a bad trade — the obvious candidate, the
/// first inter-agent message addressed to this child, also appears inside the
/// replayed prefix whenever the parent had already talked to an earlier
/// sub-agent of the same name.
///
/// Independent of the by-reference splice in `rollout_lines`, which runs it on
/// every rollout it reads before splicing them together, so the seed is dropped
/// from whichever file of a lineage holds it.
fn trim_subagent_replay_prefix(own: Vec<String>) -> Vec<String> {
    let Some((header_idx, cut)) = own
        .iter()
        .enumerate()
        .take_while(|(idx, _)| *idx < FORK_HEADER_SCAN_LINES)
        .find_map(|(idx, line)| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
                return None;
            }
            let payload = value.get("payload")?;
            // Both are required: the ordinal alone would let an ordinary thread
            // that happens to carry the field lose its opening records.
            codex_parent_thread_id(payload)?;
            let cut = payload
                .get("subagent_history_start_ordinal")
                .and_then(serde_json::Value::as_u64)?;
            Some((idx, cut))
        })
    else {
        return own;
    };

    // The seed is a PREFIX of the stream — `subagent_history_start_ordinal` is
    // an index into it, not a predicate — so scan for the boundary and keep the
    // rest verbatim. That costs one JSON parse per SEEDED record (ten or so in
    // practice) instead of one per line: the session viewer re-reads a running
    // child every couple of seconds, and these files run past a thousand lines.
    //
    // A record with no ordinal ends the scan too. It cannot be placed on either
    // side, and stopping there can only keep more than necessary — never drop
    // work the child did.
    let boundary = own
        .iter()
        .position(|line| codex_line_ordinal(line).is_none_or(|ord| ord >= cut))
        .unwrap_or(own.len());
    // Nothing was seeded ahead of the child's own stream (a `cut` of 0, or a
    // header that is already at or past it) — there is nothing to drop, and
    // splicing the header back in would duplicate it.
    if boundary <= header_idx {
        return own;
    }

    // The header declares the lineage the parser latches identity from, and
    // sits below the cut itself.
    let mut kept = Vec::with_capacity(own.len() - boundary + 1);
    kept.push(own[header_idx].clone());
    kept.extend(own.into_iter().skip(boundary));
    kept
}

impl Default for CodexParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexParser {
    pub fn new() -> Self {
        let base_dir = resolve_codex_home_dir().join("sessions");
        Self { base_dir }
    }

    /// Test-only constructor that lets callers point the parser at a fixture
    /// directory instead of `~/.codex/sessions`.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Every line of a rollout, with the history it inherits BY REFERENCE
    /// spliced in ahead of its own.
    ///
    /// Two things write a rollout that holds only part of its history plus a
    /// pointer to the rest (see `inherited_history_pointer`):
    ///
    /// * A fork. codex-acp 1.8.0's `session/fork` writes the child a rollout
    ///   that contains no history at all — just `session_meta` naming where it
    ///   forked from, then whatever the child does next. Read alone it parses to
    ///   zero turns, which is what put "this session has no messages" under
    ///   every `[Fork] …` row.
    /// * `thread/revert` (Codex Desktop's edit / retry). The thread keeps its
    ///   id but moves onto a NEW rollout that inherits the old one only up to
    ///   the reverted turn. Read alone it loses every turn before that; the old
    ///   file instead shows the turns the revert discarded and nothing after.
    ///
    /// Older forks are not like this: they REPLAY the parent inline (that is the
    /// second `session_meta` header `is_forked_thread_header` keys off) and so
    /// need no help. They carry no cut, which is what tells the shapes apart,
    /// and the ordinal filter makes that distinction self-enforcing rather than
    /// a bet: the inherited rollout contributes ordinals BELOW the cut and this
    /// one only its own at-or-above, so a child that did replay inline can't
    /// end up with the history twice. The same filter drops a revert's
    /// abandoned tail — the new rollout's ordinals resume AT the cut, so the old
    /// file's records past it are exactly the ones it replaced.
    ///
    /// Pointers chain — a fork of a fork, and every revert adds a rollout — so
    /// they are followed to the root. A chain ends early at a rollout it has
    /// already visited (a malformed cycle) or one codeg cannot find, which is
    /// not an error: the rollout may have been pruned, or live in a codex home
    /// this parser isn't pointed at. What was reached renders on its own rather
    /// than the conversation being refused. One that is there but fails to
    /// open is an error, as this rollout's own file would be: the summary cache
    /// keys on this rollout alone, so rendering what was reached would keep
    /// serving the truncated history after the failure cleared.
    ///
    /// The assembled order reproduces codex's own inline shape exactly — the
    /// child's header, then the inherited stream, then the child's body —
    /// because the parser latches `parent_id` from the FIRST header it sees and
    /// the child's is the one that declares the lineage.
    fn rollout_lines(&self, path: &Path) -> Result<Vec<String>, ParseError> {
        let read = |path: &Path| -> Result<Vec<String>, ParseError> {
            let own = BufReader::new(fs::File::open(path)?)
                .lines()
                .map_while(Result::ok)
                .collect();
            Ok(trim_subagent_replay_prefix(own))
        };

        let mut lines = read(path)?;
        // Newest first: every rollout that continues another, with its header's
        // index and the cut into the one it continues.
        let mut continuations = Vec::new();
        let mut visited: HashSet<String> = RolloutFileName::parse(path)
            .map(|name| name.rollout_id.to_string())
            .into_iter()
            .collect();
        while let Some((header_idx, base_id, cut)) = inherited_history_pointer(&lines) {
            if !visited.insert(base_id.clone()) {
                tracing::warn!(
                    rollout_id = %base_id,
                    "[codex] rollout history points back into itself; rendering what was reached"
                );
                break;
            }
            let Some(base_path) = self.find_rollout_by_rollout_id(&base_id) else {
                tracing::debug!(
                    rollout_id = %base_id,
                    "[codex] rollout inherits from a rollout with no file here"
                );
                break;
            };
            let base = read(&base_path)?;
            continuations.push((std::mem::replace(&mut lines, base), header_idx, cut));
        }
        if continuations.is_empty() {
            return Ok(lines);
        }

        // Oldest first from here, each record's ordinal read once however many
        // splices it passes through.
        let with_ordinals = |lines: Vec<String>| -> Vec<(Option<u64>, String)> {
            lines
                .into_iter()
                .map(|line| (codex_line_ordinal(&line), line))
                .collect()
        };
        let mut assembled = with_ordinals(lines);
        for (own, header_idx, cut) in continuations.into_iter().rev() {
            let mut own = with_ordinals(own);
            let header = own.remove(header_idx);
            let mut spliced = Vec::with_capacity(assembled.len() + own.len() + 1);
            spliced.push(header);
            spliced.extend(
                assembled
                    .into_iter()
                    .filter(|(ord, _)| ord.is_none_or(|ord| ord < cut)),
            );
            spliced.extend(
                own.into_iter()
                    .filter(|(ord, _)| ord.is_none_or(|ord| ord >= cut)),
            );
            assembled = spliced;
        }
        Ok(assembled.into_iter().map(|(_, line)| line).collect())
    }

    /// Every rollout file under the sessions root, in traversal order.
    fn rollout_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        WalkDir::new(&self.base_dir)
            .into_iter()
            .filter_map(Result::ok)
            .map(walkdir::DirEntry::into_path)
            .filter(|path| {
                path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .starts_with("rollout-")
            })
    }

    /// The rollout file a history pointer names. Codex embeds the id in the
    /// filename, so this never opens a file — but it matches the rollout id the
    /// name encodes, never a substring: a revert's new file carries the thread
    /// id too, and a pointer to the thread's original rollout must not resolve
    /// to the file holding the pointer.
    fn find_rollout_by_rollout_id(&self, rollout_id: &str) -> Option<PathBuf> {
        self.rollout_paths().find(|path| {
            RolloutFileName::parse(path).is_some_and(|name| name.rollout_id == rollout_id)
        })
    }

    /// The rollout a thread currently uses.
    ///
    /// Usually its only one. `thread/revert` moves a thread onto a new rollout
    /// under the same thread id and leaves the old file in place, so the id
    /// matches several files and traversal order would decide — on NTFS, whose
    /// listings are name-sorted, always in favour of the stale original. codex
    /// records the switch in its SQLite state; codeg reads only the rollouts, so
    /// this is codex's own rule for when that state is unavailable: the newest
    /// file.
    fn find_current_rollout(&self, thread_id: &str) -> Option<PathBuf> {
        self.rollout_paths()
            .filter_map(|path| {
                let name = RolloutFileName::parse(&path)?;
                let recency = (name.thread_id == thread_id)
                    .then(|| (name.timestamp.to_string(), name.rollout_id.to_string()))?;
                Some((recency, path))
            })
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, path)| path)
    }

    /// Load Codex's append-only session title index. The transcript remains the
    /// fallback source, so a missing/unreadable index or a malformed line is
    /// deliberately ignored. Later non-empty records for the same session win.
    pub(crate) fn load_thread_name_index(&self) -> HashMap<String, String> {
        let mut titles = HashMap::new();
        let Some(home_dir) = self.base_dir.parent() else {
            return titles;
        };
        let Ok(file) = fs::File::open(home_dir.join("session_index.jsonl")) else {
            return titles;
        };

        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let session_id = value.get("id").and_then(serde_json::Value::as_str);
            let thread_name = value
                .get("thread_name")
                .and_then(serde_json::Value::as_str)
                .and_then(codex_thread_title);
            if let (Some(id), Some(name)) = (session_id, thread_name) {
                titles.insert(id.to_string(), name);
            }
        }

        titles
    }

    fn parse_jsonl_summary(
        &self,
        path: &Path,
    ) -> Result<Option<ConversationSummary>, ParseError> {
        let lines = self.rollout_lines(path)?;

        let mut conversation_id: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut parent_id: Option<String> = None;
        let mut session_header_seen = false;
        let mut git_branch: Option<String> = None;
        let mut model: Option<String> = None;
        let mut title: Option<String> = None;
        let mut _cli_version: Option<String> = None;
        let mut first_timestamp: Option<DateTime<Utc>> = None;
        let mut last_timestamp: Option<DateTime<Utc>> = None;
        let mut message_count: u32 = 0;
        // Mirror the detail parser's leading-`/goal` fallback in the lightweight
        // list path: newer codex records `/goal` only as `thread_goal_updated` (no
        // `user_message`), so without this the sidebar/import entry is titleless
        // and under-counted. Decide it POSITIONALLY, exactly like the detail
        // parser: the goal is the opener iff no real user turn preceded it, and it
        // then supplies the title + one synthetic-turn count even when a LATER real
        // reply (e.g. "确认") exists. `has_real_user` tracks the same real-user-turn
        // sources detail uses (an `event_msg.user_message`, or an image-bearing
        // `response_item` user); `goal_objective` latches the first opening goal;
        // `goal_opens_session` snapshots whether it opened the session.
        let mut has_real_user = false;
        let mut goal_objective: Option<String> = None;
        let mut goal_opens_session = false;
        // Mirror of the detail parser's `response_item.message` promotion — see
        // [`ResponseItemPromotion`]. The two must agree or the sidebar entry and
        // the opened conversation disagree about count and title. Only the
        // per-candidate PAYLOAD differs (the list path needs no blocks): here it
        // is `(is_user, title_candidate)`, kept parallel to the shared tracker's
        // own candidate vector.
        let mut promotion = ResponseItemPromotion::new();
        let mut pending_promotions: Vec<(bool, Option<String>)> = Vec::new();
        let mut first_goal_ordinal: Option<u64> = None;
        let mut title_source_ordinal: Option<u64> = None;
        let mut title_from_thread_name = false;
        // Cross-channel dedup, mirroring the detail parser's
        // `should_skip_duplicate_user_message`: codex writes the same prompt
        // through BOTH `event_msg` and `response_item`, so counting each one
        // would put the sidebar's message count one ahead of the turns the
        // opened conversation actually renders.
        let mut recent_user_records: Vec<(DateTime<Utc>, UserTurnFingerprint)> = Vec::new();
        // Plan mode, mirroring the detail parser so the sidebar count tracks the
        // turns the opened conversation renders: a plan document counts once
        // (whichever of its two copies is seen first), and the synthetic
        // approval prompt counts not at all. Pairing slot and approval guard are
        // separate for the same reason they are there — see that parser.
        let mut collaboration_mode_is_plan = false;
        let mut plan_approval_expected = false;
        let mut pending_plan_twin: Option<String> = None;
        let mut plan_counted = false;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let msg_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

            let record_ordinal =
                promotion.note_record(msg_type, promotion_payload_type(msg_type, &value));

            if let Some(ts_str) = value.get("timestamp").and_then(|t| t.as_str()) {
                if let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                    if first_timestamp.is_none() {
                        first_timestamp = Some(ts);
                    }
                    last_timestamp = Some(ts);
                }
            }

            match msg_type {
                "session_meta" => {
                    // The FIRST header is this thread's own; every later one
                    // belongs to a parent. Two shapes put a second header in
                    // this stream: an inline-replay fork writes the parent's
                    // header into its own file, and a by-reference fork gets
                    // the parent's lines spliced in by `rollout_lines`. In both
                    // the parent header carries the PARENT's `id`, `cwd` and
                    // branch, so a last-one-wins read would file the child's
                    // whole summary under the parent's session id — two rollouts
                    // claiming one id, which the conversation-list dedup then
                    // collapses, losing the fork. Latch every identity field,
                    // not just `parent_id`. Same rule `parse_codex_subagent_stats`
                    // uses.
                    if let Some(payload) = value.get("payload").filter(|_| !session_header_seen) {
                        session_header_seen = true;
                        conversation_id = payload
                            .get("id")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        cwd = payload
                            .get("cwd")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        parent_id = codex_parent_thread_id(payload);
                        _cli_version = payload
                            .get("cli_version")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        git_branch = payload
                            .get("git")
                            .and_then(|g| g.get("branch"))
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string());
                    }
                }
                "turn_context" => {
                    if model.is_none() {
                        model = value
                            .get("payload")
                            .and_then(|p| p.get("model"))
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                    }
                    // Arm the plan-approval filter — see the detail parser's
                    // `turn_context` arm for why the mode flip is the signal and
                    // why a repeated context must not disarm.
                    if let Some(mode) = turn_collaboration_mode(&value) {
                        let is_plan = mode == "plan";
                        if is_plan {
                            plan_approval_expected = false;
                        } else if collaboration_mode_is_plan {
                            plan_approval_expected = true;
                        }
                        collaboration_mode_is_plan = is_plan;
                    }
                }
                "event_msg" => {
                    if let Some(payload) = value.get("payload") {
                        let payload_type =
                            payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match payload_type {
                            "user_message" => {
                                let raw_text = payload
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");
                                // codex's own post-approval prompt — suppressed in
                                // the detail parser, so it must not be counted here
                                // either. Both signals required, and the wording
                                // compared verbatim; see that arm. Consumed as the
                                // FIRST thing this arm does, matching the detail
                                // parser: the arm names the next user record, so a
                                // record that later `continue`s must still spend it.
                                let approval_armed = std::mem::take(&mut plan_approval_expected);
                                if approval_armed
                                    && raw_text == CODEX_PLAN_APPROVAL_PROMPT
                                    && std::mem::take(&mut plan_counted)
                                {
                                    continue;
                                }

                                let visible_text =
                                    strip_internal_agent_routes(&decode_user_text(raw_text));
                                let has_images = payload
                                    .get("images")
                                    .and_then(|v| v.as_array())
                                    .is_some_and(|images| !images.is_empty());
                                if contains_only_internal_agent_routes(raw_text) && !has_images {
                                    continue;
                                }
                                if skip_duplicate_user_record(
                                    &mut recent_user_records,
                                    UserTurnFingerprint::from_event_message(payload),
                                    parse_codex_timestamp(&value).unwrap_or_else(Utc::now),
                                ) {
                                    continue;
                                }
                                message_count += 1;
                                has_real_user = true;
                                if title.is_none() {
                                    title = extract_codex_title_candidate(&visible_text, true);
                                    if title.is_some() {
                                        title_source_ordinal = Some(record_ordinal);
                                    }
                                }
                            }
                            "agent_message" => {
                                message_count += 1;
                            }
                            "item_completed" => {
                                // A Plan turn publishes its answer here instead of
                                // on `agent_message`, so it counts like one. The
                                // body is remembered so this plan's assistant
                                // `response_item` copy does not count a second time.
                                if let Some(plan) = completed_plan_item_text(payload) {
                                    message_count += 1;
                                    pending_plan_twin = Some(plan.to_string());
                                    plan_counted = true;
                                }
                            }
                            "thread_goal_updated" => {
                                // Capture the first OPENING goal for the fallback,
                                // through the SAME shared mapping the detail parser
                                // uses — so the summary keys off exactly the objective
                                // the detail parser would synthesize from: only a
                                // `create_goal` (an active goal with an objective),
                                // never a `goal:null` clear, a blank objective, or a
                                // terminal-status goal.
                                if goal_objective.is_none() {
                                    if let Some(marker) = payload
                                        .get("goal")
                                        .and_then(crate::acp::codex_goal::goal_marker)
                                    {
                                        if marker.tool_name == "create_goal" {
                                            // Positional, mirroring the detail parser:
                                            // the goal opened the session iff no real
                                            // user turn preceded it. Claim the title
                                            // from the objective HERE, in stream order,
                                            // so a later `user_message` can't steal it
                                            // while a native `thread_name_updated` still
                                            // overrides it.
                                            goal_opens_session = !has_real_user;
                                            if goal_opens_session && title.is_none() {
                                                title = extract_codex_title_candidate(
                                                    &marker.objective,
                                                    true,
                                                );
                                                if title.is_some() {
                                                    title_source_ordinal = Some(record_ordinal);
                                                }
                                            }
                                            goal_objective = Some(marker.objective);
                                            first_goal_ordinal = Some(record_ordinal);
                                        }
                                    }
                                }
                            }
                            "thread_name_updated" => {
                                // Codex native thread name — newest non-empty wins
                                // (parity with the detail parser). Accept both the
                                // rollout `thread_name` and the live `threadName`.
                                if let Some(name) = payload
                                    .get("thread_name")
                                    .or_else(|| payload.get("threadName"))
                                    .or_else(|| payload.get("name"))
                                    .and_then(|n| n.as_str())
                                    .and_then(codex_thread_title)
                                {
                                    title = Some(name);
                                    title_from_thread_name = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "response_item" => {
                    if let Some(payload) = value.get("payload") {
                        let payload_type =
                            payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if payload_type == "message" {
                            let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                            // The detail parser turns an IMAGE-bearing
                            // `response_item` user into a real user turn
                            // (`extract_response_item_user_image_blocks`) and titles
                            // from that same turn — unconditionally, because the
                            // canonical channel never carries the image. Everything
                            // else goes through the same held-back promotion the
                            // detail parser uses, so the two stay in exact sync on
                            // both the count and the pure-`/goal` fallback.
                            if role == "user" && response_item_user_has_image(payload) {
                                // Same dedup the detail parser applies before
                                // pushing this turn — without it a prompt codex
                                // wrote to BOTH channels counts twice here and
                                // once there.
                                if skip_duplicate_user_record(
                                    &mut recent_user_records,
                                    UserTurnFingerprint::from_response_item(payload),
                                    parse_codex_timestamp(&value).unwrap_or_else(Utc::now),
                                ) {
                                    continue;
                                }
                                message_count += 1;
                                has_real_user = true;
                                if title.is_none() {
                                    title = extract_codex_text_content(payload).and_then(|t| {
                                        extract_codex_title_candidate(&decode_user_text(&t), false)
                                    });
                                    if title.is_some() {
                                        title_source_ordinal = Some(record_ordinal);
                                    }
                                }
                            } else if let Some(is_user) = match role {
                                "user" => Some(true),
                                "assistant" => Some(false),
                                _ => None,
                            } {
                                if let Some(blocks) =
                                    extract_response_item_message_blocks(payload, is_user)
                                {
                                    let text = first_text_block(&blocks).unwrap_or_default();

                                    // Plan document, counted outside the promotion
                                    // gate exactly as the detail parser renders it
                                    // outside that gate — once per plan, no matter
                                    // which of its two copies the rollout carries.
                                    // The pairing slot is consumed either way, so
                                    // two plan turns proposing the same body still
                                    // count twice.
                                    if !is_user {
                                        if let Some(body) = proposed_plan_body(&text) {
                                            let is_twin = pending_plan_twin
                                                .take()
                                                .is_some_and(|seen| seen == body);
                                            if !is_twin {
                                                message_count += 1;
                                            }
                                            plan_counted = true;
                                            continue;
                                        }
                                    }

                                    let promotable = if text.trim().is_empty() {
                                        true
                                    } else if is_user {
                                        is_promotable_user_text(&text)
                                    } else {
                                        is_promotable_assistant_text(&text)
                                    };
                                    if promotable {
                                        promotion.push_candidate(record_ordinal, is_user, &text);
                                        pending_promotions.push((
                                            is_user,
                                            is_user
                                                .then(|| {
                                                    extract_codex_title_candidate(&text, true)
                                                })
                                                .flatten(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                // The compaction's handoff summary is no message — the same
                // denial the detail parser applies before drawing its divider.
                "compacted" => {
                    let summary = codex_compacted_summary(value.get("payload"));
                    promotion.deny_compaction_summary(summary);
                }
                _ => {}
            }
        }

        // Promote the held-back `response_item.message` records — see the twin
        // block in `parse_conversation_detail`, whose `turns.len()` this count
        // has to track.
        let survivors = promotion.resolve();
        let promoted_user_ordinal = promotion.first_surviving_user_ordinal(&survivors);
        message_count += survivors.iter().filter(|keep| **keep).count() as u32;

        if let (Some(goal_ordinal), Some(user_ordinal)) =
            (first_goal_ordinal, promoted_user_ordinal)
        {
            if user_ordinal < goal_ordinal {
                goal_opens_session = false;
            }
        }

        // NOTE: `has_real_user` deliberately is NOT back-filled here. Its only
        // consumer is the in-loop `goal_opens_session = !has_real_user`, whose
        // outcome the ordinal comparison above already corrects; assigning it
        // post-loop would be dead code that reads as if it did something.

        if !title_from_thread_name {
            if let Some(user_ordinal) = promoted_user_ordinal {
                let promoted_title = pending_promotions
                    .into_iter()
                    .zip(&survivors)
                    .find(|((is_user, _), keep)| **keep && *is_user)
                    .and_then(|((_, candidate), _)| candidate);
                if let Some(candidate) = promoted_title {
                    if title.is_none()
                        || title_source_ordinal.is_none_or(|current| user_ordinal < current)
                    {
                        title = Some(candidate);
                    }
                }
            }
        }

        let started_at = match first_timestamp {
            Some(ts) => ts,
            None => return Ok(None),
        };

        // Leading-`/goal` fallback, positional and mirroring the detail parser:
        // when a `/goal` opened the session (before any real user turn), the detail
        // view synthesizes a leading user message from the objective — so count
        // that one turn here, even when a LATER real reply exists, keeping the list
        // entry in sync with the opened conversation. The title was already claimed
        // in-loop (see the goal arm).
        if goal_opens_session {
            message_count += 1;
        }

        let id = conversation_id.unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

        let folder_path = cwd.clone();
        let folder_name = folder_path.as_ref().map(|p| folder_name_from_path(p));

        Ok(Some(ConversationSummary {
            id,
            agent_type: AgentType::Codex,
            folder_path,
            folder_name,
            title,
            started_at,
            ended_at: last_timestamp,
            message_count,
            model,
            git_branch,
            parent_id,
            parent_tool_use_id: None,
            delegation_call_id: None,
        }))
    }
}

pub(crate) fn resolve_codex_home_dir() -> PathBuf {
    resolve_codex_home_dir_from(std::env::var_os("CODEX_HOME"), dirs::home_dir())
}

fn resolve_codex_home_dir_from(
    codex_home_env: Option<std::ffi::OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    codex_home_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir.unwrap_or_default().join(".codex"))
}

impl AgentParser for CodexParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut conversations = Vec::new();

        if !self.base_dir.exists() {
            return Ok(conversations);
        }

        // Apply this outside `summary_cache`: changing only session_index.jsonl
        // must refresh a title even when the rollout itself is unchanged.
        let indexed_titles = self.load_thread_name_index();

        let paths: Vec<PathBuf> = self.rollout_paths().collect();
        let names: Vec<Option<RolloutFileName>> = paths
            .iter()
            .map(|path| RolloutFileName::parse(path))
            .collect();
        // A reverted thread's earlier rollouts stay on disk under its id, but
        // they are prefixes its current one inherits (see `rollout_lines`), not
        // conversations of their own.
        let mut current: HashMap<&str, (&str, &str)> = HashMap::new();
        for name in names.iter().flatten() {
            current
                .entry(name.thread_id)
                .and_modify(|newest| *newest = (*newest).max(name.recency()))
                .or_insert(name.recency());
        }

        for (path, name) in paths.iter().zip(&names) {
            if name.is_some_and(|name| current.get(name.thread_id) != Some(&name.recency())) {
                continue;
            }

            match super::summary_cache::get_or_parse(AgentType::Codex, path, || {
                self.parse_jsonl_summary(path)
            }) {
                Ok(Some(mut summary)) => {
                    if let Some(title) = indexed_titles.get(&summary.id) {
                        summary.title = Some(title.clone());
                    }
                    conversations.push(summary);
                }
                _ => continue,
            }
        }

        conversations.sort_by_key(|b| std::cmp::Reverse(b.started_at));
        Ok(conversations)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        if !self.base_dir.exists() {
            return Err(ParseError::ConversationNotFound(
                conversation_id.to_string(),
            ));
        }

        // A conversation id is a thread id, read from the rollout the thread
        // currently uses. One that names no rollout's thread — a summary that
        // fell back to its file stem — still finds its file by name.
        let path = self.find_current_rollout(conversation_id).or_else(|| {
            WalkDir::new(&self.base_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .map(walkdir::DirEntry::into_path)
                .find(|path| {
                    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                        && path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .contains(conversation_id)
                })
        });
        let Some(path) = path else {
            return Err(ParseError::ConversationNotFound(
                conversation_id.to_string(),
            ));
        };

        let mut detail = self.parse_conversation_detail(&path, conversation_id)?;
        if let Some(title) = self.load_thread_name_index().get(conversation_id) {
            detail.summary.title = Some(title.clone());
        }
        Ok(detail)
    }
}

fn parse_codex_json_arg(payload: &serde_json::Value) -> Option<serde_json::Value> {
    let args = payload.get("arguments").or_else(|| payload.get("input"))?;
    if let Some(s) = args.as_str() {
        serde_json::from_str(s).ok()
    } else if args.is_object() || args.is_array() {
        Some(args.clone())
    } else {
        None
    }
}

fn parse_codex_json_output(payload: &serde_json::Value) -> Option<serde_json::Value> {
    let output = payload.get("output")?;
    if let Some(s) = output.as_str() {
        serde_json::from_str(s).ok()
    } else if output.is_object() || output.is_array() {
        Some(output.clone())
    } else {
        None
    }
}

/// A `tools.exec_command()` result the script printed verbatim
/// (`text(JSON.stringify(r))`) — the object form of the very envelope the
/// string path spells out as `Chunk ID: …` / `Wall time: …` / `Output:`.
/// Reduce it to the output it wraps so the card reads like the terminal it is:
/// 9% of code-mode chunks are this shape, and the ones announcing a background
/// session carry an empty `output`, so today those cards show nothing but the
/// envelope. Callers must read whatever they need from the raw text (the
/// session announcement lives in `session_id`) before cleaning.
fn unwrap_exec_chunk_envelope(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let obj = value.as_object()?;
    // The whole shape, not just `chunk_id`: a command whose own stdout is JSON
    // carrying those two keys would otherwise be truncated to its `output`
    // field, silently dropping the rest. Every envelope in real transcripts
    // carries all four.
    if !ENVELOPE_KEYS.iter().all(|key| obj.contains_key(*key)) {
        return None;
    }
    Some(obj.get("output")?.as_str()?.to_string())
}

/// Keys every `tools.exec_command()` result object carries, in both the
/// `exit_code` (finished) and `session_id` (still running) variants.
const ENVELOPE_KEYS: [&str; 4] = [
    "chunk_id",
    "wall_time_seconds",
    "original_token_count",
    "output",
];

/// The command line and body of a codex exec envelope, or `None` when the text
/// carries neither — i.e. when there is no envelope to strip. Distinct from
/// `clean_codex_exec_output` in exactly one case that matters to a background
/// session: an envelope whose body is empty answers `Some("")` (that poll
/// collected nothing) rather than falling back to the header text.
fn split_codex_exec_output(text: &str) -> Option<String> {
    let mut cmd_line: Option<&str> = None;
    let mut in_output = false;
    let mut output_lines = Vec::new();

    for line in text.lines() {
        if cmd_line.is_none() && line.starts_with("$ ") {
            cmd_line = Some(line);
            continue;
        }
        if line.trim_end() == "Output:" {
            in_output = true;
            continue;
        }
        if in_output {
            output_lines.push(line);
        }
    }

    if cmd_line.is_none() && !in_output {
        return None;
    }

    let mut result = String::new();
    if let Some(cmd) = cmd_line {
        result.push_str(cmd);
    }
    if !output_lines.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&output_lines.join("\n"));
    }
    Some(result)
}

fn clean_codex_exec_output(text: &str) -> String {
    split_codex_exec_output(text)
        .filter(|cleaned| !cleaned.is_empty())
        .unwrap_or_else(|| text.to_string())
}

/// Strip whichever exec envelope `text` is wrapped in — the object form or the
/// `Chunk ID:` / `Wall time:` / `Output:` header — or `None` when it is wrapped
/// in neither. The header form is only stripped when the FIRST line announces
/// it: an `Output:` line in the middle of a build log is a build log, not an
/// envelope, and stripping on that alone would swallow everything above it.
fn strip_exec_envelope(text: &str) -> Option<String> {
    if let Some(unwrapped) = unwrap_exec_chunk_envelope(text) {
        return Some(unwrapped);
    }
    let first = text.lines().next()?.trim_start().to_ascii_lowercase();
    let announced = ["chunk id:", "wall time", "process ", "script "]
        .iter()
        .any(|prefix| first.starts_with(prefix));
    if !announced {
        return None;
    }
    split_codex_exec_output(text)
}

/// Banner codex puts in front of a script's `text()` stream when it re-rendered
/// the whole stream as one blob instead of passing the parts through.
const TRUNCATION_WARNING: &str = "Warning: truncated output";
const TOTAL_OUTPUT_LINES: &str = "Total output lines: ";

/// The per-`text()` chunks hiding inside a collapsed output blob, or `None` when
/// recovering them is not provable.
///
/// Codex re-renders a long script's whole `text()` stream as ONE chunk behind a
/// `Warning: truncated output (original token count: N)` / `Total output lines:
/// M` banner. The boundaries survive in it — one physical line per `text()` —
/// so the per-call split is still there to be had, and without it a
/// `Promise.all` of four commands shows up as a single card whose body is four
/// lines of raw envelope JSON.
///
/// Recovering it is only sound while all M lines are present: past the cap codex
/// drops whole lines, and it drops them from the MIDDLE, so the survivors'
/// positions stop naming their calls. Measured over the newest 400 rollouts: with
/// every line present, line *i* is call *i* in 36 of 36 checkable cases; with
/// lines dropped, 13 of 57 land on the wrong command. So all three counts have to
/// agree — declared, present, and calls — and every line has to be a complete
/// exec envelope, which is also what tells this blob apart from a single
/// command's stdout that merely got truncated (the far more common banner).
fn uncollapse_truncated_chunks(blob: Option<&TruncatedBlob>, calls: usize) -> Option<Vec<String>> {
    if calls == 0 {
        return None;
    }
    let blob = blob?;
    if blob.body.len() != blob.declared || blob.declared != calls {
        return None;
    }
    blob.body
        .iter()
        .all(|line| unwrap_exec_chunk_envelope(line).is_some())
        .then(|| blob.body.iter().map(|line| line.to_string()).collect())
}

/// The line count a collapsed blob declares, and the lines it actually kept.
///
/// Reading it costs a pass over the whole blob, and three of the paths below
/// want it, so `unwrap_code_mode_script` reads it once and hands it down.
struct TruncatedBlob<'a> {
    declared: usize,
    body: Vec<&'a str>,
}

impl TruncatedBlob<'_> {
    /// Codex dropped lines from the middle to fit the cap.
    fn truncated(&self) -> bool {
        self.declared > self.body.len()
    }
}

fn truncated_blob(parsed: &CodeModeOutput) -> Option<TruncatedBlob<'_>> {
    let [collapsed] = parsed.chunks.as_slice() else {
        return None;
    };
    let mut lines = collapsed.lines();
    if !lines.next()?.starts_with(TRUNCATION_WARNING) {
        return None;
    }
    let declared: usize = lines
        .next()?
        .strip_prefix(TOTAL_OUTPUT_LINES)?
        .trim()
        .parse()
        .ok()?;
    if !lines.next()?.is_empty() {
        return None;
    }
    Some(TruncatedBlob {
        declared,
        body: lines.collect(),
    })
}

/// One call's share of a collapsed blob that was split on the separator lines
/// the script printed.
struct SeparatorSlot {
    /// The lines this call owns. `None` means it printed nothing between its
    /// separator and the next — which is not the same as having no separator,
    /// hence `placed`.
    text: Option<String>,
    /// Whether a span of the blob could be attributed to this call at all. False
    /// when truncation removed its separator, leaving nothing to cut on.
    placed: bool,
    /// Calls whose separators were truncated away, leaving their output inside
    /// this slot with no boundary to cut on. Empty in the normal case.
    shares_with: Vec<usize>,
}

/// At least this many separators have to be found, so a split rests on at least
/// one proven boundary. One anchor divides nothing — it would hand the whole
/// blob to the first call and leave the rest empty.
const MIN_ANCHORS: usize = 2;

/// The blob split on the separator lines the script printed before each result,
/// or `None` when it cannot be read that way.
///
/// This is the only handle left once codex re-renders a long `text()` stream as
/// one truncated blob: 89% of collapsed blobs are missing lines, so the counts
/// `uncollapse_truncated_chunks` compares never agree. But scripts of this shape
/// label their results — ``text(`===== ${k} =====\n${r.output}`)`` — and the
/// labels come from the same literal table the commands do. So the separator
/// each call printed can be *predicted* from the source and then looked up in
/// the output, which is a far stronger check than counting: a wrong prediction
/// does not match and the split is simply refused.
///
/// Truncation cannot reorder — it only deletes — so lines between two surviving
/// separators belong to the earlier one and nothing else. When the separator
/// *between* them was dropped, the span covers several calls with no way to
/// tell where each begins; that span stays whole, attributed to the call it
/// provably starts with, and the calls sharing it are named in `shares_with`
/// rather than silently given someone else's output.
///
/// One residual, deliberately accepted: a matched line is only *probably* the
/// separator the script printed. A command whose own stdout contains a line
/// identical to another row's separator — printing this very transcript, say —
/// would be cut there, and the tail of its output would be filed under that
/// row. Uniqueness makes this impossible whenever every separator survived (a
/// look-alike would be a second match and reject the candidate), so the risk
/// only exists in the truncated case, and only when the look-alike stands in
/// for a separator that is genuinely gone. Refusing truncated blobs would avoid
/// it at the cost of the case this exists for — 89% of collapsed blobs are
/// missing lines — so the split is kept and the labels are what it rests on.
fn split_by_separators(
    blob: Option<&TruncatedBlob>,
    calls: &[CodeModeCall],
    separators: &[Separator],
) -> Option<Vec<SeparatorSlot>> {
    if calls.len() < 2 || separators.is_empty() {
        return None;
    }
    let body = &blob?.body;

    let labels: Option<Vec<&str>> = calls.iter().map(|c| c.label.as_deref()).collect();
    let by_index = |offset: usize| (0..calls.len()).map(|i| (i + offset).to_string()).collect();
    // `---RESULT ${i+1}---` is as common as a label in this corpus, and costs
    // nothing to try: the output either contains the line or it does not.
    let candidates: Vec<Vec<String>> = labels
        .map(|names| names.iter().map(|n| n.to_string()).collect())
        .into_iter()
        .chain([by_index(0), by_index(1)])
        .collect();

    let index = index_body_lines(body);
    let mut best: Option<Vec<Option<usize>>> = None;
    for separator in separators {
        for values in &candidates {
            let Some(found) = locate_anchors(&index, separator, values) else {
                continue;
            };
            let hits = found.iter().flatten().count();
            if hits < MIN_ANCHORS {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|b| hits > b.iter().flatten().count())
            {
                best = Some(found);
            }
        }
    }

    Some(slots_from_anchors(body, &best?))
}

/// Every line of the blob, trimmed of trailing whitespace, mapped to where it
/// sits — or to `None` when it occurs more than once.
///
/// Built once and read by every candidate, which is what keeps the split from
/// costing a pass over the blob per command: a run of separators is looked up,
/// not searched for.
fn index_body_lines<'a>(body: &[&'a str]) -> HashMap<&'a str, Option<usize>> {
    let mut index = HashMap::with_capacity(body.len());
    for (at, line) in body.iter().enumerate() {
        index
            .entry(line.trim_end())
            .and_modify(|seen| *seen = None)
            .or_insert(Some(at));
    }
    index
}

/// Where each value's separator line sits in the blob, or `None` for the ones
/// truncation removed. Refuses the whole candidate when a line repeats or the
/// lines run backwards — either means the match is not the separator run.
fn locate_anchors(
    index: &HashMap<&str, Option<usize>>,
    separator: &Separator,
    values: &[String],
) -> Option<Vec<Option<usize>>> {
    let mut found = Vec::with_capacity(values.len());
    let mut previous = None;

    for value in values {
        let anchor = separator.anchor(value);
        let hit = match index.get(anchor.trim_end()) {
            // The line occurs twice, so it is not a boundary to cut on. Only a
            // line some value actually predicts can refuse a candidate; an
            // ordinary output line repeating is nobody's separator.
            Some(None) => return None,
            Some(Some(at)) => Some(*at),
            None => None,
        };
        if let Some(at) = hit {
            if previous.is_some_and(|before| at <= before) {
                return None;
            }
            previous = Some(at);
        }
        found.push(hit);
    }

    Some(found)
}

fn slots_from_anchors(body: &[&str], found: &[Option<usize>]) -> Vec<SeparatorSlot> {
    // A call whose separator survived owns the lines after it. The first call
    // owns the head of the blob even without one: deletion never moves a line,
    // so nothing can precede the first call's output.
    let mut owners: Vec<(usize, Option<usize>)> = Vec::new();
    if found.first().is_some_and(Option::is_none) {
        owners.push((0, None));
    }
    owners.extend(
        found
            .iter()
            .enumerate()
            .filter_map(|(call, at)| at.map(|at| (call, Some(at)))),
    );

    let mut slots: Vec<SeparatorSlot> = (0..found.len())
        .map(|_| SeparatorSlot {
            text: None,
            placed: false,
            shares_with: Vec::new(),
        })
        .collect();

    for (position, (call, anchor)) in owners.iter().enumerate() {
        let start = anchor.map_or(0, |at| at + 1);
        let (end, next_call) = match owners.get(position + 1) {
            // The next owner's separator line is not part of this slot.
            Some((next_call, Some(at))) => (*at, *next_call),
            _ => (body.len(), found.len()),
        };
        let mut text = body.get(start..end).unwrap_or_default().join("\n");
        let kept = text.trim_end().len();
        text.truncate(kept);
        slots[*call] = SeparatorSlot {
            text: (!text.is_empty()).then_some(text),
            placed: true,
            shares_with: (call + 1..next_call).collect(),
        };
    }

    slots
}

/// `text` trimmed with every run of digits collapsed to `#`, so `---RESULT 1---`
/// and `---RESULT 2---` compare equal while `--- dto diff ---` and
/// `--- handler diff ---` do not.
fn digit_blind(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut digits = false;
    for ch in text.trim().chars() {
        if ch.is_ascii_digit() {
            if !digits {
                out.push('#');
                digits = true;
            }
        } else {
            digits = false;
            out.push(ch);
        }
    }
    out
}

/// How many `text()` chunks each call contributed — 1 for the plain
/// `results.forEach(r => text(r.output))` shape — or `None` when the chunks do
/// not provably divide into one group per call.
///
/// Scripts routinely print more than one chunk per call: a `---RESULT n---`
/// header before the output, an `exit_code=0` after it. 268 of the 1297
/// multi-call scripts that fail the 1:1 count in the newest 400 rollouts are
/// exactly this, and they render as one undecomposable script card today.
///
/// The count alone proves nothing — a script that prints every output first and
/// every exit code after hands over the same chunks in the wrong order, and
/// slicing those into pairs would pin each command's output to the previous
/// command's card. The proof is `text_run` (see `CodeModeScript`): exactly
/// `stride` `text()` calls with no loop between the first and the last, so that
/// run emitted its chunks together, `calls` times over. A repeating slot is kept
/// as corroboration — it is only evidence, since a command's own stdout can be
/// `exit_code=0` — and costs 13 of 256 candidates, which stay script cards.
///
/// Measured with commands whose output names its own call (`nl -ba F | sed -n
/// 'A,Bp'` must start at line A): 152 of 153 groups land on the right command,
/// and the one exception is the check's own blind spot — that `nl` failed and
/// printed a usage error, so there was no line number to match.
fn call_chunk_stride(chunks: &[String], calls: usize, text_run: Option<usize>) -> Option<usize> {
    if calls == 0 || chunks.is_empty() || !chunks.len().is_multiple_of(calls) {
        return None;
    }
    let stride = chunks.len() / calls;
    if stride == 1 {
        return Some(1);
    }
    if text_run != Some(stride) {
        return None;
    }
    (0..stride)
        .any(|slot| {
            let marker = digit_blind(&chunks[slot]);
            !marker.is_empty()
                && (1..calls).all(|index| digit_blind(&chunks[index * stride + slot]) == marker)
        })
        .then_some(stride)
}

/// Turn one code-mode script + its output envelope into the tool blocks the
/// pre-code-mode history path produced.
///
/// Returns `(replacement tool_use blocks, tool_result blocks)`. The first is
/// `None` when the script could not be decomposed — the placeholder script card
/// stays and simply receives the whole output.
///
/// Splitting per call needs the chunks to divide into one group per call, in
/// order — see `call_chunk_stride` for what counts as proof. Anything less falls
/// back to the script card rather than misattributing output, including the
/// collapsed blob codex renders in place of the chunks when the stream is long,
/// unless `uncollapse_truncated_chunks` can prove the chunks back out of it.
fn unwrap_code_mode_script(
    call_id: &str,
    script: &CodeModeScript,
    parsed: &CodeModeOutput,
    payload: &serde_json::Value,
    sessions: &mut HashMap<String, ShellSession>,
    poll_origins: &mut HashMap<String, String>,
) -> (Option<Vec<ContentBlock>>, Vec<ContentBlock>) {
    let calls = script.calls.as_deref().unwrap_or_default();

    let result_block = |tool_use_id: Option<String>, text: Option<String>, note: Option<&str>| {
        let output_preview = with_note(text, note);
        let is_error = parsed.is_error()
            || infer_tool_call_output_is_error(payload, None, output_preview.as_deref());
        ContentBlock::ToolResult {
            tool_use_id,
            output_preview,
            is_error,
            agent_stats: None,
            images: Vec::new(),
        }
    };

    let non_empty = |text: String| {
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    };

    // NB: the envelope note (`Script running with cell ID N`) is deliberately
    // NOT registered as a shell session here. That cell is the SCRIPT — it has
    // not finished — so the `wait` collecting it carries this very script's
    // return value and is folded back onto these cards by the caller
    // (`DeferredScript`), never rendered as a session of its own.

    // A collapsed blob stands in for the chunks it was rendered from, when the
    // counts prove which is which. Nothing changes when it does not: the
    // fallback is `parsed.chunks` itself, banner and all.
    // Every path that reads it bails on the call count first, so a script that
    // recovered nothing does not pay to have its blob split into lines.
    let blob = (!calls.is_empty())
        .then(|| truncated_blob(parsed))
        .flatten();
    let uncollapsed = uncollapse_truncated_chunks(blob.as_ref(), calls.len());
    let chunks: &[String] = uncollapsed.as_deref().unwrap_or(&parsed.chunks);

    if calls.len() == 1 {
        let call = &calls[0];
        let joined = chunks.join("\n");
        let uses = vec![ContentBlock::ToolUse {
            tool_use_id: Some(call_id.to_string()),
            tool_name: call.tool_name.clone(),
            input_preview: Some(shell_session_input_preview(
                call,
                call_id,
                sessions,
                poll_origins,
            )),
            status: None,
            meta: codex_script_meta(call.label.as_deref(), false, &[], false),
        }];
        register_shell_sessions(call, call_id, &joined, sessions);
        let results = vec![result_block(
            Some(call_id.to_string()),
            non_empty(exec_chunk_display(call, joined)),
            parsed.note.as_deref(),
        )];
        return (Some(uses), results);
    }

    if let Some(stride) = (calls.len() > 1)
        .then(|| call_chunk_stride(chunks, calls.len(), script.text_run))
        .flatten()
    {
        let last = calls.len() - 1;
        let mut uses = Vec::with_capacity(calls.len());
        let mut results = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let id = format!("{call_id}#{index}");
            let note = if index == last {
                parsed.note.as_deref()
            } else {
                None
            };
            let group = &chunks[index * stride..(index + 1) * stride];
            uses.push(ContentBlock::ToolUse {
                tool_use_id: Some(id.clone()),
                tool_name: call.tool_name.clone(),
                input_preview: Some(shell_session_input_preview(
                    call,
                    &id,
                    sessions,
                    poll_origins,
                )),
                status: None,
                meta: codex_script_meta(call.label.as_deref(), false, &[], false),
            });
            // Announcements are read from the chunks as written: unwrapping an
            // envelope drops the `session_id` that names the session.
            register_shell_sessions(call, &id, &group.join("\n"), sessions);
            let shown = group
                .iter()
                .map(|chunk| exec_chunk_display(call, chunk.clone()))
                .collect::<Vec<_>>()
                .join("\n");
            results.push(result_block(Some(id), non_empty(shown), note));
        }
        return (Some(uses), results);
    }

    // The chunks did not divide, but the script may have labelled its results
    // on their way out. Only reachable for a collapsed blob — anything with real
    // chunks was already handled above.
    if let Some(slots) = split_by_separators(blob.as_ref(), calls, &script.separators) {
        let truncated = blob.as_ref().is_some_and(TruncatedBlob::truncated);
        let last = calls.len() - 1;
        let mut uses = Vec::with_capacity(calls.len());
        let mut results = Vec::with_capacity(calls.len());
        for (index, (call, slot)) in calls.iter().zip(&slots).enumerate() {
            let id = format!("{call_id}#{index}");
            let note = if index == last {
                parsed.note.as_deref()
            } else {
                None
            };
            let shared: Vec<String> = slot
                .shares_with
                .iter()
                .filter_map(|other| calls.get(*other))
                .map(call_display_name)
                .collect();
            uses.push(ContentBlock::ToolUse {
                tool_use_id: Some(id.clone()),
                tool_name: call.tool_name.clone(),
                input_preview: Some(shell_session_input_preview(
                    call,
                    &id,
                    sessions,
                    poll_origins,
                )),
                status: None,
                meta: codex_script_meta(
                    call.label.as_deref(),
                    // A command that ran and printed nothing is not a command
                    // whose output went missing; only the second is worth a
                    // notice, and saying it of the first would be false.
                    !slot.placed,
                    &shared,
                    truncated,
                ),
            });
            if let Some(text) = &slot.text {
                register_shell_sessions(call, &id, text, sessions);
            }
            let shown = slot
                .text
                .clone()
                .and_then(|text| non_empty(exec_chunk_display(call, text)));
            results.push(result_block(Some(id), shown, note));
        }
        return (Some(uses), results);
    }

    // Undecomposable script: the card stays a script card, and no session it
    // announced is attributed. There is no way to tell which of its calls
    // started one — that is exactly why it did not decompose.
    let joined = chunks.join("\n");
    (
        None,
        vec![result_block(
            Some(call_id.to_string()),
            non_empty(joined),
            parsed.note.as_deref(),
        )],
    )
}

#[derive(Debug)]
struct CompletedMcpCall {
    id: String,
    server: String,
    tool: String,
    input_preview: Option<String>,
    output_preview: Option<String>,
    is_error: bool,
}

/// How much of a serialized MCP result stands in for a call that answered in
/// blocks with no text of its own. Matches `pi`'s cap on the same shape: enough
/// to show what came back, not enough for a base64 blob to swamp the card.
const MCP_RESULT_FALLBACK_CAP: usize = 4000;

/// A sink that accepts `budget` bytes and then refuses, so a serializer writing
/// into it stops instead of running to the end of its input.
struct BudgetedSink {
    buf: Vec<u8>,
    budget: usize,
}

impl std::io::Write for BudgetedSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = self.budget.saturating_sub(self.buf.len());
        if room == 0 {
            return Err(std::io::Error::other("preview budget reached"));
        }
        let take = room.min(data.len());
        self.buf.extend_from_slice(&data[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `value` serialized into a `max_chars` preview without ever building an
/// UNBOUNDED serialization. A value that fits is still written out whole —
/// into a buffer that cannot grow past the budget.
///
/// `serde_json::to_string` materializes all of it first — for an image-only
/// MCP result that is the entire base64 blob, allocated and then walked twice
/// more by `truncate_str`, to keep a few thousand characters of it. This
/// produces exactly the same string while the buffer stays at
/// `4 * max_chars + 1` bytes. UTF-8 spends at most 4 bytes per character, so
/// that many always cover `max_chars` of them — and the `+ 1` is what makes it
/// STRICTLY more, which is the whole proof: a full buffer therefore always
/// decodes to more than `max_chars` characters, so it is always truncated, so
/// the partial character a byte cut leaves behind is always among the ones
/// dropped. At `4 * max_chars` alone the strictness would rest on JSON always
/// opening with an ASCII byte and so never letting a full buffer land on
/// exactly `max_chars` — which holds for every `max_chars` but zero, and even
/// then is a fact about the format rather than about this function. The `+ 1`
/// is what makes it a property of the arithmetic.
///
/// What it bounds is the MEMORY, which is the part that can fail. Time is only
/// mostly bounded: `serde_json` walks a string looking for escapes before
/// offering any of it to the writer, so one oversized string is still read
/// through once — a pass over bytes already resident, not a second copy of
/// them. Stopping even that would mean replacing the serializer.
///
/// `None` for a value that serializes to nothing at all.
fn serialize_preview(value: &serde_json::Value, max_chars: usize) -> Option<String> {
    let mut sink = BudgetedSink {
        buf: Vec::new(),
        budget: max_chars.saturating_mul(4).saturating_add(1),
    };
    // A value that fits reports `Ok`; one that does not aborts with the sink's
    // own error. Both leave `buf` holding the prefix, and `serde_json` cannot
    // fail on a `Value` for any other reason.
    let _ = serde_json::to_writer(&mut sink, value);
    let text = String::from_utf8_lossy(&sink.buf);
    (!text.is_empty()).then(|| truncate_str(&text, max_chars))
}

fn completed_mcp_call(payload: &serde_json::Value) -> Option<CompletedMcpCall> {
    let item = payload.get("item")?;
    if item.get("type").and_then(|v| v.as_str()) != Some("McpToolCall") {
        return None;
    }
    let result = item.get("result").filter(|value| !value.is_null());
    let stated_error = item.get("error").filter(|value| is_stated_error(value));
    // Text first, then the structured twin. The last two are for the shapes
    // that carry neither — a transport failure that answered with `error` and
    // no result, or a `content` array holding only blocks this reader cannot
    // render (an image, a resource). The semantic path DISCARDS the wrapper's
    // own printed output, so a `None` here is not a quiet degradation, it is a
    // card that says nothing at all where the script card used to show the
    // run's text.
    //
    // Only the LAST one is truncated, and only for what the card SHOWS — it is
    // the shape that can be a base64 blob, and a card must not be flooded with
    // one. Nothing may decide an OUTCOME from a cut string: the heuristic below
    // re-parses a preview that opens with `{` or `[` and looks for a failed
    // `status` inside it (`infer_output_text_is_error`), and truncating valid
    // JSON makes that parse fail silently, settling a call that reported
    // failure GREEN. So the cut branch also hands back the value it cut, and
    // the outcome is read from that instead.
    let output_preview = result
        .and_then(|result| result.get("content"))
        .and_then(crate::parsers::pi::tool_result_content_text)
        .or_else(|| {
            result
                .and_then(|result| result.get("structuredContent"))
                .and_then(|value| serde_json::to_string(value).ok())
        })
        .or_else(|| value_to_preview(stated_error));
    // `content` rather than the whole envelope. A call that returned NOTHING
    // still says nothing — `{"content":[]}` is not worth rendering.
    let blocks = output_preview
        .is_none()
        .then(|| result?.get("content"))
        .flatten()
        .filter(|content| content.as_array().is_some_and(|blocks| !blocks.is_empty()));
    let output_preview = output_preview
        .or_else(|| blocks.and_then(|content| serialize_preview(content, MCP_RESULT_FALLBACK_CAP)));
    // The record STATES its outcome — `result.isError`, the item's own terminal
    // `status`, and an `error` when the call never reached the server. Believe
    // them. `infer_tool_call_output_is_error` reads tea leaves out of the
    // output text because a script card has no such field; run against an
    // authoritative record it can only invent failures, and a tool that
    // legitimately PRINTS `exit code: 1` or answers with a line opening
    // `Error:` returned perfectly well. Kept as the fallback for a record that
    // states nothing. Stated failure outranks stated success, so a record
    // contradicting itself settles as the error it reported.
    let stated_is_error = result
        .and_then(|result| result.get("isError"))
        .and_then(serde_json::Value::as_bool);
    let claimed_failed =
        stated_is_error == Some(true)
            || stated_error.is_some()
            || item
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(is_failed_status);
    let claimed_ok = stated_is_error == Some(false)
        || item.get("status").and_then(serde_json::Value::as_str) == Some("completed");
    Some(CompletedMcpCall {
        id: item.get("id")?.as_str()?.to_string(),
        server: item.get("server")?.as_str()?.to_string(),
        tool: item.get("tool")?.as_str()?.to_string(),
        input_preview: value_to_preview(item.get("arguments")),
        is_error: claimed_failed
            || (!claimed_ok
                && (infer_tool_call_output_is_error(item, result, output_preview.as_deref())
                    || blocks_report_failure(blocks))),
        output_preview,
    })
}

/// Whether any block in a result's `content` array REPORTS a failure.
///
/// The blocks are what the preview above was cut out of, and the cut string
/// no longer re-parses, so the outcome has to be read here or not at all —
/// truncation may cost a card characters, never a call its verdict.
///
/// A block's own report, deliberately, and no descent. A full
/// `infer_output_value_is_error` walk follows `data`, which in a tool-output
/// envelope is a nested result but on an MCP block is the PAYLOAD — the base64
/// the cap above refuses to copy, and which `infer_output_text_is_error` would
/// lowercase into a second copy of itself anyway. A payload that happens to
/// read like an error is still just bytes. Depth 4 is how that is said to a
/// walker whose own limit is 4: it reads the outcome fields it recognizes and
/// then every descent refuses. Only OBJECT blocks are asked, because only they
/// can carry such a field — MCP `content` holds typed blocks, and a bare
/// string among them is not a shape this can read a verdict out of.
///
/// Still not free in the worst case: a recognized field can itself be huge
/// (`{"stderr": "<megabytes of spaces>"}` costs a `trim`). That is a pass over
/// one already-resident string, not a copy of it, and unlike `data` it is a
/// field a block would have to have gone out of its way to carry.
fn blocks_report_failure(blocks: Option<&serde_json::Value>) -> bool {
    blocks
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .filter(|block| block.is_object())
                .any(|block| infer_output_value_is_error(block, 4))
        })
}

fn unwrap_completed_mcp_calls(
    script: &CodeModeScript,
    completed: Vec<CompletedMcpCall>,
) -> Option<(Vec<ContentBlock>, Vec<ContentBlock>)> {
    if script.tool_names.len() != completed.len() || script.tool_names.is_empty() {
        return None;
    }
    let names_match = script
        .tool_names
        .iter()
        .zip(&completed)
        .all(|(tool_name, item)| {
            let server = item.server.replace('-', "_");
            tool_name == &format!("mcp__{server}__{}", item.tool)
        });
    if !names_match {
        return None;
    }
    let mut uses = Vec::with_capacity(completed.len());
    let mut results = Vec::with_capacity(completed.len());
    for (index, item) in completed.into_iter().enumerate() {
        uses.push(ContentBlock::ToolUse {
            tool_use_id: Some(item.id.clone()),
            tool_name: script.tool_names[index].clone(),
            input_preview: item.input_preview,
            // Read off the item's OWN outcome, never hardcoded: a code-mode
            // script whose MCP call failed still prints `Script completed`
            // (measured: a `delegate_to_agent` refused for `depth_limit`
            // settles the script fine), so claiming `completed` here would
            // contradict the very result block written next to it. This is a
            // per-call terminal record — not `ScriptStatus`, which
            // `ContentBlock::ToolUse::status` documents as unsafe to copy.
            status: Some(if item.is_error { "failed" } else { "completed" }.into()),
            meta: None,
        });
        results.push(ContentBlock::ToolResult {
            tool_use_id: Some(item.id),
            output_preview: item.output_preview,
            is_error: item.is_error,
            agent_stats: None,
            images: Vec::new(),
        });
    }
    Some((uses, results))
}

/// What the renderer needs to know about a call recovered from a code-mode
/// script, as facts rather than prose: the backend states them, the frontend
/// words them in the reader's language.
fn codex_script_meta(
    label: Option<&str>,
    output_missing: bool,
    shares_with: &[String],
    truncated: bool,
) -> Option<serde_json::Value> {
    let mut marks = serde_json::Map::new();
    if let Some(label) = label {
        marks.insert("label".to_string(), label.into());
    }
    if output_missing {
        marks.insert("outputMissing".to_string(), true.into());
    }
    if !shares_with.is_empty() {
        marks.insert("sharedWith".to_string(), shares_with.into());
    }
    if truncated {
        marks.insert("truncated".to_string(), true.into());
    }
    (!marks.is_empty()).then(|| serde_json::json!({ "codeg.codexScript": marks }))
}

/// How to name a call when telling the reader whose output shares a card.
fn call_display_name(call: &CodeModeCall) -> String {
    call.label.clone().unwrap_or_else(|| {
        truncate_str(
            call.input_preview.lines().next().unwrap_or_default().trim(),
            60,
        )
    })
}

/// A code-mode script whose output was `Script running with cell ID N`: the
/// script itself has not finished. Remembers where its cards were written so
/// the `wait` that collects cell N — its real return value, arriving any number
/// of turns later — can be folded back onto them instead of rendering as a
/// separate card. Indices are stable because the parse loop only appends.
#[derive(Clone)]
struct DeferredScript {
    call_id: String,
    /// Index of the message holding the script's ToolUse block(s).
    use_index: usize,
    /// Index of the message holding its ToolResult block(s).
    result_index: usize,
    script: CodeModeScript,
    /// Everything the script has printed so far. A `wait` answers with what has
    /// been printed SINCE — measured on real transcripts, the collected answer
    /// shares nothing with the one that parked the script — so the chunks have
    /// to be accumulated and re-decomposed as one sequence. Replacing the cards
    /// from the `wait` alone would drop whatever the script had already printed,
    /// and would decompose against a chunk count that is missing its front.
    chunks: Vec<String>,
}

/// codex's unified-exec session tools. Neither carries a command of its own:
/// both address a background shell started by an earlier `exec_command`, by the
/// id that command's output announced (`wait` calls it `cell_id`,
/// `write_stdin` calls it `session_id`).
const SHELL_SESSION_TOOLS: [&str; 2] = ["wait", "write_stdin"];

/// Key added to a `wait` / `write_stdin` `input_preview` carrying the command
/// that started the session it addresses.
///
/// Deliberately NOT `command`: `inferFromInput` (`tool-call-normalization.ts`)
/// classifies any live input carrying `command`/`cmd`/… as a terminal call, so
/// that spelling would hijack the live classification of these tools.
const SESSION_COMMAND_KEY: &str = "session_command";

/// A background shell an `exec_command` left running: unified-exec answers that
/// command with whatever it printed so far plus `Process running with session
/// ID N`, and the rest of the output has to be collected by later `wait` /
/// `write_stdin` calls addressing N.
#[derive(Clone)]
struct ShellSession {
    /// The command that started it. Titles the calls that address it.
    command: String,
    /// `tool_use_id` of the card holding that command's output, while more of
    /// it may still be appended there. `None` once the session got a card of
    /// its own (keystrokes, termination): output arriving after that card
    /// belongs below it, not folded back above it.
    origin: Option<String>,
}

fn is_shell_session_tool(tool_name: &str) -> bool {
    SHELL_SESSION_TOOLS.contains(&tool_name)
}

/// The text a recovered call's card shows. Only the exec family answers with
/// the chunk envelope, and only there is unwrapping it provably right — another
/// tool returning an object that happens to carry `chunk_id` would be showing
/// its own result, not a terminal's.
fn exec_chunk_display(call: &CodeModeCall, chunk: String) -> String {
    if call.tool_name != "exec_command" && !is_shell_session_tool(&call.tool_name) {
        return chunk;
    }
    strip_exec_envelope(&chunk).unwrap_or(chunk)
}

/// Whether a `wait` / `write_stdin` only collects output — no keystrokes to
/// show, no termination to report. Such a call *is* the earlier command still
/// running, so it gets no card and its output is appended to that command's.
fn is_pure_poll(args: &serde_json::Map<String, serde_json::Value>) -> bool {
    let sends_input = args
        .get("chars")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    let terminates = args
        .get("terminate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    !sends_input && !terminates
}

/// The session id a `wait` / `write_stdin` argument object addresses, as a
/// string — `wait` sends `"cell_id":"7106"`, `write_stdin` sends
/// `"session_id":33067`.
fn shell_session_id(args: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let value = args.get("cell_id").or_else(|| args.get("session_id"))?;
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Annotate a `wait` / `write_stdin` argument JSON with the command that
/// started the session it addresses, so the card can be titled by that command
/// instead of a bare `wait`. Returns `None` when the arguments aren't an
/// object, carry no session id, or the id was never announced — the caller then
/// keeps the arguments untouched rather than inventing a command.
fn annotate_shell_session_args(
    args: &serde_json::Value,
    sessions: &HashMap<String, ShellSession>,
) -> Option<String> {
    let obj = args.as_object()?;
    let session = sessions.get(&shell_session_id(obj)?)?;
    let mut annotated = obj.clone();
    annotated.insert(
        SESSION_COMMAND_KEY.to_string(),
        serde_json::Value::String(session.command.clone()),
    );
    Some(serde_json::Value::Object(annotated).to_string())
}

/// Same annotation for a session tool recovered from a code-mode script, whose
/// `input_preview` is already the argument object's JSON.
fn shell_session_input_preview(
    call: &CodeModeCall,
    tool_use_id: &str,
    sessions: &mut HashMap<String, ShellSession>,
    poll_origins: &mut HashMap<String, String>,
) -> String {
    if !is_shell_session_tool(&call.tool_name) {
        return call.input_preview.clone();
    }
    let args = serde_json::from_str::<serde_json::Value>(&call.input_preview).ok();
    note_shell_session_call(Some(tool_use_id), args.as_ref(), sessions, poll_origins);
    args.and_then(|args| annotate_shell_session_args(&args, sessions))
        .unwrap_or_else(|| call.input_preview.clone())
}

/// Record `session id → command` for every background session this call's
/// output announced. Only `exec_command` starts sessions, and only its own
/// output can name them, so nothing else is registered — a wrong mapping would
/// put someone else's command in a `wait` card's title.
fn register_shell_sessions(
    call: &CodeModeCall,
    origin: &str,
    output: &str,
    sessions: &mut HashMap<String, ShellSession>,
) {
    // `args_inline` is the proof that this command is what actually ran: one
    // call site executes any number of times, and arguments reached through a
    // variable can be reassigned between iterations, leaving the recovered
    // command a guess. Titling — and now folding — a session under a guess is
    // exactly the failure this parser must not have.
    if call.tool_name != "exec_command" || !call.args_inline {
        return;
    }
    // `input_preview` of a recovered `exec_command` is the bare command string.
    register_announced_sessions(&call.input_preview, origin, output, sessions);
}

/// `origin` is the `tool_use_id` of the card this output is being written to —
/// where the session's remaining output gets appended once a later poll
/// collects it.
fn register_announced_sessions(
    command: &str,
    origin: &str,
    output: &str,
    sessions: &mut HashMap<String, ShellSession>,
) {
    if command.trim().is_empty() {
        return;
    }
    let announced = extract_shell_session_ids(output);
    if announced.is_empty() {
        return;
    }
    // The chunk ids of the same output are aliases for the cells it announced —
    // codex addresses them either way. Registered only alongside a real
    // announcement, so a chunk id from a command that already exited stays
    // meaningless.
    for id in announced.into_iter().chain(extract_chunk_ids(output)) {
        sessions.insert(
            id,
            ShellSession {
                command: command.to_string(),
                origin: Some(origin.to_string()),
            },
        );
    }
}

/// Classify a `wait` / `write_stdin` against the session it addresses: either
/// it only collects more of that session's output — in which case it is marked
/// for folding into the card of the command producing it — or it is an action
/// (keystrokes, termination) that earns a card, and everything the session
/// prints from then on belongs below that card rather than folded back above
/// it. Sessions nobody announced fall through both: nothing to fold into.
fn note_shell_session_call(
    tool_use_id: Option<&str>,
    args: Option<&serde_json::Value>,
    sessions: &mut HashMap<String, ShellSession>,
    poll_origins: &mut HashMap<String, String>,
) {
    let Some(args) = args.and_then(|a| a.as_object()) else {
        return;
    };
    let Some(session_id) = shell_session_id(args) else {
        return;
    };
    let Some(origin) = sessions.get(&session_id).and_then(|s| s.origin.clone()) else {
        return;
    };
    if is_pure_poll(args) {
        if let Some(call_id) = tool_use_id {
            poll_origins.insert(call_id.to_string(), origin);
        }
        return;
    }
    // End the folding for every id naming this cell, not just the one this call
    // addressed: the numeric session id and the hex chunk id are aliases, and a
    // poll arriving through the other spelling would otherwise still be folded
    // in above the card that caused it.
    for session in sessions.values_mut() {
        if session.origin.as_deref() == Some(origin.as_str()) {
            session.origin = None;
        }
    }
}

/// Drop every card that only collects more of an earlier command's output,
/// moving what it collected onto that command's card. Runs on the finished
/// message list so a poll codex called directly and one a code-mode script
/// wrote are folded the same way — the script path builds its cards inside
/// `unwrap_code_mode_script`, long after the call itself was read.
fn fold_shell_session_polls(
    messages: &mut Vec<UnifiedMessage>,
    poll_origins: &HashMap<String, String>,
) {
    if poll_origins.is_empty() {
        return;
    }

    // Where every tool block lives, and — in transcript order — the polls to
    // fold. Collected up front: a poll's origin always sits earlier, so folding
    // as we walk would need the map anyway, and both blocks of a poll have to be
    // located before either can be dropped.
    let mut result_at: HashMap<String, (usize, usize)> = HashMap::new();
    let mut use_at: HashMap<String, (usize, usize)> = HashMap::new();
    let mut folds: Vec<(String, String, usize, usize)> = Vec::new();
    for (mi, message) in messages.iter().enumerate() {
        for (bi, block) in message.content.iter().enumerate() {
            match block {
                ContentBlock::ToolUse {
                    tool_use_id: Some(id),
                    ..
                } => {
                    use_at.insert(id.clone(), (mi, bi));
                }
                ContentBlock::ToolResult {
                    tool_use_id: Some(id),
                    ..
                } => match poll_origins.get(id) {
                    Some(origin) => folds.push((id.clone(), origin.clone(), mi, bi)),
                    None => {
                        result_at.insert(id.clone(), (mi, bi));
                    }
                },
                _ => {}
            }
        }
    }

    let mut drop_at: Vec<(usize, usize)> = Vec::new();
    for (poll_id, origin, mi, bi) in folds {
        // No origin block left — a code-mode script re-decomposed under it, say.
        // Leave the poll's own card alone rather than dropping what it collected.
        let Some(&(omi, obi)) = result_at.get(&origin) else {
            continue;
        };
        let ContentBlock::ToolResult {
            output_preview,
            is_error,
            ..
        } = &messages[mi].content[bi]
        else {
            continue;
        };
        let collected = output_preview.clone().unwrap_or_default();
        let collected = collected.trim_end().to_string();
        let collected_error = *is_error;

        let ContentBlock::ToolResult {
            output_preview,
            is_error,
            ..
        } = &mut messages[omi].content[obi]
        else {
            continue;
        };
        if !collected.is_empty() {
            match output_preview {
                Some(existing) if !existing.trim().is_empty() => {
                    while existing.ends_with('\n') {
                        existing.pop();
                    }
                    existing.push('\n');
                    existing.push_str(&collected);
                }
                _ => *output_preview = Some(collected),
            }
        }
        *is_error = *is_error || collected_error;

        drop_at.push((mi, bi));
        if let Some(&at) = use_at.get(&poll_id) {
            drop_at.push(at);
        }
    }

    if drop_at.is_empty() {
        return;
    }
    let emptied: HashSet<usize> = drop_at.iter().map(|(mi, _)| *mi).collect();
    drop_at.sort_unstable();
    // Two polls sharing a `tool_use_id` would name the same use block twice, and
    // removing a block index twice panics. Ids are unique in practice; a broken
    // transcript must not take the whole conversation down with it.
    drop_at.dedup();
    for (mi, bi) in drop_at.into_iter().rev() {
        messages[mi].content.remove(bi);
    }
    let mut index = 0;
    messages.retain(|m| {
        let keep = !m.content.is_empty() || !emptied.contains(&index);
        index += 1;
        keep
    });
}

fn value_to_preview(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    if v.is_null() {
        return None;
    }
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    serde_json::to_string(v).ok()
}

fn is_failed_status(status: &str) -> bool {
    let status = status.trim();
    status.eq_ignore_ascii_case("error")
        || status.eq_ignore_ascii_case("failed")
        || status.eq_ignore_ascii_case("failure")
        || status.eq_ignore_ascii_case("cancelled")
        || status.eq_ignore_ascii_case("canceled")
}

fn parse_nonzero_exit_code_from_line(line: &str) -> Option<i64> {
    let trimmed = line.trim();
    let (label, rest) = trimmed.split_once(':')?;
    if !label.trim_end().eq_ignore_ascii_case("exit code") {
        return None;
    }
    let number_text = rest.split_whitespace().next()?;
    let code = number_text.parse::<i64>().ok()?;
    if code == 0 {
        None
    } else {
        Some(code)
    }
}

fn infer_output_text_is_error(text: &str) -> bool {
    for line in text.lines().take(16) {
        if parse_nonzero_exit_code_from_line(line).is_some() {
            return true;
        }
    }

    for line in text.lines().take(32) {
        let lower = line.trim().to_ascii_lowercase();
        let shell_prefix =
            lower.starts_with("bash:") || lower.starts_with("zsh:") || lower.starts_with("sh:");
        if shell_prefix
            && (lower.contains("command not found")
                || lower.contains("no such file or directory")
                || lower.contains("permission denied"))
        {
            return true;
        }
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .map(|v| infer_output_value_is_error(&v, 0))
            .unwrap_or(false)
    {
        return true;
    }

    trimmed
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("error:"))
}

/// Whether an `error` field STATES an error rather than merely existing.
/// `null`, `false` and a blank string are how a record says "no error", and a
/// reader that took their presence for failure would fail every clean call
/// that carries the key.
fn is_stated_error(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(false) => false,
        serde_json::Value::String(text) => !text.trim().is_empty(),
        _ => true,
    }
}

fn infer_output_value_is_error(value: &serde_json::Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }

    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => false,
        serde_json::Value::String(text) => infer_output_text_is_error(text),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| infer_output_value_is_error(item, depth + 1)),
        serde_json::Value::Object(map) => {
            if map
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return true;
            }

            if map.get("ok").and_then(|v| v.as_bool()) == Some(false)
                || map.get("success").and_then(|v| v.as_bool()) == Some(false)
            {
                return true;
            }

            if let Some(status) = map.get("status").and_then(|v| v.as_str()) {
                if is_failed_status(status) {
                    return true;
                }
            }

            if let Some(exit_code) = map.get("exit_code").and_then(|v| v.as_i64()) {
                if exit_code != 0 {
                    return true;
                }
            }

            if let Some(stderr) = map.get("stderr").and_then(|v| v.as_str()) {
                if !stderr.trim().is_empty() {
                    return true;
                }
            }

            if map.get("error").is_some_and(is_stated_error) {
                return true;
            }

            for key in ["output", "result", "details", "data"] {
                if let Some(child) = map.get(key) {
                    if infer_output_value_is_error(child, depth + 1) {
                        return true;
                    }
                }
            }

            false
        }
    }
}

fn infer_tool_call_output_is_error(
    payload: &serde_json::Value,
    output_value: Option<&serde_json::Value>,
    output_preview: Option<&str>,
) -> bool {
    if payload
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if let Some(status) = payload.get("status").and_then(|s| s.as_str()) {
        if is_failed_status(status) {
            return true;
        }
    }

    if payload.get("error").is_some_and(is_stated_error) {
        return true;
    }

    if let Some(output) = output_value {
        if infer_output_value_is_error(output, 0) {
            return true;
        }
    }

    output_preview
        .map(infer_output_text_is_error)
        .unwrap_or(false)
}

/// Synthetic rawInput key the live input shaper uses to carry the collab op
/// through to the card (see frontend `collab-tool.ts` `COLLAB_OP_KEY`). Kept in
/// sync here so history `wait_agent` capsules render with an op-aware title.
const COLLAB_OP_KEY: &str = "__codegCollabOp";

/// Whether a collab status string is an error (mirrors the frontend
/// `isErrorCollabStatusKind`: only `errored` / `failed` / `notFound`).
fn is_error_collab_status(status: &str) -> bool {
    matches!(status, "errored" | "failed" | "notFound")
}

/// Prefix of codex's encrypted payload envelope — a Fernet token, whose version
/// byte `0x80` always base64s to `gA`. codex uses it for `reasoning`'s
/// `encrypted_content` and, since 0.147, for the inter-agent `message` a
/// `spawn_agent` / `send_message` carries.
const CODEX_ENCRYPTED_PREFIX: &str = "gAAAAA";

/// Shortest blob worth treating as an envelope: the token's own header (version
/// byte + 8-byte timestamp + 16-byte IV + HMAC) is already well past this, so
/// the bound only rules out a short string that merely starts the same way.
const CODEX_ENCRYPTED_MIN_LEN: usize = 64;

/// Whether a payload is one of codex's opaque encrypted envelopes rather than
/// text a human wrote.
///
/// codex 0.147 encrypts every inter-agent message: what reaches the rollout for
/// a `spawn_agent` is `"gAAAAABqgWsi0g7g…"`, ~500 characters of base64 that only
/// codex can open. Rendering it verbatim is what put a wall of base64 in the
/// sub-agent capsule's title and prompt. There is no plaintext to recover — the
/// capsule simply shows no prompt.
///
/// Deliberately narrow: the exact prefix AND no whitespace AND long enough. A
/// prompt that merely *contains* base64, or discusses one, still renders.
fn is_encrypted_envelope(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with(CODEX_ENCRYPTED_PREFIX)
        && trimmed.len() >= CODEX_ENCRYPTED_MIN_LEN
        && !trimmed.chars().any(char::is_whitespace)
}

/// What an encrypted envelope renders as inside a tool card's argument preview.
const CODEX_ENCRYPTED_PLACEHOLDER: &str = "[encrypted]";

/// Synthetic input key marking a sub-agent capsule as ONLY a launch — the card
/// settles when codex acknowledges the spawn, not when the child finishes (see
/// the frontend `agent-tool-call.tsx`, which turns it into a translated note).
///
/// Only codex 0.147's native team-of-agents needs it. In the older collab flow
/// the spawn capsule really did stand for the sub-agent's run, because a
/// `wait_agent` / `close_agent` capsule carried its result; 0.147 emits neither,
/// so an unmarked "completed" would claim a still-running child had finished.
///
/// Public because the LIVE path writes the same key
/// (`acp/connection.rs::classify_codex_subagent_activity`) — streaming and
/// reload must not disagree about what the card means.
pub const CODEX_SUBAGENT_LAUNCH_KEY: &str = "__codegCodexSubagentLaunch";

/// Whether a `spawn_agent`'s arguments are codex 0.147's native team-of-agents
/// shape: `task_name` and no `agent_type` (which that release removed).
fn is_native_team_spawn(args: Option<&serde_json::Value>) -> bool {
    args.is_some_and(|a| a.get("agent_type").is_none() && a.get("task_name").is_some())
}

/// Synthetic input key naming the sub-agent's TERMINAL state, when codex
/// reported one. Absent while the child is still working (or was never heard
/// from again), which is the state [`CODEX_SUBAGENT_LAUNCH_KEY`] describes.
///
/// Written by both the rollout parser and the live path
/// (`acp/connection.rs`), so a reload cannot disagree with the stream about
/// whether the child finished.
pub const CODEX_SUBAGENT_STATE_KEY: &str = "__codegCodexSubagentState";

/// One `SubAgentActivity` record, normalized across the two on-disk shapes.
///
/// `call_id` is the SPAWN's own `call_id` for a `started` record — the key that
/// ties a child thread back to the capsule that launched it. A terminal record
/// carries a synthetic id of its own (`subagent-completed-<uuid>`) instead, so
/// only `thread_id` correlates there.
struct CodexSubagentActivityRecord<'a> {
    call_id: Option<&'a str>,
    thread_id: &'a str,
    agent_path: Option<&'a str>,
    kind: &'a str,
}

/// Read one `event_msg` payload as a `SubAgentActivity`, whichever shape codex
/// wrote it in.
///
/// TWO shapes are live on disk and neither may be dropped:
///
/// * `event_msg.sub_agent_activity` with flat
///   `{event_id, agent_thread_id, agent_path, kind}` — codex ≤ 0.147.
/// * `event_msg.item_completed.item` with
///   `{type: "SubAgentActivity", id, agent_thread_id, agent_path, kind}` —
///   codex 0.153.4, which retired the flat event entirely (measured on a real
///   0.153.4 parent rollout: flat 0 records, nested 26).
///
/// Reading only the flat one — as this did before — meant every 0.153.4
/// sub-agent capsule reloaded with no `agent_id` at all, so the badge the live
/// stream showed disappeared on refresh and nothing could resolve the child's
/// own rollout. `item.id` is the same spawn `call_id` the flat `event_id`
/// carried, so the two normalize onto one record with no correlation loss.
fn codex_subagent_activity_record<'a>(
    payload_type: &str,
    payload: &'a serde_json::Value,
) -> Option<CodexSubagentActivityRecord<'a>> {
    let source = match payload_type {
        "sub_agent_activity" => payload,
        "item_completed" => {
            let item = payload.get("item")?;
            if item.get("type").and_then(serde_json::Value::as_str) != Some("SubAgentActivity") {
                return None;
            }
            item
        }
        _ => return None,
    };
    let str_field = |key: &str| {
        source
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    Some(CodexSubagentActivityRecord {
        // `event_id` on the flat shape, `id` on the nested item.
        call_id: str_field("event_id").or_else(|| str_field("id")),
        thread_id: str_field("agent_thread_id")?,
        agent_path: str_field("agent_path"),
        kind: str_field("kind").unwrap_or(""),
    })
}

/// The envelope header codex puts on an inter-agent `agent_message`, and the
/// only message type whose payload is readable.
const CODEX_FINAL_ANSWER_HEADER: &str = "Message Type: FINAL_ANSWER";

/// Where the envelope's own preamble ends and the sender's text begins.
const CODEX_INTER_AGENT_PAYLOAD_MARKER: &str = "Payload:\n";

/// A sub-agent's finished report, read off a `response_item.agent_message` in
/// the PARENT's rollout: `(author path, body)`.
///
/// This is the one piece of a codex team-of-agents exchange that is not sealed.
/// Every inter-agent message rides the same envelope, but only the terminal one
/// carries plaintext — measured across every such record on disk for two
/// months: `FINAL_ANSWER` 8/8 plaintext with a body, `MESSAGE` and `NEW_TASK`
/// 0/82 (both are Fernet blobs in a sibling `encrypted_content` part, in the
/// child's own rollout too, so there is nothing to recover for those).
///
/// The header match is anchored at the start of the text rather than a
/// substring search: a sub-agent that merely writes ABOUT the protocol — which
/// one reviewing this repository will — must not have its `MESSAGE` mistaken
/// for a report.
fn codex_inter_agent_final_answer(payload: &serde_json::Value) -> Option<(&str, String)> {
    let author = payload
        .get("author")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let text: String = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .collect();
    if !text.starts_with(CODEX_FINAL_ANSWER_HEADER) {
        return None;
    }
    let body = text
        .split_once(CODEX_INTER_AGENT_PAYLOAD_MARKER)
        .map(|(_, body)| body)?
        .trim();
    (!body.is_empty()).then(|| (author, body.to_string()))
}

/// Replace every encrypted envelope inside a parsed argument tree with
/// [`CODEX_ENCRYPTED_PLACEHOLDER`], returning whether anything was replaced.
///
/// `spawn_agent` is not the only carrier: `send_message` (the collaboration call
/// a parent uses to talk to a running sub-agent, and a sub-agent to answer)
/// takes the same sealed `message`, and it has no capsule of its own — it lands
/// on the generic tool card, whose preview is the whole argument JSON. Without
/// this, that card shows ~500 characters of base64. Nothing is lost by
/// replacing it: only codex can open the envelope.
///
/// Recurses so a nested payload is caught too, and reports whether it changed
/// anything so the caller can leave every other tool's preview byte-identical.
fn redact_encrypted_args(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => {
            if is_encrypted_envelope(s) {
                *s = CODEX_ENCRYPTED_PLACEHOLDER.to_string();
                return true;
            }
            false
        }
        serde_json::Value::Array(items) => redact_encrypted_children(items.iter_mut()),
        serde_json::Value::Object(map) => redact_encrypted_children(map.values_mut()),
        _ => false,
    }
}

/// Redact every child of a container and report whether any of them changed.
///
/// Deliberately a loop and not `Iterator::any`: the return value is a
/// by-product, the traversal is the point, and `any` short-circuits — it would
/// leave every envelope after the first one unredacted.
fn redact_encrypted_children<'a>(
    children: impl Iterator<Item = &'a mut serde_json::Value>,
) -> bool {
    let mut changed = false;
    for child in children {
        changed |= redact_encrypted_args(child);
    }
    changed
}

/// Add `agent_id` — and the child's terminal state, once codex has reported one
/// — to a spawn execution capsule's input JSON (the
/// `{subagent_type,prompt,description}` object), so the card can show the
/// sub-agent UUID and stop claiming the child's fate is unknowable. Tolerates a
/// missing/!object input by starting fresh.
fn inject_agent_id_into_input(
    input: Option<&str>,
    agent_id: &str,
    terminal_kind: Option<&str>,
) -> String {
    let mut obj = input
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    obj.insert(
        "agent_id".to_string(),
        serde_json::Value::String(agent_id.to_string()),
    );
    if let Some(kind) = terminal_kind {
        obj.insert(
            CODEX_SUBAGENT_STATE_KEY.to_string(),
            serde_json::Value::String(kind.to_string()),
        );
    }
    serde_json::Value::Object(obj).to_string()
}

/// Pull a single sub-agent's `(status, message)` out of one `wait_agent`
/// `output.status` value, e.g. `{ "completed": "<result>" }`. Generalizes over
/// the terminal key: prefer `completed`, else the first string-valued key, so a
/// future `{ "errored": "<msg>" }` maps to `status="errored"`.
fn extract_wait_agent_status(value: &serde_json::Value) -> (String, Option<String>) {
    if let Some(obj) = value.as_object() {
        if let Some(text) = obj.get("completed").and_then(|v| v.as_str()) {
            return ("completed".to_string(), Some(text.to_string()));
        }
        for (key, val) in obj {
            if let Some(text) = val.as_str() {
                return (key.clone(), Some(text.to_string()));
            }
        }
    } else if let Some(text) = value.as_str() {
        return (text.to_string(), None);
    }
    ("completed".to_string(), None)
}

/// Build a synthesized live-shaped collab `rawInput` JSON (and whether any agent
/// errored) for a history `wait_agent` capsule, from that wait's own
/// `output.status` map `{ agent_id: { <terminal-key>: <text> } }`. The result
/// routes through the same `CollabAgentCard` as the live `wait` capsule (matches
/// the shape `collab-tool.ts` `parseCollabToolInput` expects). Caller guarantees
/// `status` is non-empty.
/// Build the history capsule for codex 0.147's `wait_agent`, whose output is
/// `{"message":"Wait completed.","timed_out":false}` — no per-agent map at all.
///
/// It stays worth showing even though it carries nothing: with the native
/// team-of-agents the spawn capsule settles the instant codex acknowledges the
/// launch, so this wait is the ONLY thing in the timeline that spans the child's
/// actual run. Live already renders it (codex-acp forwards the wait as a
/// `collabAgentToolCall`, unlike the spawn), and the reload must agree — a
/// contentless capsule live and nothing at all on reload is the disagreement
/// this exists to remove. Both sides come out as a bare pill: no agents, no
/// prompt, so `AgentCapsule` renders no body.
///
/// The legacy shape is NOT routed here — it carries per-agent results, and a
/// content-free one there means "timed out with nothing to report", which is
/// noise the caller still drops. `timed_out` (a real bool) is the shape gate:
/// only the native-team output has it, and a timeout is a real outcome, so it
/// flags the capsule as failed.
fn native_team_wait_input(output: &serde_json::Value) -> Option<(String, bool)> {
    let timed_out = output.get("timed_out")?.as_bool()?;
    let input = serde_json::json!({
        "senderThreadId": "",
        "receiverThreadIds": [],
        "agentsStates": {},
        "status": if timed_out { "failed" } else { "completed" },
        COLLAB_OP_KEY: "wait",
    });
    Some((input.to_string(), timed_out))
}

fn build_collab_wait_input(status: &serde_json::Map<String, serde_json::Value>) -> (String, bool) {
    let mut receiver_ids: Vec<serde_json::Value> = Vec::new();
    let mut agents_states = serde_json::Map::new();
    let mut any_error = false;
    for (agent_id, value) in status {
        receiver_ids.push(serde_json::Value::String(agent_id.clone()));
        let (st, msg) = extract_wait_agent_status(value);
        if is_error_collab_status(&st) {
            any_error = true;
        }
        agents_states.insert(
            agent_id.clone(),
            serde_json::json!({
                "status": st,
                "message": msg,
            }),
        );
    }
    let input = serde_json::json!({
        "senderThreadId": "",
        "receiverThreadIds": receiver_ids,
        "agentsStates": serde_json::Value::Object(agents_states),
        "status": if any_error { "failed" } else { "completed" },
        COLLAB_OP_KEY: "wait",
    });
    (input.to_string(), any_error)
}

/// The primary agent's own path in codex's team-of-agents tree. Every other
/// path under it is a sub-agent.
const CODEX_ROOT_AGENT_PATH: &str = "/root";

/// Build a `collab_agent` capsule for `list_agents`, whose output is a roster:
/// `{"agents":[{"agent_name","agent_status"}]}` where `agent_status` is either a
/// bare state string (`"running"`) or the same terminal map a wait returns
/// (`{"completed": "<full report>"}`).
///
/// Worth a capsule of its own because a finished child's ENTIRE report is in
/// there: with the native team-of-agents there is no `close_agent`, and the wait
/// carries only `{"message":"Wait completed.","timed_out":false}`, so a roster
/// taken after a child finished is one of the few places its text survives in a
/// readable form. It rendered as a wall of raw JSON on the generic tool card
/// before this.
///
/// The root row is dropped: codex reports the parent through the same roster,
/// and listing the conversation you are already reading as one of its own
/// sub-agents is noise. `None` when nothing is left to show.
fn build_collab_list_input(output: &serde_json::Value) -> Option<(String, bool)> {
    let mut receiver_ids: Vec<serde_json::Value> = Vec::new();
    let mut agents_states = serde_json::Map::new();
    let mut any_error = false;
    for entry in output.get("agents")?.as_array()? {
        // `continue`, never `?`: the root row is present in every roster, so
        // bailing out on it would drop the whole capsule.
        let Some(name) = entry
            .get("agent_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty() && *n != CODEX_ROOT_AGENT_PATH)
        else {
            continue;
        };
        let (st, msg) = match entry.get("agent_status") {
            Some(value) => extract_wait_agent_status(value),
            None => continue,
        };
        if is_error_collab_status(&st) {
            any_error = true;
        }
        receiver_ids.push(serde_json::Value::String(name.to_string()));
        agents_states.insert(
            name.to_string(),
            serde_json::json!({ "status": st, "message": msg }),
        );
    }
    if agents_states.is_empty() {
        return None;
    }
    let input = serde_json::json!({
        "senderThreadId": "",
        "receiverThreadIds": receiver_ids,
        "agentsStates": serde_json::Value::Object(agents_states),
        "status": if any_error { "failed" } else { "completed" },
        COLLAB_OP_KEY: "list",
    });
    Some((input.to_string(), any_error))
}

/// The parent thread id a rollout's `session_meta` payload declares, or `None`
/// for a root session.
///
/// TWO shapes carry it and both are live on disk, so neither branch may be
/// dropped. The structured `source.subagent.thread_spawn.parent_thread_id` is
/// the one every sub-agent rollout has (checked across on-disk rollouts from
/// 0.117 through 0.147); the flat `parent_thread_id` is a later, additive
/// mirror that only some versions write, always alongside the structured form
/// and never on its own. Reading only the flat field — as this did before —
/// therefore misses real sub-agents, which is what let their rollouts list as
/// importable root sessions and let their replayed history be counted as their
/// own (see [`is_forked_thread_header`]).
///
/// A present-but-blank id is treated as absent: it names no parent, and a
/// whitespace `parent_id` would still read as "this is a child" downstream.
fn codex_parent_thread_id(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("parent_thread_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| {
            payload
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
        })
        .map(ToOwned::to_owned)
}

/// Whether a transcript's opening record declares it a FORKED thread — codex
/// 0.147's `fork_turns` sub-agent shape, where the child is a rollout of its own
/// and the parent's history is copied into its head.
///
/// That copy is the problem: the parent's own tool calls sit at the top of the
/// child's file, indistinguishable at the record level from the child's, so
/// counting the whole file would credit the child with work the PARENT did. The
/// hand-off that separates them (the addressed inter-agent `agent_message`) is
/// not a reliable boundary either — a parent that already collected an earlier
/// sibling's reply carries one inside the forked prefix too.
///
/// So a forked thread yields no stats at all rather than wrong ones. The capsule
/// still names the sub-agent and badges its thread id (both come from the
/// PARENT's rollout); only the nested tool-call list is absent. The legacy
/// `agent-<id>.jsonl` shape has no such prefix and is unaffected.
///
/// BOTH markers are required, because "is a sub-agent" and "replays the parent"
/// are different things and only the second one may be refused. A parent thread
/// id alone says a thread was spawned by another; `forked_from_id` is codex's
/// own declaration that this rollout was seeded with that thread's history. On
/// disk they split cleanly — of 46 sub-agent rollouts, the 23 carrying
/// `forked_from_id` are exactly the 23 that also replay a second `session_meta`
/// header, and the other 23 are ordinary children whose tool calls are their
/// own. Refusing on the parent id alone silently blanks the capsule for those.
fn is_forked_thread_header(value: &serde_json::Value) -> bool {
    if value.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };
    codex_parent_thread_id(payload).is_some()
        && payload
            .get("forked_from_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
}

fn parse_codex_subagent_stats(
    session_dir: &std::path::Path,
    agent_id: &str,
) -> Option<AgentExecutionStats> {
    if agent_id.len() > 64 || agent_id.contains("..") || agent_id.contains('/') {
        return None;
    }

    // Try exact filename first (e.g., "agent-{agent_id}.jsonl"), then fall
    // back to files whose stem ends with the agent_id. Collect and sort
    // candidates to ensure deterministic selection across platforms.
    let exact_path = session_dir.join(format!("agent-{}.jsonl", agent_id));
    let session_file = if exact_path.is_file() {
        exact_path
    } else {
        let mut candidates: Vec<_> = fs::read_dir(session_dir)
            .ok()?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    return None;
                }
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                // Match only if the stem ends with the agent_id after a separator
                // (e.g., "session-abc123" matches agent_id "abc123")
                if stem == agent_id
                    || stem
                        .strip_suffix(agent_id)
                        .is_some_and(|prefix| prefix.ends_with('-') || prefix.ends_with('_'))
                {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        candidates.sort();
        candidates.into_iter().next()?
    };

    let file = fs::File::open(&session_file).ok()?;
    let reader = BufReader::new(file);

    let mut tool_calls = Vec::new();
    // A code-mode `exec` call expands to one entry per inner `tools.*` call, so
    // the sub-agent stats count real tools instead of a pile of `exec`.
    let mut pending_calls: HashMap<String, Vec<AgentToolCall>> = HashMap::new();
    let mut first_ts: Option<DateTime<Utc>> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;
    let mut checked_header = false;

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // The header decides whether this file can be counted at all — see
        // `is_forked_thread_header`. Checked on the first record that parses,
        // and never again.
        if !checked_header {
            checked_header = true;
            if is_forked_thread_header(&value) {
                return None;
            }
        }

        if let Some(ts) = parse_codex_timestamp(&value) {
            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = Some(ts);
        }

        if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        let payload_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match payload_type {
            "function_call" | "custom_tool_call" => {
                let call_id = payload
                    .get("call_id")
                    .or_else(|| payload.get("tool_call_id"))
                    .or_else(|| payload.get("id"))
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string());
                let tool_name = payload
                    .get("name")
                    .or_else(|| payload.get("tool_name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let entries = if is_code_mode_call(&tool_name) {
                    let source = payload
                        .get("input")
                        .or_else(|| payload.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let script = parse_code_mode_script(source);
                    match script.calls {
                        Some(calls) if !calls.is_empty() => calls
                            .into_iter()
                            .map(|call| AgentToolCall {
                                tool_name: call.tool_name,
                                input_preview: Some(truncate_str(&call.input_preview, 500)),
                                output_preview: None,
                                is_error: false,
                            })
                            .collect(),
                        _ => vec![AgentToolCall {
                            tool_name: CODEX_SCRIPT_TOOL_NAME.to_string(),
                            input_preview: script
                                .summary
                                .or_else(|| Some(source.to_string()))
                                .map(|s| truncate_str(&s, 500)),
                            output_preview: None,
                            is_error: false,
                        }],
                    }
                } else {
                    let input_preview = if tool_name == "exec_command" {
                        parse_codex_json_arg(payload)
                            .and_then(|a| {
                                a.get("cmd").and_then(|v| v.as_str()).map(|s| s.to_string())
                            })
                            .or_else(|| {
                                value_to_preview(
                                    payload.get("arguments").or_else(|| payload.get("input")),
                                )
                            })
                    } else {
                        value_to_preview(payload.get("arguments").or_else(|| payload.get("input")))
                    };

                    vec![AgentToolCall {
                        tool_name,
                        input_preview: input_preview.map(|s| truncate_str(&s, 500)),
                        output_preview: None,
                        is_error: false,
                    }]
                };

                if let Some(id) = call_id {
                    pending_calls.insert(id, entries);
                } else {
                    tool_calls.extend(entries);
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = payload
                    .get("call_id")
                    .or_else(|| payload.get("tool_call_id"))
                    .or_else(|| payload.get("id"))
                    .and_then(|id| id.as_str());

                if let Some(id) = call_id {
                    if let Some(entries) = pending_calls.remove(id) {
                        let output_value = payload.get("output");
                        let envelope = split_code_mode_output(output_value);
                        let per_call = entries.len() > 1 && envelope.chunks.len() == entries.len();
                        let joined = if envelope.status != ScriptStatus::Unknown
                            || output_value.is_some_and(|v| v.is_array())
                        {
                            Some(envelope.joined())
                        } else {
                            value_to_preview(output_value)
                        };

                        for (index, mut tc) in entries.into_iter().enumerate() {
                            let raw_output = if per_call {
                                Some(envelope.chunks[index].clone())
                            } else if index == 0 {
                                joined.clone()
                            } else {
                                None
                            };
                            if tc.tool_name == "exec_command" {
                                tc.output_preview = raw_output
                                    .map(|s| truncate_str(&clean_codex_exec_output(&s), 500));
                            } else {
                                tc.output_preview = raw_output.map(|s| truncate_str(&s, 500));
                            }
                            tc.is_error = envelope.is_error()
                                || infer_tool_call_output_is_error(
                                    payload,
                                    output_value,
                                    tc.output_preview.as_deref(),
                                );
                            tool_calls.push(tc);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    tool_calls.extend(pending_calls.into_values().flatten());

    let total_duration_ms = match (first_ts, last_ts) {
        (Some(f), Some(l)) => {
            let dur = (l - f).num_milliseconds();
            if dur > 0 {
                Some(dur as u64)
            } else {
                None
            }
        }
        _ => None,
    };

    let tool_count = tool_calls.len() as u32;
    Some(AgentExecutionStats {
        agent_type: None,
        status: None,
        total_duration_ms,
        total_tokens: None,
        total_tool_use_count: if tool_count > 0 {
            Some(tool_count)
        } else {
            None
        },
        read_count: None,
        search_count: None,
        bash_count: None,
        edit_file_count: None,
        lines_added: None,
        lines_removed: None,
        other_tool_count: None,
        tool_calls,
        // Codex's sub-agent rollout is folded into these stats; there is no
        // separate session for the card to open.
        child_session_id: None,
    })
}

impl CodexParser {
    fn parse_conversation_detail(
        &self,
        path: &Path,
        conversation_id: &str,
    ) -> Result<ConversationDetail, ParseError> {
        let lines = self.rollout_lines(path)?;

        let mut messages = Vec::new();
        let mut cwd: Option<String> = None;
        let mut parent_id: Option<String> = None;
        let mut session_header_seen = false;
        let mut git_branch: Option<String> = None;
        let mut model: Option<String> = None;
        let mut title: Option<String> = None;
        // Objective of the goal run currently open while replaying `/goal`
        // transitions, so a persisted `thread_goal_updated` with `goal: null`
        // closes the run by objective — identical to the live path. See
        // `crate::acp::codex_goal::next_goal_marker`.
        let mut codex_open_goal: Option<String> = None;
        // Objective of the FIRST goal opened, captured for a post-parse fallback:
        // newer codex consumes `/goal <objective>` as a slash command (persists
        // `thread_goal_updated` but no `user_message`), so the typed `/goal …`
        // prompt has no user turn on reload. We surface the objective as the
        // leading user message AFTER the loop — never mid-loop — so the synthetic
        // message can never poison `should_skip_duplicate_user_message` and drop a
        // real same-text user message that arrives later in the file.
        let mut first_goal_objective: Option<String> = None;
        // Whether that first `/goal` OPENED the session — i.e. no real user turn
        // had been recorded when it arrived. This is positional, not "no user turn
        // anywhere": newer codex records the goal first with no `user_message`, so
        // the objective IS the opening prompt and must be surfaced as the leading
        // user turn even when a LATER real reply (e.g. a "确认") exists. Older codex
        // persisted the `/goal` text as the opening `user_message`, which arrives
        // BEFORE the goal — there the flag stays false and nothing is synthesized.
        let mut goal_opens_session = false;
        // Start-of-turn markers, chronological, fed to
        // `backfill_turn_durations` so the first reply of a turn is measured
        // from when codex began working — not from the previous turn's end,
        // which would charge it the user's thinking time.
        //
        // Two lists because the two events are not equally trustworthy.
        // `task_started` fires exactly once per turn. `turn_context` normally
        // follows it within milliseconds, but newer codex re-emits it MID-turn
        // (it carries a `turn_id` and is rewritten when the turn's config
        // changes) — 6 of 495 records across the local rollout corpus. Since a
        // marker only ever moves the boundary forward, a mid-turn one would
        // truncate the reply that follows it and silently drop that slice from
        // the turn's total. So `turn_context` is used only as a fallback, for
        // rollouts old enough to predate `task_started`.
        let mut task_start_markers: Vec<DateTime<Utc>> = Vec::new();
        let mut turn_context_markers: Vec<DateTime<Utc>> = Vec::new();
        let mut context_window_used_tokens: Option<u64> = None;
        // Not every `token_count` reads the context: see
        // `crate::acp::codex_context`, which the live ring shares.
        let mut context_readings = crate::acp::codex_context::ContextReadings::default();
        let mut context_window_max_tokens: Option<u64> = None;
        let mut latest_total_usage: Option<TurnUsage> = None;
        let mut latest_total_tokens: Option<u64> = None;
        // Cumulative `total_token_usage` as of the previous `token_count`, so
        // each event can be reduced to what its own round-trip added.
        let mut previous_total_usage: Option<TurnUsage> = None;
        // Previous `last_token_usage`, used only by the no-total fallback to
        // recognize a restated event.
        let mut previous_last_usage: Option<TurnUsage> = None;
        // Round-trip spend recorded before any assistant message existed to
        // carry it; flushed onto the first one that appears.
        let mut pending_round_usage: Option<TurnUsage> = None;
        // Everything every round-trip reported, kept independently of which
        // message ended up carrying it — see `reconcile_turn_usage`.
        let mut recorded_round_usage = TurnUsage::default();

        let mut first_timestamp: Option<DateTime<Utc>> = None;
        let mut last_timestamp: Option<DateTime<Utc>> = None;

        // Agent subagent tracking (spawn_agent / wait_agent / close_agent).
        //
        // Capsule model (mirrors the live frontend, see collab-tool.ts):
        //   - spawn_agent → an "Agent" execution capsule (this file + nested
        //     stats from `agent-<id>.jsonl`). Shows the task + process; it does
        //     NOT carry the final result text (that lives in the wait capsule).
        //   - wait_agent  → a synthesized `collab_agent` capsule per wait, built
        //     from THAT wait's own `output.status` (the agents it returned). The
        //     full result text is shown here, via the same `CollabAgentCard` the
        //     live `wait` capsule uses. codex returns each agent's result in
        //     exactly one wait, so wait capsules never overlap.
        //   - close_agent → folded into the execution capsule (no own capsule);
        //     its result is only a fallback for agents never waited on.
        // codex-acp 1.0.1 (#223) maps `collabAgentToolCall` onto live ACP
        // `tool_call`s; 1.1.3+ (#304) additionally emits `subAgentActivity` as a
        // SEPARATE live `tool_call` (`_meta.codex.subagent`), but codeg
        // suppresses it (redundant with the collab capsule — see
        // `is_codex_subagent_activity`) and it carries no transcript content, so
        // the nested `agent-<id>.jsonl` stats still only exist on history reload.
        // Live and reconstructed capsules never double-render (live during
        // streaming, this on reload).
        let mut spawn_agent_call_ids: HashSet<String> = HashSet::new();
        let mut agent_id_to_spawn_call_id: HashMap<String, String> = HashMap::new();
        // `agent_path` ("/root/history_limits") → the thread id currently
        // answering to it. Last write wins, which is exactly the pairing an
        // inter-agent message needs: the child that replies is whichever one
        // that path most recently named. A path CAN be reused (codex re-spawns
        // under the same task name), so a first-wins map would misfile the
        // second child's result onto the first child's capsule.
        let mut agent_path_to_thread_id: HashMap<String, String> = HashMap::new();
        // Terminal `SubAgentActivity` kinds by thread id (`completed` /
        // `interrupted`). Stamped onto the launch capsule so it stops reading as
        // "codex will never report this child again" once codex has.
        let mut agent_terminal_kind: HashMap<String, String> = HashMap::new();
        // Result text used to FILL the execution capsule only as a fallback for
        // agents that were never returned by a wait (keyed by agent_id). Filled
        // from close_agent's `previous_status`.
        let mut agent_fallback_results: HashMap<String, String> = HashMap::new();
        // Agents whose result was already shown in a wait capsule — their
        // execution capsule must NOT also show the result (no duplication).
        let mut agent_waited: HashSet<String> = HashSet::new();
        // Agents that ended in an error state (see `is_error_collab_status`:
        // errored/failed/notFound) in any wait or close — used to mark the
        // execution capsule as failed (live parity).
        let mut agent_errored: HashSet<String> = HashSet::new();
        let mut wait_agent_call_ids: HashSet<String> = HashSet::new();
        let mut list_agents_call_ids: HashSet<String> = HashSet::new();
        let mut close_agent_call_ids: HashSet<String> = HashSet::new();
        let mut close_agent_targets: HashMap<String, String> = HashMap::new();
        let mut active_agent_count: u32 = 0;
        let mut call_id_tool_names: HashMap<String, String> = HashMap::new();
        // Code-mode scripts (`custom_tool_call` named `exec`) awaiting their
        // output. A placeholder script card is pushed when the call is read, so
        // an interrupted turn still shows it in place; the output arm rewrites
        // that message's blocks once it knows how many `text()` chunks came
        // back. See `parsers/codex_code_mode.rs`.
        let mut pending_exec_scripts: HashMap<String, (usize, CodeModeScript)> = HashMap::new();
        // App-server persists each MCP call executed inside a code-mode script
        // as a semantic `item_completed.McpToolCall`. Keep those authoritative
        // ids/results with the sole open script; its output can then replace the
        // wrapper even when several results were printed as one JSON chunk.
        let mut completed_mcp_by_exec: HashMap<String, Vec<CompletedMcpCall>> = HashMap::new();
        // `exec_command` call_id → the command it ran, and the background shell
        // sessions that command's output announced (`session id → command`).
        // A later `wait` / `write_stdin` carries only the session id, so this is
        // what lets its card be titled by the command it is waiting on instead
        // of a bare `wait`. See `extract_shell_session_ids`.
        let mut call_id_commands: HashMap<String, String> = HashMap::new();
        let mut shell_sessions: HashMap<String, ShellSession> = HashMap::new();
        // Calls that only collect more of an earlier command's output: poll
        // `tool_use_id` → `tool_use_id` of the card holding that command. The
        // fold happens once parsing is done (`fold_shell_session_polls`), which
        // is what lets a poll written inside a code-mode script be folded on the
        // same footing as one codex called directly.
        let mut poll_origins: HashMap<String, String> = HashMap::new();
        // Scripts that answered `Script running with cell ID N` — they have NOT
        // finished, and the `wait` that later collects cell N carries their own
        // return value. Keyed by cell id so a parallel batch of waits each folds
        // into the right script no matter what order the records interleave in.
        let mut deferred_scripts: HashMap<String, DeferredScript> = HashMap::new();
        let mut deferred_waits: HashMap<String, String> = HashMap::new();
        // Codex 0.129+ writes a generated image both as `event_msg.image_generation_end`
        // and as `response_item.image_generation_call`, sharing the same call_id/id.
        // Emit at most one ContentBlock::Image per id to avoid duplicate display.
        let mut emitted_image_ids: HashSet<String> = HashSet::new();
        // Streaming reasoning buffer, held open across a whole reasoning RUN —
        // every section codex wrote before the next visible record (a tool call,
        // a message, …). Live streams such a run as one growing thought, so
        // history has to as well, and neither of codex's two on-disk records is
        // that run on its own:
        //   - `event_msg.agent_reasoning` — one per section;
        //   - `response_item.reasoning.summary` — the sections of ONE model
        //     response, grouped. A long run spans several responses, so codex
        //     writes several of these back to back (up to 28 in real rollouts),
        //     and emitting a card per record is what tore one thought into a
        //     column of 思考 cards.
        // codex 0.149+ writes no section events: it announces each item with an
        // `event_msg.item_completed` restating the summary that follows. That
        // ends the run below like any record that is not reasoning, and so does
        // everything else that turns out to render nothing — the cards they
        // leave touching are joined once the transcript is assembled
        // (`merge_touching_thinking_cards`).
        // So `grouped_reasoning` accumulates the settled text of the run while
        // `pending_reasoning` holds the section events not yet restated by a
        // grouped summary; the summary supersedes them (it is the same text,
        // grouped) and joins the run. Both are flushed as ONE Thinking block
        // when the run ends, so an interrupted rollout that never wrote its
        // summary still keeps its streaming reasoning. `pending_reasoning_ts`
        // stamps that block with the run's last reasoning record.
        let mut grouped_reasoning: Vec<String> = Vec::new();
        let mut pending_reasoning: Vec<String> = Vec::new();
        let mut pending_reasoning_ts: Option<DateTime<Utc>> = None;

        // Plan mode. `turn_context.collaboration_mode.mode` is the structured,
        // per-turn record of which mode produced the turn, and its flip out of
        // `plan` is the only in-band evidence that the user approved the plan
        // (see the `user_message` arm).
        //
        // The two plan flags are deliberately separate. `pending_plan_twin` is
        // a PAIRING slot — filled only by an `item_completed` announcement,
        // claimed only by the next `<proposed_plan>` record — and conflating it
        // with "a plan exists" would make two identical plan bodies collapse
        // into one. `plan_rendered` is the approval guard, so a stray approval
        // can never emit a decision marker with no plan above it.
        let mut collaboration_mode_is_plan = false;
        let mut plan_approval_expected = false;
        let mut pending_plan_twin: Option<(usize, String)> = None;
        let mut plan_rendered = false;

        // `response_item.message` records held back until EOF, when their turn
        // segment's canonical-channel coverage is known. See
        // [`ResponseItemPromotion`] for why the gate is per-segment.
        let mut promotion = ResponseItemPromotion::new();
        let mut pending_promotions: Vec<PendingPromotedMessage> = Vec::new();
        // Ordinals of the two in-loop decisions that promoted records can
        // legitimately override, recorded so they can be replayed POSITIONALLY
        // once the survivors are known — never "does a user exist anywhere",
        // which would cancel a valid goal opener answered by a later prompt.
        let mut first_goal_ordinal: Option<u64> = None;
        let mut title_source_ordinal: Option<u64> = None;
        // codex's own thread name outranks any prompt-derived title, and its arm
        // assigns unconditionally (newest wins), so it needs its own flag rather
        // than an ordinal comparison.
        let mut title_from_thread_name = false;
        // Index in `messages` of the compaction divider the next
        // `item_completed.ContextCompaction` names (see the `compacted` arm).
        let mut unnamed_compaction_divider: Option<usize> = None;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let msg_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

            // Fed EVERY record, before the parser's own handling: segment
            // boundaries, canonical-channel coverage and compaction adjacency
            // are all positional properties of the raw stream.
            let record_ordinal =
                promotion.note_record(msg_type, promotion_payload_type(msg_type, &value));

            if let Some(ts_str) = value.get("timestamp").and_then(|t| t.as_str()) {
                if let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                    if first_timestamp.is_none() {
                        first_timestamp = Some(ts);
                    }
                    last_timestamp = Some(ts);
                }
            }

            match msg_type {
                "session_meta" => {
                    // First header wins for every identity field — see the same
                    // latch in `parse_jsonl_summary`: the parent header that
                    // follows a forked child's own (replayed inline, or spliced
                    // in by `rollout_lines`) carries the PARENT's cwd and branch.
                    if let Some(payload) = value.get("payload").filter(|_| !session_header_seen) {
                        session_header_seen = true;
                        cwd = payload
                            .get("cwd")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        parent_id = codex_parent_thread_id(payload);
                        git_branch = payload
                            .get("git")
                            .and_then(|g| g.get("branch"))
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string());
                    }
                }
                "turn_context" => {
                    // A new API turn means any prior agent lifecycle is complete.
                    active_agent_count = 0;
                    if model.is_none() {
                        model = value
                            .get("payload")
                            .and_then(|p| p.get("model"))
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                    }
                    // Approving a plan ends Plan mode: codex opens the very next
                    // turn with a non-`plan` collaboration mode and prompts
                    // ITSELF with `CODEX_PLAN_APPROVAL_PROMPT`. Arm the one-shot
                    // flag the `user_message` arm consumes. A rollout predating
                    // Plan mode reports no mode at all and so never arms.
                    //
                    // Only the flip arms, and only re-entering `plan` disarms —
                    // a REPEATED non-plan context must leave the arm standing.
                    // Newer codex re-emits `turn_context` mid-turn (the same
                    // reason `ResponseItemPromotion` prefers `task_started` for
                    // segmentation), and rewriting the flag on every context
                    // would let a second `default` context between the flip and
                    // the prompt silently restore the bug this filter fixes.
                    if let Some(mode) = turn_collaboration_mode(&value) {
                        let is_plan = mode == "plan";
                        if is_plan {
                            plan_approval_expected = false;
                        } else if collaboration_mode_is_plan {
                            plan_approval_expected = true;
                        }
                        collaboration_mode_is_plan = is_plan;
                    }
                    if let Some(ts) = parse_codex_timestamp(&value) {
                        push_turn_start(&mut turn_context_markers, ts);
                    }
                }
                "event_msg" => {
                    if let Some(payload) = value.get("payload") {
                        let payload_type =
                            payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

                        let timestamp = parse_codex_timestamp(&value).unwrap_or_else(Utc::now);

                        // A new reasoning section keeps the run open; `token_count`
                        // is metadata with no visible message and never splits one.
                        // Anything else closes the run — emit the reasoning gathered
                        // so far as one card, here, so it can't be reordered behind
                        // this event.
                        if payload_type != "agent_reasoning" && payload_type != "token_count" {
                            flush_pending_reasoning(
                                &mut messages,
                                &mut grouped_reasoning,
                                &mut pending_reasoning,
                                pending_reasoning_ts,
                            );
                        }

                        // codex 0.147 stopped returning the sub-agent's id from
                        // `spawn_agent` (its output is empty, or just
                        // `{"task_name":"/root/pnpm_build"}`). `SubAgentActivity`
                        // is now the only place the parent's rollout names the
                        // child thread, and it correlates back by carrying the
                        // spawn's own `call_id`.
                        //
                        // Read BEFORE the match, not as an arm of it: 0.153.4
                        // moved these records inside `item_completed`, whose arm
                        // `continue`s on anything that is not a plan document.
                        if let Some(activity) =
                            codex_subagent_activity_record(payload_type, payload)
                        {
                            if let Some(path) = activity.agent_path {
                                agent_path_to_thread_id
                                    .insert(path.to_string(), activity.thread_id.to_string());
                            }
                            // Only a launch names the capsule to attach to; a
                            // terminal record carries a synthetic id of its own.
                            if let Some(call_id) = activity
                                .call_id
                                .filter(|id| spawn_agent_call_ids.contains(*id))
                            {
                                agent_id_to_spawn_call_id
                                    .entry(activity.thread_id.to_string())
                                    .or_insert_with(|| call_id.to_string());
                            }
                            match activity.kind {
                                "completed" | "interrupted" => {
                                    agent_terminal_kind.insert(
                                        activity.thread_id.to_string(),
                                        activity.kind.to_string(),
                                    );
                                }
                                // A terminal child can be brought back
                                // (`resumeAgent` / `followup_task`), and it
                                // announces that with a fresh `started`. Clear
                                // the old outcome rather than leave the capsule
                                // claiming a run that has since resumed. The
                                // live path self-corrects the same way: a new
                                // launch replaces the remembered input, which
                                // carries no state key.
                                "started" => {
                                    agent_terminal_kind.remove(activity.thread_id);
                                }
                                _ => {}
                            }
                        }

                        match payload_type {
                            "task_started" => {
                                if context_window_max_tokens.is_none() {
                                    context_window_max_tokens = payload
                                        .get("model_context_window")
                                        .and_then(|v| v.as_u64());
                                }
                                // The one marker codex writes exactly once per
                                // turn; it precedes `turn_context` and the
                                // prompt.
                                if let Some(ts) = parse_codex_timestamp(&value) {
                                    push_turn_start(&mut task_start_markers, ts);
                                }
                                context_readings.turn_started();
                            }
                            "user_message" => {
                                active_agent_count = 0;
                                let text = payload
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                // Plan-mode approval, not a prompt. codex writes
                                // its own follow-up as an ordinary user message,
                                // structurally identical to typed input, so
                                // rendering it splits ONE plan interaction into two
                                // user turns (live keeps both halves inside a
                                // single `session/prompt`). Both signals are
                                // required — the mode flip this turn AND codex's
                                // fixed wording — so a user who literally types
                                // that sentence still gets their bubble.
                                //
                                // The decision is not dropped, it MOVES: the plan
                                // call settles with codex-acp's own approval
                                // wording, which is what renders the live
                                // <PlanModeCard>'s "已同意" marker.
                                //
                                // Compared verbatim, NOT trimmed: codex writes
                                // this sentence with no surrounding whitespace
                                // (18/18 occurrences across the local corpus), so
                                // trimming would only widen the filter onto text a
                                // person could have typed.
                                let approval_armed = std::mem::take(&mut plan_approval_expected);
                                if approval_armed
                                    && text == CODEX_PLAN_APPROVAL_PROMPT
                                    && std::mem::take(&mut plan_rendered)
                                {
                                    push_plan_review_marker(&mut messages, timestamp);
                                    continue;
                                }

                                let normalized = normalize_user_text(&text);
                                if title.is_none() {
                                    title = extract_codex_title_candidate(&normalized, true);
                                    if title.is_some() {
                                        title_source_ordinal = Some(record_ordinal);
                                    }
                                }
                                let mut blocks: Vec<ContentBlock> = Vec::new();
                                if !normalized.is_empty() {
                                    blocks.push(ContentBlock::Text { text: normalized });
                                }

                                if let Some(images) =
                                    payload.get("images").and_then(|v| v.as_array())
                                {
                                    for image in images {
                                        let Some(raw) = image.as_str() else {
                                            continue;
                                        };
                                        let Some((mime_type, data)) = parse_data_uri_image(raw)
                                        else {
                                            continue;
                                        };
                                        blocks.push(ContentBlock::Image {
                                            data,
                                            mime_type,
                                            uri: None,
                                        });
                                    }
                                }

                                if blocks.is_empty() && contains_only_internal_agent_routes(&text) {
                                    continue;
                                }

                                if blocks.is_empty() {
                                    blocks.push(ContentBlock::Text {
                                        text: "Attached resources".to_string(),
                                    });
                                }

                                if should_skip_duplicate_user_message(&messages, &blocks, timestamp)
                                {
                                    continue;
                                }

                                messages.push(UnifiedMessage {
                                    id: format!("user-{}", messages.len()),
                                    role: MessageRole::User,
                                    content: blocks,
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: Some(timestamp),
                                agent_message_id: None,
                                });
                            }
                            "agent_message" => {
                                // Parent narration is emitted even while a
                                // sub-agent is active (active_agent_count > 0).
                                // Every `event_msg.agent_message` is the
                                // parent's: a sub-agent's own work goes to a
                                // transcript of its own, never into this channel
                                // (verified across 180 real rollouts: 0
                                // sub-agent leaks). The old
                                // `active_agent_count == 0` guard wrongly dropped
                                // the parent's between-capsule narration — and,
                                // when no close_agent ran (active never returns to
                                // 0), even the final answer. Images keep their own
                                // guard (see image_generation arms).
                                //
                                // A sub-agent CAN speak into the parent's
                                // rollout, but on a different channel: the
                                // addressed `response_item.agent_message`
                                // handled below. That one carries `author` /
                                // `recipient` and a `message` ARRAY, so it
                                // cannot be confused with this shape's bare
                                // `message` string.
                                let text = payload
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                messages.push(UnifiedMessage {
                                    id: format!("assistant-{}", messages.len()),
                                    role: MessageRole::Assistant,
                                    content: vec![ContentBlock::Text { text }],
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: Some(timestamp),
                                agent_message_id: None,
                                });
                            }
                            "thread_goal_updated" => {
                                // codex-acp v1.1.0 (#263) routes live goals through
                                // `session_info_update`; the CLI has always persisted
                                // each `/goal` transition to the rollout as
                                // `event_msg.thread_goal_updated.goal`. Synthesize the
                                // same canonical create_goal/update_goal
                                // tool_use+tool_result the live path emits (shared
                                // mapping in `crate::acp::codex_goal`) so a reloaded
                                // conversation renders goal cards identical to live —
                                // history that never surfaced goals before.
                                if let Some(marker) = payload.get("goal").and_then(|goal| {
                                    crate::acp::codex_goal::next_goal_marker(
                                        &mut codex_open_goal,
                                        goal,
                                    )
                                }) {
                                    // Remember the first opened goal's objective for
                                    // the post-parse leading-user-message fallback (see
                                    // `first_goal_objective`). Deferred, not synthesized
                                    // here, so it can't interfere with duplicate
                                    // suppression of a later real user message.
                                    if marker.tool_name == "create_goal"
                                        && first_goal_objective.is_none()
                                    {
                                        // Positional: the goal opened the session iff no
                                        // real user turn exists yet.
                                        goal_opens_session = !messages
                                            .iter()
                                            .any(|m| matches!(m.role, MessageRole::User));
                                        // Claim the title from the objective HERE, in
                                        // stream order, when the goal is the opener — so
                                        // a LATER `user_message` (its own guard is
                                        // `title.is_none()`) can't steal it, while an
                                        // EARLIER real user (older codex) or a native
                                        // `thread_name_updated` still wins.
                                        if goal_opens_session && title.is_none() {
                                            title = extract_codex_title_candidate(
                                                &marker.objective,
                                                true,
                                            );
                                            if title.is_some() {
                                                title_source_ordinal = Some(record_ordinal);
                                            }
                                        }
                                        first_goal_objective = Some(marker.objective.clone());
                                        first_goal_ordinal = Some(record_ordinal);
                                    }
                                    // Occurrence id from the message index — unique
                                    // per goal event, stable across reparse, and
                                    // shared by this event's ToolUse + ToolResult.
                                    let id = crate::acp::codex_goal::goal_tool_call_id(
                                        messages.len() as u64,
                                    );
                                    messages.push(UnifiedMessage {
                                        id: format!("tool-{}", messages.len()),
                                        role: MessageRole::Assistant,
                                        content: vec![
                                            ContentBlock::ToolUse {
                                                tool_use_id: Some(id.clone()),
                                                tool_name: marker.tool_name.to_string(),
                                                input_preview: Some(marker.input_json),
                                                status: None,
                                                meta: None,
                                            },
                                            ContentBlock::ToolResult {
                                                tool_use_id: Some(id),
                                                output_preview: Some(marker.output_json),
                                                is_error: false,
                                                agent_stats: None,
                                                images: Vec::new(),
                                            },
                                        ],
                                        timestamp,
                                        usage: None,
                                        duration_ms: None,
                                        model: None,
                                        completed_at: Some(timestamp),
                                    agent_message_id: None,
                                    });
                                }
                            }
                            "thread_name_updated" => {
                                // Codex's native thread name — adopt it as the
                                // auto-title (parity with Claude `aiTitle`, Gemini
                                // `update_topic`, OpenCode `session.title`). Newest
                                // non-empty wins, overriding the first-prompt
                                // fallback; `refresh_auto_title`'s `title_locked`
                                // guard still respects a manual rename.
                                // Rollout persists `thread_name` (snake_case); the
                                // live ACP notification uses `threadName`. Accept
                                // both so the parser is robust to either source.
                                if let Some(name) = payload
                                    .get("thread_name")
                                    .or_else(|| payload.get("threadName"))
                                    .or_else(|| payload.get("name"))
                                    .and_then(|n| n.as_str())
                                    .and_then(codex_thread_title)
                                {
                                    title = Some(name);
                                    title_from_thread_name = true;
                                }
                            }
                            "task_complete" => {
                                // A FAILED turn closes on the error that ended
                                // it, the record's only trace of the failure
                                // (codex persists no `error` event). An
                                // interrupted turn closes on `turn_aborted`
                                // instead and is no failure.
                                if let Some(failure) =
                                    codex_turn_failure(payload).and_then(|message| {
                                        super::turn_error_message(
                                            format!("turn-error-{}", messages.len()),
                                            &message,
                                            timestamp,
                                        )
                                    })
                                {
                                    messages.push(failure);
                                }
                            }
                            "item_completed" => {
                                // The compaction the last `compacted` record
                                // drew a divider for, now under the id codex-acp
                                // gave the live divider (`compactionId` IS this
                                // item id): the two are then one event to
                                // anything that meets both, and the frontend
                                // keeps one (`dedupeCompactionItems`).
                                if let Some(item_id) = context_compaction_item_id(payload) {
                                    if let Some(divider) = unnamed_compaction_divider
                                        .take()
                                        .and_then(|at| messages.get_mut(at))
                                    {
                                        rename_compaction_divider(divider, item_id);
                                    }
                                    continue;
                                }
                                if let Some(call) = completed_mcp_call(payload) {
                                    let exec_id = if deferred_scripts.is_empty()
                                        && pending_exec_scripts.len() == 1
                                    {
                                        pending_exec_scripts
                                            .keys()
                                            .next()
                                            .expect("one pending exec")
                                            .clone()
                                    } else if pending_exec_scripts.is_empty() {
                                        let mut deferred_exec_ids = deferred_scripts
                                            .values()
                                            .map(|script| script.call_id.as_str());
                                        let Some(exec_id) = deferred_exec_ids.next() else {
                                            continue;
                                        };
                                        if deferred_exec_ids.all(|id| id == exec_id) {
                                            exec_id.to_string()
                                        } else {
                                            continue;
                                        }
                                    } else {
                                        continue;
                                    };
                                    completed_mcp_by_exec
                                        .entry(exec_id)
                                        .or_default()
                                        .push(call);
                                    continue;
                                }
                                // Plan mode's finished plan document. This is the
                                // ONLY place a plan turn speaks on the canonical
                                // event channel — codex publishes the plan here
                                // INSTEAD of as `agent_message` — so without this
                                // arm the whole turn renders as nothing but its
                                // reasoning (issue: plan card vanishes on reload).
                                //
                                // Re-wrapped in codex's own `<proposed_plan>` tags
                                // so it lands on the exact adapter path the live
                                // stream uses (`expandProposedPlanText` → plan
                                // card). The assistant `response_item` twin, which
                                // carries the same body PLUS any surrounding prose,
                                // upgrades this message in place when it arrives.
                                let Some(plan) = completed_plan_item_text(payload) else {
                                    continue;
                                };
                                if active_agent_count > 0 {
                                    continue;
                                }
                                messages.push(UnifiedMessage {
                                    id: format!("assistant-plan-{}", messages.len()),
                                    role: MessageRole::Assistant,
                                    content: vec![ContentBlock::Text {
                                        text: format!(
                                            "{PROPOSED_PLAN_OPEN}\n{plan}\n{PROPOSED_PLAN_CLOSE}"
                                        ),
                                    }],
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: Some(timestamp),
                                    agent_message_id: None,
                                });
                                pending_plan_twin = Some((messages.len() - 1, plan.to_string()));
                                plan_rendered = true;
                            }
                            "agent_reasoning" => {
                                // Buffer this streaming reasoning section into the
                                // open run. The grouped
                                // `response_item.reasoning.summary` (parsed in the
                                // `response_item` match below) normally arrives right
                                // after the section events and supersedes the buffer
                                // with the same text; either way the run renders as
                                // ONE 思考 card (live parity) instead of one card per
                                // section, and if no grouped summary arrives
                                // (interrupted/older rollouts) nothing is lost.
                                let text = payload
                                    .get("text")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("");
                                if !text.trim().is_empty() {
                                    pending_reasoning.push(text.to_string());
                                    pending_reasoning_ts = Some(timestamp);
                                }
                            }
                            "image_generation_end" => {
                                if active_agent_count > 0 {
                                    continue;
                                }
                                let call_id = payload
                                    .get("call_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let result =
                                    payload.get("result").and_then(|v| v.as_str()).unwrap_or("");
                                if result.is_empty() {
                                    continue;
                                }
                                if !call_id.is_empty() && emitted_image_ids.contains(&call_id) {
                                    continue;
                                }
                                let mime_type = payload
                                    .get("mime_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("image/png")
                                    .to_string();
                                let uri = payload
                                    .get("saved_path")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let revised_prompt = payload
                                    .get("revised_prompt")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .filter(|s| !s.trim().is_empty());
                                messages.push(UnifiedMessage {
                                    id: format!("assistant-imagegen-{}", messages.len()),
                                    role: MessageRole::Assistant,
                                    content: vec![ContentBlock::ImageGeneration {
                                        revised_prompt,
                                        image: Some(ImageData {
                                            data: result.to_string(),
                                            mime_type,
                                            uri,
                                        }),
                                    }],
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: Some(timestamp),
                                agent_message_id: None,
                                });
                                if !call_id.is_empty() {
                                    emitted_image_ids.insert(call_id);
                                }
                            }
                            "token_count" => {
                                if let Some(info) = payload.get("info") {
                                    if let Some(total_usage_payload) = info.get("total_token_usage")
                                    {
                                        if let Some(total_usage) =
                                            extract_turn_usage_from_codex_usage(total_usage_payload)
                                        {
                                            latest_total_usage = Some(total_usage);
                                        }
                                        if let Some(total_tokens) =
                                            extract_total_tokens_from_usage(total_usage_payload)
                                        {
                                            latest_total_tokens = Some(total_tokens);
                                        }
                                    }

                                    if let Some(used) =
                                        extract_context_window_used_tokens_from_token_count_info(
                                            info,
                                        )
                                    {
                                        if context_readings.admit(used) {
                                            context_window_used_tokens = Some(used);
                                        }
                                    }

                                    let context_window =
                                        info.get("model_context_window").and_then(|v| v.as_u64());
                                    if context_window.is_some() {
                                        context_window_max_tokens = context_window;
                                    }

                                    if !info.is_null() {
                                        // What this round-trip spent. Preferred
                                        // as the rise in the session's own
                                        // cumulative counter; `last_token_usage`
                                        // is the fallback for transcripts that
                                        // report no total, and there a repeat of
                                        // the previous event is dropped since it
                                        // restates one call rather than adding
                                        // another.
                                        let round = match info.get("total_token_usage") {
                                            Some(total_payload) => {
                                                let total = codex_usage_counters(total_payload);
                                                let round = codex_round_usage(
                                                    previous_total_usage.as_ref(),
                                                    &total,
                                                );
                                                previous_total_usage = Some(total);
                                                round
                                            }
                                            None => match info.get("last_token_usage") {
                                                Some(last_payload) => {
                                                    let last = codex_usage_counters(last_payload);
                                                    let repeated = previous_last_usage
                                                        .as_ref()
                                                        .is_some_and(|prev| prev == &last);
                                                    previous_last_usage = Some(last.clone());
                                                    if repeated {
                                                        TurnUsage::default()
                                                    } else {
                                                        // Carry this round into the
                                                        // cumulative baseline too.
                                                        // A transcript that mixes
                                                        // both shapes would
                                                        // otherwise count it twice:
                                                        // once here, and again
                                                        // inside the next
                                                        // total-bearing event's
                                                        // delta, which is measured
                                                        // from a baseline that
                                                        // never learned about it.
                                                        let base = previous_total_usage
                                                            .clone()
                                                            .unwrap_or_default();
                                                        previous_total_usage =
                                                            Some(codex_usage_add(&base, &last));
                                                        last
                                                    }
                                                }
                                                None => TurnUsage::default(),
                                            },
                                        };

                                        if !codex_usage_is_zero(&round) {
                                            recorded_round_usage =
                                                codex_usage_add(&recorded_round_usage, &round);
                                            // Every round of a turn belongs to
                                            // the assistant message it worked
                                            // for, so they accumulate onto it
                                            // rather than the first one winning
                                            // and the rest being dropped. A
                                            // round that ran before any
                                            // assistant message (the model went
                                            // straight to a tool call) waits
                                            // here for one to arrive — 3 % of
                                            // all recorded spend, previously
                                            // discarded outright.
                                            pending_round_usage = Some(match pending_round_usage {
                                                Some(ref pending) => {
                                                    codex_usage_add(pending, &round)
                                                }
                                                None => round,
                                            });
                                        }
                                        // A `token_count` that lands INSIDE an open
                                        // reasoning run reports what the response
                                        // that produced that reasoning spent, and
                                        // the run's card has not been emitted yet.
                                        // Attaching now would bill the round to the
                                        // previous turn, so let it keep waiting for
                                        // the card the run is still gathering.
                                        let last_assistant = if grouped_reasoning.is_empty()
                                            && pending_reasoning.is_empty()
                                        {
                                            messages.iter_mut().rev().find(|m| bills_codex_usage(m))
                                        } else {
                                            None
                                        };
                                        if let (Some(pending), Some(last_msg)) =
                                            (pending_round_usage.clone(), last_assistant)
                                        {
                                            last_msg.usage = Some(match last_msg.usage {
                                                Some(ref existing) => {
                                                    codex_usage_add(existing, &pending)
                                                }
                                                None => pending,
                                            });
                                            pending_round_usage = None;
                                        }
                                    }
                                }
                                // Durations are NOT derived here. `token_count`
                                // fires once per model request, so measuring
                                // turn_context → token_count restated the whole
                                // elapsed turn on every sub-turn; the UI sums
                                // sub-turns into one card, which multiplied a
                                // reply's reported time several-fold. See
                                // `backfill_turn_durations`, applied after
                                // grouping, which partitions the turn instead.
                            }
                            _ => {}
                        }
                    }
                }
                "response_item" => {
                    if let Some(payload) = value.get("payload") {
                        let payload_type =
                            payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let timestamp = parse_codex_timestamp(&value).unwrap_or_else(Utc::now);

                        // A `reasoning` item joins the open run (handled in its arm).
                        // Any other response item closes it — emit the reasoning
                        // gathered so far as one card, here, so it can't be reordered
                        // behind this item.
                        let before_flush = messages.len();
                        if payload_type != "reasoning" {
                            flush_pending_reasoning(
                                &mut messages,
                                &mut grouped_reasoning,
                                &mut pending_reasoning,
                                pending_reasoning_ts,
                            );
                        }
                        // The card this record just closed, if any. When the
                        // record is a compaction's handoff, that card is the
                        // compaction's own thinking (see the `compacted` arm).
                        let closed_reasoning_card =
                            (messages.len() > before_flush).then(|| messages.len() - 1);

                        match payload_type {
                            // A sub-agent reporting back. Distinct from the
                            // `event_msg.agent_message` arm above (which is the
                            // PARENT speaking, and carries a bare `message`
                            // string): this one is addressed
                            // `author` → `recipient` and wraps its body in
                            // codex's inter-agent envelope.
                            //
                            // It has no `item_completed` twin — codex publishes
                            // no ThreadItem for it — so codex-acp never sees it
                            // and it cannot arrive live. The rollout is the only
                            // place a child's report exists, which is why an
                            // otherwise-complete team run used to show nothing
                            // at all of what its sub-agents concluded.
                            //
                            // Not emitted as a message of its own: it belongs to
                            // the child, not the parent's narration. It is filed
                            // by thread id and the back-patch at the end of the
                            // parse folds it into that child's launch capsule,
                            // through the same `agent_fallback_results` channel
                            // the legacy `close_agent` result uses.
                            "agent_message" => {
                                if let Some((author, body)) =
                                    codex_inter_agent_final_answer(payload)
                                {
                                    if let Some(thread_id) = agent_path_to_thread_id.get(author) {
                                        agent_fallback_results
                                            .insert(thread_id.clone(), body);
                                    }
                                }
                            }
                            "reasoning" => {
                                // Codex records one model response's reasoning as a
                                // `summary` array of `{type:"summary_text", text}`
                                // parts — one part per section — grouping the same
                                // sections the streaming `event_msg.agent_reasoning`
                                // events carry one-by-one (buffered in
                                // `pending_reasoning`). So this item settles the
                                // buffer, superseding it; it does NOT end the run,
                                // because the next record may be one more of these
                                // (a run spanning several model responses) and live
                                // shows that as a single growing thought. The card
                                // is emitted when something visible finally closes
                                // the run.
                                //
                                // An empty summary (encrypted-only reasoning, the
                                // common case) restates nothing, so it must not
                                // clear the buffer — it only seals what is buffered
                                // so far, keeping those sections out of reach of a
                                // LATER summary that never covered them.
                                let text = payload
                                    .get("summary")
                                    .and_then(|s| s.as_array())
                                    .map(|parts| {
                                        parts
                                            .iter()
                                            .filter_map(|p| {
                                                p.get("text").and_then(|t| t.as_str())
                                            })
                                            .filter(|t| !t.trim().is_empty())
                                            .collect::<Vec<_>>()
                                            .join("\n\n")
                                    })
                                    .unwrap_or_default();
                                if !text.is_empty() {
                                    pending_reasoning.clear();
                                    grouped_reasoning.push(text);
                                    pending_reasoning_ts = Some(timestamp);
                                } else {
                                    grouped_reasoning.append(&mut pending_reasoning);
                                }
                            }
                            "function_call" | "custom_tool_call" => {
                                let tool_use_id = payload
                                    .get("call_id")
                                    .or_else(|| payload.get("tool_call_id"))
                                    .or_else(|| payload.get("id"))
                                    .and_then(|id| id.as_str())
                                    .map(|s| s.to_string());
                                let raw_tool_name = payload
                                    .get("name")
                                    .or_else(|| payload.get("tool_name"))
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown");

                                // A `wait` collecting a code-mode script that
                                // has not finished. Matched by cell id, never by
                                // adjacency — parallel calls interleave freely.
                                let deferred_cell = if raw_tool_name == "wait" {
                                    parse_codex_json_arg(payload)
                                        .as_ref()
                                        .and_then(|a| a.as_object())
                                        .and_then(shell_session_id)
                                        .filter(|cell| deferred_scripts.contains_key(cell))
                                } else {
                                    None
                                };

                                match raw_tool_name {
                                    // No card of its own: the output it is about
                                    // to collect IS the parked script's return
                                    // value, and the output arm writes it back
                                    // onto that script's cards.
                                    _ if deferred_cell.is_some() => {
                                        if let (Some(id), Some(cell)) = (tool_use_id, deferred_cell)
                                        {
                                            deferred_waits.insert(id, cell);
                                        }
                                    }
                                    "spawn_agent" => {
                                        let args = parse_codex_json_arg(payload);
                                        // codex 0.147's team-of-agents renamed the
                                        // label: the old `agent_type` became
                                        // `task_name` (`pnpm_build`). Reading only
                                        // the old key left every capsule titled
                                        // "agent".
                                        let agent_type = args
                                            .as_ref()
                                            .and_then(|a| {
                                                a.get("agent_type").or_else(|| a.get("task_name"))
                                            })
                                            .and_then(|v| v.as_str())
                                            .filter(|s| !s.trim().is_empty())
                                            .unwrap_or("agent");
                                        // Same release made the hand-off message an
                                        // encrypted envelope; rendering it verbatim
                                        // filled the capsule's title and prompt with
                                        // a wall of base64 (see `is_encrypted_envelope`).
                                        let message = args
                                            .as_ref()
                                            .and_then(|a| a.get("message"))
                                            .and_then(|v| v.as_str())
                                            .filter(|m| !is_encrypted_envelope(m))
                                            .unwrap_or("");
                                        let description =
                                            truncate_str(message.lines().next().unwrap_or(""), 60);

                                        if let Some(ref id) = tool_use_id {
                                            spawn_agent_call_ids.insert(id.clone());
                                        }
                                        active_agent_count += 1;

                                        let mut agent_input = serde_json::json!({
                                            "subagent_type": agent_type,
                                            "prompt": message,
                                            "description": description,
                                        });
                                        // The 0.147 shape (`task_name`, no
                                        // `agent_type`) is the one where this
                                        // capsule is ONLY a launch: there is no
                                        // wait/close capsule to carry the result,
                                        // so the card must not read as "the
                                        // sub-agent finished". Legacy spawns keep
                                        // their old meaning and no marker.
                                        if is_native_team_spawn(args.as_ref()) {
                                            if let Some(obj) = agent_input.as_object_mut() {
                                                obj.insert(
                                                    CODEX_SUBAGENT_LAUNCH_KEY.to_string(),
                                                    serde_json::Value::Bool(true),
                                                );
                                            }
                                        }

                                        messages.push(UnifiedMessage {
                                            id: format!("tool-{}", messages.len()),
                                            role: MessageRole::Assistant,
                                            content: vec![ContentBlock::ToolUse {
                                                tool_use_id,
                                                tool_name: "Agent".to_string(),
                                                input_preview: Some(agent_input.to_string()),
                                                status: None,
                                                meta: None,
                                            }],
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                    }
                                    "wait_agent" => {
                                        if let Some(ref id) = tool_use_id {
                                            wait_agent_call_ids.insert(id.clone());
                                        }
                                    }
                                    "list_agents" => {
                                        if let Some(ref id) = tool_use_id {
                                            list_agents_call_ids.insert(id.clone());
                                        }
                                    }
                                    "close_agent" => {
                                        if let Some(ref id) = tool_use_id {
                                            close_agent_call_ids.insert(id.clone());
                                            let target =
                                                parse_codex_json_arg(payload).and_then(|a| {
                                                    a.get("target")
                                                        .and_then(|v| v.as_str())
                                                        .map(|s| s.to_string())
                                                });
                                            if let Some(target) = target {
                                                close_agent_targets.insert(id.clone(), target);
                                            }
                                        }
                                    }
                                    // Code mode: the whole turn's tool calls are
                                    // wrapped in one JS script. Park a script
                                    // card here and let the output arm replace
                                    // it with the real per-call cards — the
                                    // split depends on how many `text()` chunks
                                    // came back, which is only known then. An
                                    // interrupted turn never gets an output, so
                                    // the placeholder must be pushed now to keep
                                    // its position.
                                    _ if is_code_mode_call(raw_tool_name) => {
                                        let source = payload
                                            .get("input")
                                            .or_else(|| payload.get("arguments"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let script = parse_code_mode_script(source);
                                        if let Some(ref id) = tool_use_id {
                                            pending_exec_scripts.insert(
                                                id.clone(),
                                                (messages.len(), script.clone()),
                                            );
                                        }
                                        messages.push(UnifiedMessage {
                                            id: format!("tool-{}", messages.len()),
                                            role: MessageRole::Assistant,
                                            content: vec![ContentBlock::ToolUse {
                                                tool_use_id,
                                                tool_name: CODEX_SCRIPT_TOOL_NAME.to_string(),
                                                input_preview: Some(script_card_input(
                                                    source,
                                                    script.summary.as_deref(),
                                                    script.call_sites,
                                                )),
                                                status: None,
                                                meta: None,
                                            }],
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                    }
                                    _ => {
                                        if let Some(ref id) = tool_use_id {
                                            call_id_tool_names
                                                .insert(id.clone(), raw_tool_name.to_string());
                                        }
                                        let raw_args = || {
                                            // `send_message` and friends carry a
                                            // sealed inter-agent `message`; show a
                                            // marker instead of half a kilobyte of
                                            // base64 (see `redact_encrypted_args`).
                                            // Every other tool takes the original
                                            // path and renders byte-identically.
                                            if let Some(mut args) = parse_codex_json_arg(payload) {
                                                if redact_encrypted_args(&mut args) {
                                                    return value_to_preview(Some(&args));
                                                }
                                            }
                                            value_to_preview(
                                                payload
                                                    .get("arguments")
                                                    .or_else(|| payload.get("input")),
                                            )
                                        };
                                        let input_preview = if raw_tool_name == "exec_command" {
                                            let cmd = parse_codex_json_arg(payload).and_then(|a| {
                                                a.get("cmd")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                            });
                                            // Remembered so this command can be
                                            // attached to whatever background
                                            // session its output announces.
                                            if let (Some(id), Some(cmd)) =
                                                (tool_use_id.as_ref(), cmd.as_ref())
                                            {
                                                call_id_commands.insert(id.clone(), cmd.clone());
                                            }
                                            cmd.or_else(raw_args)
                                        } else if is_shell_session_tool(raw_tool_name) {
                                            // `wait` / `write_stdin`: carry the
                                            // command that started the session
                                            // they address, when it is known.
                                            let args = parse_codex_json_arg(payload);
                                            note_shell_session_call(
                                                tool_use_id.as_deref(),
                                                args.as_ref(),
                                                &mut shell_sessions,
                                                &mut poll_origins,
                                            );
                                            args.and_then(|args| {
                                                annotate_shell_session_args(&args, &shell_sessions)
                                            })
                                            .or_else(raw_args)
                                        } else {
                                            raw_args()
                                        };
                                        messages.push(UnifiedMessage {
                                            id: format!("tool-{}", messages.len()),
                                            role: MessageRole::Assistant,
                                            content: vec![ContentBlock::ToolUse {
                                                tool_use_id,
                                                tool_name: raw_tool_name.to_string(),
                                                input_preview,
                                                status: None,
                                                meta: None,
                                            }],
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                    }
                                }
                            }
                            "function_call_output" | "custom_tool_call_output" => {
                                let tool_use_id = payload
                                    .get("call_id")
                                    .or_else(|| payload.get("tool_call_id"))
                                    .or_else(|| payload.get("id"))
                                    .and_then(|id| id.as_str())
                                    .map(|s| s.to_string());

                                let is_spawn = tool_use_id
                                    .as_ref()
                                    .is_some_and(|id| spawn_agent_call_ids.contains(id));
                                let is_wait = tool_use_id
                                    .as_ref()
                                    .is_some_and(|id| wait_agent_call_ids.contains(id));
                                let is_list = tool_use_id
                                    .as_ref()
                                    .is_some_and(|id| list_agents_call_ids.contains(id));
                                let is_close = tool_use_id
                                    .as_ref()
                                    .is_some_and(|id| close_agent_call_ids.contains(id));
                                let pending_script = tool_use_id
                                    .as_ref()
                                    .and_then(|id| pending_exec_scripts.remove(id));
                                let deferred = tool_use_id
                                    .as_ref()
                                    .and_then(|id| deferred_waits.remove(id))
                                    .and_then(|cell| deferred_scripts.remove(&cell));

                                if let Some(deferred) = deferred {
                                    // The parked script's real return value, at
                                    // last. Re-decompose it against that script
                                    // and rewrite the cards it already owns —
                                    // this `wait` contributes no card of its own.
                                    let collected = split_code_mode_output(payload.get("output"));
                                    // The whole run, not just this instalment:
                                    // see `DeferredScript::chunks`.
                                    let parsed = CodeModeOutput {
                                        status: collected.status,
                                        chunks: deferred
                                            .chunks
                                            .iter()
                                            .cloned()
                                            .chain(collected.chunks)
                                            .collect(),
                                        note: collected.note,
                                    };
                                    let semantic = (parsed.status == ScriptStatus::Completed)
                                        .then(|| {
                                            completed_mcp_by_exec.remove(&deferred.call_id)
                                        })
                                        .flatten();
                                    let (uses, results) = semantic
                                        .and_then(|calls| {
                                            unwrap_completed_mcp_calls(&deferred.script, calls)
                                        })
                                        .map(|(uses, results)| (Some(uses), results))
                                        .unwrap_or_else(|| {
                                            unwrap_code_mode_script(
                                                &deferred.call_id,
                                                &deferred.script,
                                                &parsed,
                                                payload,
                                                &mut shell_sessions,
                                                &mut poll_origins,
                                            )
                                        });
                                    if let Some(uses) = uses {
                                        messages[deferred.use_index].content = uses;
                                    }
                                    messages[deferred.result_index].content = results;
                                    // Still not done: whatever cell it reports
                                    // now is what the next `wait` collects.
                                    if parsed.status == ScriptStatus::Running {
                                        let deferred = DeferredScript {
                                            chunks: parsed.chunks,
                                            ..deferred
                                        };
                                        for cell in extract_shell_session_ids(
                                            parsed.note.as_deref().unwrap_or_default(),
                                        ) {
                                            deferred_scripts.insert(cell, deferred.clone());
                                        }
                                    }
                                } else if let Some((message_index, script)) = pending_script {
                                    let call_id = tool_use_id.unwrap_or_default();
                                    let parsed = split_code_mode_output(payload.get("output"));
                                    let semantic = (parsed.status == ScriptStatus::Completed)
                                        .then(|| completed_mcp_by_exec.remove(&call_id))
                                        .flatten();
                                    let (uses, results) = semantic
                                        .and_then(|calls| {
                                            unwrap_completed_mcp_calls(&script, calls)
                                        })
                                        .map(|(uses, results)| (Some(uses), results))
                                        .unwrap_or_else(|| {
                                            unwrap_code_mode_script(
                                                &call_id,
                                                &script,
                                                &parsed,
                                                payload,
                                                &mut shell_sessions,
                                                &mut poll_origins,
                                            )
                                        });
                                    if let Some(uses) = uses {
                                        messages[message_index].content = uses;
                                    }
                                    if parsed.status == ScriptStatus::Running {
                                        let deferred = DeferredScript {
                                            call_id: call_id.clone(),
                                            use_index: message_index,
                                            result_index: messages.len(),
                                            script: script.clone(),
                                            chunks: parsed.chunks.clone(),
                                        };
                                        for cell in extract_shell_session_ids(
                                            parsed.note.as_deref().unwrap_or_default(),
                                        ) {
                                            deferred_scripts.insert(cell, deferred.clone());
                                        }
                                    }
                                    messages.push(UnifiedMessage {
                                        id: format!("tool-result-{}", messages.len()),
                                        role: MessageRole::Tool,
                                        content: results,
                                        timestamp,
                                        usage: None,
                                        duration_ms: None,
                                        model: None,
                                        completed_at: Some(timestamp),
                                    agent_message_id: None,
                                    });
                                } else if is_spawn {
                                    if let Some(output_obj) = parse_codex_json_output(payload) {
                                        if let (Some(agent_id), Some(call_id)) = (
                                            output_obj.get("agent_id").and_then(|v| v.as_str()),
                                            tool_use_id.as_ref(),
                                        ) {
                                            agent_id_to_spawn_call_id
                                                .insert(agent_id.to_string(), call_id.clone());
                                        }
                                    }
                                    messages.push(UnifiedMessage {
                                        id: format!("tool-result-{}", messages.len()),
                                        role: MessageRole::Tool,
                                        content: vec![ContentBlock::ToolResult {
                                            tool_use_id,
                                            output_preview: None,
                                            is_error: false,
                                            agent_stats: None,
                                            images: Vec::new(),
                                        }],
                                        timestamp,
                                        usage: None,
                                        duration_ms: None,
                                        model: None,
                                        completed_at: Some(timestamp),
                                    agent_message_id: None,
                                    });
                                } else if is_wait || is_list {
                                    // Emit one `collab_agent` capsule per wait or
                                    // roster, routed through the same
                                    // CollabAgentCard as the live capsule. Two
                                    // wait output shapes — see
                                    // `native_team_wait_input`.
                                    //
                                    // A roster deliberately does NOT mark its
                                    // agents `agent_waited`: listing an agent is
                                    // not collecting it, and suppressing the
                                    // spawn capsule's own result on the strength
                                    // of a `list_agents` the model happened to
                                    // call would lose the report entirely.
                                    let capsule = parse_codex_json_output(payload).and_then(
                                        |output_obj| {
                                            if is_list {
                                                return build_collab_list_input(&output_obj);
                                            }
                                            match output_obj.get("status").and_then(|s| s.as_object())
                                            {
                                                Some(status) => {
                                                    // Mark returned agents so the spawn
                                                    // capsule won't also show their
                                                    // result, and record per-agent error
                                                    // state so the execution capsule can
                                                    // render failed (live parity).
                                                    for (agent_id, value) in status {
                                                        agent_waited.insert(agent_id.clone());
                                                        let (st, _) =
                                                            extract_wait_agent_status(value);
                                                        if is_error_collab_status(&st) {
                                                            agent_errored.insert(agent_id.clone());
                                                        }
                                                    }
                                                    (!status.is_empty())
                                                        .then(|| build_collab_wait_input(status))
                                                }
                                                None => native_team_wait_input(&output_obj),
                                            }
                                        },
                                    );
                                    if let Some((collab_input, is_error)) = capsule {
                                        messages.push(UnifiedMessage {
                                            id: format!("tool-{}", messages.len()),
                                            role: MessageRole::Assistant,
                                            content: vec![ContentBlock::ToolUse {
                                                tool_use_id: tool_use_id.clone(),
                                                tool_name: "collab_agent".to_string(),
                                                input_preview: Some(collab_input),
                                                status: None,
                                                meta: None,
                                            }],
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                        messages.push(UnifiedMessage {
                                            id: format!("tool-result-{}", messages.len()),
                                            role: MessageRole::Tool,
                                            content: vec![ContentBlock::ToolResult {
                                                tool_use_id,
                                                output_preview: None,
                                                is_error,
                                                agent_stats: None,
                                                images: Vec::new(),
                                            }],
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                    }
                                } else if is_close {
                                    active_agent_count = active_agent_count.saturating_sub(1);
                                    if let Some(output_obj) = parse_codex_json_output(payload) {
                                        if let Some(agent_id) = tool_use_id
                                            .as_ref()
                                            .and_then(|id| close_agent_targets.get(id))
                                        {
                                            // Generalize over the terminal key (not
                                            // just `completed`): an errored/notFound
                                            // close with no wait must not lose its
                                            // message or its error state.
                                            if let Some(prev) =
                                                output_obj.get("previous_status")
                                            {
                                                let (st, msg) =
                                                    extract_wait_agent_status(prev);
                                                if let Some(text) = msg {
                                                    agent_fallback_results
                                                        .entry(agent_id.clone())
                                                        .or_insert(text);
                                                }
                                                if is_error_collab_status(&st) {
                                                    agent_errored.insert(agent_id.clone());
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    let is_exec = tool_use_id.as_ref().is_some_and(|id| {
                                        call_id_tool_names
                                            .get(id)
                                            .is_some_and(|n| n == "exec_command")
                                    });
                                    let output_value = payload.get("output");
                                    // Unified-exec tools (`wait`, …) share the
                                    // code-mode output shape: an ARRAY of
                                    // `{type:"input_text", text}` parts behind a
                                    // `Script …/Wall time …/Output:` header.
                                    // `value_to_preview` would dump that array as
                                    // raw JSON into the result panel.
                                    let envelope = split_code_mode_output(output_value);
                                    // An `exec_command` that left a background
                                    // shell running announces its id here; bind
                                    // it to the command so the `wait` /
                                    // `write_stdin` calls that address it later
                                    // can be titled by that command. Read before
                                    // `clean_codex_exec_output`, which drops
                                    // everything above the `Output:` line — the
                                    // announcement included.
                                    if let Some((origin, command)) =
                                        tool_use_id.as_ref().and_then(|id| {
                                            call_id_commands.get(id).map(|cmd| (id.clone(), cmd))
                                        })
                                    {
                                        let mut announced = envelope.joined();
                                        if let Some(note) = envelope.note.as_deref() {
                                            announced.push('\n');
                                            announced.push_str(note);
                                        }
                                        register_announced_sessions(
                                            command,
                                            &origin,
                                            &announced,
                                            &mut shell_sessions,
                                        );
                                    }
                                    let (raw_output, envelope_error) =
                                        if envelope.status != ScriptStatus::Unknown
                                            || output_value.is_some_and(|v| v.is_array())
                                        {
                                            (
                                                with_note(
                                                    Some(envelope.joined()),
                                                    envelope.note.as_deref(),
                                                )
                                                .filter(|s| !s.is_empty()),
                                                envelope.is_error(),
                                            )
                                        } else {
                                            (value_to_preview(output_value), false)
                                        };
                                    // A poll about to be folded into the card of
                                    // the command it is collecting for: its
                                    // envelope has to go, and an envelope that
                                    // wrapped nothing has to end up empty rather
                                    // than falling back to its own header.
                                    let is_folded_poll = tool_use_id
                                        .as_ref()
                                        .is_some_and(|id| poll_origins.contains_key(id));
                                    let output = if is_folded_poll {
                                        raw_output
                                            .map(|s| strip_exec_envelope(&s).unwrap_or(s))
                                            .filter(|s| !s.is_empty())
                                    } else if is_exec {
                                        raw_output.map(|s| clean_codex_exec_output(&s))
                                    } else {
                                        raw_output
                                    };
                                    let is_error = envelope_error
                                        || infer_tool_call_output_is_error(
                                            payload,
                                            output_value,
                                            output.as_deref(),
                                        );
                                    messages.push(UnifiedMessage {
                                        id: format!("tool-result-{}", messages.len()),
                                        role: MessageRole::Tool,
                                        content: vec![ContentBlock::ToolResult {
                                            tool_use_id,
                                            output_preview: output,
                                            is_error,
                                            agent_stats: None,
                                            images: Vec::new(),
                                        }],
                                        timestamp,
                                        usage: None,
                                        duration_ms: None,
                                        model: None,
                                        completed_at: Some(timestamp),
                                    agent_message_id: None,
                                    });
                                }
                            }
                            "message" => {
                                let role =
                                    payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                                if role == "user" {
                                    active_agent_count = 0;
                                    if let Some(blocks) =
                                        extract_response_item_user_image_blocks(payload)
                                    {
                                        if should_skip_duplicate_user_message(
                                            &messages, &blocks, timestamp,
                                        ) {
                                            continue;
                                        }

                                        if title.is_none() {
                                            if let Some(text) = first_text_block(&blocks) {
                                                title = extract_codex_title_candidate(
                                                    text.as_str(),
                                                    true,
                                                );
                                                if title.is_some() {
                                                    title_source_ordinal = Some(record_ordinal);
                                                }
                                            }
                                        }

                                        messages.push(UnifiedMessage {
                                            id: format!("user-{}", messages.len()),
                                            role: MessageRole::User,
                                            content: blocks,
                                            timestamp,
                                            usage: None,
                                            duration_ms: None,
                                            model: None,
                                            completed_at: Some(timestamp),
                                        agent_message_id: None,
                                        });
                                        continue;
                                    }
                                }

                                // Everything the arm above did not emit: text-only
                                // users and EVERY assistant record. Normally these
                                // are same-millisecond duplicates of the
                                // `event_msg` channel and must stay dropped — but
                                // in a rollout whose event channel never spoke for
                                // this turn they are the only copy that exists.
                                // Held back rather than pushed, because coverage is
                                // not known until the segment (or the file) ends.
                                let is_user = match role {
                                    "user" => true,
                                    "assistant" => false,
                                    // `developer` / `system` / anything else is
                                    // machinery, never a conversation turn.
                                    _ => continue,
                                };
                                let Some(blocks) =
                                    extract_response_item_message_blocks(payload, is_user)
                                else {
                                    continue;
                                };
                                // An image-only record has nothing for the
                                // deny-lists (which are text rules) to judge.
                                let text = first_text_block(&blocks).unwrap_or_default();

                                // Plan mode's plan document, intercepted ahead of
                                // the promotion gate. It must render even where the
                                // event channel DID speak for this turn, so the
                                // per-segment coverage rule (right for prose) is
                                // simply the wrong test here.
                                //
                                // This record is the richer of the plan's two
                                // copies: `item_completed` announces the body
                                // alone, while this one keeps the prose codex
                                // writes around the block. So when it IS the
                                // pending announcement's twin, it takes that
                                // message over rather than adding a second.
                                //
                                // The slot is consumed either way. Only an
                                // announcement may fill it, and only the next plan
                                // record may claim it — otherwise two plan turns
                                // that happen to propose the SAME body (a legacy
                                // rollout with no announcements, where codex
                                // re-proposes an unchanged plan) would read as one
                                // plan written twice, and the second turn would
                                // render empty.
                                if !is_user {
                                    if let Some(body) = proposed_plan_body(&text) {
                                        let twin = pending_plan_twin
                                            .take()
                                            .filter(|(_, seen)| seen == body)
                                            .map(|(index, _)| index);
                                        match twin.and_then(|index| messages.get_mut(index)) {
                                            Some(existing) => existing.content = blocks,
                                            None => {
                                                messages.push(UnifiedMessage {
                                                    id: format!("assistant-plan-{}", messages.len()),
                                                    role: MessageRole::Assistant,
                                                    content: blocks,
                                                    timestamp,
                                                    usage: None,
                                                    duration_ms: None,
                                                    model: None,
                                                    completed_at: Some(timestamp),
                                                    agent_message_id: None,
                                                });
                                            }
                                        }
                                        plan_rendered = true;
                                        continue;
                                    }
                                }

                                let promotable = if text.trim().is_empty() {
                                    true
                                } else if is_user {
                                    is_promotable_user_text(&text)
                                } else {
                                    is_promotable_assistant_text(&text)
                                };
                                if !promotable {
                                    continue;
                                }

                                let title_candidate = if is_user {
                                    extract_codex_title_candidate(&text, true)
                                } else {
                                    None
                                };
                                promotion.push_candidate(record_ordinal, is_user, &text);
                                pending_promotions.push(PendingPromotedMessage {
                                    insert_at: messages.len(),
                                    is_user,
                                    blocks,
                                    timestamp,
                                    title_candidate,
                                    closed_reasoning_card: closed_reasoning_card
                                        .filter(|_| !is_user),
                                });
                            }
                            "image_generation_call" => {
                                // codex 0.129+ writes the same generated image as both an
                                // `event_msg.image_generation_end` (earlier in the file) and
                                // a `response_item.image_generation_call` (here). They share
                                // the same id; emit at most once via emitted_image_ids.
                                // Subagent suppression mirrors the event_msg arm: parent
                                // timeline must not host children's generated images.
                                if active_agent_count > 0 {
                                    continue;
                                }
                                let id = payload
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !id.is_empty() && emitted_image_ids.contains(&id) {
                                    continue;
                                }
                                let result =
                                    payload.get("result").and_then(|v| v.as_str()).unwrap_or("");
                                if result.is_empty() {
                                    continue;
                                }
                                let mime_type = payload
                                    .get("mime_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("image/png")
                                    .to_string();
                                let revised_prompt = payload
                                    .get("revised_prompt")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .filter(|s| !s.trim().is_empty());
                                messages.push(UnifiedMessage {
                                    id: format!("assistant-imagegen-{}", messages.len()),
                                    role: MessageRole::Assistant,
                                    content: vec![ContentBlock::ImageGeneration {
                                        revised_prompt,
                                        image: Some(ImageData {
                                            data: result.to_string(),
                                            mime_type,
                                            // response_item.image_generation_call has no
                                            // saved_path; event_msg.image_generation_end is
                                            // the only carrier of the on-disk file URI.
                                            uri: None,
                                        }),
                                    }],
                                    timestamp,
                                    usage: None,
                                    duration_ms: None,
                                    model: None,
                                    completed_at: Some(timestamp),
                                agent_message_id: None,
                                });
                                if !id.is_empty() {
                                    emitted_image_ids.insert(id);
                                }
                            }
                            // Not rendered; it only marks the request whose
                            // `token_count` will cover the provider's search
                            // loop rather than the context.
                            "web_search_call" => context_readings.web_search_ran(),
                            _ => {}
                        }
                    }
                }
                // A context compaction. Live, codex-acp announces it as the
                // `_meta.contextCompaction` call `<ContextCompactionCard>` draws as
                // a divider between the work before it and after; this is the
                // history half, so the reopened conversation keeps the divider
                // where the live one stood. Without it the compaction left no
                // mark at all, and the handoff codex wrote for its next model
                // read as the tail of the reply before it.
                //
                // This record draws it rather than `event_msg.context_compacted`
                // (≤ 0.148) or `item_completed.ContextCompaction` (0.149+): it is
                // the one every codex version writes, once per compaction (521 of
                // 521 across the local corpus, 0.104 – 0.156).
                "compacted" => {
                    let timestamp = parse_codex_timestamp(&value).unwrap_or_else(Utc::now);
                    // Whatever the reasoning run still holds came before.
                    flush_pending_reasoning(
                        &mut messages,
                        &mut grouped_reasoning,
                        &mut pending_reasoning,
                        pending_reasoning_ts,
                    );
                    let summary = codex_compacted_summary(value.get("payload"));
                    let handoff = promotion
                        .deny_compaction_summary(summary)
                        .and_then(|index| pending_promotions.get(index));
                    // What the compaction's own model call thought before it
                    // wrote the handoff: codex publishes no item for it, so live
                    // never shows it, and kept here it read as the last thought
                    // of the reply before. Only while nothing has landed since,
                    // which also keeps every other index into `messages` valid.
                    let compaction_thinking = handoff
                        .filter(|handoff| handoff.insert_at == messages.len())
                        .and_then(|handoff| handoff.closed_reasoning_card)
                        .filter(|&card| {
                            card + 1 == messages.len()
                                && matches!(
                                    messages[card].content.as_slice(),
                                    [ContentBlock::Thinking { .. }]
                                )
                        });
                    if let Some(card) = compaction_thinking.and_then(|_| messages.pop()) {
                        // Its round was already billed to it; hand that to the
                        // reply it would have been shown with.
                        if let Some(spent) = card.usage {
                            match messages.iter_mut().rev().find(|m| bills_codex_usage(m)) {
                                Some(owner) => {
                                    owner.usage = Some(match owner.usage {
                                        Some(ref existing) => codex_usage_add(existing, &spent),
                                        None => spent,
                                    });
                                }
                                None => {
                                    pending_round_usage = Some(match pending_round_usage {
                                        Some(ref pending) => codex_usage_add(pending, &spent),
                                        None => spent,
                                    });
                                }
                            }
                        }
                    }
                    messages.push(codex_compaction_divider(
                        format!("compaction-{}", messages.len()),
                        format!("codex-compaction-{record_ordinal}"),
                        summary.map(str::to_string),
                        timestamp,
                    ));
                    unnamed_compaction_divider = Some(messages.len() - 1);
                }
                _ => {}
            }
        }

        // A reasoning run the file ended on — either the last thing the session
        // did, or a truncated/interrupted rollout whose `agent_reasoning` events
        // were written before the grouped summary. Emit it so it isn't lost.
        flush_pending_reasoning(
            &mut messages,
            &mut grouped_reasoning,
            &mut pending_reasoning,
            pending_reasoning_ts,
        );

        // A round still waiting for an assistant message to bill — the transcript
        // ended before the next `token_count` could hand it to one (a run's card
        // was only just flushed above, or the model went straight to a tool call
        // and never spoke again). Bill it here rather than discard it.
        if let (Some(pending), Some(last_msg)) = (
            pending_round_usage.take(),
            messages.iter_mut().rev().find(|m| bills_codex_usage(m)),
        ) {
            last_msg.usage = Some(match last_msg.usage {
                Some(ref existing) => codex_usage_add(existing, &pending),
                None => pending,
            });
        }

        // Fill in subagent tool call stats (and, only as a fallback, the result)
        // on each spawn execution capsule.
        if !agent_id_to_spawn_call_id.is_empty() {
            let spawn_call_to_agent: HashMap<&str, &str> = agent_id_to_spawn_call_id
                .iter()
                .map(|(agent_id, call_id)| (call_id.as_str(), agent_id.as_str()))
                .collect();

            let session_dir = path.parent();
            let mut agent_stats_cache: HashMap<String, Option<AgentExecutionStats>> =
                HashMap::new();

            for msg in &mut messages {
                for block in &mut msg.content {
                    match block {
                        ContentBlock::ToolResult {
                            tool_use_id: Some(ref id),
                            ref mut output_preview,
                            ref mut is_error,
                            ref mut agent_stats,
                            ..
                        } => {
                            if let Some(&agent_id) = spawn_call_to_agent.get(id.as_str()) {
                                // The result text normally lives in the wait
                                // capsule; only show it on the execution capsule
                                // when this agent was never returned by a wait
                                // (else duplicate).
                                if !agent_waited.contains(agent_id) {
                                    if let Some(result) = agent_fallback_results.get(agent_id) {
                                        *output_preview = Some(result.clone());
                                    }
                                }
                                // Mark the execution capsule failed when the agent
                                // ended in error (in a wait or close) — live parity.
                                if agent_errored.contains(agent_id) {
                                    *is_error = true;
                                }
                                if let Some(dir) = session_dir {
                                    let stats = agent_stats_cache.entry(agent_id.to_string())
                                        .or_insert_with(|| {
                                            parse_codex_subagent_stats(dir, agent_id)
                                        });
                                    if stats.is_some() {
                                        *agent_stats = stats.clone();
                                    }
                                }
                            }
                        }
                        // Stamp the sub-agent's id onto the spawn execution capsule
                        // input so the card can render it (parity with the wait
                        // capsule, whose agentsStates already carry the id), plus
                        // the terminal state when codex reported one.
                        ContentBlock::ToolUse {
                            tool_use_id: Some(ref id),
                            ref tool_name,
                            ref mut input_preview,
                            ..
                        } if tool_name == "Agent" => {
                            if let Some(&agent_id) = spawn_call_to_agent.get(id.as_str()) {
                                *input_preview = Some(inject_agent_id_into_input(
                                    input_preview.as_deref(),
                                    agent_id,
                                    agent_terminal_kind.get(agent_id).map(String::as_str),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Promote the held-back `response_item.message` records, now that the
        // whole file has been seen and every turn segment's canonical-channel
        // coverage is known.
        //
        // Ordering matters and is load-bearing:
        //   1. splice FIRST — `insert_at` indexes `messages` as it stood at EOF,
        //      and both steps below mutate it;
        //   2. then the `/goal` opener, which inserts at index 0;
        //   3. then `fold_shell_session_polls`, which rebuilds all of its own
        //      (message, block) indices from whatever vector it is handed.
        let survivors = promotion.resolve();
        let promoted_user_ordinal = promotion.first_surviving_user_ordinal(&survivors);
        let mut kept: Vec<PendingPromotedMessage> = pending_promotions
            .into_iter()
            .zip(&survivors)
            .filter(|(_, keep)| **keep)
            .map(|(pending, _)| pending)
            .collect();

        // Resolved before the vector is drained by the splice below.
        let promoted_title = kept
            .iter()
            .find(|pending| pending.is_user)
            .and_then(|pending| pending.title_candidate.clone());

        // Descending by insertion point so earlier indices stay valid — but
        // GROUPED, because several records can share one `insert_at` (nothing
        // was pushed between them) and splicing those one at a time in reverse
        // would invert their transcript order.
        while let Some(insert_at) = kept.last().map(|pending| pending.insert_at) {
            let group_start = kept
                .iter()
                .rposition(|pending| pending.insert_at != insert_at)
                .map(|index| index + 1)
                .unwrap_or(0);
            let group: Vec<UnifiedMessage> = kept
                .split_off(group_start)
                .into_iter()
                .enumerate()
                .map(|(offset, pending)| UnifiedMessage {
                    // Namespaced so a promoted record can never collide with a
                    // `user-N` / `assistant-N` id minted inside the loop.
                    id: format!("codex-ri-{insert_at}-{offset}"),
                    role: if pending.is_user {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    content: pending.blocks,
                    timestamp: pending.timestamp,
                    usage: None,
                    duration_ms: None,
                    model: None,
                    completed_at: Some(pending.timestamp),
                agent_message_id: None,
                })
                .collect();
            messages.splice(insert_at..insert_at, group);
        }

        // A promoted user that PRECEDES the goal means the goal did not open the
        // session after all — the same positional rule the in-loop code applies
        // to native users. One that comes AFTER it (the "确认" reply shape) must
        // leave the opener intact, which is why this compares ordinals rather
        // than asking whether a user exists anywhere.
        if let (Some(goal_ordinal), Some(user_ordinal)) =
            (first_goal_ordinal, promoted_user_ordinal)
        {
            if user_ordinal < goal_ordinal {
                goal_opens_session = false;
            }
        }

        // Same precedence the in-loop claims follow: codex's own thread name
        // always wins, otherwise the EARLIEST prompt-shaped source does.
        if !title_from_thread_name {
            if let (Some(user_ordinal), Some(candidate)) = (promoted_user_ordinal, promoted_title) {
                if title.is_none()
                    || title_source_ordinal.is_none_or(|current| user_ordinal < current)
                {
                    title = Some(candidate);
                }
            }
        }

        // Leading-`/goal` fallback: when a `/goal` opened the session (before any
        // real user turn), newer codex recorded only `thread_goal_updated` — no
        // `user_message` — so the typed `/goal <objective>` prompt would be missing
        // on reload (headless, or, when a later reply like "确认" exists, starting
        // mid-conversation). Surface it as the leading user message, prefixed with
        // `/goal ` to match what the user actually typed and the live optimistic
        // bubble. The title was already claimed in-loop (see the goal-capture
        // block). Applied here, after parsing, so the synthetic turn never
        // participates in `should_skip_duplicate_user_message`.
        if let Some(objective) = first_goal_objective {
            if goal_opens_session {
                messages.insert(
                    0,
                    UnifiedMessage {
                        id: "codex-goal-user".to_string(),
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text {
                            text: format!("/goal {objective}"),
                        }],
                        // Earliest event time so it sorts ahead of the goal card.
                        timestamp: first_timestamp.unwrap_or_else(Utc::now),
                        usage: None,
                        duration_ms: None,
                        model: None,
                        completed_at: first_timestamp,
                    agent_message_id: None,
                    },
                );
            }
        }

        let folder_path = cwd.clone();
        let folder_name = folder_path.as_ref().map(|p| folder_name_from_path(p));

        let turn_starts = if task_start_markers.is_empty() {
            &turn_context_markers
        } else {
            &task_start_markers
        };
        fold_shell_session_polls(&mut messages, &poll_origins);
        // After every pass that drops messages, so it sees every pair of cards
        // they leave touching.
        merge_touching_thinking_cards(&mut messages, turn_starts);
        let mut turns = group_into_turns(messages);
        reconcile_turn_usage(&mut turns, &recorded_round_usage);
        super::relocate_orphaned_tool_results(&mut turns);
        super::structurize_read_tool_output(&mut turns);
        super::resolve_patch_line_numbers(&mut turns, cwd.as_deref());
        // After relocation every turn's `completed_at` is final — tile the
        // timeline into per-reply durations before stats aggregate them.
        super::backfill_turn_durations(&mut turns, turn_starts);
        let mut session_stats = super::compute_session_stats(&turns);
        session_stats =
            merge_codex_total_usage_stats(session_stats, latest_total_usage, latest_total_tokens);
        session_stats = merge_codex_context_window_stats(
            session_stats,
            context_window_used_tokens,
            context_window_max_tokens,
        );

        let summary = ConversationSummary {
            id: conversation_id.to_string(),
            agent_type: AgentType::Codex,
            folder_path,
            folder_name,
            title,
            started_at: first_timestamp.unwrap_or_else(Utc::now),
            ended_at: last_timestamp,
            message_count: super::message_turn_count(&turns),
            model,
            git_branch,
            parent_id,
            parent_tool_use_id: None,
            delegation_call_id: None,
        };

        Ok(ConversationDetail {
            summary,
            turns,
            session_stats,
            transcript_watermark: None,
        })
    }
}

fn extract_total_tokens_from_usage(usage: &serde_json::Value) -> Option<u64> {
    if let Some(total_tokens) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
        return Some(total_tokens);
    }

    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached_input_tokens = usage
        .get("cached_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reasoning_output_tokens = usage
        .get("reasoning_output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Codex payloads use `input_tokens` as the full input (cache read included),
    // so fallback totals should not double-count cached tokens.
    let total = if cached_input_tokens <= input_tokens {
        input_tokens + output_tokens + reasoning_output_tokens
    } else {
        input_tokens + cached_input_tokens + output_tokens + reasoning_output_tokens
    };
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

fn extract_turn_usage_from_codex_usage(usage: &serde_json::Value) -> Option<TurnUsage> {
    let counters = codex_usage_counters(usage);
    if codex_usage_is_zero(&counters) {
        return None;
    }
    Some(counters)
}

/// A codex usage payload read as raw counters, keeping zeros.
///
/// [`extract_turn_usage_from_codex_usage`] answers "is there anything to show",
/// so it collapses an all-zero payload to `None`. The running-total arithmetic
/// below needs the opposite: a cumulative counter that legitimately still reads
/// zero is a real datapoint, not an absent one.
///
/// Codex's `input_tokens` is the WHOLE prompt. It includes the tokens read from
/// the prompt cache (`cached_input_tokens`) and the tokens written to it
/// (`cache_write_input_tokens`, which some providers and gateways report and
/// OpenAI's API leaves at 0), while codeg's four counters are disjoint. So
/// both are split out of the input rather than counted as fresh input;
/// codex-acp 2.2.0 made the same correction to its own `TokenCount`. A real
/// rollout on codeg's bound provider reads `input 19915, cached 17152, write
/// 2304`: 459 fresh.
///
/// The write is capped at what the input has left after the read, so a provider
/// that over-reports it cannot make the counters add up to more than the input
/// did before the split.
fn codex_usage_counters(usage: &serde_json::Value) -> TurnUsage {
    let field = |name: &str| usage.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
    let input = field("input_tokens");
    let cache_read = field("cached_input_tokens");
    let cache_write = field("cache_write_input_tokens").min(input.saturating_sub(cache_read));
    TurnUsage {
        input_tokens: input.saturating_sub(cache_read).saturating_sub(cache_write),
        output_tokens: field("output_tokens"),
        cache_creation_input_tokens: cache_write,
        cache_read_input_tokens: cache_read,
    }
}

fn codex_usage_is_zero(usage: &TurnUsage) -> bool {
    usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_creation_input_tokens == 0
        && usage.cache_read_input_tokens == 0
}

fn codex_usage_add(base: &TurnUsage, extra: &TurnUsage) -> TurnUsage {
    TurnUsage {
        input_tokens: base.input_tokens.saturating_add(extra.input_tokens),
        output_tokens: base.output_tokens.saturating_add(extra.output_tokens),
        cache_creation_input_tokens: base
            .cache_creation_input_tokens
            .saturating_add(extra.cache_creation_input_tokens),
        cache_read_input_tokens: base
            .cache_read_input_tokens
            .saturating_add(extra.cache_read_input_tokens),
    }
}

/// What one model round-trip added, as the rise in the session's cumulative
/// counter.
///
/// Codex emits a `token_count` event after **every** model call — including the
/// tool-calling rounds inside a turn — and each one restates
/// `total_token_usage` for the whole session. Differencing that counter is what
/// makes the accounting exact: it cannot double-count a `token_count` that is
/// emitted twice for the same call (7 380 of 31 846 events in a real session
/// tree are such repeats), and it cannot lose a round whose event went
/// unrecorded, because the next event's total absorbs it.
///
/// A total that moves *backwards* means the counter restarted (a fresh context
/// after compaction), so the new value is the round's own spend.
fn codex_round_usage(previous_total: Option<&TurnUsage>, total: &TurnUsage) -> TurnUsage {
    let grand = |u: &TurnUsage| {
        u.input_tokens
            .saturating_add(u.output_tokens)
            .saturating_add(u.cache_creation_input_tokens)
            .saturating_add(u.cache_read_input_tokens)
    };
    // No baseline, or the counter restarted below it: the whole figure is this
    // round's own spend. Differencing per field instead would clamp every field
    // to zero and silently drop the rest of the session.
    let Some(previous) = previous_total.filter(|prev| grand(prev) <= grand(total)) else {
        return total.clone();
    };
    TurnUsage {
        input_tokens: total.input_tokens.saturating_sub(previous.input_tokens),
        output_tokens: total.output_tokens.saturating_sub(previous.output_tokens),
        cache_creation_input_tokens: total
            .cache_creation_input_tokens
            .saturating_sub(previous.cache_creation_input_tokens),
        cache_read_input_tokens: total
            .cache_read_input_tokens
            .saturating_sub(previous.cache_read_input_tokens),
    }
}

/// Make the turns account for every token the transcript reported.
///
/// Round-trip usage is attached to the assistant message that was current when
/// the `token_count` arrived, which is right whenever that message survives —
/// but presentation is allowed to drop or fold messages (a tool call absorbed
/// into a capsule, a duplicate agent message collapsed away), and a message
/// that disappears takes its usage with it. Across a real session tree that
/// silently lost 7 % of Codex spend, concentrated in exactly the sessions that
/// worked hardest.
///
/// So the recorded rounds are also summed independently, and any shortfall is
/// put back on the last turn that can hold it. The invariant this establishes
/// is worth stating plainly: **the per-turn usage of a Codex session always
/// sums to what its own counter reported**, whatever the renderer did to the
/// turns in between.
///
/// A surplus is left alone. It would mean the turns claim more than the
/// transcript ever reported, which no path here can produce, and inventing a
/// correction for an impossible state would only hide the bug that caused it.
fn reconcile_turn_usage(turns: &mut [MessageTurn], recorded: &TurnUsage) {
    if codex_usage_is_zero(recorded) {
        return;
    }
    let attributed = turns
        .iter()
        .filter_map(|t| t.usage.as_ref())
        .fold(TurnUsage::default(), |acc, u| codex_usage_add(&acc, u));

    let missing = TurnUsage {
        input_tokens: recorded.input_tokens.saturating_sub(attributed.input_tokens),
        output_tokens: recorded
            .output_tokens
            .saturating_sub(attributed.output_tokens),
        cache_creation_input_tokens: recorded
            .cache_creation_input_tokens
            .saturating_sub(attributed.cache_creation_input_tokens),
        cache_read_input_tokens: recorded
            .cache_read_input_tokens
            .saturating_sub(attributed.cache_read_input_tokens),
    };
    if codex_usage_is_zero(&missing) {
        return;
    }

    // Prefer a turn that already reports usage — it is one the transcript
    // itself tied to a model call, so the recovered tokens land beside spend
    // that really happened rather than on an unrelated bubble.
    let target = turns
        .iter()
        .rposition(|t| t.usage.is_some())
        .or_else(|| {
            turns.iter().rposition(|t| {
                matches!(t.role, TurnRole::Assistant) && !is_codex_compaction_divider(&t.blocks)
            })
        });
    if let Some(turn) = target.and_then(|i| turns.get_mut(i)) {
        turn.usage = Some(match turn.usage {
            Some(ref existing) => codex_usage_add(existing, &missing),
            None => missing,
        });
    }
}

fn extract_context_window_used_tokens_from_token_count_info(
    info: &serde_json::Value,
) -> Option<u64> {
    // `last_token_usage` is the current turn usage and best matches context window occupancy.
    if let Some(last_usage) = info.get("last_token_usage") {
        if let Some(total) = extract_total_tokens_from_usage(last_usage) {
            return Some(total);
        }
    }

    // Fallback: some payloads may only have cumulative totals.
    info.get("total_token_usage")
        .and_then(extract_total_tokens_from_usage)
}

fn merge_codex_context_window_stats(
    stats: Option<SessionStats>,
    used_tokens: Option<u64>,
    max_tokens: Option<u64>,
) -> Option<SessionStats> {
    if used_tokens.is_none() && max_tokens.is_none() {
        return stats;
    }

    let usage_percent = match (used_tokens, max_tokens) {
        (Some(used), Some(max)) if max > 0 => Some((used as f64 / max as f64) * 100.0),
        _ => None,
    };

    match stats {
        Some(mut s) => {
            s.context_window_used_tokens = used_tokens;
            s.context_window_max_tokens = max_tokens;
            s.context_window_usage_percent = usage_percent;
            Some(s)
        }
        None => Some(SessionStats {
            total_usage: None,
            total_tokens: None,
            total_duration_ms: 0,
            context_window_used_tokens: used_tokens,
            context_window_max_tokens: max_tokens,
            context_window_usage_percent: usage_percent,
        }),
    }
}

fn merge_codex_total_usage_stats(
    stats: Option<SessionStats>,
    total_usage: Option<TurnUsage>,
    total_tokens: Option<u64>,
) -> Option<SessionStats> {
    match stats {
        Some(mut s) => {
            if let Some(total) = total_usage {
                s.total_usage = Some(total);
            }
            if total_tokens.is_some() {
                s.total_tokens = total_tokens;
            }
            Some(s)
        }
        None if total_usage.is_some() || total_tokens.is_some() => Some(SessionStats {
            total_usage,
            total_tokens,
            total_duration_ms: 0,
            context_window_used_tokens: None,
            context_window_max_tokens: None,
            context_window_usage_percent: None,
        }),
        None => None,
    }
}

fn parse_codex_timestamp(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
}

/// Append a start-of-turn marker to one of the two marker lists (see their
/// declaration for why `task_started` and `turn_context` are kept apart).
///
/// An out-of-order arrival (skewed clocks in a rollout) is dropped rather than
/// inserted: the backfill scans the list once, in order.
fn push_turn_start(turn_starts: &mut Vec<DateTime<Utc>>, ts: DateTime<Utc>) {
    match turn_starts.last() {
        Some(last) if ts <= *last => {}
        _ => turn_starts.push(ts),
    }
}

/// Join the thinking cards left touching once the transcript is assembled.
///
/// A reasoning run ends at the first record that is not reasoning, and many of
/// those render nothing: codex 0.149+ announces every reasoning item with an
/// `event_msg.item_completed` restating the summary that follows, an addressed
/// inter-agent message never renders in place, a `write_stdin` poll or code-mode
/// `wait` is folded into the call it collects. Each split one thought — the
/// announcements alone gave every reasoning item a 思考 card of its own, where
/// live streams the whole think as one. Whether a record renders is often known
/// only now, so the cards are joined here, the way the run itself would have
/// joined them: text in order, stamped with the later card's time, both cards'
/// spend kept.
fn merge_touching_thinking_cards(
    messages: &mut Vec<UnifiedMessage>,
    turn_starts: &[DateTime<Utc>],
) {
    let mut kept: Vec<UnifiedMessage> = Vec::with_capacity(messages.len());
    for message in std::mem::take(messages) {
        match kept.last_mut() {
            Some(previous) if continues_thought(previous, &message, turn_starts) => {
                if let (
                    [ContentBlock::Thinking { text: earlier }],
                    [ContentBlock::Thinking { text: later }],
                ) = (previous.content.as_mut_slice(), message.content.as_slice())
                {
                    earlier.push_str("\n\n");
                    earlier.push_str(later);
                }
                previous.timestamp = message.timestamp;
                previous.completed_at = message.completed_at;
                previous.usage = match (previous.usage.take(), message.usage) {
                    (Some(earlier), Some(later)) => Some(codex_usage_add(&earlier, &later)),
                    (earlier, later) => earlier.or(later),
                };
            }
            _ => kept.push(message),
        }
    }
    *messages = kept;
}

/// Whether `next` resumes the thought `previous` paused: both are thinking cards
/// and no turn started between them. A new turn is a new prompt even when
/// nothing visible marks it, and the duration tiling measures `next` from that
/// start — a merged card would lose everything before it.
fn continues_thought(
    previous: &UnifiedMessage,
    next: &UnifiedMessage,
    turn_starts: &[DateTime<Utc>],
) -> bool {
    let is_thinking_card = |message: &UnifiedMessage| {
        matches!(message.role, MessageRole::Assistant)
            && matches!(message.content.as_slice(), [ContentBlock::Thinking { .. }])
    };
    let paused_at = previous.completed_at.unwrap_or(previous.timestamp);
    let resumed_by = next.completed_at.unwrap_or(next.timestamp);
    is_thinking_card(previous)
        && is_thinking_card(next)
        && !turn_starts
            .iter()
            .any(|start| *start > paused_at && *start <= resumed_by)
}

/// Close an open reasoning run: emit everything it gathered as a single Thinking
/// message and reset both buffers. No-op when the run is empty.
///
/// `grouped` is the settled text — the summaries codex wrote for each model
/// response the run spanned — and `pending` the streaming sections no summary
/// has restated yet (an interrupted rollout, or the tail of a run that is still
/// being written). Joining the two in that order is the run in document order,
/// which is the one 思考 card live shows for it.
fn flush_pending_reasoning(
    messages: &mut Vec<UnifiedMessage>,
    grouped: &mut Vec<String>,
    pending: &mut Vec<String>,
    ts: Option<DateTime<Utc>>,
) {
    if grouped.is_empty() && pending.is_empty() {
        return;
    }
    let text = grouped
        .iter()
        .chain(pending.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    grouped.clear();
    pending.clear();
    let timestamp = ts.unwrap_or_else(Utc::now);
    messages.push(UnifiedMessage {
        id: format!("thinking-{}", messages.len()),
        role: MessageRole::Assistant,
        content: vec![ContentBlock::Thinking { text }],
        timestamp,
        usage: None,
        duration_ms: None,
        model: None,
        completed_at: Some(timestamp),
    agent_message_id: None,
    });
}

fn agents_instructions_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?s)\A# AGENTS\.md instructions for [^\n]+\n\s*\n<INSTRUCTIONS>.*?</INSTRUCTIONS>\s*",
        )
        .expect("valid agents instructions regex")
    })
}

fn strip_agents_instructions_block(input: &str) -> String {
    let text = agents_instructions_regex().replace(input, "");
    text.trim().to_string()
}

/// Whether `input` is the AGENTS.md context codex injects as a `role: "user"`
/// record — in either of the two shapes it ships in.
///
/// Upstream (`UserInstructions` in
/// `codex-rs/core/src/context/user_instructions.rs`) renders it as the header,
/// then `{" for {dir}"|""}`, then `\n\n<INSTRUCTIONS>\n{text}\n`, then the
/// closing tag. The directory is an `Option`, and a GLOBAL `~/.codex/AGENTS.md`
/// has no directory to name — so newer codex emits the header bare, which used
/// to slip past the old `"…instructions for "` prefix and become the
/// conversation title (#789).
///
/// The `<INSTRUCTIONS>` opener is unconditional in both shapes (2554 of 2554
/// such records in the local corpus carry it), so requiring it costs nothing
/// and keeps a human prompt that merely OPENS with this heading from being
/// swallowed as machinery. The record often carries `<environment_context>`
/// as a second content item, joined onto the same text, which is why the
/// closing tag is deliberately NOT anchored to the end.
fn is_agents_instruction_message(input: &str) -> bool {
    const HEADER: &str = "# AGENTS.md instructions";

    let Some(suffix) = input.trim_start().strip_prefix(HEADER) else {
        return false;
    };

    // `\r` covers the CRLF rollouts Windows codex writes.
    (suffix.starts_with('\n') || suffix.starts_with('\r') || suffix.starts_with(" for "))
        && suffix.contains("<INSTRUCTIONS>")
}

fn is_environment_context_message(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.starts_with("<environment_context>") && trimmed.ends_with("</environment_context>")
}

/// codex re-injects `<codex_internal_context source="goal">Continue working …`
/// user turns while a `/goal` is active. These are machine context, never a real
/// prompt, so they must never become a conversation title (they otherwise leak in
/// on the summary path, whose title fallback doesn't gate on image blocks).
fn is_codex_internal_context_message(input: &str) -> bool {
    input.trim_start().starts_with("<codex_internal_context")
}

/// Machine-authored `role: "user"` records that codex injects into the model's
/// history but never shows as a prompt. They are unreachable on the canonical
/// path (`event_msg.user_message` carries only what the human typed) and only
/// become visible through [`ResponseItemPromotion`], so the list lives with the
/// promotion logic rather than with the title helpers.
///
/// Derived from a census of every user-role `response_item.message` text in the
/// local rollout corpus, not from guesswork: `<environment_context>` (1248),
/// `<turn_aborted>` (152), `<codex_internal_context source="goal">` (52),
/// `<subagent_notification>` (24), `<skill>` (10). The one untagged member is
/// the correction codex injects when a model calls `apply_patch` through
/// `exec_command` (34 occurrences) — matching English prose is admittedly
/// brittle, but the failure mode is one stray user bubble in a rollout that
/// would otherwise render nothing at all.
///
/// `<recommended_plugins>` is the newest member and was NOT in that census —
/// codex only writes it once the `recommended_plugins` feature is on, which no
/// local rollout had. It is upstream's `RecommendedPluginsInstructions`
/// (`codex-rs/core/src/context/recommended_plugins_instructions.rs`), a
/// `role: "user"` fragment rendered as the marker pair wrapped around
/// `"\nHere is a list of plugins that are available but not installed.\n\n…"`.
/// Because codex injects it BEFORE the first prompt it does not merely add a
/// stray bubble: it wins the promoted-title race, which is how whole fleets of
/// sessions ended up named `<recommended_plugins> Here is a list of plugins…`.
const PROMOTED_USER_DENY_PREFIXES: &[&str] = &[
    "<turn_aborted>",
    "<subagent_notification",
    "<skill",
    "<user_instructions",
    "<permissions instructions",
    "<skills_instructions",
    "<recommended_plugins>",
    "Warning: apply_patch was requested via exec_command",
];

/// Whether a candidate user record is machine context rather than a prompt.
fn is_promotable_user_text(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || is_agents_instruction_message(trimmed)
        || is_environment_context_message(trimmed)
        || is_codex_internal_context_message(trimmed)
    {
        return false;
    }
    !PROMOTED_USER_DENY_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Whether a candidate assistant record is renderable prose.
///
/// `<proposed_plan>` is codex's plan-mode payload. It is NOT prose and never
/// rides the generic promotion path: the detail parser intercepts it in a
/// dedicated arm (see [`proposed_plan_body`]) which renders it regardless of
/// the segment's canonical-channel coverage, because a plan turn has no
/// `agent_message` twin to be covered by in the first place. Keeping the deny
/// here is what stops the two paths from both emitting the same plan.
/// The compaction/handoff summary is excluded separately, by adjacency
/// (see [`ResponseItemPromotion::note_record`]).
fn is_promotable_assistant_text(input: &str) -> bool {
    let trimmed = input.trim();
    !trimmed.is_empty() && !trimmed.starts_with(PROPOSED_PLAN_OPEN)
}

/// codex Plan-mode markers. The plan document reaches the model's own history
/// as an assistant `response_item` wrapped in these tags, optionally with prose
/// before or after the block ("如果你希望调整…" follow-ups are common). The
/// frontend adapter (`expandProposedPlanText`) already splits that shape into a
/// plan card plus the surrounding text parts, so the parser hands the record
/// over verbatim instead of reshaping it here.
const PROPOSED_PLAN_OPEN: &str = "<proposed_plan>";
const PROPOSED_PLAN_CLOSE: &str = "</proposed_plan>";

/// The plan document inside a `<proposed_plan>` block, or `None` when `input`
/// carries no such block.
///
/// Used to pair an assistant record with the `event_msg.item_completed`
/// announcement of the SAME plan: codex writes every plan twice — once as a
/// structured `Plan` item (body only) and once as this record (body plus any
/// surrounding prose) — and only one of them may render. An unclosed block
/// (the turn was interrupted mid-plan) yields everything after the opener.
fn proposed_plan_body(input: &str) -> Option<&str> {
    let opened = input.find(PROPOSED_PLAN_OPEN)? + PROPOSED_PLAN_OPEN.len();
    let rest = &input[opened..];
    let body = match rest.find(PROPOSED_PLAN_CLOSE) {
        Some(closed) => &rest[..closed],
        None => rest,
    };
    Some(body.trim())
}

/// The plan document announced by `event_msg.item_completed`, or `None` for
/// every other completed item. codex's Plan-mode turn publishes its final
/// answer here INSTEAD of on `event_msg.agent_message`, which is why a plan
/// turn otherwise parses to nothing but its reasoning.
fn completed_plan_item_text(payload: &serde_json::Value) -> Option<&str> {
    let item = payload.get("item")?;
    if item.get("type").and_then(|v| v.as_str()) != Some("Plan") {
        return None;
    }
    let text = item.get("text").and_then(|v| v.as_str())?.trim();
    (!text.is_empty()).then_some(text)
}

/// codex's own follow-up prompt after the user approves a plan. It is written
/// to the rollout as an ordinary `user_message` — same fields, same empty
/// `text_elements` as something the user typed — so the wording is the only
/// per-record signal, and it is deliberately paired with the collaboration-mode
/// flip below rather than trusted alone.
const CODEX_PLAN_APPROVAL_PROMPT: &str = "Implement the approved plan.";

/// codex-acp's wording for an approved plan review, echoed as the historical
/// `plan_review` call's output so the card reports the same decision the live
/// one does (`CODEX_PLAN_APPROVED_PREFIX` in `plan-mode-card.tsx`).
const CODEX_PLAN_APPROVED_OUTPUT: &str = "User approved the plan.";

/// Append the settled `plan_review` call that reports an approved plan.
///
/// Deliberately the same shape the LIVE path produces: codeg seeds codex-acp's
/// unannounced plan-review call with `raw_input: None` (see
/// `handle_permission_request`) because the plan is already in the transcript,
/// and the frontend renders that input-less call as a bare decision marker. So
/// history and live resolve to one `<PlanModeCard>` with no rendering change
/// on either side.
///
/// Appended rather than inserted next to the plan: held `insert_at` positions
/// in `pending_promotions` are indices into this very vector, and the approval
/// belongs after the plan chronologically anyway.
fn push_plan_review_marker(messages: &mut Vec<UnifiedMessage>, timestamp: DateTime<Utc>) {
    let tool_use_id = format!("codex-plan-review-{}", messages.len());
    messages.push(UnifiedMessage {
        id: format!("assistant-plan-review-{}", messages.len()),
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                tool_use_id: Some(tool_use_id.clone()),
                // Resolved verbatim by the historical adapter, which passes
                // `block.tool_name` straight through to the renderer's
                // underscore-preserving gate.
                tool_name: "plan_review".to_string(),
                input_preview: None,
                status: None,
                meta: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: Some(tool_use_id),
                output_preview: Some(CODEX_PLAN_APPROVED_OUTPUT.to_string()),
                is_error: false,
                agent_stats: None,
                images: Vec::new(),
            },
        ],
        timestamp,
        usage: None,
        duration_ms: None,
        model: None,
        completed_at: Some(timestamp),
        agent_message_id: None,
    });
}

/// This turn's collaboration mode, from `turn_context.collaboration_mode.mode`
/// (`"plan"` while Plan mode is active). Absent on rollouts predating Plan
/// mode, which is why every caller treats `None` as "not plan".
fn turn_collaboration_mode(value: &serde_json::Value) -> Option<&str> {
    value
        .get("payload")?
        .get("collaboration_mode")?
        .get("mode")?
        .as_str()
}

/// The title a codex thread name reads as: exactly what the live path makes
/// of the same name, so the session index, a rollout's `thread_name_updated`
/// and `session_info_update.title` all title the row with one string.
///
/// codex stores a name as it was given, only trimmed (`thread/name/set` runs
/// `codex_core::util::normalize_thread_name`), and codex-acp's `/rename` hands
/// it the raw argument: `/rename   Fix  the\nflaky\ttest` puts
/// `"Fix  the\nflaky\ttest"` in `session_index.jsonl`. codex-acp publishes a
/// name through [`codex_acp_session_title`], on a rename echo and on
/// `session/load` alike, and the live path then normalizes that through
/// `acp::session_title::native_title_from_session_info`. Reading the raw name
/// had every live title and every list or detail load rewrite the row with
/// the other spelling, and the list path forwards each rewrite to the chat
/// channels. Taking both steps here agrees by construction.
fn codex_thread_title(name: &str) -> Option<String> {
    crate::acp::session_title::native_title_from_session_info(Some(&codex_acp_session_title(name)))
}

/// Whether a session-index name is one `load_thread_name_index` takes; backup
/// restore merges the index by the same test (`ExternalSource::indexes`).
pub(crate) fn is_usable_thread_name(name: &str) -> bool {
    codex_thread_title(name).is_some()
}

/// `MAX_SESSION_TITLE_LENGTH` of codex-acp's `SessionTitle.ts`: UTF-16 units,
/// the ellipsis included.
const CODEX_ACP_TITLE_MAX_UNITS: usize = 256;

/// codex-acp's `normalizeSessionTitle` (`SessionTitle.ts`), which makes every
/// `session_info_update.title` it publishes (a `session/list` title is not cut
/// since 2.2.2, but codeg never lists): whitespace runs collapsed to one
/// space, then a title over 256 UTF-16 units cut to 255, never through a
/// surrogate pair, and ended with "…". The cut cannot be left to codeg's own
/// 100-character one: it comes first, and the archive markers the live path
/// strips afterwards can pull it inside the characters codeg keeps.
fn codex_acp_session_title(name: &str) -> String {
    let collapsed = name
        .split(is_js_whitespace)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.encode_utf16().count() <= CODEX_ACP_TITLE_MAX_UNITS {
        return collapsed;
    }
    let mut units = 0;
    let kept: String = collapsed
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units < CODEX_ACP_TITLE_MAX_UNITS
        })
        .collect();
    format!("{}…", kept.trim_end_matches(is_js_whitespace))
}

/// What a failed turn died of, from the `error` its closing `task_complete`
/// carries (`{"message", "codex_error_info"}` as of 0.160). Read the way
/// codex-acp titles the live failure record with the same message
/// ([`crate::acp::service_error`]), so the line a reload draws is the line the
/// turn showed live.
fn codex_turn_failure(payload: &serde_json::Value) -> Option<String> {
    let error = payload.get("error")?;
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| error.as_str())?;
    Some(
        crate::acp::service_error::readable_service_error_message(message)
            .unwrap_or_else(|| message.to_string()),
    )
}

fn extract_codex_title_candidate(input: &str, fallback_attached: bool) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || is_agents_instruction_message(trimmed)
        || is_environment_context_message(trimmed)
        || is_codex_internal_context_message(trimmed)
    {
        return None;
    }

    let without_agents = strip_agents_instructions_block(trimmed);
    if without_agents.is_empty()
        || is_agents_instruction_message(&without_agents)
        || is_environment_context_message(&without_agents)
        || is_codex_internal_context_message(&without_agents)
    {
        return None;
    }

    let cleaned = strip_blocked_resource_mentions(&without_agents);
    if cleaned.is_empty() {
        if fallback_attached {
            Some("Attached resources".to_string())
        } else {
            None
        }
    } else {
        Some(title_from_user_text(&cleaned))
    }
}

fn extract_codex_text_content(payload: &serde_json::Value) -> Option<String> {
    let content = payload.get("content")?;
    if let Some(arr) = content.as_array() {
        for item in arr {
            let t = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if t == "input_text" {
                return item
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|t| t.to_string());
            }
        }
    }
    None
}

fn parse_data_uri_image(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("data:") {
        return None;
    }
    let marker = ";base64,";
    let marker_idx = trimmed.find(marker)?;
    let mime_type = trimmed.get(5..marker_idx)?.trim();
    if !mime_type.starts_with("image/") {
        return None;
    }
    let data = trimmed.get(marker_idx + marker.len()..)?.trim();
    if data.is_empty() {
        return None;
    }
    Some((mime_type.to_string(), data.to_string()))
}

fn parse_input_image_data_uri(item: &serde_json::Value) -> Option<(String, String)> {
    let data_uri = item
        .get("image_url")
        .and_then(|v| v.as_str())
        .or_else(|| {
            item.get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| item.get("url").and_then(|v| v.as_str()))?;
    parse_data_uri_image(data_uri)
}

fn first_text_block(blocks: &[ContentBlock]) -> Option<String> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::Text { text } => Some(text.clone()),
        _ => None,
    })
}

fn blocks_equal(a: &[ContentBlock], b: &[ContentBlock]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

fn should_skip_duplicate_user_message(
    messages: &[UnifiedMessage],
    blocks: &[ContentBlock],
    timestamp: DateTime<Utc>,
) -> bool {
    // Some Codex logs emit the same user message through both `response_item`
    // and `event_msg`, sometimes with a non-trivial delay. Deduplicate by
    // content in a bounded recent time window.
    const DUP_WINDOW_MS: i64 = 120_000;

    for msg in messages.iter().rev() {
        if !matches!(msg.role, MessageRole::User) {
            continue;
        }
        let delta_ms = (timestamp - msg.timestamp).num_milliseconds().abs();
        if delta_ms > DUP_WINDOW_MS {
            break;
        }
        if blocks_equal(&msg.content, blocks) {
            return true;
        }
    }

    false
}

/// Content identity of a user turn, in the exact terms the DETAIL parser
/// compares (`blocks_equal` over the blocks it would build for that record).
///
/// The summary parser must apply the same cross-channel dedup as
/// [`should_skip_duplicate_user_message`] — codex writes the same prompt
/// through both `event_msg` and `response_item` — but it is the LIGHTWEIGHT
/// pass and must not materialize blocks (or decode images) to do it. This is
/// the cheap stand-in: same text normalization, same image-parse predicate,
/// same ordering, no allocation of the block vector itself.
#[derive(Debug, PartialEq, Eq)]
struct UserTurnFingerprint {
    text: Option<String>,
    images: Vec<(String, String)>,
}

impl UserTurnFingerprint {
    /// Mirrors the detail parser's `event_msg`/`user_message` arm.
    fn from_event_message(payload: &serde_json::Value) -> Self {
        let text = normalize_user_text(
            payload
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or(""),
        );
        let images = payload
            .get("images")
            .and_then(|v| v.as_array())
            .map(|images| {
                images
                    .iter()
                    .filter_map(|image| image.as_str())
                    .filter_map(parse_data_uri_image)
                    .collect()
            })
            .unwrap_or_default();
        Self::new(text, images)
    }

    /// Mirrors [`extract_response_item_user_image_blocks`].
    fn from_response_item(payload: &serde_json::Value) -> Self {
        let mut text_parts: Vec<String> = Vec::new();
        let mut images = Vec::new();
        if let Some(content) = payload.get("content").and_then(|c| c.as_array()) {
            for item in content {
                match item.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "input_text" => {
                        let Some(text) = item.get("text").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if text.is_empty() || text.trim() == "<image>" {
                            continue;
                        }
                        text_parts.push(text.to_string());
                    }
                    "input_image" => {
                        if let Some(image) = parse_input_image_data_uri(item) {
                            images.push(image);
                        }
                    }
                    _ => {}
                }
            }
        }
        Self::new(normalize_user_text(&text_parts.join("\n")), images)
    }

    fn new(text: String, images: Vec<(String, String)>) -> Self {
        // The detail parser substitutes this placeholder when a user record
        // would otherwise carry nothing, so two such records compare equal
        // there and must compare equal here too.
        let text = if text.is_empty() {
            images.is_empty().then(|| "Attached resources".to_string())
        } else {
            Some(text)
        };
        Self { text, images }
    }
}

/// Summary-side mirror of [`should_skip_duplicate_user_message`]: same 120s
/// window, same reverse scan, same content equality. Prunes as it goes so the
/// list stays bounded by the window rather than by the transcript length.
fn skip_duplicate_user_record(
    recent: &mut Vec<(DateTime<Utc>, UserTurnFingerprint)>,
    fingerprint: UserTurnFingerprint,
    timestamp: DateTime<Utc>,
) -> bool {
    const DUP_WINDOW_MS: i64 = 120_000;
    while let Some((first, _)) = recent.first() {
        if (timestamp - *first).num_milliseconds().abs() > DUP_WINDOW_MS {
            recent.remove(0);
        } else {
            break;
        }
    }
    if recent.iter().any(|(_, seen)| *seen == fingerprint) {
        return true;
    }
    recent.push((timestamp, fingerprint));
    false
}

/// Route-only ACP blocks are transport metadata, not canonical user coverage.
/// Some codex-acp versions persist each text block as its own `user_message`;
/// letting that record mark coverage would suppress a later visible
/// `response_item` fallback in the same turn segment.
fn promotion_payload_type<'a>(msg_type: &str, value: &'a serde_json::Value) -> &'a str {
    let payload = value.get("payload");
    let payload_type = payload
        .and_then(|payload| payload.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if msg_type == "event_msg" && payload_type == "user_message" {
        let route_only = payload
            .and_then(|payload| payload.get("message"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(contains_only_internal_agent_routes);
        let has_images = payload
            .and_then(|payload| payload.get("images"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|images| !images.is_empty());
        if route_only && !has_images {
            return "";
        }
    }
    payload_type
}

/// Canonical-channel coverage for one turn segment.
#[derive(Debug, Default, Clone, Copy)]
struct SegmentCoverage {
    user: bool,
    assistant: bool,
}

/// Bookkeeping for one held-back `response_item.message` record.
#[derive(Debug, Clone, Copy)]
struct PromotionCandidate {
    /// Per-record counter. The ONLY safe key for "did this come before the
    /// goal / before the title claim": several candidates can share one
    /// insertion point in `messages`, so vector positions cannot order them.
    ordinal: u64,
    task_seg: usize,
    ctx_seg: usize,
    is_user: bool,
    /// Set once the record turns out to be a compaction's handoff summary —
    /// see [`ResponseItemPromotion::deny_compaction_summary`].
    denied: bool,
    /// [`compaction_summary_key`] of an assistant record's text, so a
    /// compaction can recognize its own handoff among the candidates.
    summary_key: Option<u64>,
}

/// A held-back `response_item.message`, kept parallel to
/// [`ResponseItemPromotion::candidates`] so one shared decision drives both.
#[derive(Debug)]
struct PendingPromotedMessage {
    /// Where in `messages` this record would have been pushed. Valid only until
    /// something else mutates `messages`, which is why the splice runs first in
    /// the post-loop sequence.
    insert_at: usize,
    is_user: bool,
    blocks: Vec<ContentBlock>,
    timestamp: DateTime<Utc>,
    /// Title this record would claim if it turns out to be the opening prompt.
    /// `None` for assistants, and for a user whose text is not title-worthy.
    title_candidate: Option<String>,
    /// Index in `messages` of the reasoning card this assistant record's arrival
    /// closed. Read only if the record turns out to be a compaction's handoff,
    /// whose reasoning that card then is.
    closed_reasoning_card: Option<usize>,
}

/// Decides which `response_item.payload.type == "message"` records may be
/// promoted into the transcript.
///
/// # Why this exists
///
/// Every message codex writes to a rollout is recorded TWICE: once on the
/// canonical event channel (`event_msg.user_message` / `event_msg.agent_message`)
/// and once as a `response_item` for the model's own history. This parser has
/// always read the event channel and ignored the `response_item` twin, which is
/// correct — until a producer writes a rollout with no event channel at all
/// (issue #452: a session created by an embedder that sets its own `CODEX_HOME`).
/// Then the tool calls still parse from `response_item.function_call` while every
/// user and assistant bubble vanishes, and the conversation renders as a bare
/// "Used N tools".
///
/// # Why the gate is per-segment
///
/// The obvious rules both fail:
///
/// * *Per record, when no same-text twin exists* — 101 assistant records across
///   60 files in the local corpus have no twin, and they are all `<proposed_plan>`
///   blocks and compaction summaries, i.e. text codex deliberately keeps out of
///   the chat. That rule regresses 60 existing conversations.
/// * *Per file, when the event channel is absent anywhere* — this is inert on the
///   corpus, but it evaporates the moment the user resumes the session in codeg:
///   native `event_msg` records append to the SAME rollout, the gate flips off,
///   and the whole imported prefix disappears again. That is exactly the workflow
///   #452 reports.
///
/// So coverage is tracked per turn segment and per role: a `response_item`
/// message is promoted only where the event channel never spoke for its role in
/// its own turn. A resumed mixed file keeps the imported prefix AND the native
/// suffix, each exactly once.
///
/// Verified inert: 0 promotions across all ~2.7k rollouts in the local corpus.
#[derive(Debug)]
struct ResponseItemPromotion {
    ordinal: u64,
    /// Coverage under each segmentation. Both are accumulated because which one
    /// applies is only known at EOF (see [`Self::resolve`]).
    task_segments: Vec<SegmentCoverage>,
    ctx_segments: Vec<SegmentCoverage>,
    saw_task_started: bool,
    candidates: Vec<PromotionCandidate>,
    /// Index of the assistant candidate that is still a compaction-summary
    /// suspect — i.e. nothing but the accounting of the response that wrote it
    /// has been seen since.
    compaction_watch: Option<usize>,
    /// The suspect the `compacted` record just denied by adjacency, held for
    /// [`Self::deny_compaction_summary`] to report.
    adjacent_summary: Option<usize>,
    /// First candidate registered since the previous `compacted` record: a
    /// compaction's handoff is always written after the compaction before it,
    /// so nothing earlier can be one.
    summary_floor: usize,
}

impl ResponseItemPromotion {
    fn new() -> Self {
        Self {
            ordinal: 0,
            task_segments: vec![SegmentCoverage::default()],
            ctx_segments: vec![SegmentCoverage::default()],
            saw_task_started: false,
            candidates: Vec::new(),
            compaction_watch: None,
            adjacent_summary: None,
            summary_floor: 0,
        }
    }

    /// Feed one parsed rollout record, BEFORE the parser's own handling of it.
    /// Returns the record's ordinal.
    fn note_record(&mut self, msg_type: &str, payload_type: &str) -> u64 {
        self.ordinal += 1;

        // Compaction adjacency. codex writes the pre-compaction handoff summary
        // as an assistant message immediately followed by the `compacted`
        // record, with nothing between them but the accounting of the response
        // that wrote it: a `token_count`, and since 0.152 a `token_usage_record`
        // before it (111 of 503 real handoffs). Denying by adjacency rather than
        // "anywhere in this segment" matters: a rollout with no turn markers
        // collapses into ONE segment, so a segment-wide rule would let a single
        // compaction erase an entire imported prefix. The handoff's own text is
        // the second, independent identification — see
        // [`Self::deny_compaction_summary`].
        match (msg_type, payload_type) {
            // Transparent — keeps the suspect under watch.
            ("event_msg", "token_count") | ("token_usage_record", _) => {}
            ("compacted", _) | ("event_msg", "context_compacted") => {
                if let Some(index) = self.compaction_watch.take() {
                    self.candidates[index].denied = true;
                    if msg_type == "compacted" {
                        self.adjacent_summary = Some(index);
                    }
                }
            }
            _ => self.compaction_watch = None,
        }

        // Segment boundaries. `task_started` fires exactly once per turn;
        // `turn_context` is the weaker fallback because newer codex re-emits it
        // MID-turn (same trade-off, same precedent as `backfill_turn_durations`).
        if msg_type == "event_msg" && payload_type == "task_started" {
            self.saw_task_started = true;
            self.task_segments.push(SegmentCoverage::default());
        } else if msg_type == "turn_context" {
            self.ctx_segments.push(SegmentCoverage::default());
        }

        // Canonical-channel coverage — "did the event channel already speak for
        // this role in this turn?".
        //
        // `thread_goal_updated` counts as USER input: newer codex consumes a
        // typed `/goal <objective>` as a slash command and records it as this
        // event INSTEAD of a `user_message`, and both parsers already synthesize
        // the opening user turn from it. Without this, the `response_item` twin
        // of that same prompt would be promoted alongside the synthesized turn
        // and the opener would render twice.
        //
        // Deliberately not narrowed to an opening `create_goal` (which would
        // mean threading the payload in and re-running `goal_marker`): a
        // `goal: null` clear is ALSO something the user typed, and a
        // terminal-status update is not, but suppressing a promotion after one
        // needs a turn that carries a goal event yet no `user_message` — i.e. a
        // rollout that HAS the event channel, which is exactly where promotion
        // is not needed. Accepted, not overlooked.
        //
        // Notably absent: `item_completed`. It has no parsing arm here, so
        // treating it as coverage would suppress a candidate and render nothing
        // in its place.
        let (user, assistant) = match (msg_type, payload_type) {
            ("event_msg", "user_message") | ("event_msg", "thread_goal_updated") => (true, false),
            ("event_msg", "agent_message") => (false, true),
            _ => (false, false),
        };
        if user || assistant {
            for segments in [&mut self.task_segments, &mut self.ctx_segments] {
                if let Some(current) = segments.last_mut() {
                    current.user |= user;
                    current.assistant |= assistant;
                }
            }
        }

        self.ordinal
    }

    /// Register a record the caller has already accepted (role allowlisted,
    /// deny-lists passed, blocks non-empty). `text` is its first text block.
    /// Returns its candidate index, which is also its index into the caller's
    /// own parallel payload vector.
    fn push_candidate(&mut self, ordinal: u64, is_user: bool, text: &str) -> usize {
        let index = self.candidates.len();
        self.candidates.push(PromotionCandidate {
            ordinal,
            task_seg: self.task_segments.len() - 1,
            ctx_seg: self.ctx_segments.len() - 1,
            is_user,
            denied: false,
            summary_key: (!is_user).then(|| compaction_summary_key(text)),
        });
        if !is_user {
            self.compaction_watch = Some(index);
        }
        index
    }

    /// At a `compacted` record (after [`Self::note_record`] saw it): deny the
    /// handoff summary this compaction wrote and return its candidate index.
    ///
    /// The handoff is text codex wrote for its next model, not a reply: live,
    /// it is never streamed, and promoted it read as the tail of the answer
    /// before it. `summary` is the text the record restates, and the handoff is
    /// only ever the candidate carrying exactly that text — the one adjacency
    /// picked, or, when a record codex has since wedged between the two broke
    /// adjacency (the way `token_usage_record` once did), the latest one written
    /// since the previous compaction. With nothing restated (an empty
    /// `message`) there is nothing to confirm a candidate by, so `None`;
    /// adjacency has still denied its suspect by then, exactly as before. `None`
    /// too for a rollout that wrote no handoff at all (older codex kept the
    /// summary in the `compacted` record alone).
    fn deny_compaction_summary(&mut self, summary: Option<&str>) -> Option<usize> {
        let adjacent = self.adjacent_summary.take();
        let floor = std::mem::replace(&mut self.summary_floor, self.candidates.len());
        let key = compaction_summary_key(summary?);
        let index = adjacent
            .filter(|&index| self.candidates[index].summary_key == Some(key))
            .or_else(|| {
                self.candidates[floor..]
                    .iter()
                    .rposition(|candidate| candidate.summary_key == Some(key))
                    .map(|offset| floor + offset)
            })?;
        self.candidates[index].denied = true;
        Some(index)
    }

    /// Which candidates survive, as a mask parallel to `candidates` (and to the
    /// caller's payload vector). Callable only at EOF — the segmentation choice
    /// depends on whether the whole file ever produced a `task_started`.
    fn resolve(&self) -> Vec<bool> {
        self.candidates
            .iter()
            .map(|candidate| {
                if candidate.denied {
                    return false;
                }
                let coverage = if self.saw_task_started {
                    self.task_segments[candidate.task_seg]
                } else {
                    self.ctx_segments[candidate.ctx_seg]
                };
                if candidate.is_user {
                    !coverage.user
                } else {
                    !coverage.assistant
                }
            })
            .collect()
    }

    /// Ordinal of the earliest surviving user candidate, used to replay the
    /// positional `/goal`-opener and title decisions the in-loop code made
    /// before these records were known.
    fn first_surviving_user_ordinal(&self, survivors: &[bool]) -> Option<u64> {
        self.candidates
            .iter()
            .zip(survivors)
            .filter(|(candidate, kept)| **kept && candidate.is_user)
            .map(|(candidate, _)| candidate.ordinal)
            .min()
    }
}

/// What codex puts in front of a compaction's handoff summary when it opens the
/// compacted history with it (`SUMMARY_PREFIX`, its `compact/summary_prefix.md`),
/// and restates in the rollout's `compacted` record: words for the next model,
/// not part of the summary. Byte-identical, then a newline, in all 521
/// compactions of the local corpus (codex 0.104 – 0.156).
const CODEX_COMPACTION_SUMMARY_PREFIX: &str = concat!(
    "Another language model started to solve this problem and produced a summary of its ",
    "thinking process. You also have access to the state of the tools that were used by that ",
    "language model. Use this to build on the work that has already been done and avoid ",
    "duplicating work. Here is the summary produced by the other language model, use the ",
    "information in this summary to assist with your own analysis:",
);

/// The handoff summary a `compacted` record restates, without codex's framing;
/// `None` when it restates none.
fn codex_compacted_summary(payload: Option<&serde_json::Value>) -> Option<&str> {
    let message = payload?.get("message")?.as_str()?;
    let summary = message
        .strip_prefix(CODEX_COMPACTION_SUMMARY_PREFIX)
        .unwrap_or(message)
        .trim();
    (!summary.is_empty()).then_some(summary)
}

/// Identity of a handoff summary's text, whitespace at its ends aside — what a
/// `compacted` record's restatement is matched against.
fn compaction_summary_key(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.trim().hash(&mut hasher);
    hasher.finish()
}

/// Tool name of the compaction divider, spelled as every parser that draws one
/// spells it (`parsers::claude::compaction_blocks` among them).
const CODEX_COMPACTION_TOOL_NAME: &str = "context_compaction";

/// The divider for a compaction, as the provider-neutral tool pair every
/// agent's compaction renders through: a ToolUse tagged `_meta.contextCompaction`
/// — which `<ContextCompactionCard>` matches on alone — and its paired
/// ToolResult, without which the card reads as a compaction still running.
///
/// The marker is the bare `{version: 1}` codex-acp sends live, so the label
/// reads the same reopened; codex records no token counts or trigger for it.
/// The summary, when the rollout kept one, opens behind the divider's
/// "Summary" toggle — see [`super::attach_compaction_summary`].
fn codex_compaction_divider(
    id: String,
    tool_use_id: String,
    summary: Option<String>,
    timestamp: DateTime<Utc>,
) -> UnifiedMessage {
    let mut content = vec![
        ContentBlock::ToolUse {
            tool_use_id: Some(tool_use_id.clone()),
            tool_name: CODEX_COMPACTION_TOOL_NAME.to_string(),
            input_preview: None,
            status: None,
            meta: Some(serde_json::json!({ "contextCompaction": { "version": 1 } })),
        },
        ContentBlock::ToolResult {
            tool_use_id: Some(tool_use_id),
            output_preview: None,
            is_error: false,
            agent_stats: None,
            images: Vec::new(),
        },
    ];
    if let Some(summary) = summary {
        super::attach_compaction_summary(&mut content, summary);
    }
    UnifiedMessage {
        id,
        role: MessageRole::Assistant,
        content,
        timestamp,
        // Bookkeeping, not a reply: it never carries spend (see
        // `bills_codex_usage`), and there is no model message to fork at.
        usage: None,
        duration_ms: None,
        model: None,
        completed_at: Some(timestamp),
        agent_message_id: None,
    }
}

/// Whether `blocks` are a compaction divider drawn by [`codex_compaction_divider`].
fn is_codex_compaction_divider(blocks: &[ContentBlock]) -> bool {
    matches!(
        blocks.first(),
        Some(ContentBlock::ToolUse { tool_name, .. }) if tool_name == CODEX_COMPACTION_TOOL_NAME
    )
}

/// Whether `message` may carry a model round's spend: any assistant message
/// but a compaction divider, which the frontend lifts out of the reply it sits
/// in — spend billed to it would drop out of every reply's footer. (The same
/// goes for `reconcile_turn_usage`'s last-resort target.)
fn bills_codex_usage(message: &UnifiedMessage) -> bool {
    matches!(message.role, MessageRole::Assistant) && !is_codex_compaction_divider(&message.content)
}

/// The id of the app-server item a `ContextCompaction` `item_completed`
/// announces (codex 0.149+), or `None` for any other record.
fn context_compaction_item_id(payload: &serde_json::Value) -> Option<&str> {
    let item = payload.get("item")?;
    if item.get("type").and_then(serde_json::Value::as_str) != Some("ContextCompaction") {
        return None;
    }
    item.get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Re-address a divider — its call and the paired result alike — by `id`.
fn rename_compaction_divider(divider: &mut UnifiedMessage, id: &str) {
    for block in &mut divider.content {
        if let ContentBlock::ToolUse { tool_use_id, .. }
        | ContentBlock::ToolResult { tool_use_id, .. } = block
        {
            *tool_use_id = Some(id.to_string());
        }
    }
}

/// Pull renderable blocks out of a `response_item.payload` of type `message`,
/// deliberately WITHOUT keying on each content item's `type` tag.
///
/// codex's item vocabulary keeps growing upstream, and this path only ever runs
/// for a rollout whose canonical channel is missing — the one situation where
/// guessing the tag wrong costs the user the entire transcript. So `input_image`
/// gets its handling and everything else carrying a string `text` is taken as
/// text (`output_text`, `input_text`, `text`, `summary_text`, …); an item with
/// neither is skipped rather than voiding the record.
///
/// [`normalize_user_text`] is applied to user text only. It collapses runs of
/// whitespace, which is right for a typed prompt (and is what the
/// `event_msg.user_message` arm does) but would mangle indentation in assistant
/// markdown — the `event_msg.agent_message` arm passes its text through
/// verbatim, and this must match it.
fn extract_response_item_message_blocks(
    payload: &serde_json::Value,
    is_user: bool,
) -> Option<Vec<ContentBlock>> {
    let content = payload.get("content")?;

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    match content {
        serde_json::Value::String(text) => text_parts.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                if item.get("type").and_then(|v| v.as_str()) == Some("input_image") {
                    if let Some((mime_type, data)) = parse_input_image_data_uri(item) {
                        blocks.push(ContentBlock::Image {
                            data,
                            mime_type,
                            uri: None,
                        });
                    }
                    continue;
                }
                let Some(text) = item.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                if text.is_empty() || text.trim() == "<image>" {
                    continue;
                }
                text_parts.push(text.to_string());
            }
        }
        _ => return None,
    }

    let joined = text_parts.join("\n");
    let text = if is_user {
        normalize_user_text(&joined)
    } else {
        joined
    };
    if !text.trim().is_empty() {
        blocks.insert(0, ContentBlock::Text { text });
    }

    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}

/// Whether a `response_item` user message carries an `input_image` — the exact
/// condition under which [`extract_response_item_user_image_blocks`] yields a
/// real user turn in the detail parser. The lightweight summary parser uses this
/// to detect the same real-user-turn so its pure-`/goal` fallback stays in sync.
fn response_item_user_has_image(payload: &serde_json::Value) -> bool {
    payload
        .get("content")
        .and_then(|c| c.as_array())
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(|v| v.as_str()) == Some("input_image")
            })
        })
}

fn extract_response_item_user_image_blocks(
    payload: &serde_json::Value,
) -> Option<Vec<ContentBlock>> {
    let content = payload.get("content")?.as_array()?;
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut has_input_image = false;

    for item in content {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "input_text" => {
                let Some(text) = item.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                if text.trim() == "<image>" {
                    continue;
                }
                if !text.is_empty() {
                    text_parts.push(text.to_string());
                }
            }
            "input_image" => {
                has_input_image = true;
                let Some((mime_type, data)) = parse_input_image_data_uri(item) else {
                    continue;
                };
                blocks.push(ContentBlock::Image {
                    data,
                    mime_type,
                    uri: None,
                });
            }
            _ => {}
        }
    }

    if !has_input_image {
        return None;
    }

    let text = normalize_user_text(&text_parts.join("\n"));
    if !text.is_empty() {
        blocks.insert(0, ContentBlock::Text { text });
    }

    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: "Attached resources".to_string(),
        });
    }

    Some(blocks)
}

/// A user record's raw text with a Codex Desktop attachment envelope read back
/// into the files it lists ([`codex_desktop_attachments::rewrite`]); any other
/// text as it is.
///
/// Applied once, to the text as the record holds it, wherever a parser reads a
/// user record: through [`normalize_user_text`] for turn text and the dedup
/// fingerprints, and directly where the summary parser titles off a record's
/// own text. Never to text that has been cleaned or trimmed already: the
/// whitespace cleanup can turn a near miss (`#  Files …`) into an envelope,
/// and a title decoded at a second pass would then disagree with the bubble.
fn decode_user_text(raw: &str) -> std::borrow::Cow<'_, str> {
    match codex_desktop_attachments::rewrite(raw) {
        Some(text) => std::borrow::Cow::Owned(text),
        None => std::borrow::Cow::Borrowed(raw),
    }
}

/// A user record's text as the transcript shows it: [`decode_user_text`], then
/// [`strip_blocked_resource_mentions`]. Both parsers' turn text and the dedup
/// fingerprints the summary parser keeps in step with the detail parser's
/// blocks go through this one function, so a record decodes the same on every
/// path that compares or counts it.
fn normalize_user_text(raw: &str) -> String {
    strip_blocked_resource_mentions(&decode_user_text(raw))
}

fn strip_blocked_resource_mentions(input: &str) -> String {
    let blocked_re = Regex::new(r"@([^\s@]+)\s*\[blocked[^\]]*\]").expect("valid blocked regex");
    let image_tag_re = Regex::new(r"(?i)</?image\s*/?>").expect("valid image tag regex");
    let collapsed_ws_re = Regex::new(r"[ \t]{2,}").expect("valid whitespace regex");
    let visible = strip_internal_agent_routes(input);
    let text = blocked_re.replace_all(&visible, "").to_string();
    let text = image_tag_re.replace_all(&text, "").to_string();
    let text = collapsed_ws_re.replace_all(&text, " ").to_string();
    text.trim().to_string()
}

/// Group flat messages into conversation turns.
/// Codex rule: consecutive Assistant + Tool messages merge into one Assistant turn.
fn group_into_turns(messages: Vec<UnifiedMessage>) -> Vec<MessageTurn> {
    let mut turns = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        if matches!(msg.role, MessageRole::User) {
            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::User,
                blocks: msg.content.clone(),
                timestamp: msg.timestamp,
                usage: None,
                duration_ms: None,
                model: None,
                completed_at: msg.completed_at,
            agent_message_id: None,
            });
            i += 1;
        } else if matches!(msg.role, MessageRole::System) {
            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::System,
                blocks: msg.content.clone(),
                timestamp: msg.timestamp,
                usage: None,
                duration_ms: None,
                model: None,
                completed_at: msg.completed_at,
            agent_message_id: None,
            });
            i += 1;
        } else {
            // Assistant or Tool — start a group
            let mut blocks: Vec<ContentBlock> = msg.content.clone();
            let mut usage = msg.usage.clone();
            let mut duration_ms = msg.duration_ms;
            let mut turn_model = msg.model.clone();
            let timestamp = msg.timestamp;
            let mut completed_at = msg.completed_at;
            i += 1;

            // Only absorb immediately following Tool messages
            // (stop at the next assistant message to keep turns small for virtualization)
            while i < messages.len() && matches!(messages[i].role, MessageRole::Tool) {
                blocks.extend(messages[i].content.clone());
                if usage.is_none() {
                    usage = messages[i].usage.clone();
                }
                if duration_ms.is_none() {
                    duration_ms = messages[i].duration_ms;
                }
                if turn_model.is_none() {
                    turn_model = messages[i].model.clone();
                }
                if messages[i].completed_at.is_some() {
                    completed_at = messages[i].completed_at;
                }
                i += 1;
            }

            turns.push(MessageTurn {
                id: format!("turn-{}", turns.len()),
                role: TurnRole::Assistant,
                blocks,
                timestamp,
                usage,
                duration_ms,
                model: turn_model,
                completed_at,
            agent_message_id: None,
            });
        }
    }

    turns
}

#[cfg(test)]
mod tests;

