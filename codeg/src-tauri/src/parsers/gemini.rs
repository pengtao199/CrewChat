use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use walkdir::WalkDir;

use crate::models::*;
use crate::parsers::{
    folder_name_from_path, title_from_user_text, truncate_str, AgentParser, ParseError,
};

pub struct GeminiParser {
    base_dir: PathBuf,
}

impl Default for GeminiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiParser {
    pub fn new() -> Self {
        let base_dir = resolve_gemini_base_dir();
        Self { base_dir }
    }

    /// Test-only constructor that lets callers point the parser at a fixture
    /// directory instead of `~/.gemini`.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn tmp_dir(&self) -> PathBuf {
        self.base_dir.join("tmp")
    }

    fn history_dir(&self) -> PathBuf {
        self.base_dir.join("history")
    }

    fn projects_json_path(&self) -> PathBuf {
        self.base_dir.join("projects.json")
    }

    /// Gemini writes two shapes under a project's `chats/` directory:
    ///
    /// - a main session as `chats/session-<ts>-<shortId>.json[l]`;
    /// - a SUBAGENT session as `chats/<parentSessionId>/<sessionId>.jsonl` —
    ///   one directory deeper and with NO `session-` prefix
    ///   (`ChatRecordingService::initialize`, gemini-cli 0.60.0).
    ///
    /// Requiring both the prefix and `chats` as the immediate parent — what this
    /// used to do — made every subagent transcript invisible. Accept both
    /// layouts; anything else under `chats/` that fails to parse is skipped by
    /// the callers anyway.
    fn is_chat_file(path: &Path) -> bool {
        let Some(extension) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
        else {
            return false;
        };
        if !matches!(extension.as_str(), "json" | "jsonl") {
            return false;
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());

        if parent_name == Some("chats") {
            return file_name.starts_with("session-");
        }

        // Subagent transcript: `chats/<parentSessionId>/<sessionId>.jsonl`.
        path.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("chats")
    }

    /// The parent session id a subagent transcript belongs to, i.e. the
    /// directory name in `chats/<parentSessionId>/<sessionId>.jsonl`. `None`
    /// for a main session (whose parent directory IS `chats`).
    fn subagent_parent_id_from_chat_path(path: &Path) -> Option<String> {
        let parent = path.parent()?;
        let parent_name = parent.file_name()?.to_str()?;
        if parent_name == "chats" {
            return None;
        }
        (parent.parent()?.file_name()?.to_str()? == "chats")
            .then(|| parent_name.to_string())
    }

    fn parse_chat_value(path: &Path, raw: &str) -> Option<Value> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if extension.as_deref() == Some("jsonl") {
            Self::parse_jsonl_chat_value(raw)
        } else {
            serde_json::from_str(raw).ok()
        }
    }

    /// Insert or REPLACE a message record, keeping the position it was first
    /// seen at. Gemini's reader is `messagesMap.set(id, record)` on a JS `Map`
    /// — a whole-record overwrite that preserves insertion order. The previous
    /// per-field merge kept stale keys (a `toolCalls` array from an earlier
    /// partial write survived a later record that dropped it), which surfaced
    /// as ghost tool calls in the transcript.
    fn upsert_message(
        messages: &mut Vec<Value>,
        index_by_id: &mut HashMap<String, usize>,
        id: &str,
        value: Value,
    ) {
        match index_by_id.get(id).copied() {
            Some(index) => messages[index] = value,
            None => {
                index_by_id.insert(id.to_string(), messages.len());
                messages.push(value);
            }
        }
    }

    /// `{"$rewindTo": "<id>"}` — drop that message and everything after it.
    /// When the id is unknown gemini clears the WHOLE history rather than
    /// leaving it untouched, so a rewind onto an already-rewound target does not
    /// resurrect anything.
    fn apply_rewind(
        messages: &mut Vec<Value>,
        index_by_id: &mut HashMap<String, usize>,
        rewind_id: &str,
    ) {
        match index_by_id.get(rewind_id).copied() {
            Some(index) => {
                messages.truncate(index);
                index_by_id.retain(|_, position| *position < index);
            }
            None => {
                messages.clear();
                index_by_id.clear();
            }
        }
    }

    fn absorb_inline_messages(
        messages: &mut Vec<Value>,
        index_by_id: &mut HashMap<String, usize>,
        list: &[Value],
    ) {
        for message in list {
            // `isMessageRecord` upstream: a string `id` is the whole test.
            let Some(id) = message.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            Self::upsert_message(messages, index_by_id, id, message.clone());
        }
    }

    /// Replay a `.jsonl` transcript the way gemini-cli's own
    /// `loadConversationRecord` (packages/core/src/services/chatRecordingService.ts,
    /// 0.60.0) does. It classifies each line into one of four record kinds, in
    /// this order:
    ///
    /// 1. `$rewindTo` (string)  — truncate from that id; clear all if unknown.
    /// 2. `id` (string)         — a message; whole-record replace, position kept.
    /// 3. `$set` (object)       — metadata merge; a `$set.messages` ARRAY CLEARS
    ///    the history and rebuilds it from that array.
    /// 4. `sessionId` + `projectHash` (both strings) — metadata merge, plus any
    ///    inline `messages` array APPENDED (not cleared).
    ///
    /// Kinds 1 and 3 were previously unhandled entirely, and kind 2 additionally
    /// required a `type` field. Since `GeminiChat.initialize()` reconciles history
    /// on startup — and resumes, aborted prompts and context compression all
    /// rewrite the full array — most real transcripts carry their messages inside
    /// `$set.messages`, and every one of them parsed as an empty session.
    ///
    /// A line that is not valid JSON is SKIPPED (upstream wraps `JSON.parse` in
    /// its own try/catch); it used to abort the entire file, so one truncated
    /// write made a whole session disappear.
    fn parse_jsonl_chat_value(raw: &str) -> Option<Value> {
        let mut metadata = Map::new();
        let mut messages: Vec<Value> = Vec::new();
        let mut index_by_id: HashMap<String, usize> = HashMap::new();

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            let Some(object) = value.as_object() else {
                continue;
            };

            if let Some(rewind_id) = object.get("$rewindTo").and_then(|v| v.as_str()) {
                Self::apply_rewind(&mut messages, &mut index_by_id, rewind_id);
                continue;
            }

            // Own the id before handing `value` over: `object` borrows `value`,
            // so a `&str` into it cannot survive the move.
            if let Some(id) = object.get("id").and_then(|v| v.as_str()).map(str::to_string) {
                Self::upsert_message(&mut messages, &mut index_by_id, &id, value);
                continue;
            }

            if let Some(set) = object.get("$set").and_then(|v| v.as_object()) {
                if let Some(list) = set.get("messages").and_then(|v| v.as_array()) {
                    messages.clear();
                    index_by_id.clear();
                    Self::absorb_inline_messages(&mut messages, &mut index_by_id, list);
                }
                for (key, value) in set {
                    metadata.insert(key.clone(), value.clone());
                }
                continue;
            }

            let is_partial_metadata = object
                .get("sessionId")
                .and_then(|v| v.as_str())
                .is_some()
                && object.get("projectHash").and_then(|v| v.as_str()).is_some();
            if is_partial_metadata {
                for (key, value) in object {
                    metadata.insert(key.clone(), value.clone());
                }
                if let Some(list) = object.get("messages").and_then(|v| v.as_array()) {
                    Self::absorb_inline_messages(&mut messages, &mut index_by_id, list);
                }
            }
        }

        let has_session_id = metadata.get("sessionId").and_then(|v| v.as_str()).is_some();
        let has_project_hash = metadata
            .get("projectHash")
            .and_then(|v| v.as_str())
            .is_some();

        if !has_session_id || !has_project_hash {
            // `parseLegacyRecordFallback`: re-read the file as ONE JSON object.
            if let Some(legacy) = Self::parse_legacy_record_value(raw) {
                return Some(legacy);
            }
            // Upstream gives up here. codeg keeps a transcript that named its
            // own session: `projectHash` is metadata codeg never reads, and
            // dropping the session would hide history the user can still see in
            // gemini's own `/resume` list for older files.
            if !has_session_id {
                return None;
            }
        }

        metadata.insert("messages".to_string(), Value::Array(messages));
        Some(Value::Object(metadata))
    }

    /// The pre-JSONL layout: the whole file is a single object carrying
    /// `sessionId` and a `messages` array (`parseLegacyRecordFallback`).
    fn parse_legacy_record_value(raw: &str) -> Option<Value> {
        let value: Value = serde_json::from_str(raw).ok()?;
        value.get("sessionId").and_then(|v| v.as_str())?;
        Some(value)
    }

    fn list_chat_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = Vec::new();

        // Scan both tmp/ (active sessions) and history/ (archived sessions)
        for dir in [self.tmp_dir(), self.history_dir()] {
            if !dir.exists() {
                continue;
            }
            let found = WalkDir::new(&dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_path_buf())
                .filter(|p| p.is_file() && Self::is_chat_file(p));
            files.extend(found);
        }

        files.sort();
        files.dedup();
        files
    }

    /// The project directory name that owns this transcript: the component
    /// directly above the `chats/` directory. Walking a fixed two levels up —
    /// what this used to do — lands on `chats` itself for a subagent transcript
    /// (`<alias>/chats/<parentSessionId>/<sessionId>.jsonl`), which would then be
    /// looked up as a project alias and never resolve.
    fn project_alias_from_chat_path(path: &Path) -> Option<String> {
        let components: Vec<_> = path.iter().collect();
        let chats_index = components
            .iter()
            .rposition(|component| component.to_str() == Some("chats"))?;
        let alias = components.get(chats_index.checked_sub(1)?)?;
        Some(alias.to_string_lossy().to_string())
    }

    fn read_project_root_file(path: PathBuf) -> Option<String> {
        let raw = fs::read_to_string(path).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn resolve_project_root(&self, alias: &str) -> Option<String> {
        let tmp_root = self.tmp_dir().join(alias).join(".project_root");
        if let Some(path) = Self::read_project_root_file(tmp_root) {
            return Some(path);
        }

        let history_root = self.history_dir().join(alias).join(".project_root");
        if let Some(path) = Self::read_project_root_file(history_root) {
            return Some(path);
        }

        self.resolve_project_root_from_projects_json(alias)
    }

    fn resolve_project_root_from_projects_json(&self, alias: &str) -> Option<String> {
        let raw = fs::read_to_string(self.projects_json_path()).ok()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let projects = value.get("projects")?.as_object()?;
        projects
            .iter()
            .find_map(|(path, mapped_alias)| (mapped_alias.as_str() == Some(alias)).then_some(path))
            .map(|s| s.to_string())
    }

    fn parse_timestamp(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
        value.and_then(|v| v.as_str()?.parse::<DateTime<Utc>>().ok())
    }

    fn extract_text(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Value::Array(items) => {
                let mut parts = Vec::new();
                for item in items {
                    if let Some(text) = item.get("text").and_then(Self::extract_text) {
                        parts.push(text);
                    } else if let Some(text) = Self::extract_text(item) {
                        parts.push(text);
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n"))
                }
            }
            Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(Self::extract_text) {
                    return Some(text);
                }
                if let Some(text) = map.get("message").and_then(Self::extract_text) {
                    return Some(text);
                }
                None
            }
            _ => None,
        }
    }

    fn extract_message_text(message: &Value) -> Option<String> {
        message
            .get("content")
            .and_then(Self::extract_text)
            .or_else(|| message.get("message").and_then(Self::extract_text))
    }

    fn parse_data_uri_image(raw: &str) -> Option<(String, String)> {
        let trimmed = raw.trim();
        let without_prefix = trimmed.strip_prefix("data:")?;
        let marker = ";base64,";
        let marker_idx = without_prefix.find(marker)?;
        let mime_type = without_prefix.get(..marker_idx)?.trim();
        if !mime_type.starts_with("image/") {
            return None;
        }
        let data = without_prefix.get(marker_idx + marker.len()..)?.trim();
        if data.is_empty() {
            return None;
        }
        Some((mime_type.to_string(), data.to_string()))
    }

    fn parse_user_image_part(part: &Value) -> Option<ContentBlock> {
        let inline = part
            .get("inlineData")
            .or_else(|| part.get("inline_data"))
            .unwrap_or(part);
        let data = inline
            .get("data")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())?;

        if let Some((mime_type, data)) = Self::parse_data_uri_image(data) {
            return Some(ContentBlock::Image {
                data,
                mime_type,
                uri: None,
            });
        }

        let mime_type = inline
            .get("mimeType")
            .or_else(|| inline.get("mime_type"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|m| !m.is_empty() && m.starts_with("image/"))?;
        let uri = inline
            .get("fileUri")
            .or_else(|| inline.get("uri"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string());

        Some(ContentBlock::Image {
            data: data.to_string(),
            mime_type: mime_type.to_string(),
            uri,
        })
    }

    fn parse_user_blocks(message: &Value) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        let content = match message.get("content") {
            Some(c) => c,
            None => {
                if let Some(text) = message.get("message").and_then(Self::extract_text) {
                    blocks.push(ContentBlock::Text { text });
                }
                return blocks;
            }
        };

        if let Some(text) = content
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
        {
            blocks.push(ContentBlock::Text { text });
            return blocks;
        }

        if let Some(parts) = content.as_array() {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Self::extract_text) {
                    blocks.push(ContentBlock::Text { text });
                } else if let Some(text) = Self::extract_text(part) {
                    blocks.push(ContentBlock::Text { text });
                }

                if let Some(image) = Self::parse_user_image_part(part) {
                    blocks.push(image);
                }
            }
            return blocks;
        }

        if let Some(image) = Self::parse_user_image_part(content) {
            blocks.push(image);
            return blocks;
        }

        if let Some(text) = Self::extract_text(content) {
            blocks.push(ContentBlock::Text { text });
        }

        blocks
    }

    fn parse_summary_from_value(&self, path: &Path, value: &Value) -> Option<ConversationSummary> {
        let id = value.get("sessionId").and_then(|v| v.as_str())?.to_string();
        let messages = value
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();

        let first_message_ts = messages
            .first()
            .and_then(|m| Self::parse_timestamp(m.get("timestamp")));
        let last_message_ts = messages
            .iter()
            .rev()
            .find_map(|m| Self::parse_timestamp(m.get("timestamp")));

        let started_at = Self::parse_timestamp(value.get("startTime"))
            .or(first_message_ts)
            .unwrap_or_else(Utc::now);
        let ended_at = Self::parse_timestamp(value.get("lastUpdated")).or(last_message_ts);

        // `$set.summary` is gemini's own session summary (`saveSummary`), the
        // closest thing to an authoritative title, so it wins. Next comes the
        // AI-generated title from the `update_topic` tool call (newest non-empty
        // one). Last resort is the first real user message, skipping the
        // injected `<session_context>` bootstrap envelope.
        let summary_title = value
            .get("summary")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| truncate_str(s, 100));

        let topic_title = messages.iter().rev().find_map(|m| {
            m.get("toolCalls")
                .and_then(|c| c.as_array())
                .and_then(|calls| {
                    calls.iter().rev().find_map(|call| {
                        if call.get("name").and_then(|n| n.as_str()) == Some("update_topic") {
                            call.get("args")
                                .and_then(|a| a.get("title"))
                                .and_then(|t| t.as_str())
                                .map(str::trim)
                                .filter(|t| !t.is_empty())
                                .map(|t| truncate_str(t, 100))
                        } else {
                            None
                        }
                    })
                })
        });

        let fallback_title = messages
            .iter()
            .filter(|m| m.get("type").and_then(|t| t.as_str()) == Some("user"))
            .filter_map(Self::extract_message_text)
            .find(|t| !t.trim_start().starts_with("<session_context"))
            .map(|t| title_from_user_text(&t));

        let title = summary_title.or(topic_title).or(fallback_title);

        let model = messages.iter().rev().find_map(|m| {
            m.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

        let folder_alias = Self::project_alias_from_chat_path(path);
        let folder_path = folder_alias
            .as_deref()
            .and_then(|alias| self.resolve_project_root(alias))
            // `$set.directories` is the workspace list gemini records for the
            // session (`recordDirectories`). It is the only in-file statement of
            // where the session ran, so it backs up the alias lookup when
            // neither `.project_root` nor `projects.json` knows the alias.
            .or_else(|| {
                value
                    .get("directories")
                    .and_then(|v| v.as_array())
                    .and_then(|list| list.first())
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            });
        let folder_name = folder_path
            .as_ref()
            .map(|p| folder_name_from_path(p))
            .or(folder_alias);

        // A subagent transcript is a CHILD: `parent_id` keeps it out of the
        // import list and the root conversation list, exactly like a delegation
        // child from any other agent.
        let parent_id = (value.get("kind").and_then(|v| v.as_str()) == Some("subagent"))
            .then(|| Self::subagent_parent_id_from_chat_path(path))
            .flatten();

        Some(ConversationSummary {
            id,
            agent_type: AgentType::Gemini,
            folder_path,
            folder_name,
            title,
            started_at,
            ended_at,
            message_count: messages.len() as u32,
            model,
            git_branch: None,
            parent_id,
            parent_tool_use_id: None,
            delegation_call_id: None,
        })
    }

    fn result_preview(result: Option<&Value>) -> Option<String> {
        let v = result?;
        if let Some(s) = v.as_str() {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            return Some(trimmed.to_string());
        }
        serde_json::to_string(v).ok()
    }

    fn result_display_preview(result_display: Option<&Value>) -> Option<String> {
        let value = result_display?;
        if let Some(summary) = value
            .get("summary")
            .and_then(Self::extract_text)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(summary);
        }

        Self::result_preview(Some(value))
    }

    fn tool_call_is_error(call: &Value, output_preview: Option<&str>) -> bool {
        if call
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "error" | "failed" | "failure" | "cancelled" | "canceled"
                )
            })
            .unwrap_or(false)
        {
            return true;
        }

        if call
            .get("result")
            .and_then(|r| r.as_array())
            .map(|items| {
                items.iter().any(|item| {
                    item.get("functionResponse")
                        .and_then(|fr| fr.get("response"))
                        .and_then(|resp| resp.get("error"))
                        .is_some()
                })
            })
            .unwrap_or(false)
        {
            return true;
        }

        output_preview
            .map(|s| s.trim_start().to_ascii_lowercase().starts_with("error"))
            .unwrap_or(false)
    }

    fn parse_assistant_blocks(message: &Value) -> Vec<ContentBlock> {
        let mut blocks: Vec<ContentBlock> = Vec::new();

        if let Some(thoughts) = message.get("thoughts").and_then(|v| v.as_array()) {
            for thought in thoughts {
                let subject = thought
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let description = thought
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let text = match (subject, description) {
                    (Some(sub), Some(desc)) => format!("{sub}: {desc}"),
                    (Some(sub), None) => sub.to_string(),
                    (None, Some(desc)) => desc.to_string(),
                    (None, None) => continue,
                };
                blocks.push(ContentBlock::Thinking { text });
            }
        }

        if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
            for call in tool_calls {
                let tool_use_id = call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let tool_name = call
                    .get("displayName")
                    .or_else(|| call.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let input_preview = call
                    .get("args")
                    .and_then(|v| serde_json::to_string(v).ok())
                    .or_else(|| {
                        call.get("input")
                            .and_then(|v| Self::result_preview(Some(v)))
                    });

                blocks.push(ContentBlock::ToolUse {
                    tool_use_id: tool_use_id.clone(),
                    tool_name,
                    input_preview,
                    status: None,
                    meta: None,
                });

                let output_preview = Self::result_display_preview(call.get("resultDisplay"))
                    .or_else(|| Self::result_preview(call.get("result")));
                let is_error = Self::tool_call_is_error(call, output_preview.as_deref());

                blocks.push(ContentBlock::ToolResult {
                    tool_use_id,
                    output_preview,
                    is_error,
                    agent_stats: None,
                    images: Vec::new(),
                });
            }
        }

        if let Some(text) = Self::extract_message_text(message) {
            blocks.push(ContentBlock::Text { text });
        }

        blocks
    }

    /// Gemini records `{input, output, cached, thoughts, tool, total}` straight
    /// from the API's `usageMetadata` (`recordMessageTokens`), where:
    ///
    /// - `input` is `promptTokenCount`, which **already includes** `cached`
    ///   (`cachedContentTokenCount`) — gemini's own telemetry derives the
    ///   uncached part as `max(0, prompt - cached)`;
    /// - `output` is `candidatesTokenCount`, which **excludes** `thoughts`
    ///   (`thoughtsTokenCount`) and `tool` (`toolUsePromptTokenCount`);
    /// - `total` is the API's `totalTokenCount`.
    ///
    /// codeg's [`TurnUsage`] buckets are Anthropic-shaped and DISJOINT —
    /// `compute_session_stats` adds all four. Passing `input` through verbatim
    /// alongside `cached` therefore counted the cache twice, and `thoughts` /
    /// `tool` were dropped entirely.
    fn parse_usage(message: &Value) -> Option<TurnUsage> {
        let tokens = message.get("tokens")?;
        let field = |key: &str| tokens.get(key).and_then(|v| v.as_u64());

        let input = field("input").unwrap_or(0);
        let cached = field("cached").unwrap_or(0);
        let output = field("output");
        let thoughts = field("thoughts");
        let tool = field("tool");

        let output_tokens = match (output, thoughts, tool) {
            // Nothing on the generated side was recorded: derive it from the
            // API total. `input` already carries `cached`, so subtracting it
            // again would undercount.
            (None, None, None) => field("total")
                .map(|total| total.saturating_sub(input))
                .unwrap_or(0),
            _ => output
                .unwrap_or(0)
                .saturating_add(thoughts.unwrap_or(0))
                .saturating_add(tool.unwrap_or(0)),
        };

        Some(TurnUsage {
            input_tokens: input.saturating_sub(cached),
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: cached,
        })
    }

    fn parse_conversation_detail(
        &self,
        path: &Path,
        value: &Value,
        conversation_id: &str,
    ) -> Result<ConversationDetail, ParseError> {
        let mut summary = self
            .parse_summary_from_value(path, value)
            .ok_or_else(|| ParseError::ConversationNotFound(conversation_id.to_string()))?;
        let messages_raw = value
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();

        let mut messages: Vec<UnifiedMessage> = Vec::new();
        for raw in messages_raw {
            let msg_id = raw
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("msg-{}", messages.len()));
            let timestamp =
                Self::parse_timestamp(raw.get("timestamp")).unwrap_or(summary.started_at);
            let msg_type = raw
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();

            match msg_type.as_str() {
                "user" => {
                    let blocks = Self::parse_user_blocks(&raw);
                    if blocks.is_empty() {
                        continue;
                    }
                    messages.push(UnifiedMessage {
                        id: msg_id,
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
                "gemini" | "assistant" | "model" => {
                    let blocks = Self::parse_assistant_blocks(&raw);
                    if blocks.is_empty() {
                        continue;
                    }
                    messages.push(UnifiedMessage {
                        id: msg_id,
                        role: MessageRole::Assistant,
                        content: blocks,
                        timestamp,
                        usage: Self::parse_usage(&raw),
                        duration_ms: None,
                        model: raw
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        completed_at: Some(timestamp),
                    agent_message_id: None,
                    });
                }
                "system" => {
                    let Some(text) = Self::extract_message_text(&raw) else {
                        continue;
                    };
                    messages.push(UnifiedMessage {
                        id: msg_id,
                        role: MessageRole::System,
                        content: vec![ContentBlock::Text { text }],
                        timestamp,
                        usage: None,
                        duration_ms: None,
                        model: None,
                        completed_at: Some(timestamp),
                    agent_message_id: None,
                    });
                }
                _ => {}
            }
        }

        let mut turns = group_into_turns(messages);
        super::relocate_orphaned_tool_results(&mut turns);
        super::structurize_read_tool_output(&mut turns);
        super::resolve_patch_line_numbers(&mut turns, summary.folder_path.as_deref());
        // Gemini logs no timings, so durations are inferred from the timeline.
        // The span that belongs to a reply is the one BEFORE it: taking the gap
        // to the *next* message charged the last reply of a turn with however
        // long the user then took to type (which the old `< 300_000` guard only
        // capped at five minutes rather than excluded).
        super::backfill_turn_durations(&mut turns, &[]);
        summary.message_count = turns.len() as u32;
        summary.id = conversation_id.to_string();
        // Gemini gauges context as `promptTokenCount / tokenLimit(model)`
        // (`getContextUsagePercentage`) — the reply is NOT resident in the
        // prompt window that produced it. `latest_turn_prompt_usage_tokens`
        // sums `input + cache_creation + cache_read`, which for the mapping in
        // `parse_usage` reconstitutes exactly `promptTokenCount`.
        let context_window_used_tokens = super::latest_turn_prompt_usage_tokens(&turns);
        let context_window_max_tokens =
            super::infer_context_window_max_tokens(summary.model.as_deref());
        let session_stats = super::merge_context_window_stats(
            super::compute_session_stats(&turns),
            context_window_used_tokens,
            context_window_max_tokens,
        );

        Ok(ConversationDetail {
            summary,
            turns,
            session_stats,
            transcript_watermark: None,
        })
    }
}

pub(crate) fn resolve_gemini_base_dir() -> PathBuf {
    resolve_gemini_base_dir_from(std::env::var_os("GEMINI_CLI_HOME"), dirs::home_dir())
}

fn resolve_gemini_base_dir_from(
    gemini_cli_home_env: Option<std::ffi::OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    gemini_cli_home_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir.unwrap_or_default())
        .join(".gemini")
}

impl AgentParser for GeminiParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        let mut conversations = Vec::new();

        for chat_file in self.list_chat_files() {
            // NOTE: not routed through `summary_cache` — unlike the other
            // single-file parsers, a Gemini summary is not a pure function of the
            // chat file's bytes: `parse_summary_from_value` resolves `folder_path`
            // from external `.project_root` / `projects.json` files that can change
            // while the chat file does not, so an (mtime, size) key on the chat
            // file alone would serve a stale folder. See summary_cache.rs.
            let raw = match fs::read_to_string(&chat_file) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            let Some(value) = Self::parse_chat_value(&chat_file, &raw) else {
                continue;
            };
            if let Some(summary) = self.parse_summary_from_value(&chat_file, &value) {
                conversations.push(summary);
            }
        }

        conversations.sort_by_key(|b| std::cmp::Reverse(b.started_at));
        Ok(conversations)
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        for chat_file in self.list_chat_files() {
            let raw = match fs::read_to_string(&chat_file) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            if !raw.contains(conversation_id) {
                continue;
            }

            let Some(value) = Self::parse_chat_value(&chat_file, &raw) else {
                continue;
            };
            let session_id = value.get("sessionId").and_then(|v| v.as_str());
            if session_id != Some(conversation_id) {
                continue;
            }

            return self.parse_conversation_detail(&chat_file, &value, conversation_id);
        }

        Err(ParseError::ConversationNotFound(
            conversation_id.to_string(),
        ))
    }
}

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
            continue;
        }

        if matches!(msg.role, MessageRole::System) {
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
            continue;
        }

        let mut blocks = msg.content.clone();
        let mut usage = msg.usage.clone();
        let mut duration_ms = msg.duration_ms;
        let mut models: Vec<String> = msg.model.iter().cloned().collect();
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
            if let Some(model) = &messages[i].model {
                models.push(model.clone());
            }
            if messages[i].completed_at.is_some() {
                completed_at = messages[i].completed_at;
            }
            i += 1;
        }

        let model = models.pop();

        turns.push(MessageTurn {
            id: format!("turn-{}", turns.len()),
            role: TurnRole::Assistant,
            blocks,
            timestamp,
            usage,
            duration_ms,
            model,
            completed_at,
        agent_message_id: None,
        });
    }

    turns
}

#[cfg(test)]
mod tests;

