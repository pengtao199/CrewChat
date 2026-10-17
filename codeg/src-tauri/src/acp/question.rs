//! Interactive multiple-choice question ("ask the user") domain types.
//!
//! Mid-turn an agent can ask the user one or more multiple-choice questions and
//! BLOCK until the user answers — the `ask_user_question` MCP tool exposed by
//! `codeg-mcp`. Unlike live-feedback ([`crate::acp::feedback`]), which is a
//! non-blocking pull the user pushes into, a question PAUSES the agent's tool
//! call: the questions render as an interactive card above the conversation
//! input box (driven by [`crate::acp::session_state::SessionState`], in-memory
//! and turn-scoped — it is real-time steering, not durable history), and the
//! tool call returns only once the user submits their choices.
//!
//! This module holds the pieces shared across layers so the manager, the
//! delegation listener, the MCP companion plumbing, and the settings command
//! don't each grow their own copy:
//!   * [`QuestionSpec`] / [`QuestionOption`] — one question + its choices.
//!   * [`PendingQuestionState`] — the awaiting-answer set stored on the session.
//!   * [`QuestionAnswer`] / [`QuestionAnswerItem`] — the user's submission
//!     (frontend → backend).
//!   * [`QuestionOutcome`] / [`QuestionAnsweredItem`] — the self-describing
//!     result handed back to the blocked tool (so the companion can render it
//!     without re-holding the questions).
//!   * [`SessionQuestionAccess`] — the listener-facing trait the production
//!     `ConnectionManager` implements (kept here so the listener can be unit
//!     tested with an in-memory stub, mirroring `SessionFeedbackAccess`).
//!   * [`QuestionRuntimeConfig`] — the hot-swappable "is the feature on?" flag,
//!     read at MCP injection time (mirrors [`crate::acp::feedback`]).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAcceptAction, ElicitationAction,
    ElicitationContentValue, ElicitationMode, ElicitationPropertySchema, ElicitationScope,
    MultiSelectItems, StringPropertySchema,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::{oneshot, RwLock};

/// Max questions per `ask_user_question` call. Matches Claude Code's
/// `AskUserQuestion` contract; the JSON schema advertises the same `maxItems`.
pub const MAX_QUESTIONS: usize = 4;
/// Min / max selectable options per question. Fewer than two options is not a
/// meaningful choice; more than four overwhelms the card. Matches Claude Code.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
/// Max characters for a question's short `header` chip.
pub const MAX_HEADER_CHARS: usize = 12;
/// Per-field sanity bound (characters) for every agent/user-supplied free-text
/// field: the question text, each option label + description, and the free-text
/// "Other" answer. The full text rides in the broadcast event, the snapshot, and
/// the agent-facing tool result, so this caps the blast radius of a pathological
/// field — whether from a malformed agent (`parse_questions`) or a hand-rolled
/// client hitting `acp_answer_question` directly (`build_outcome`). The UI can't
/// produce anything this long.
pub const MAX_QUESTION_TEXT_CHARS: usize = 4096;

/// One selectable choice in a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Concise display text. A recommended option puts itself first and ends
    /// its label with "(Recommended)" (a string convention, like Claude Code).
    pub label: String,
    /// What this choice means / its trade-off. May be empty.
    pub description: String,
}

/// A single multiple-choice question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionSpec {
    /// Backend-minted stable id. Used as the answer correlation key instead of
    /// the question text (which Claude Code keys on) so duplicate question
    /// strings or reordering can't collide.
    pub id: String,
    /// The full question shown to the user.
    pub question: String,
    /// Short category label (≤ [`MAX_HEADER_CHARS`]) rendered as a chip.
    pub header: String,
    /// When true the user may select multiple options.
    pub multi_select: bool,
    /// The choices (0..=[`MAX_OPTIONS`]). Empty means free-text: the card
    /// renders only its always-present "Other" input (codex `request_user_input`
    /// and MCP-server elicitations both ask open questions this way; the
    /// codeg-mcp ask tool still requires [`MIN_OPTIONS`] at its own parse
    /// layer).
    pub options: Vec<QuestionOption>,
    /// True when the answer is a secret (codex `request_user_input` marks API
    /// keys etc. with `_meta.codex.isSecret`): the card masks the free-text
    /// input. Default false — absent on the wire for every non-secret source.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_secret: bool,
}

/// The pending (awaiting-answer) question set stored on
/// `SessionState.pending_question` and carried on `to_snapshot()` so a client
/// attaching mid-turn (cold attach, reconnect, another window) re-renders the
/// card even though the one-shot `QuestionRequest` event won't replay for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingQuestionState {
    pub question_id: String,
    pub questions: Vec<QuestionSpec>,
    pub created_at: DateTime<Utc>,
}

/// One question's answer (frontend → backend). `labels` carries the selected
/// option labels (and any free-text "Other" the user typed, which the host UI
/// always offers); single-select submits exactly one label. camelCase on the
/// wire — this is constructed by the frontend, not read from an event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswerItem {
    pub question_id: String,
    pub labels: Vec<String>,
}

/// The user's full submission for a pending question set (frontend → backend →
/// the blocked tool). `declined` is set when the user dismissed the card
/// without choosing — the agent then proceeds with its own judgment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    #[serde(default)]
    pub answers: Vec<QuestionAnswerItem>,
    #[serde(default)]
    pub declined: bool,
}

/// One answered question, joined with its prompt text so the result is
/// self-describing (the companion renders it without holding the questions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnsweredItem {
    pub question: String,
    pub header: String,
    pub multi_select: bool,
    /// The labels the user chose (or typed via "Other").
    pub selected: Vec<String>,
}

/// The resolved outcome delivered over the broker socket to the blocked tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOutcome {
    #[serde(default)]
    pub answers: Vec<QuestionAnsweredItem>,
    #[serde(default)]
    pub declined: bool,
}

/// What [`SessionQuestionAccess::register_question`] hands back to the listener:
/// the new question id plus the receiver to await the user's answer on.
pub struct RegisteredQuestion {
    pub question_id: String,
    pub answer_rx: oneshot::Receiver<QuestionOutcome>,
}

/// Listener-facing access to register / cancel a pending question on a parent
/// connection. The production impl (`ConnectionManagerQuestionLookup`) wraps the
/// `ConnectionManager`; tests use an in-memory stub. Mirrors
/// [`crate::acp::feedback::SessionFeedbackAccess`] and
/// `crate::acp::delegation::listener::ParentSessionLookup`.
#[async_trait]
pub trait SessionQuestionAccess: Send + Sync {
    /// Register a question set on the parent connection (resolved from the
    /// per-launch token), broadcast it to every attached client, and return a
    /// receiver that resolves when the user answers (or the question is
    /// canceled). `None` when the connection is gone — nothing to ask.
    async fn register_question(
        &self,
        parent_connection_id: &str,
        questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion>;

    /// Cancel a pending question — the companion's tool call was canceled
    /// (peer-close) or the connection is tearing down. Removes it and clears
    /// the card on every client. No-op if it was already answered / gone.
    async fn cancel_question(&self, parent_connection_id: &str, question_id: &str);

    /// Cancel every pending question parked on a connection that is tearing
    /// down. Called from the `run_connection` cleanup guard (alongside the
    /// delegation `cancel_by_parent` cascade) so a question entry — and the
    /// listener task parked on it — is reclaimed synchronously on disconnect,
    /// rather than lingering until the companion's ask socket happens to close.
    /// No-op when the connection has no pending ask.
    async fn cancel_questions_by_parent(&self, parent_connection_id: &str);
}

/// Validate + parse the MCP `ask_user_question` arguments into typed
/// [`QuestionSpec`]s, minting a stable id per question. Enforces the contract
/// (1..=[`MAX_QUESTIONS`] questions, each with a non-empty question + header
/// ≤ [`MAX_HEADER_CHARS`] and [`MIN_OPTIONS`]..=[`MAX_OPTIONS`] labeled options)
/// so a malformed call is rejected synchronously with a helpful message the LLM
/// can fix, rather than round-tripping bad data. `multiSelect` defaults to
/// false; an option `description` defaults to empty (lenient).
pub fn parse_questions(arguments: &Value) -> Result<Vec<QuestionSpec>, String> {
    let arr = arguments
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "ask_user_question requires a `questions` array".to_string())?;
    if arr.is_empty() {
        return Err("ask_user_question requires at least one question".to_string());
    }
    if arr.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask_user_question supports at most {MAX_QUESTIONS} questions per call"
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (qi, q) in arr.iter().enumerate() {
        let question = q
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("questions[{qi}] is missing a non-empty `question`"))?;
        if question.chars().count() > MAX_QUESTION_TEXT_CHARS {
            return Err(format!(
                "questions[{qi}] `question` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
            ));
        }
        let header = q
            .get("header")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("questions[{qi}] is missing a non-empty `header`"))?;
        if header.chars().count() > MAX_HEADER_CHARS {
            return Err(format!(
                "questions[{qi}] `header` exceeds {MAX_HEADER_CHARS} characters"
            ));
        }
        let multi_select = q
            .get("multiSelect")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let opts = q
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("questions[{qi}] is missing an `options` array"))?;
        if opts.len() < MIN_OPTIONS || opts.len() > MAX_OPTIONS {
            return Err(format!(
                "questions[{qi}] must have between {MIN_OPTIONS} and {MAX_OPTIONS} options"
            ));
        }
        let mut options = Vec::with_capacity(opts.len());
        for (oi, o) in opts.iter().enumerate() {
            let label = o
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("questions[{qi}].options[{oi}] is missing a non-empty `label`")
                })?;
            if label.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `label` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            let description = o
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if description.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `description` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            options.push(QuestionOption {
                label: label.to_string(),
                description,
            });
        }
        // Reject duplicate option labels within a question: the UI uses the
        // label as both the React key and the selection identity, and the
        // answer is submitted by label — duplicates would be ambiguous (select
        // one, select both) and collide on the key.
        let mut seen_labels = std::collections::HashSet::new();
        for o in &options {
            if !seen_labels.insert(o.label.as_str()) {
                return Err(format!(
                    "questions[{qi}] has duplicate option label {:?}",
                    o.label
                ));
            }
        }
        out.push(QuestionSpec {
            id: uuid::Uuid::new_v4().to_string(),
            question: question.to_string(),
            header: header.to_string(),
            multi_select,
            options,
            is_secret: false,
        });
    }
    Ok(out)
}

/// Re-assert the [`parse_questions`] count + size bounds on already-typed specs.
/// The companion validates before sending, but the broker socket is only
/// token-gated, so a hand-rolled client could bypass that path and ride
/// oversized or malformed specs straight into the broadcast `QuestionRequest`
/// event and the `pending_question` snapshot. The listener registers through
/// this, declining the ask on `Err` rather than trusting unbounded input — the
/// authoritative answer-side bounds already live in [`build_outcome`], so this
/// closes the matching gap on the request side. Bounds mirror `parse_questions`.
pub fn validate_specs(specs: &[QuestionSpec]) -> Result<(), String> {
    if specs.is_empty() || specs.len() > MAX_QUESTIONS {
        return Err(format!(
            "expected 1..={MAX_QUESTIONS} questions, got {}",
            specs.len()
        ));
    }
    let mut seen_ids = std::collections::HashSet::new();
    for (qi, q) in specs.iter().enumerate() {
        // `parse_questions` mints a fresh uuid per question; a hand-rolled client
        // could send empty / colliding ids, and the answer routing + UI state map
        // key on `id`, so duplicates would misroute or collide.
        if q.id.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `id`"));
        }
        if !seen_ids.insert(q.id.as_str()) {
            return Err(format!("questions[{qi}] has a duplicate `id` {:?}", q.id));
        }
        if q.question.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `question`"));
        }
        if q.question.chars().count() > MAX_QUESTION_TEXT_CHARS {
            return Err(format!(
                "questions[{qi}] `question` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
            ));
        }
        if q.header.trim().is_empty() {
            return Err(format!("questions[{qi}] has an empty `header`"));
        }
        if q.header.chars().count() > MAX_HEADER_CHARS {
            return Err(format!(
                "questions[{qi}] `header` exceeds {MAX_HEADER_CHARS} characters"
            ));
        }
        // Unlike `parse_questions` (the codeg-mcp tool contract, which keeps
        // its [`MIN_OPTIONS`] floor), typed specs allow 0..=[`MAX_OPTIONS`]:
        // a question with no options is a legal free-text ask — the card
        // renders its always-present "Other" input alone (codex elicitation
        // and MCP-server forms ask open questions this way).
        if q.options.len() > MAX_OPTIONS {
            return Err(format!(
                "questions[{qi}] must have at most {MAX_OPTIONS} options"
            ));
        }
        let mut seen_labels = std::collections::HashSet::new();
        for (oi, o) in q.options.iter().enumerate() {
            if o.label.trim().is_empty() {
                return Err(format!("questions[{qi}].options[{oi}] has an empty `label`"));
            }
            if o.label.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `label` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
            // Mirror parse_questions: labels are the React key + selection identity
            // and answers are submitted by label, so duplicates (trimmed) are
            // ambiguous.
            if !seen_labels.insert(o.label.trim()) {
                return Err(format!(
                    "questions[{qi}] has a duplicate option label {:?}",
                    o.label
                ));
            }
            if o.description.chars().count() > MAX_QUESTION_TEXT_CHARS {
                return Err(format!(
                    "questions[{qi}].options[{oi}] `description` exceeds {MAX_QUESTION_TEXT_CHARS} characters"
                ));
            }
        }
    }
    Ok(())
}

/// Join the user's submission with the original questions into a self-describing
/// [`QuestionOutcome`], normalizing + validating against the stored specs. The
/// UI enforces these rules, but `acp_answer_question` is a plain API a stale or
/// hand-rolled client can hit directly, so the authoritative checks live here.
///
/// Iterates the TRUSTED `questions` (≤ [`MAX_QUESTIONS`]), not the client's
/// `answers`, so a flood of unknown / duplicate answer items can neither grow an
/// intermediate set nor bloat the output — extra items are simply never looked
/// up. For each spec question it takes the first matching answer (dedup) and:
///   * trims each label, drops empties, bounds each to [`MAX_QUESTION_TEXT_CHARS`];
///   * caps the count — single-select keeps 1, multi-select keeps at most every
///     real option plus one free-text "Other" (`options.len() + 1`);
///   * drops a question left with no usable label.
///
/// Output is therefore bounded by the question set's own size, in asked order.
/// A declined submission yields an empty, `declined: true` outcome.
pub fn build_outcome(questions: &[QuestionSpec], answer: &QuestionAnswer) -> QuestionOutcome {
    if answer.declined {
        return QuestionOutcome {
            answers: Vec::new(),
            declined: true,
        };
    }
    let answers = questions
        .iter()
        .filter_map(|spec| {
            let a = answer.answers.iter().find(|a| a.question_id == spec.id)?;
            // Cap selections to the question's own size: single-select → 1;
            // multi-select → every real option plus one "Other". Enforce the cap
            // DURING iteration (early break, allocate only kept labels) so a
            // pathological `labels` array can't do unbounded intermediate work.
            let cap = if spec.multi_select {
                spec.options.len() + 1
            } else {
                1
            };
            let mut labels: Vec<String> = Vec::with_capacity(cap);
            for l in &a.labels {
                if labels.len() == cap {
                    break;
                }
                let trimmed = l.trim();
                if trimmed.is_empty() {
                    continue;
                }
                labels.push(trimmed.chars().take(MAX_QUESTION_TEXT_CHARS).collect());
            }
            if labels.is_empty() {
                return None;
            }
            Some(QuestionAnsweredItem {
                question: spec.question.clone(),
                header: spec.header.clone(),
                multi_select: spec.multi_select,
                selected: labels,
            })
        })
        .collect();
    QuestionOutcome {
        answers,
        declined: false,
    }
}

/// Some native asks carry NO `header` (the short category chip codeg renders):
/// grok's `ask_user_question` tool and pi's extension-UI `select` both send bare
/// question text. Synthesize one from the leading characters, bounded to
/// [`MAX_HEADER_CHARS`]. Always returns a non-empty, in-bounds string so
/// [`validate_specs`] accepts it.
pub(crate) fn synthesize_header(question: &str) -> String {
    let header: String = question.trim().chars().take(MAX_HEADER_CHARS).collect();
    let header = header.trim();
    if header.is_empty() {
        "Ask".to_string()
    } else {
        header.to_string()
    }
}

/// Convert grok's native `_x.ai/ask_user_question` ext-request params into codeg
/// [`QuestionSpec`]s, so grok's own questions render in the SAME interactive card
/// as the `codeg-mcp` ask tool (grok emits an ACP ext request and blocks on the
/// reply; if codeg doesn't answer it, grok falls back to inert fire-and-forget
/// rendering — see the connection handler). Grok's wire shape per question is
/// `{question, options:[{label, description}], multiSelect}` — no `header`, which
/// we synthesize. Counts are clamped to codeg's bounds because
/// [`crate::acp::manager::ConnectionManager::register_question`] re-runs
/// [`validate_specs`] and would otherwise decline the whole ask: more than
/// [`MAX_QUESTIONS`] questions or [`MAX_OPTIONS`] options are truncated (logged,
/// never silently dropped), and duplicate option labels — which `validate_specs`
/// rejects as ambiguous — are deduped. A question left with fewer than
/// [`MIN_OPTIONS`] usable options is not a real choice and fails the request
/// (grok then falls back — no worse than before the bridge existed).
pub fn parse_grok_ext_questions(params: &Value) -> Result<Vec<QuestionSpec>, String> {
    let arr = params
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "ask_user_question ext request missing `questions` array".to_string())?;
    if arr.is_empty() {
        return Err("ask_user_question ext request has no questions".to_string());
    }
    if arr.len() > MAX_QUESTIONS {
        tracing::warn!(
            "[grok ask] dropping {} question(s) past the max of {MAX_QUESTIONS}",
            arr.len() - MAX_QUESTIONS
        );
    }
    let mut out = Vec::with_capacity(arr.len().min(MAX_QUESTIONS));
    for (qi, q) in arr.iter().take(MAX_QUESTIONS).enumerate() {
        let question = q
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("questions[{qi}] is missing a non-empty `question`"))?;
        let multi_select = q
            .get("multiSelect")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let opts = q
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("questions[{qi}] is missing an `options` array"))?;
        if opts.len() > MAX_OPTIONS {
            tracing::warn!(
                "[grok ask] questions[{qi}] has {} options; truncating to {MAX_OPTIONS}",
                opts.len()
            );
        }
        let mut options = Vec::with_capacity(opts.len().min(MAX_OPTIONS));
        let mut seen_labels = std::collections::HashSet::new();
        for o in opts {
            if options.len() == MAX_OPTIONS {
                break;
            }
            let label = o
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(label) = label else { continue };
            // `validate_specs` rejects duplicate labels (the label is the card's
            // React key + selection identity); dedup rather than fail the ask.
            if !seen_labels.insert(label.to_string()) {
                continue;
            }
            let description: String = o
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .chars()
                .take(MAX_QUESTION_TEXT_CHARS)
                .collect();
            options.push(QuestionOption {
                label: label.chars().take(MAX_QUESTION_TEXT_CHARS).collect(),
                description,
            });
        }
        if options.len() < MIN_OPTIONS {
            return Err(format!(
                "questions[{qi}] has fewer than {MIN_OPTIONS} usable options"
            ));
        }
        out.push(QuestionSpec {
            id: uuid::Uuid::new_v4().to_string(),
            question: question.chars().take(MAX_QUESTION_TEXT_CHARS).collect(),
            header: synthesize_header(question),
            multi_select,
            options,
            is_secret: false,
        });
    }
    Ok(out)
}

/// pi-acp's tool-call id prefix for an extension-UI request
/// (`extensionUiToolCall`, pi-acp 0.0.33). It is the only marker that
/// distinguishes a `ctx.ui.*` dialog from a real tool approval, since both
/// arrive as a plain `session/request_permission`. Public so the connection
/// handler can screen requests on it before paying for anything else — one
/// definition, so the cheap screen and the full parse can never drift.
pub const PI_EXTENSION_UI_ID_PREFIX: &str = "pi-ui-";

/// pi-acp's option-id prefix for a `select` choice (`CHOICE_OPTION_PREFIX`); it
/// maps the id back to the index of its own options array when replying to the
/// extension.
const PI_CHOICE_OPTION_PREFIX: &str = "choice-";

/// pi's extension-UI `select` ask, lifted off a `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiSelectAsk {
    /// The single question to register on the interactive card.
    pub spec: QuestionSpec,
    /// `(option label, permission optionId)` in wire order. The user picks a
    /// LABEL on the card, but pi must be answered with the id that label arrived
    /// with — pi-acp turns it back into an index into its own options array — so
    /// the pairing has to survive the round trip.
    pub option_ids: Vec<(String, String)>,
}

/// Recognize a `session/request_permission` that is really pi asking the user a
/// multiple-choice question, and convert it into a [`QuestionSpec`] so it renders
/// in the SAME interactive card as the codeg-mcp ask tool.
///
/// pi has no dedicated question channel: `ctx.ui.select` from an extension
/// becomes an `extension_ui_request`, which pi-acp translates into a permission
/// request whose `toolCall` is a synthetic `{toolCallId: "pi-ui-<id>", kind:
/// "other", status: "pending", rawInput: {method: "select", title, options:
/// [...]}}` and whose permission options are the choices, keyed
/// `choice-<index>` (verified against pi-acp 0.0.33 on a live run). Without this
/// the choices render as a generic approval card that dumps that JSON verbatim.
///
/// Returns `None` — leaving the caller on the untouched permission path — for
/// anything that does not fingerprint as pi's select, and for a select the card
/// could not represent FAITHFULLY: more than [`MAX_OPTIONS`] choices (truncating
/// would hide choices the user must be able to pick), fewer than
/// [`MIN_OPTIONS`], or duplicate/blank labels (the label is the card's selection
/// identity, so a duplicate would answer pi with the wrong index). Those keep
/// today's approval card, which lists every option as its own button.
///
/// `options` is `(optionId, name)` in wire order.
pub fn parse_pi_select_ask(tool_call: &Value, options: &[(String, String)]) -> Option<PiSelectAsk> {
    let id = tool_call
        .get("toolCallId")
        .or_else(|| tool_call.get("tool_call_id"))
        .and_then(|v| v.as_str())?;
    if !id.starts_with(PI_EXTENSION_UI_ID_PREFIX) {
        return None;
    }
    let raw_input = tool_call
        .get("rawInput")
        .or_else(|| tool_call.get("raw_input"))?;
    if raw_input.get("method").and_then(|v| v.as_str()) != Some("select") {
        return None;
    }
    // The dialog prompt: `rawInput.title` is the extension's own text, while the
    // tool call's `title` falls back to a bare `Pi select` when it has none.
    let question = raw_input
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| tool_call.get("title").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
        return None;
    }
    let mut choices = Vec::with_capacity(options.len());
    let mut option_ids = Vec::with_capacity(options.len());
    let mut seen = std::collections::HashSet::new();
    for (option_id, name) in options {
        if !option_id.starts_with(PI_CHOICE_OPTION_PREFIX) {
            return None;
        }
        let label = name.trim();
        if label.is_empty() || label.chars().count() > MAX_QUESTION_TEXT_CHARS {
            return None;
        }
        if !seen.insert(label) {
            return None;
        }
        choices.push(QuestionOption {
            label: label.to_string(),
            // pi's select carries no per-option description; the card renders
            // fine without one.
            description: String::new(),
        });
        option_ids.push((label.to_string(), option_id.clone()));
    }
    Some(PiSelectAsk {
        spec: QuestionSpec {
            id: uuid::Uuid::new_v4().to_string(),
            question: question.chars().take(MAX_QUESTION_TEXT_CHARS).collect(),
            header: synthesize_header(question),
            // `ctx.ui.select` resolves to ONE value; pi-acp's reply carries a
            // single `optionId`.
            multi_select: false,
            options: choices,
            is_secret: false,
        },
        option_ids,
    })
}

/// The permission `optionId` that answers pi with what the user picked, or
/// `None` when the card produced nothing pi can accept: a declined card, or a
/// free-text "Other" answer (the card always offers one, but pi's select can
/// only be answered with one of ITS options). `None` means reply `Cancelled`,
/// which pi-acp forwards to the extension as `{cancelled: true}` — the same
/// thing `ctx.ui.select` returns when a TUI user presses Escape.
pub fn pi_select_option_id(outcome: &QuestionOutcome, ask: &PiSelectAsk) -> Option<String> {
    if outcome.declined {
        return None;
    }
    let picked = outcome.answers.first()?.selected.first()?;
    ask.option_ids
        .iter()
        .find(|(label, _)| label == picked)
        .map(|(_, option_id)| option_id.clone())
}

/// True when a host's name for a tool is codeg's OWN `ask_user_question`
/// companion tool, in whatever spelling the host composed it
/// (`mcp__codeg-mcp__ask_user_question` from claude-agent-acp,
/// `codeg-mcp/ask_user_question`, `codeg-mcp: ask_user_question`, …). Separators
/// are folded and case is ignored, so only the two identifying words matter.
///
/// BOTH halves have to be present — the server name codeg itself injects
/// (`codeg-mcp`, see `acp::connection::inject_codeg_mcp`) AND the tool name.
/// The frontend can afford a bare `*ask_user_question` suffix rule because a
/// wrong match there only picks a nicer card; this one unlocks an AUTO-APPROVAL
/// of a blocked `session/request_permission`, and a third-party MCP server's
/// similarly named tool is the user's to approve, not codeg's.
///
/// Auto-approving codeg's own ask tool is not a permission being skipped: the
/// tool's entire effect is to put the interactive question card on screen and
/// block until the user answers it. The consent IS the next dialog, so gating it
/// behind a generic "run this tool?" card asks the user to approve being asked.
pub fn is_codeg_ask_tool_name(name: &str) -> bool {
    let normalized = name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' ', '.', '/', ':'], "_");
    normalized.ends_with("ask_user_question") && normalized.contains("codeg_mcp")
}

/// Serialize a resolved [`QuestionOutcome`] into grok's `AskUserQuestionExtResponse`
/// — the reply to a `_x.ai/ask_user_question` ext request. Verified against grok
/// 0.2.101 on a real run: the response is internally tagged by `outcome`; the
/// `accepted` variant carries an `answers` map keyed by the QUESTION TEXT (grok's
/// questions have no id and it correlates the answer by text) whose value is a
/// bare string for single-select or an array for multi-select (grok's
/// `StringOrVec`), plus an empty `partial_answers`. A declined card maps to
/// `skip_interview` (a variant grok's enum accepts). A wrong shape only makes grok
/// fall back to inert rendering, so this can never regress the pre-bridge state.
pub fn build_grok_ext_response(outcome: &QuestionOutcome) -> Value {
    if outcome.declined {
        return grok_ext_skip_response();
    }
    let mut answers = serde_json::Map::new();
    for item in &outcome.answers {
        let value = if item.multi_select {
            Value::Array(item.selected.iter().cloned().map(Value::String).collect())
        } else {
            // Single-select keeps exactly one label (capped in `build_outcome`);
            // grok's `StringOrVec` wants a bare string here, not a 1-element array.
            match item.selected.first() {
                Some(label) => Value::String(label.clone()),
                None => continue,
            }
        };
        answers.insert(item.question.clone(), value);
    }
    serde_json::json!({
        "outcome": "accepted",
        "answers": Value::Object(answers),
        "partial_answers": Value::Object(serde_json::Map::new()),
    })
}

/// The `_x.ai/ask_user_question` reply for a declined card or an ask that was
/// canceled/torn down before the user submitted (the answer one-shot dropped).
/// Grok's `skip_interview` outcome — chosen over `cancelled` because the ACP
/// response enum only exposes `Accepted`/`ChatAboutThis`/`SkipInterview`.
pub fn grok_ext_skip_response() -> Value {
    serde_json::json!({ "outcome": "skip_interview" })
}

/// One selectable choice lifted from a schema property: the display `label`
/// (`title`, falling back to the wire value) and the `value` (`const` / `enum`
/// entry) the accept response must send back. The two differ when a generic
/// MCP server elicits with `enum` + `enumNames` (codex-acp normalizes that to
/// `oneOf` with `const` ≠ `title`); codex's own `request_user_input` sets them
/// identical. The typed schema drops per-option descriptions, so they are left
/// empty (the card renders fine without one).
struct ElicitationChoice {
    label: String,
    value: String,
}

fn string_prop_choices(s: &StringPropertySchema) -> Vec<ElicitationChoice> {
    if let Some(one_of) = &s.one_of {
        return one_of
            .iter()
            .map(|o| ElicitationChoice {
                label: if o.title.trim().is_empty() {
                    o.value.clone()
                } else {
                    o.title.clone()
                },
                value: o.value.clone(),
            })
            .collect();
    }
    if let Some(values) = &s.enum_values {
        return values
            .iter()
            .map(|v| ElicitationChoice {
                label: v.clone(),
                value: v.clone(),
            })
            .collect();
    }
    Vec::new()
}

fn multi_select_choices(items: &MultiSelectItems) -> Vec<ElicitationChoice> {
    match items {
        MultiSelectItems::Titled(t) => t
            .options
            .iter()
            .map(|o| ElicitationChoice {
                label: if o.title.trim().is_empty() {
                    o.value.clone()
                } else {
                    o.title.clone()
                },
                value: o.value.clone(),
            })
            .collect(),
        MultiSelectItems::String(u) => u
            .values
            .iter()
            .map(|v| ElicitationChoice {
                label: v.clone(),
                value: v.clone(),
            })
            .collect(),
        // `MultiSelectItems` is `#[non_exhaustive]`; an unknown item kind
        // yields no options — the question degrades to free text.
        _ => Vec::new(),
    }
}

/// How one form field's answer must be typed in the elicitation response.
/// MCP servers validate the accepted content against their schema, so a
/// boolean/number field answered with a string would be rejected — each
/// answer is rebuilt into the schema's own primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationFieldKind {
    /// Single-select or free-text string → bare string.
    Text,
    /// Multi-select array → string array.
    MultiSelect,
    /// Yes/No choice → JSON boolean.
    Boolean,
    /// Free-text parsed as f64 → JSON number (unparseable answers are omitted).
    Number,
    /// Free-text parsed as i64 → JSON integer (unparseable answers are omitted).
    Integer,
}

/// The response-side rebuild plan for one schema property, parallel (same
/// index) to the [`QuestionSpec`] minted for it. Keeps the property id, the
/// primitive kind, and the label→value map for choice fields — everything
/// [`build_elicitation_response`] needs that [`QuestionSpec`] (a shared wire
/// type the card renders) must not carry.
pub struct ElicitationField {
    /// Schema property name — the key the accepted content is sent under.
    pub id: String,
    pub kind: ElicitationFieldKind,
    /// Display label → wire value for choice fields. A label the user typed
    /// via "Other" (absent here) is sent verbatim.
    pub value_by_label: std::collections::HashMap<String, String>,
}

/// A form elicitation that renders as ask-card questions.
pub struct ElicitationQuestions {
    pub specs: Vec<QuestionSpec>,
    /// Parallel to `specs` (same index).
    pub fields: Vec<ElicitationField>,
    /// The elicitation's `toolCallId` (codex sets it to the `request_user_input`
    /// item id). The connection handler keys the synthesized in-stream result
    /// card to it, so the live card and the reloaded history card (parsed by
    /// `codex.rs` from the same id) are one card. `None` when the wire carried
    /// no session scope tool_call_id.
    pub tool_call_id: Option<String>,
}

/// One selectable option of an approval-style elicitation, shaped for the
/// permission card (`PermissionOptionInfo` mirrors these fields 1:1).
pub struct ElicitationApprovalOption {
    pub option_id: String,
    pub label: String,
    /// `allow_once` / `allow_always` / `reject_once` — the permission-option
    /// kinds the frontend already styles.
    pub kind: &'static str,
}

/// The reserved option id for rejecting an approval-style elicitation. Never
/// collides with a persist `const` (codex only mints `once`/`session`/`always`)
/// and is filtered out of the accept mapping defensively.
pub const ELICITATION_DECLINE_OPTION_ID: &str = "__decline";

/// A form elicitation that renders through the PERMISSION card rather than the
/// ask card: an MCP tool-call approval (`_meta.codex_approval_kind ==
/// "mcp_tool_call"`) or a message-only confirm (a form with no renderable
/// fields). Routing these through the permission flow keeps MCP approvals
/// looking exactly like they did before codeg advertised `elicitation.form`
/// (codex-acp then used `session/request_permission` with the same options).
pub struct ElicitationApproval {
    /// The human prompt ("Allow tool call?" / the MCP server's message).
    pub message: String,
    /// The tool call this approval correlates to (codex emits the mcpToolCall
    /// item before eliciting), when the wire carried one.
    pub tool_call_id: Option<String>,
    /// The selectable options in display order; always ends with the
    /// [`ELICITATION_DECLINE_OPTION_ID`] entry.
    pub options: Vec<ElicitationApprovalOption>,
    /// True when the accept response must echo the chosen option back as
    /// `content.persist` (the request carried codex's persist-choice
    /// property; codex-acp pops it into the app-server `_meta.persist`).
    pub persist_in_content: bool,
}

/// How a form `elicitation/create` request must be presented.
pub enum ElicitationPlan {
    Questions(ElicitationQuestions),
    Approval(ElicitationApproval),
}

/// Codex's synthetic free-text "Other" companion fields are named
/// `<questionId>__other` (`__other1`, `__other2`… on collision). The card
/// offers its own "Other" on every question, so companions are skipped and the
/// typed answer rides the main field (codex falls back to it).
///
/// Name-based, and deliberately kept alongside the `_meta` markers below.
///
/// codex-acp 1.12.0 renamed the companion to `<questionId>_note` — which this
/// deliberately does NOT match. `_note` is a plausible suffix for a REAL field
/// in a generic MCP server's form (`release_note`, `user_note`), and skipping
/// one would silently drop a question the user has to answer. The rename needs
/// no name heuristic anyway: `buildUserInputRequest` stamps
/// `_meta.codex.role = "user_note"` on the new companion unconditionally, so
/// [`codex_companion_role`] recognizes it exactly.
fn is_other_companion(id: &str) -> bool {
    let Some(pos) = id.rfind("__other") else {
        return false;
    };
    pos > 0 && id[pos + "__other".len()..].chars().all(|c| c.is_ascii_digit())
}

/// True when the raw schema property carries codex's own companion marker, the
/// `_meta.codex` half of [`is_other_companion`]: `isOtherAnswer: true`
/// (codex-acp ≤ 1.11.0) or `role: "user_note"` (≥ 1.12.0). Either one means the
/// property is the free-text companion of a sibling select question, so the
/// card must not render it as a question of its own.
///
/// Both markers are unconditional in `buildUserInputRequest` — the companion is
/// created and marked in the same expression — so this is the exact recognizer
/// for both generations, and the ONLY one for 1.12.0's `_note` naming.
fn codex_companion_role(raw: &Value, id: &str) -> Option<CodexUserInputShape> {
    let codex = codex_property_meta(raw, id)?;
    if codex.get("isOtherAnswer").and_then(Value::as_bool) == Some(true) {
        return Some(CodexUserInputShape::QuestionInDescription);
    }
    if codex.get("role").and_then(Value::as_str) == Some("user_note") {
        return Some(CodexUserInputShape::QuestionInTitle);
    }
    None
}

/// Which generation of codex-acp's `request_user_input` bridge produced a form
/// — i.e. which of a question property's `title` / `description` holds the
/// QUESTION, and which holds the short tab header.
///
/// codex-acp 1.12.0 swapped the two:
///
/// ```text
///   ≤ 1.11.0   title = header (the short tab label)   description = question
///   ≥ 1.12.0   title = question                       description = header
/// ```
///
/// Reading them the wrong way round is not cosmetic: a single-question ask has
/// no tab strip, so the card would show only the header ("Approach") and the
/// actual question would never be displayed at all.
///
/// The connection pins this at `initialize` from the RUNNING adapter's
/// `agentInfo.version` (`SessionState::codex_user_input_shape`) — codeg's pin is
/// 1.12.0, but launch may resolve an older PATH install or a custom pinned
/// version, and both are supported configurations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodexUserInputShape {
    /// codex-acp ≤ 1.11.0 — the question is in `description`, and the free-text
    /// companion is `<id>__other` marked `_meta.codex.isOtherAnswer`.
    QuestionInDescription,
    /// codex-acp ≥ 1.12.0 — the question is in `title`, the companion is
    /// `<id>_note` marked `_meta.codex.role = "user_note"`, and an `isOther`
    /// question's `oneOf` carries an injected "None of the above".
    QuestionInTitle,
}

/// Who sent this `elicitation/create` — and the ONLY thing that may unlock a
/// codex-specific fixup in this parser.
///
/// The identity has to come from the connection, not from the payload. `_meta`
/// is an OPEN namespace: the ACP spec tells implementations to make no
/// assumptions about keys they do not own, and this parser is shared — codeg
/// advertises `elicitation.form` to DeepSeek as well as Codex, and EITHER
/// adapter also forwards arbitrary MCP-server forms through it. Deciding "this
/// is codex" from `_meta.codex.*` in the request would let a third party, by
/// coincidence or on purpose, switch on codex's orientation flip, its companion
/// skip and its synthetic-option filter for a form codex never sent — which can
/// hide a legitimate field or change how an answer is encoded.
///
/// So `Other` is absolute: no codex fixup runs for it, whatever `_meta` claims.
/// Only inside `Codex` does the payload get a say, and then only about WHICH
/// generation sent it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElicitationPeer {
    /// Anything that is not codex-acp (DeepSeek today; any future
    /// `elicitation.form` speaker). The form is parsed generically.
    Other,
    /// codex-acp, carrying the `request_user_input` shape its RUNNING version
    /// implies — `None` when `agentInfo.version` was absent or unparseable, in
    /// which case the form's own markers have to date it.
    Codex(Option<CodexUserInputShape>),
}

impl ElicitationPeer {
    /// Whether codex's `_meta.codex` markers may be trusted on this connection.
    fn is_codex(self) -> bool {
        matches!(self, Self::Codex(_))
    }
}

/// The `_meta.codex` object of one schema property, when present. codex-acp
/// stamps it on every `request_user_input` field (question properties carry
/// `isOther`/`isSecret`, companions carry `questionId` plus the role marker),
/// and on nothing else — a generic MCP server's form has no `codex` namespace.
/// The typed schema property structs carry no `_meta`, so this reads the raw
/// JSON.
fn codex_property_meta<'a>(raw: &'a Value, id: &str) -> Option<&'a Value> {
    raw.get("requestedSchema")?
        .get("properties")?
        .get(id)?
        .get("_meta")?
        .get("codex")
}

/// Resolve a form's [`CodexUserInputShape`], or `None` when no codex-specific
/// reading may be applied to it.
///
/// `None` is load-bearing well beyond orientation — it is the single switch that
/// keeps EVERY 1.12.0 fixup (the orientation flip and the injected "None of the
/// above") off forms codex did not send. Two independent things have to hold
/// before it returns `Some`:
///
///   * the PEER is codex-acp ([`ElicitationPeer`]). This comes from the
///     connection, never from the payload, because DeepSeek and arbitrary
///     MCP-server forms share this parser and `_meta` is an open namespace;
///   * the FORM is one of codex's `request_user_input` asks rather than an MCP
///     tool-call approval or a server form codex is merely relaying —
///     `buildUserInputRequest` stamps `_meta.codex.isOther` on every question
///     property in both generations, and only it does.
///
/// Which generation then sent it is the remaining question, and nothing in the
/// form distinguishes the two reliably — question properties look identical —
/// so the answer comes from the ADAPTER, in descending order of authority:
///
///   1. `running` — the version the connection read out of the running
///      adapter's `agentInfo` at `initialize`. This is the process that built
///      the frame, so it is authoritative when known.
///   2. The form's own free-text companion marker (`isOtherAnswer` ⇒ ≤1.11.0,
///      `role: "user_note"` ⇒ ≥1.12.0), for the case where the adapter reported
///      no `agentInfo` at all. Only present when the form has an `isOther`
///      question.
///   3. The pinned adapter's shape, which is 1.12.0's — the last resort for an
///      undatable form from an unidentified adapter, and the likeliest truth
///      since the pin is what codeg launches.
fn codex_form_shape(raw: &Value, peer: ElicitationPeer) -> Option<CodexUserInputShape> {
    // Identity first, and unconditionally: a non-codex peer gets the generic
    // reading no matter what its `_meta` says.
    let ElicitationPeer::Codex(running) = peer else {
        return None;
    };
    let properties = raw
        .get("requestedSchema")?
        .get("properties")?
        .as_object()?;
    let mut is_codex_form = false;
    let mut companion_shape = None;
    for id in properties.keys() {
        if let Some(shape) = codex_companion_role(raw, id) {
            companion_shape = Some(shape);
            continue;
        }
        is_codex_form |= codex_property_meta(raw, id)
            .is_some_and(|codex| codex.get("isOther").is_some());
    }
    if !is_codex_form && companion_shape.is_none() {
        return None;
    }
    Some(
        running
            .or(companion_shape)
            .unwrap_or(CodexUserInputShape::QuestionInTitle),
    )
}

/// The synthetic choice codex-acp 1.12.0–2.0.x appends to an `isOther`
/// question's `oneOf`, verbatim — label and description both.
///
/// codex-acp 2.1.0 (#570) stopped appending it for AIR clients — and codeg is
/// one, by advertising `_meta.jetbrains.air` at all — in favour of the
/// AIR custom answer (see [`build_elicitation_response`]). So against the pin
/// this filter never fires; it stays for the older adapters a custom pin or a
/// PATH install can still launch, which keep injecting the option for every
/// client.
///
/// codeg's card already offers a free-text "Other" input on every question and
/// writes it to the MAIN field, while the note field this option points at is
/// skipped as a companion. So the option is a dead end here: picking it sends
/// codex the bare string "None of the above" and no elaboration.
///
/// "None of the above" is an entirely plausible option for a MODEL to write, and
/// codex-acp suppresses its own injection when one already did
/// (`!options.some(o => o.label === USER_INPUT_OTHER_OPTION)`) — so dropping a
/// bare label match would silently delete a real choice. Four conditions must
/// hold together before one is removed, and each narrows a different axis:
///
///   * the PEER is codex-acp ([`ElicitationPeer`]) — DeepSeek and generic MCP
///     forms share this parser and are never touched, and that is decided from
///     the connection rather than from the form's `_meta`;
///   * the form is a codex ≥1.12.0 `request_user_input`;
///   * the property is an `isOther` question — the only kind codex injects into;
///   * THIS choice's own raw `oneOf` entry — located by its `const`, not by
///     scanning the array — carries codex's description verbatim.
///
/// That last point is why the match is by `const` rather than "does the array
/// contain a synthetic entry anywhere": a scan drops any option that merely
/// SHARES the label with a synthetic one sitting elsewhere in the same `oneOf`.
/// A conforming codex cannot produce that pair — it builds every option as
/// `{const: label, title: label}` and skips its injection entirely when the
/// label is taken — but "the adapter would never" is not a property worth
/// relying on when the exact check costs nothing.
///
/// Every failure lands on "keep the option". If upstream rewords the copy the
/// filter simply stops firing and the redundant choice reappears.
const CODEX_SYNTHETIC_OTHER_LABEL: &str = "None of the above";
const CODEX_SYNTHETIC_OTHER_DESCRIPTION: &str = "Provide a different answer in the note field.";

/// True when this property is an `isOther` codex question, i.e. the only place
/// codex-acp ≥1.12.0 injects [`CODEX_SYNTHETIC_OTHER_LABEL`].
fn is_codex_other_question(raw: &Value, id: &str) -> bool {
    codex_property_meta(raw, id)
        .and_then(|codex| codex.get("isOther"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// True when THIS choice is codex's injected option: its label and its `const`
/// are both [`CODEX_SYNTHETIC_OTHER_LABEL`], and the raw `oneOf` entry carrying
/// that same `const` describes it exactly as codex-acp does. Callers must have
/// already established the peer, the form's generation and the property's
/// `isOther`-ness.
///
/// `value` is the choice's `const`, which is what pins the lookup to the entry
/// being rendered instead of to any entry in the array.
fn is_codex_synthetic_other_choice(raw: &Value, id: &str, label: &str, value: &str) -> bool {
    // codex writes `const: option.label` for every option, injected or
    // model-authored, so a choice whose `const` is not the synthetic label is
    // someone else's option that happens to display the same text.
    if label != CODEX_SYNTHETIC_OTHER_LABEL || value != CODEX_SYNTHETIC_OTHER_LABEL {
        return false;
    }
    let Some(one_of) = raw
        .get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(id))
        .and_then(|prop| prop.get("oneOf"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    one_of.iter().any(|entry| {
        entry.get("const").and_then(Value::as_str) == Some(value)
            && entry.get("description").and_then(Value::as_str)
                == Some(CODEX_SYNTHETIC_OTHER_DESCRIPTION)
    })
}

/// True when the raw schema property carries the shared custom-answer marker
/// (`_meta._askUserQuestionCustomAnswer.isCustomAnswer`), i.e. it is the
/// free-text "Other" companion of a sibling select question.
///
/// claude-agent-acp 0.64.0 (#929) introduced this key deliberately WITHOUT an
/// agent namespace so every AskUserQuestion bridge can be recognized the same
/// way; its own companion fields are named `question_<n>_custom`, which
/// [`is_other_companion`]'s `__other` heuristic does not match. Reading the
/// marker means codeg keeps collapsing the companion into the card's built-in
/// "Other" input no matter which adapter produced the form.
///
/// claude-agent-acp 0.82.0 moved the marker to
/// `_meta.jetbrains.air.customAnswer` (same value) and sends the old key to no
/// client, so both spellings are read.
///
/// codex-acp 2.1.0 (#570) marks its `request_user_input` note field with the
/// same AIR key but a different value: a bare `true` rather than claude's
/// `{questionId, isCustomAnswer: true}` (both adapters' `docs/air-extensions.md`
/// say so). 2.1.1 (#577) adds the old root key with that same bare `true`,
/// because released AIR builds read only the old key. Either value is read
/// under either key. A codex peer's note field is already skipped by its
/// `_meta.codex.role`; reading the marker too means the companion is
/// recognised by the shared AIR key alone, whoever sends it.
///
/// Like [`is_secret_property`], this reads the raw JSON: the typed schema
/// property structs carry no `_meta`.
fn is_custom_answer_property(raw: &Value, id: &str) -> bool {
    let Some(meta) = raw
        .get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(id))
        .and_then(|prop| prop.get("_meta"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    let is_marked = |marker: &Value| match marker {
        Value::Bool(marked) => *marked,
        marker => marker
            .get("isCustomAnswer")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    meta.get("_askUserQuestionCustomAnswer")
        .is_some_and(is_marked)
        || crate::acp::air_contract::air_meta_value(Some(meta), "customAnswer")
            .is_some_and(is_marked)
}

/// True when the request is codex's MCP tool-call approval elicitation. The
/// marker rides the request `_meta` verbatim (codex-acp passes the app-server
/// `_meta` through), NOT under the `codex` namespace.
fn is_mcp_tool_call_approval(raw: &Value) -> bool {
    raw.get("_meta")
        .and_then(|m| m.get("codex_approval_kind"))
        .and_then(Value::as_str)
        == Some("mcp_tool_call")
}

/// Classify a form `elicitation/create` request (the raw JSON params) into its
/// presentation plan. Everything codex-acp can send once `elicitation.form` is
/// advertised lands here, so every shape must resolve to SOMETHING the user
/// can act on — an unhandled shape would stall or silently reject the agent's
/// blocked request:
///
///   * MCP tool-call approvals (`_meta.codex_approval_kind`) → [`ElicitationApproval`]
///     with the persist choices (`once`/`session`/`always`) + Decline — the
///     permission card, exactly like the pre-capability `request_permission`
///     fallback. This includes approvals for codeg-mcp's OWN tools in
///     consent-requiring permission modes, so declining them here would break
///     ask/delegation for codex outright.
///   * Forms with no renderable fields (message-only MCP confirms) →
///     [`ElicitationApproval`] with Accept/Decline.
///   * Everything else (codex `request_user_input`, generic MCP forms) →
///     [`ElicitationQuestions`]: string `oneOf`/`enum` render as choices
///     (title displayed, `const` sent back), plain strings/numbers/integers as
///     free text, booleans as Yes/No, arrays as multi-select; the free-text
///     companion is skipped (the card has its own "Other").
///
/// `peer` is who the connection is talking to — see [`ElicitationPeer`]. It is
/// the sole gate on every codex-specific fixup here, and for a codex peer it
/// also carries what `initialize` learned about the running adapter
/// (`SessionState::codex_user_input_shape`), which settles the one thing the
/// wire cannot: which of a codex question's `title`/`description` is the
/// question — see [`CodexUserInputShape`]. `Codex(None)` is safe: the parser
/// then dates the form from its own markers and falls back to the pinned
/// adapter's shape.
///
/// Counts are clamped to codeg's bounds because
/// [`crate::acp::manager::ConnectionManager::register_question`] re-runs
/// [`validate_specs`]. Errors only on non-form / undeserializable requests,
/// which the connection handler turns into a graceful decline.
pub fn classify_elicitation(
    raw: &Value,
    peer: ElicitationPeer,
) -> Result<ElicitationPlan, String> {
    let req: CreateElicitationRequest = serde_json::from_value(raw.clone())
        .map_err(|e| format!("unparseable elicitation request: {e}"))?;
    let ElicitationMode::Form(form) = &req.mode else {
        // codeg only advertises `elicitation.form` — URL mode goes down
        // codex-acp's `request_permission` fallback and never reaches here.
        return Err("elicitation is not form mode".to_string());
    };
    let message: String = req
        .message
        .trim()
        .chars()
        .take(MAX_QUESTION_TEXT_CHARS)
        .collect();
    let tool_call_id = match &form.scope {
        ElicitationScope::Session(s) => s.tool_call_id.as_ref().map(|t| t.0.to_string()),
        _ => None,
    };
    if is_mcp_tool_call_approval(raw) {
        return Ok(ElicitationPlan::Approval(approval_from_form(
            form,
            message,
            tool_call_id,
        )));
    }
    let mut questions = parse_form_questions(form, raw, peer);
    if questions.specs.is_empty() {
        // A form with nothing to fill in is a bare confirmation — mirror
        // codex-acp's own no-capability fallback (Accept/Decline options).
        return Ok(ElicitationPlan::Approval(ElicitationApproval {
            message,
            tool_call_id,
            options: vec![
                ElicitationApprovalOption {
                    option_id: "accept".to_string(),
                    label: "Accept".to_string(),
                    kind: "allow_once",
                },
                decline_approval_option(),
            ],
            persist_in_content: false,
        }));
    }
    // Carry the elicitation's tool_call_id so the connection handler can key the
    // synthesized in-stream result card to it — codex delivers `request_user_input`
    // ONLY as this elicitation, never as a stream tool_call.
    questions.tool_call_id = tool_call_id;
    Ok(ElicitationPlan::Questions(questions))
}

fn decline_approval_option() -> ElicitationApprovalOption {
    ElicitationApprovalOption {
        option_id: ELICITATION_DECLINE_OPTION_ID.to_string(),
        label: "Decline".to_string(),
        kind: "reject_once",
    }
}

/// Build the approval plan for an MCP tool-call approval elicitation. The
/// request's only (optional) field is codex-acp's injected `persist` choice —
/// `oneOf` of `once` (+ `session`/`always` when codex advertises them). Its
/// options become the allow choices (wire titles displayed, `const` as the
/// option id) plus Decline; with no persist property the choices are plain
/// Allow/Decline. Mirrors codex-acp's own `request_permission` fallback
/// (`buildToolApprovalOptions`) so approvals look identical either way.
fn approval_from_form(
    form: &agent_client_protocol::schema::v1::ElicitationFormMode,
    message: String,
    tool_call_id: Option<String>,
) -> ElicitationApproval {
    let persist_choices = form
        .requested_schema
        .properties
        .get("persist")
        .and_then(|p| match p {
            ElicitationPropertySchema::String(s) => Some(string_prop_choices(s)),
            _ => None,
        })
        .filter(|c| !c.is_empty());
    let persist_in_content = persist_choices.is_some();
    let mut options = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(choices) = persist_choices {
        for c in choices {
            if options.len() == MAX_OPTIONS {
                break;
            }
            let value = c.value.trim();
            if value.is_empty()
                || value == ELICITATION_DECLINE_OPTION_ID
                || !seen.insert(value.to_string())
            {
                continue;
            }
            options.push(ElicitationApprovalOption {
                option_id: value.to_string(),
                label: if c.label.trim().is_empty() {
                    value.to_string()
                } else {
                    c.label.trim().chars().take(MAX_QUESTION_TEXT_CHARS).collect()
                },
                kind: if value == "once" {
                    "allow_once"
                } else {
                    "allow_always"
                },
            });
        }
    }
    if options.is_empty() {
        options.push(ElicitationApprovalOption {
            option_id: "accept".to_string(),
            label: "Allow".to_string(),
            kind: "allow_once",
        });
    }
    options.push(decline_approval_option());
    ElicitationApproval {
        message,
        tool_call_id,
        options,
        persist_in_content,
    }
}

/// True when the raw schema property carries codex's secret marker
/// (`_meta.codex.isSecret`). The typed schema property structs carry no
/// `_meta`, so this reads the raw JSON alongside them.
fn is_secret_property(raw: &Value, id: &str) -> bool {
    raw.get("requestedSchema")
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(id))
        .and_then(|prop| prop.get("_meta"))
        .and_then(|m| m.get("codex"))
        .and_then(|c| c.get("isSecret"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Convert a form's schema properties into ask-card questions + their typed
/// rebuild plans (see [`classify_elicitation`] for the shape taxonomy). A
/// property with no usable choices renders as free text (the card's
/// always-present "Other" input) — including plain strings, numbers, integers,
/// and choice fields whose options were all empty/duplicate.
fn parse_form_questions(
    form: &agent_client_protocol::schema::v1::ElicitationFormMode,
    raw: &Value,
    peer: ElicitationPeer,
) -> ElicitationQuestions {
    // Decided once for the whole form: codex-acp 1.12.0 swapped which of
    // `title`/`description` carries the question (see [`codex_form_shape`]).
    let codex_shape = codex_form_shape(raw, peer);
    // The 1.12.0-only fixups below apply to exactly one generation of exactly
    // one adapter; everything else — DeepSeek, generic MCP servers — shares this
    // parser and must come through untouched.
    let is_codex_112_form = codex_shape == Some(CodexUserInputShape::QuestionInTitle);
    // `_meta.codex` is only codex's to interpret. On any other peer the same
    // keys are an unrelated extension that must not silently eat a field.
    let peer_is_codex = peer.is_codex();
    let mut specs = Vec::new();
    let mut fields = Vec::new();
    for (id, prop) in &form.requested_schema.properties {
        if specs.len() == MAX_QUESTIONS {
            tracing::warn!(
                "[codex elicitation] dropping question(s) past the max of {MAX_QUESTIONS}"
            );
            break;
        }
        // Skip synthetic free-text "Other" companion fields — codex's
        // name-based `<id>__other[N]` shape, codex's own `_meta.codex` marker
        // in either generation (codex peers only: on anything else those keys
        // are not codex's and dropping the field would hide a real question),
        // and any adapter's `_meta`-marked companion (claude-agent-acp ≥0.64,
        // deliberately un-namespaced so every bridge is recognized alike).
        // The card always offers its own "Other" input, so a companion would
        // render as a duplicate question.
        if is_other_companion(id)
            || (peer_is_codex && codex_companion_role(raw, id).is_some())
            || is_custom_answer_property(raw, id)
        {
            continue;
        }
        let (title, description, kind, multi_select, choices) = match prop {
            ElicitationPropertySchema::String(s) => (
                s.title.clone(),
                s.description.clone(),
                ElicitationFieldKind::Text,
                false,
                string_prop_choices(s),
            ),
            ElicitationPropertySchema::Array(a) => (
                a.title.clone(),
                a.description.clone(),
                ElicitationFieldKind::MultiSelect,
                true,
                multi_select_choices(&a.items),
            ),
            ElicitationPropertySchema::Boolean(b) => (
                b.title.clone(),
                b.description.clone(),
                ElicitationFieldKind::Boolean,
                false,
                vec![
                    ElicitationChoice {
                        label: "Yes".to_string(),
                        value: "true".to_string(),
                    },
                    ElicitationChoice {
                        label: "No".to_string(),
                        value: "false".to_string(),
                    },
                ],
            ),
            ElicitationPropertySchema::Number(n) => (
                n.title.clone(),
                n.description.clone(),
                ElicitationFieldKind::Number,
                false,
                Vec::new(),
            ),
            ElicitationPropertySchema::Integer(i) => (
                i.title.clone(),
                i.description.clone(),
                ElicitationFieldKind::Integer,
                false,
                Vec::new(),
            ),
            // `ElicitationPropertySchema` is `#[non_exhaustive]`; a future
            // property type has no rendering here, so skip it (the agent
            // reads the missing key as unanswered and proceeds).
            _ => continue,
        };
        // Which string is the question and which is the tab label. Everything
        // except a codex-acp ≥1.12.0 `request_user_input` form keeps the
        // original reading (`description` is the prose, `title` the label);
        // 1.12.0 swapped codex's two slots, so that one form is read the other
        // way round. See [`codex_form_shape`].
        let (question_text, header_text) = if is_codex_112_form {
            (&title, &description)
        } else {
            (&description, &title)
        };
        let question = question_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                header_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(id.as_str());
        // Dedup + clamp option labels exactly like the grok bridge, keeping
        // the label→value map for the response rebuild.
        let drop_synthetic_other = is_codex_112_form && is_codex_other_question(raw, id);
        let mut options = Vec::with_capacity(choices.len().min(MAX_OPTIONS));
        let mut value_by_label = std::collections::HashMap::new();
        let mut seen = std::collections::HashSet::new();
        for c in &choices {
            if options.len() == MAX_OPTIONS {
                break;
            }
            let label = c.label.trim();
            if label.is_empty() || !seen.insert(label.to_string()) {
                continue;
            }
            // codex-acp ≥1.12.0's injected "None of the above" choice — the
            // card's own "Other" input already covers it, and better.
            if drop_synthetic_other && is_codex_synthetic_other_choice(raw, id, label, &c.value) {
                continue;
            }
            let label: String = label.chars().take(MAX_QUESTION_TEXT_CHARS).collect();
            value_by_label.insert(label.clone(), c.value.clone());
            options.push(QuestionOption {
                label,
                description: String::new(),
            });
        }
        let header = header_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|h| h.chars().take(MAX_HEADER_CHARS).collect())
            .unwrap_or_else(|| synthesize_header(question));
        specs.push(QuestionSpec {
            id: id.clone(),
            question: question.chars().take(MAX_QUESTION_TEXT_CHARS).collect(),
            header,
            multi_select,
            options,
            is_secret: is_secret_property(raw, id),
        });
        fields.push(ElicitationField {
            id: id.clone(),
            kind,
            value_by_label,
        });
    }
    // `tool_call_id` is filled in by `classify_elicitation` (which reads the
    // form scope); the parser itself only shapes the questions.
    ElicitationQuestions {
        specs,
        fields,
        tool_call_id: None,
    }
}

/// "Yes"/"No" answers (and free-text variants) for a boolean field.
fn parse_bool_answer(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" => Some(true),
        "false" | "no" | "n" => Some(false),
        _ => None,
    }
}

/// Serialize a resolved [`QuestionOutcome`] into the elicitation response.
/// The agent keys answers by the schema property id, which [`QuestionOutcome`]
/// drops — so correlate each answered item back to its spec by
/// `(question, header)` (unique per ask) and rebuild the value through the
/// parallel [`ElicitationField`]: chosen labels map back to their wire values
/// (free-text "Other" answers ride verbatim), typed per the field's kind — an
/// answer that can't be coerced (e.g. non-numeric text for a number field) is
/// omitted rather than sent mistyped. A declined card or an empty result maps
/// to `Decline`, which the agent reads as "no answer" and proceeds — never
/// worse than the pre-bridge behavior.
///
/// Every value is written under the MAIN field id, including a free-text
/// "Other": the companion property skipped by [`is_custom_answer_property`] /
/// [`is_other_companion`] is never written back. That is correct for codex,
/// which falls back to the main field — and from codex-acp 2.1.0 (#570) it is
/// the documented AIR contract: a choice field's string that matches none of
/// an `isOther` question's options is the user's own answer, which the adapter
/// hands codex in codex's own "Other" encoding,
/// `["None of the above", "user_note: <text>"]` (≤2.0.x passed the text through
/// as the answer itself). The reloaded rollout therefore carries that pair, and
/// the frontend's `parseCodexAnswers` decodes it back to the text the user
/// typed, so the live card and the reloaded one agree. If codeg ever advertises
/// `elicitation.form` to an agent that requires the answer under the companion
/// key instead (claude-agent-acp reads `question_<n>_custom` separately), this
/// has to route the free-text answer there using the marker's `questionId`.
pub fn build_elicitation_response(
    questions: &ElicitationQuestions,
    outcome: &QuestionOutcome,
) -> CreateElicitationResponse {
    if outcome.declined {
        return elicitation_decline_response();
    }
    let mut content: BTreeMap<String, ElicitationContentValue> = BTreeMap::new();
    for item in &outcome.answers {
        let Some(idx) = questions
            .specs
            .iter()
            .position(|s| s.question == item.question && s.header == item.header)
        else {
            continue;
        };
        let field = &questions.fields[idx];
        let mapped: Vec<String> = item
            .selected
            .iter()
            .map(|l| field.value_by_label.get(l).cloned().unwrap_or_else(|| l.clone()))
            .collect();
        let value = match field.kind {
            ElicitationFieldKind::Text => match mapped.into_iter().next() {
                Some(v) => ElicitationContentValue::String(v),
                None => continue,
            },
            ElicitationFieldKind::MultiSelect => ElicitationContentValue::StringArray(mapped),
            ElicitationFieldKind::Boolean => {
                match mapped.first().and_then(|v| parse_bool_answer(v)) {
                    Some(b) => ElicitationContentValue::Boolean(b),
                    None => continue,
                }
            }
            ElicitationFieldKind::Number => {
                match mapped
                    .first()
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|f| f.is_finite())
                {
                    Some(f) => ElicitationContentValue::Number(f),
                    None => continue,
                }
            }
            ElicitationFieldKind::Integer => {
                match mapped.first().and_then(|v| v.trim().parse::<i64>().ok()) {
                    Some(i) => ElicitationContentValue::Integer(i),
                    None => continue,
                }
            }
        };
        content.insert(field.id.clone(), value);
    }
    if content.is_empty() {
        return elicitation_decline_response();
    }
    CreateElicitationResponse::new(ElicitationAction::Accept(
        ElicitationAcceptAction::new().content(content),
    ))
}

/// Serialize the user's permission-card choice for an approval-style
/// elicitation. The decline option (and any unknown option id, defensively)
/// maps to `Decline`; an allow choice maps to `Accept`, echoing the chosen
/// persist value back as `content.persist` when the request carried the
/// persist property (codex-acp pops it into the app-server `_meta.persist`;
/// a plain Accept sends no content, which codex-acp reads the same as
/// `content: null`).
pub fn build_elicitation_approval_response(
    approval: &ElicitationApproval,
    option_id: &str,
) -> CreateElicitationResponse {
    let accepted = option_id != ELICITATION_DECLINE_OPTION_ID
        && approval.options.iter().any(|o| o.option_id == option_id);
    if !accepted {
        return elicitation_decline_response();
    }
    let mut accept = ElicitationAcceptAction::new();
    if approval.persist_in_content {
        let mut content: BTreeMap<String, ElicitationContentValue> = BTreeMap::new();
        content.insert(
            "persist".to_string(),
            ElicitationContentValue::String(option_id.to_string()),
        );
        accept = accept.content(content);
    }
    CreateElicitationResponse::new(ElicitationAction::Accept(accept))
}

/// The elicitation reply for a declined / unrenderable / torn-down ask.
/// `Decline` makes the agent proceed with its own judgment (it reads it as no
/// answer), so no path through the bridge can regress the pre-bridge behavior.
pub fn elicitation_decline_response() -> CreateElicitationResponse {
    CreateElicitationResponse::new(ElicitationAction::Decline)
}

/// The elicitation reply when the pending card is torn down without a user
/// choice (turn cancel, disconnect): `Cancel`, mirroring codex-acp's own
/// "cancelled" outcome mapping.
pub fn elicitation_cancel_response() -> CreateElicitationResponse {
    CreateElicitationResponse::new(ElicitationAction::Cancel)
}

/// Build the `raw_input` (questions) for the in-stream `AskQuestionResultCard`
/// codeg synthesizes for a native ask that resolves out-of-band rather than as a
/// completed stream tool_call. Three callers: grok (answers over the
/// `_x.ai/ask_user_question` ext round-trip — `handle_grok_ask_user_question`),
/// codex `request_user_input` (answers over the `elicitation/create` round-trip
/// — `handle_elicitation_request`), and pi's extension-UI `select` (answers over
/// the `session/request_permission` round-trip — `try_bridge_pi_select_ask`).
/// None of them emits a completed tool result into the ACP stream, so the
/// connection handler emits this once the user submits.
///
/// Deliberately omits `header`, so the frontend parses `header:""` and matches
/// answers to questions by `question` text alone. The paired
/// [`grok_result_card_output`] uses the SAME empty header. For grok this is exact
/// live↔reload parity (its history parser emits `header:""` too); for codex the
/// reloaded history card (`codex.rs`) does carry the question header, so a
/// multi-question ask shows header tab labels after reload but not live — a
/// cosmetic gap only (single-question asks, the common case, render identically
/// and the answer always shows). Built from the already-clamped [`QuestionSpec`]s
/// so input and output stay in lockstep.
pub fn grok_result_card_input(specs: &[QuestionSpec]) -> Value {
    let questions: Vec<Value> = specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "question": s.question,
                "multiSelect": s.multi_select,
                "options": s.options,
            })
        })
        .collect();
    serde_json::json!({ "questions": questions })
}

/// Build the `raw_output` (`{answers, declined}` envelope) for the in-stream
/// `AskQuestionResultCard` — the codeg-mcp `structuredContent` shape the frontend
/// already parses (`parseAskQuestionOutcome`). Emits `header:""` to match
/// [`grok_result_card_input`] (see its docs). Companion to [`grok_result_card_input`].
pub fn grok_result_card_output(outcome: &QuestionOutcome) -> Value {
    let answers: Vec<Value> = outcome
        .answers
        .iter()
        .map(|a| {
            serde_json::json!({
                "header": "",
                "question": a.question,
                "multi_select": a.multi_select,
                "selected": a.selected,
            })
        })
        .collect();
    serde_json::json!({ "answers": answers, "declined": outcome.declined })
}

/// The hot-swappable feature config read at MCP injection time. Kept tiny and
/// separate from `FeedbackConfig` / `DelegationConfig` so the three features
/// toggle independently — `codeg-mcp` is injected when ANY is enabled, and each
/// tool is listed only when its own feature is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionConfig {
    pub enabled: bool,
}

/// Shared, hot-swappable handle to [`QuestionConfig`]. Cloned into
/// `DelegationInjection` (read at injection) and `AppState` (updated on save).
/// Mirrors [`crate::acp::feedback::FeedbackRuntimeConfig`].
#[derive(Clone, Default)]
pub struct QuestionRuntimeConfig {
    inner: Arc<RwLock<QuestionConfig>>,
}

impl QuestionRuntimeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> QuestionConfig {
        self.inner.read().await.clone()
    }

    pub async fn set(&self, cfg: QuestionConfig) {
        *self.inner.write().await = cfg;
    }

    /// Convenience read used at MCP injection time.
    pub async fn is_enabled(&self) -> bool {
        self.inner.read().await.enabled
    }
}

#[cfg(test)]
mod tests;

