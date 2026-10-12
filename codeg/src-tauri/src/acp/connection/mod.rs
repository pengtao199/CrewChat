use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    BlobResourceContents, CancelNotification, ClientCapabilities, ClientSessionCapabilities,
    CloseSessionRequest, CompactionCapabilities, ContentBlock, ContentChunk,
    CreateTerminalRequest, CreateTerminalResponse, ElicitationCapabilities,
    ElicitationFormCapabilities, EmbeddedResource, EmbeddedResourceResource,
    FileSystemCapabilities, ImageContent, InitializeRequest,
    KillTerminalRequest,
    KillTerminalResponse, LoadSessionRequest, NewSessionRequest,
    NewSessionResponse, NoticeCapabilities, PermissionOptionKind, Plan, PlanEntryPriority, PlanEntryStatus,
    PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResourceLink, ResumeSessionRequest,
    ResumeSessionResponse, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectGroup,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionId, SessionModeState,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionModeRequest, StopReason, TerminalExitStatus,
    TerminalOutputRequest, TerminalOutputResponse, TextContent, TextResourceContents,
    ToolCallContent, ToolCallLocation, ToolKind, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::v1::{
    HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio, AGENT_METHOD_NAMES,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    on_receive_notification, on_receive_request, Agent, Client, ConnectionTo, Dispatch,
    HandleDispatchFrom, Handled, JsonRpcRequest, RequestCancellation, Responder, Role,
    UntypedMessage,
};
use crate::acp::agent_process::AcpAgent;
use crate::acp::agent_session::AgentSession;
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::acp::agent_mentions::{append_agent_routes, append_role_frame, append_team_frame};
use crate::acp::delegation::team::TeamMember;
use crate::acp::background_watch;
use crate::acp::cursor_ext::{
    CursorAskQuestionRequest, CursorCreatePlanRequest, CursorGenerateImageRequest,
    CursorTaskRequest, CursorUpdateTodosRequest,
};
use crate::acp::error::AcpError;
use crate::acp::file_system_runtime::{
    FileSystemRuntime, FileSystemRuntimeError, FsAccessPolicy, FS_POLICY_ENV,
};
use crate::acp::host_tools_policy::{HostToolsPolicy, HOST_TOOLS_ENV};
use crate::acp::registry::{self, AgentDistribution};
use crate::acp::session_state::SessionState;
use crate::acp::stderr_tail::{summarize_parser_error, StderrTail, TailScope};
use crate::acp::terminal_runtime::{
    TerminalRuntime, TerminalRuntimeError, TerminalShellRuntimeConfig,
};
use crate::acp::types::{
    AcpEvent, AsyncTaskDelta, AsyncTaskUsage, AvailableCommandInfo, ConnectionInfo,
    ConnectionStatus, GrokModelCatalog, GrokModelSpec, PermissionOptionInfo, PlanEntryInfo,
    PluginLoadFailure, PromptCapabilitiesInfo, PromptInputBlock, SessionConfigBooleanInfo,
    SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectGroupInfo,
    SessionConfigSelectInfo, SessionConfigSelectOptionInfo, SessionFailureRecord, SessionModeInfo,
    SessionModeStateInfo, SessionNotice, ToolCallImageInfo, UserMessageBlock,
};
use crate::logging::throttle::LeadingEdgeThrottle;
use crate::models::agent::AgentType;
use crate::network::proxy;
use crate::parsers::COMPACTION_SUMMARY_META_KEY;
use crate::web::event_bridge::{
    emit_with_state, emit_with_state_built, emit_with_state_gated, EventEmitter,
};

/// Injected into the agent process only when the user has opted in — see
/// [`force_command_color_enabled`] for why it is not a default.
///
/// Four variables because no single one reaches the toolchain an agent actually
/// shells out to, and a toggle that leaves the common commands plain is one
/// users reasonably read as broken. Each covers a different half of a decision
/// some tool makes:
///
/// - `CLICOLOR` — the BSD *enable* flag. `CLICOLOR_FORCE` on its own is NOT one:
///   in BSD `ls` the two conditions are separate (`getenv("CLICOLOR")` decides
///   whether color exists at all, `getenv("CLICOLOR_FORCE")` only waives the
///   `isatty` requirement), so forcing without enabling leaves a bare `ls`
///   monochrome. Verified on macOS 26.6: `CLICOLOR_FORCE=1 FORCE_COLOR=1
///   TERM=xterm-256color ls` emits zero escapes; adding `CLICOLOR=1` colors it.
/// - `CLICOLOR_FORCE` — the waiver above, and the convention Go/Rust CLIs
///   (`gh` via go-gh, ripgrep) read as "force". An agent's stdout is a pipe, so
///   without it every `isatty` check answers no.
/// - `FORCE_COLOR` — what Node's `supports-color` reads, and so what `pnpm`,
///   `next`, `eslint`, `vitest` and the rest of the npm toolchain honor. That
///   family ignores both `CLICOLOR*` variables entirely, which is why the
///   toggle used to do nothing for the build command most people run first.
/// - `TERM` — not a color flag, a precondition for the others. BSD `ls` resolves
///   its palette through `tgetent(getenv("TERM"))` and stays monochrome when
///   TERM names nothing, which is the normal case for a codeg launched from
///   Finder rather than from a shell. `supports-color` reads it too, and answers
///   256 colors for a `-256color` suffix where `FORCE_COLOR=1` alone caps at 16.
///
/// `TERM` is the one entry here that OVERRIDES an inherited value rather than
/// filling in a missing one (`merge_agent_env` lists what a launch sets;
/// everything else is inherited). That is deliberate: the agent's stdout is a
/// pipe to codeg and never a terminal, so an inherited `TERM` describes the
/// shell that happened to start codeg — `screen-256color` under tmux, nothing
/// at all under Finder — not anything the agent is attached to. Pinning one
/// known-good entry is what makes the toggle behave the same in a packaged app
/// as in `pnpm tauri dev`. A per-agent env row still outranks all four.
const DEFAULT_COMMAND_COLOR_ENV: [(&str, &str); 4] = [
    ("CLICOLOR", "1"),
    ("CLICOLOR_FORCE", "1"),
    ("FORCE_COLOR", "1"),
    ("TERM", "xterm-256color"),
];

/// Whether launches force color out of agent-run commands. Mirrors
/// `SystemTerminalSettings.colorize_command_output`, applied at startup and on
/// every save.
///
/// A process global rather than a handle threaded through the launch path
/// because [`merge_agent_env`] is a sync function with a dozen callers (down to
/// `antigravity_launch_env`, which the settings panel calls with no connection
/// in hand), and it already reads two other ambient sources the same way —
/// `proxy::current_proxy_env_vars` and `prepend_officecli_path`.
static FORCE_COMMAND_COLOR: AtomicBool = AtomicBool::new(false);

/// Whether a launch should put [`DEFAULT_COMMAND_COLOR_ENV`] in the agent's
/// environment.
///
/// **Off by default**, which is a behavior change: launches used to force
/// `CLICOLOR_FORCE` unconditionally. The feature it buys is real — codeg
/// preserves ANSI through tool-call output streaming ([`trim_partial_ansi_tail`])
/// so the transcript's `<Terminal>` card renders command output in color — but
/// the cost was paid by everything else in the process tree.
///
/// codeg cannot scope the force to the output it renders. Agents like Claude
/// Code run their bash tool IN-PROCESS, so the commands whose color shows up in
/// the card are not spawned by codeg at all; the only reachable lever is the
/// agent's own environment, which every descendant inherits. So the same
/// variables that color the terminal card also color the output the agent pipes
/// into `jq` — and neither force flag can be vetoed downstream: `CLICOLOR_FORCE`
/// is by ecosystem convention the one color variable `NO_COLOR` cannot override
/// (`gh`, via go-gh, computes `forced || (!disabled && isTTY)`), and
/// `supports-color` reads `FORCE_COLOR` before it reads anything else. `gh pr
/// list --json number` emits ANSI *inside* the JSON, and the parse fails.
///
/// The quieter cost is that the agent captures those escapes into its OWN
/// context: every command it runs spends tokens on escape sequences and risks
/// the model misreading output it needs to parse. That is charged on every turn
/// whether or not anyone looks at the colored card, which is why the default is
/// off rather than on-with-an-escape-hatch.
///
/// Users who want the colored transcript turn it on in General Settings. A
/// per-agent row still wins over every variable this injects — `runtime_env`
/// outranks these defaults in [`merge_agent_env`] — so one agent can be exempted
/// while the toggle stays on globally. Exempt it with EMPTY values, not `0`: the
/// BSD pair is presence-checked (`CLICOLOR_FORCE=0` still reads as forced —
/// verified on macOS 26.6), and an empty value is what the spawn layer turns
/// into `env_remove`, the same convention `child_env_value` documents in
/// [`crate::acp::file_system_runtime`].
pub fn force_command_color_enabled() -> bool {
    FORCE_COMMAND_COLOR.load(Ordering::Relaxed)
}

/// Point live launches at the current setting. Called once at startup from the
/// persisted row and again on every save, so an already-running app picks the
/// change up on its next connection without a restart.
pub fn set_force_command_color(enabled: bool) {
    FORCE_COMMAND_COLOR.store(enabled, Ordering::Relaxed);
}

/// `scratch` is the per-launch temp directory from
/// [`crate::acp::scratch_dir`], or `None` when isolation is off or the
/// directory could not be created (then the child inherits the ambient temp
/// dir, exactly as it did before).
fn merge_agent_env(
    env: &[(&'static str, &'static str)],
    runtime_env: &BTreeMap<String, String>,
    scratch: Option<&Path>,
) -> Vec<(String, String)> {
    merge_agent_env_with_color(force_command_color_enabled(), env, runtime_env, scratch)
}

/// [`merge_agent_env`] with the color decision handed in.
///
/// Split out for the same reason as [`antigravity_acp_dir_with_inherited`]: the
/// setting lives in a process global, and a test that wrote it would silently
/// race every other test in this module that merges an env.
fn merge_agent_env_with_color(
    force_color: bool,
    env: &[(&'static str, &'static str)],
    runtime_env: &BTreeMap<String, String>,
    scratch: Option<&Path>,
) -> Vec<(String, String)> {
    // Env var order is not semantically meaningful; use map overwrite semantics
    // to keep precedence while avoiding repeated O(n) scans.
    let mut merged = BTreeMap::<String, String>::new();

    if force_color {
        for (key, value) in DEFAULT_COMMAND_COLOR_ENV {
            merged.insert(key.to_string(), value.to_string());
        }
    }

    for (key, value) in env {
        merged.insert((*key).to_string(), (*value).to_string());
    }

    for (key, value) in runtime_env {
        merged.insert(key.clone(), value.clone());
    }

    for (key, value) in proxy::current_proxy_env_vars() {
        merged.insert(key, value);
    }
    proxy::add_no_proxy_to_launch_env(&mut merged, &proxy::current_no_proxy_env_vars());

    // Ensure agent-invoked `officecli …` (from an enabled office skill) resolves
    // even when codeg installed the binary outside the user's shell PATH — the
    // Windows self-managed dir, or `~/.local/bin` under a GUI launch.
    prepend_officecli_path(&mut merged);

    // LAST, after `runtime_env`, and that ordering is the whole fix rather than
    // a style choice. A self-extracting agent binary resolves its unpack
    // location from `TMP` before `TEMP` (Windows `GetTempPathW`), so leaving a
    // per-agent `env_json` `TMP` in place would send a 1.17 GB extraction
    // wherever that points while codeg cheerfully deleted an empty scratch
    // directory and reported the leak fixed. Users who want the churn on
    // another volume set `CODEG_ACP_TMP_ROOT`; users who want the old
    // pass-through wholesale set `CODEG_ACP_TMP_ISOLATION=0`.
    if let Some(dir) = scratch {
        crate::acp::scratch_dir::apply_to_env(&mut merged, dir);
    }

    merged.into_iter().collect()
}

/// Whether a Cursor launch gets the root `--force` (Run Everything) flag, from
/// the panel's `CURSOR_FORCE` knob.
///
/// The knob is TRI-state on purpose. It used to be written as "1" for on and
/// *deleted* for off, which made "the user chose Ask" indistinguishable from
/// "never configured" — and since the panel rendered the missing key as Run
/// Everything while this function rendered it as Ask, the switch showed one
/// thing and the session did another. Off is now written as an explicit "0",
/// and both sides read the same rule: unset means Ask.
///
/// Unset resolving to Ask (not Run Everything) is deliberate. It is what every
/// Cursor session has actually been doing all along, so no existing install
/// silently loses its confirmation prompts; `--force` also turns cursor's own
/// sandbox off (`approvalMode: unrestricted` → `insecure_none`), which is not
/// something to switch on for someone who never asked.
pub(crate) fn cursor_force_enabled(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim) else {
        return false;
    };
    value == "1" || value.eq_ignore_ascii_case("true")
}

/// Cursor subscription-mode launch policy. When the user picked the official
/// subscription (browser login), guarantee the launched CLI sees NONE of the
/// custom-endpoint credentials — not even a stale `CURSOR_API_KEY` /
/// `CURSOR_API_BASE_URL` inherited from this process's environment (e.g. a dev
/// shell export). cursor-agent would otherwise validate that leaked key and
/// refuse to fall back to the login credential. An empty value tells the spawn
/// layer (`acp::agent_process`) to `env_remove` the inherited var.
///
/// Gated on the explicit `CURSOR_AUTH_MODE` knob (written by the Cursor panel),
/// so legacy rows and operator-provided container env are left untouched. In
/// custom mode the credentials are present and non-empty, so nothing is cleared.
fn apply_cursor_env_policy(merged: &mut Vec<(String, String)>, runtime_env: &BTreeMap<String, String>) {
    if runtime_env.get("CURSOR_AUTH_MODE").map(String::as_str) != Some("subscription") {
        return;
    }
    for key in ["CURSOR_API_KEY", "CURSOR_API_BASE_URL"] {
        let already_set = merged
            .iter()
            .any(|(k, v)| k == key && !v.trim().is_empty());
        if !already_set {
            merged.retain(|(k, _)| k != key);
            merged.push((key.to_string(), String::new()));
        }
    }
}

/// Grok's launch-time credential policy, mirroring [`apply_cursor_env_policy`].
/// When the user picked the `grok login` subscription (recorded as
/// `GROK_AUTH_MODE=subscription` by the Grok settings panel), scrub any
/// `XAI_API_KEY` inherited from this process's environment so the CLI falls back
/// to the browser-login credential in `~/.grok/auth.json` rather than a leaked
/// shell/container export. An empty value tells the spawn layer
/// (`acp::agent_process`) to `env_remove` the inherited var. In api_key mode the key is
/// present and non-empty, so nothing is cleared; legacy/no-mode rows are left
/// untouched.
fn apply_grok_env_policy(merged: &mut Vec<(String, String)>, runtime_env: &BTreeMap<String, String>) {
    if runtime_env.get("GROK_AUTH_MODE").map(String::as_str) != Some("subscription") {
        return;
    }
    let key = "XAI_API_KEY";
    let already_set = merged
        .iter()
        .any(|(k, v)| k == key && !v.trim().is_empty());
    if !already_set {
        merged.retain(|(k, _)| k != key);
        merged.push((key.to_string(), String::new()));
    }
}

/// codeg-side knob recording which Antigravity auth method the settings panel
/// chose. It is NOT read by the agent — the server takes its auth intent from
/// `auth.type` in `antigravity-acp/settings.json` — so the launch path uses it
/// twice: to decide which credential env vars may reach the process
/// ([`apply_antigravity_env_policy`]) and to write that file
/// ([`sync_antigravity_settings_file`]).
const ANTIGRAVITY_AUTH_METHOD_ENV: &str = "AGY_AUTH_METHOD";

/// The four `auth.type` values Antigravity's ACP server accepts, canonical
/// spellings only. `vertex-ai` is the pre-rebrand alias for `agent-platform`;
/// the server still accepts it, but codeg never writes it.
const ANTIGRAVITY_AUTH_METHODS: &[&str] = &[
    "oauth-personal",
    "oauth-business",
    "gemini-api-key",
    "agent-platform",
];

/// Credential env vars the Antigravity server reads, grouped by the auth method
/// that actually uses them. Anything outside the selected method's group is
/// cleared at launch so a value inherited from the developer's shell cannot
/// silently take over.
const ANTIGRAVITY_CREDENTIAL_ENV_VARS: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
];

/// Which of [`ANTIGRAVITY_CREDENTIAL_ENV_VARS`] the given method consumes.
fn antigravity_env_vars_for_method(method: &str) -> &'static [&'static str] {
    match method {
        // `auth.type = gemini-api-key` reads the key from GEMINI_API_KEY and
        // nothing else (the server's own auth_required message says so).
        "gemini-api-key" => &["GEMINI_API_KEY"],
        // Agent Platform (formerly Vertex AI) takes GOOGLE_API_KEY, or a
        // project + location from the GOOGLE_CLOUD_* pair (with the
        // settings.json `gcp` block as a per-value fallback behind them).
        "agent-platform" => &[
            "GOOGLE_API_KEY",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
        ],
        // Both OAuth paths authenticate through the browser; Gemini Enterprise
        // additionally reads gcp.project/location from settings.json ONLY,
        // never from the environment.
        _ => &[],
    }
}

/// Antigravity's launch credential policy, in the spirit of
/// [`apply_cursor_env_policy`] but strictly stronger.
///
/// Once the panel has recorded a method, the panel OWNS all four credential
/// vars: a value survives into the child only if the chosen method reads it AND
/// the panel actually stored one. Everything else is cleared — an empty value
/// tells the spawn layer (`acp::agent_process`) to `env_remove` the inherited
/// one.
///
/// UNCONDITIONALLY, unlike Cursor's version, which skips a key the caller's own
/// `runtime_env` already set to a non-empty value. That guard makes sense when
/// the credential and the mode are independent; here they are not. A
/// `GEMINI_API_KEY` sitting in `merged` on an `oauth-personal` session can only
/// come from a stale panel row (the user switched away from API-key auth) or a
/// shell/container export — and in both cases honoring it authenticates as
/// something other than what the user picked, silently. Keeping it would also
/// disagree with the `auth.type` this same launch writes to settings.json.
///
/// The non-empty half is what makes "reads it" insufficient on its own, and it
/// is not hypothetical — it is the Agent Platform panel's central choice. That
/// method takes EITHER a `GOOGLE_API_KEY` or a project + location, and the
/// server suppresses the pair whenever the key is set (its `_vertex_config`
/// logs "project and location suppressed by the key"); the panel encodes that
/// by hiding the project/location fields while a key is typed and DELETING
/// `GOOGLE_API_KEY` from the stored row when it is not. Leaving the key merely
/// "allowed" therefore let an inherited one — a dev shell, a CI container —
/// override the project the user explicitly filled in, sending the session to
/// another account and another billing target with nothing on screen to say so.
/// The same reasoning covers an inherited `GOOGLE_CLOUD_PROJECT`, which would
/// otherwise outrank the `gcp` block in the settings file codeg just wrote.
///
/// The cost is that a credential supplied ONLY by the surrounding environment
/// stops working once a method is recorded — but the panel already warns about
/// exactly that state (`missingGeminiApiKey`, `missingAgentPlatformConfig`), so
/// this makes the launch agree with what the user was told rather than quietly
/// contradict it.
///
/// Legacy rows with no recorded method — and any unrecognized value — are left
/// completely untouched, so an operator-provisioned container env that never
/// went through the panel keeps working.
fn apply_antigravity_env_policy(
    merged: &mut Vec<(String, String)>,
    runtime_env: &BTreeMap<String, String>,
) {
    let Some(method) = runtime_env
        .get(ANTIGRAVITY_AUTH_METHOD_ENV)
        .map(String::as_str)
        .map(str::trim)
        .filter(|method| ANTIGRAVITY_AUTH_METHODS.contains(method))
    else {
        return;
    };
    let keep = antigravity_env_vars_for_method(method);
    for key in ANTIGRAVITY_CREDENTIAL_ENV_VARS {
        let kept = keep.contains(key)
            && merged
                .iter()
                .any(|(k, v)| k == key && !v.trim().is_empty());
        if kept {
            continue;
        }
        merged.retain(|(k, _)| k != key);
        merged.push(((*key).to_string(), String::new()));
    }
}

/// Project the panel's Antigravity auth choice into
/// `<GEMINI_HOME>/antigravity-acp/settings.json`, the ONLY place the server
/// looks for it.
///
/// This is load-bearing, not a convenience: `session/new` fails outright with
/// `-32000 Authentication required` when that file declares no `auth.type`
/// (environment-based selection was removed upstream), and codeg does not
/// implement the ACP `authenticate` request that would otherwise set it. With
/// the file in place the server runs its own browser OAuth loopback flow inside
/// `session/new`, so writing it is what makes the agent usable at all.
///
/// Deliberately a READ-MODIFY-WRITE merge of only three keys. The file is the
/// user's (the server parses it as Hjson and documents it as user-provided), so
/// unknown keys and any hand-written `gcp` block survive a codeg write.
///
/// FAILS CLOSED, exactly like the server's own `settings_writer`: "a file that
/// cannot be parsed is left alone, since rewriting it would delete content we
/// could not read." A missing file means "create"; a read error, a parse error
/// or a non-object root all mean "give up". The one place codeg is stricter is
/// the dialect — the server parses Hjson (comments, trailing commas) and codeg
/// only strict JSON, so a hand-commented file lands in the give-up branch
/// rather than being flattened. The warning names the file so the user can set
/// `auth.type` there themselves; the panel shows that same path.
///
/// Every failure is a warning, never a spawn failure: an `auth.type` already in
/// the file (written by hand, by an earlier launch, or by the server's own auth
/// picker) may well still be valid.
///
/// It RETURNS what happened rather than only logging it, because a silent skip
/// here is indistinguishable from success at the only moment the user is
/// looking. The settings panel saves the env row and says "saved" — but the row
/// is not what authenticates the agent, this file is, and when the file cannot
/// be rewritten the two disagree from that moment on. Switching methods is the
/// sharp edge: the launch scrubs the credential vars for the NEW method
/// ([`apply_antigravity_env_policy`]) while the server keeps reading the OLD
/// `auth.type`, so the next session fails with no credential for the method it
/// thinks it is using. The panel calls this on save and reports the answer.
fn sync_antigravity_settings_file(runtime_env: &BTreeMap<String, String>) -> AntigravitySyncReport {
    let recorded = runtime_env
        .get(ANTIGRAVITY_AUTH_METHOD_ENV)
        .map(String::as_str)
        .map(str::trim)
        .filter(|method| ANTIGRAVITY_AUTH_METHODS.contains(method));

    let acp_dir = match antigravity_acp_dir_for_env(runtime_env) {
        Ok(dir) => dir,
        Err(reason) => {
            tracing::warn!("[ACP][Antigravity] not editing settings.json: {reason}.");
            return AntigravitySyncReport::skipped(Path::new("<unknown>"), reason);
        }
    };
    let path = acp_dir.join("settings.json");

    let existing = match read_antigravity_settings(&path) {
        Ok(existing) => existing,
        Err(reason) => {
            tracing::warn!(
                "[ACP][Antigravity] not editing {}: {reason}. \
                 Set `auth.type` in it yourself, or move it aside.",
                path.display()
            );
            return AntigravitySyncReport::skipped(&path, reason);
        }
    };

    // Fall back to the method the settings panel DISPLAYS as selected, but only
    // when the file names none: the agent cannot start a single session without
    // an `auth.type`, and erroring out on a user who never opened the panel
    // would be a dead end. `oauth-personal` is the free-tier default and merely
    // makes the server open a browser — itself a consent step — so it is safe
    // to imply. An `auth.type` already in the file is NEVER overridden by it.
    let existing_auth_type = existing
        .as_ref()
        .and_then(|root| root.get("auth"))
        .and_then(|auth| auth.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let method = match (recorded, existing_auth_type) {
        (Some(method), _) => method,
        (None, Some(_)) => return AntigravitySyncReport::current(&path),
        (None, None) => "oauth-personal",
    };

    // Gemini Enterprise reads project/location from this file ONLY (never the
    // environment), so the panel's values ride along for that path — including
    // their ABSENCE, for the methods the panel owns them for.
    let gcp_project = antigravity_gcp_field(runtime_env, recorded, "GOOGLE_CLOUD_PROJECT");
    let gcp_location = antigravity_gcp_field(runtime_env, recorded, "GOOGLE_CLOUD_LOCATION");

    let updated = match merge_antigravity_settings(existing, method, gcp_project, gcp_location) {
        Ok(Some(updated)) => updated,
        // Already says exactly this; skip the write so a running server's file
        // is not needlessly rewritten.
        Ok(None) => return AntigravitySyncReport::current(&path),
        Err(reason) => {
            tracing::warn!(
                "[ACP][Antigravity] not editing {}: {reason}. \
                 Set `auth.type` in it yourself, or move it aside.",
                path.display()
            );
            return AntigravitySyncReport::skipped(&path, reason);
        }
    };

    match write_antigravity_settings(&acp_dir, &path, &updated) {
        Ok(()) => {
            tracing::info!(
                "[ACP][Antigravity] auth.type={method} recorded in {}",
                path.display()
            );
            AntigravitySyncReport::written(&path)
        }
        Err(err) => {
            tracing::warn!("[ACP][Antigravity] cannot write {}: {err}", path.display());
            AntigravitySyncReport::skipped(&path, format!("codeg could not write it ({err})"))
        }
    }
}

/// `<GEMINI_HOME>/antigravity-acp` for a launch carrying `runtime_env`.
///
/// Resolved through the agent's OWN rules ([`crate::parsers::antigravity`]), not
/// a local `PathBuf::from`, because `GEMINI_HOME` is one of the variables whose
/// upstream runs `os.path.expanduser`. Building the path by hand made
/// `GEMINI_HOME=~/somewhere` mean two different directories: the server read
/// `$HOME/somewhere`, while codeg created a folder literally named `~` under
/// whatever directory it happened to be launched from — and wrote the
/// `auth.type` there, so `session/new` still failed with `Authentication
/// required` no matter how many times the panel was saved.
///
/// `runtime_env` alone is enough even though the launch also merges registry,
/// proxy and PATH entries on top: none of them sets `GEMINI_HOME`
/// (Antigravity's registry entry declares `env: &[]`), and `merge_agent_env`
/// gives `runtime_env` the highest precedence regardless. Taking one map rather
/// than the merged pair is what lets the settings panel call this before any
/// launch has been composed.
///
/// `Err` when the directory cannot be named at all — which happens exactly when
/// the answer depends on the child's home and that home is unknowable (the
/// launch removes `HOME`, or sets it to a relative path). Writing anyway would
/// mean guessing, and a guess here creates a stray tree AND leaves the real
/// `auth.type` unwritten, so the sync reports the skip instead.
fn antigravity_acp_dir_for_env(runtime_env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    antigravity_acp_dir_with_inherited(runtime_env, std::env::var_os("GEMINI_HOME"))
}

/// [`antigravity_acp_dir_for_env`] with codeg's own `GEMINI_HOME` handed in.
///
/// Split out so the three-state resolution can be tested without mutating the
/// process environment. A `temp_env` writer would race every other test that
/// reads `GEMINI_HOME` — `resolve_antigravity_acp_dir` does, one assertion away
/// in this same module — and that race is silent: the writer's value simply
/// leaks into the reader's expectation.
fn antigravity_acp_dir_with_inherited(
    runtime_env: &BTreeMap<String, String>,
    inherited: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    // Three states, and they are NOT interchangeable — the same distinction
    // [`crate::acp::file_system_runtime::child_home_dir`] spells out for `HOME`,
    // for the same reason. `merge_agent_env` names only the variables a launch
    // SETS, so an ABSENT key means the child inherits codeg's value, and a
    // container that relocates the tree does exactly that: `GEMINI_HOME` in the
    // image's own environment, nothing in the per-agent row. Reading only the
    // row made codeg write `auth.type` — and name the token file — under
    // `~/.gemini` (i.e. `/root/.gemini`) while the agent used the relocated
    // one, so the file the panel talked about was never the file the session
    // read.
    let configured = match runtime_env.get("GEMINI_HOME") {
        // Explicitly removed (blank ⇒ `env_remove`): the child sees no
        // `GEMINI_HOME` at all and falls back to `~/.gemini`.
        Some(value) if value.is_empty() => None,
        // Overridden. NOT trimmed: the spawn layer's "is this var removed" test
        // is an exact empty-string check
        // (`acp/agent_process.rs`), so a whitespace-only value
        // reaches the child verbatim and trimming here would name a directory
        // it never opens.
        Some(value) => Some(std::ffi::OsString::from(value)),
        // Absent: the child inherits codeg's environment, so codeg's own answer
        // is exact. Empty is filtered because the server's `paths.py` treats an
        // empty `GEMINI_HOME` as unset (`if not home`).
        None => inherited.filter(|value| !value.is_empty()),
    };

    // The CHILD's home, not codeg's. `merge_agent_env` copies `HOME` into the
    // child like any other variable, so a launch that relocates it moves both
    // the `~/.gemini` default and any `~` in `GEMINI_HOME` with it — and the
    // server, running `os.path.expanduser` in that environment, resolves them
    // there. Only a value that is already absolute is independent of it.
    let needs_home = configured.as_ref().is_none_or(|value| {
        let value = value.to_string_lossy();
        value == "~" || value.starts_with("~/") || value.starts_with("~\\")
    });
    let home = crate::acp::file_system_runtime::child_home_dir(runtime_env);
    if needs_home && home.is_none() {
        return Err(
            "codeg cannot tell which home directory the agent will use (this launch removes \
             HOME, or points it somewhere relative), so it cannot tell where the file is"
                .to_string(),
        );
    }

    Ok(
        crate::parsers::antigravity::resolve_gemini_home_from_value(configured, home)
            .join(ANTIGRAVITY_ACP_SUBDIR),
    )
}

/// `<GEMINI_HOME>/antigravity-acp`, the ACP server's private subtree.
const ANTIGRAVITY_ACP_SUBDIR: &str = "antigravity-acp";

/// What the panel is saying about one `gcp` field.
///
/// The distinction the two-state `Option` could not carry: "the panel does not
/// manage this field" and "the panel manages it and the user cleared it" both
/// arrived as `None`, and the merge treated both as leave-alone. So clearing
/// the project and location in the panel and saving left the values already on
/// disk in force forever — for `oauth-business` they are the ONLY place the
/// project comes from, so the session kept authenticating against a project the
/// UI no longer showed anywhere.
///
/// Ownership follows the recorded METHOD, not the value: `oauth-business` and
/// `agent-platform` are the two the panel renders the project/location inputs
/// for, so for those an empty value is a deletion. For every other method — and
/// for a legacy row with no recorded method at all — the panel never showed the
/// fields, so whatever is in the file was hand-written and is left untouched.
enum GcpField<'a> {
    /// Not the panel's to touch.
    Keep,
    Set(&'a str),
    /// The panel owns it and it is empty: remove the key.
    Clear,
}

fn antigravity_gcp_field<'a>(
    runtime_env: &'a BTreeMap<String, String>,
    recorded_method: Option<&str>,
    key: &str,
) -> GcpField<'a> {
    let value = runtime_env
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match value {
        Some(value) => GcpField::Set(value),
        None if matches!(recorded_method, Some("oauth-business" | "agent-platform")) => {
            GcpField::Clear
        }
        None => GcpField::Keep,
    }
}

/// The outcome of one [`sync_antigravity_settings_file`] pass, for the panel.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravitySyncReport {
    /// The file this was about, shown alongside the reason so the user can go
    /// and set `auth.type` by hand.
    pub path: String,
    pub status: AntigravitySyncStatus,
    /// Present only for `skipped`, in the words the log uses.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravitySyncStatus {
    /// The file now declares the chosen method.
    Written,
    /// It already did; nothing to write.
    AlreadyCurrent,
    /// Left untouched. The agent's auth is NOT what the panel shows.
    Skipped,
}

impl AntigravitySyncReport {
    fn written(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            status: AntigravitySyncStatus::Written,
            reason: None,
        }
    }

    fn current(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            status: AntigravitySyncStatus::AlreadyCurrent,
            reason: None,
        }
    }

    fn skipped(path: &Path, reason: impl Into<String>) -> Self {
        Self {
            path: path.display().to_string(),
            status: AntigravitySyncStatus::Skipped,
            reason: Some(reason.into()),
        }
    }
}

/// Run the settings-file sync for an agent's STORED environment.
///
/// The panel's half of the launch-time call: same function, same rules, run at
/// save time so the answer can be shown while the user is still looking at the
/// form they just submitted.
pub fn sync_antigravity_settings_for_env(
    runtime_env: &BTreeMap<String, String>,
) -> AntigravitySyncReport {
    sync_antigravity_settings_file(runtime_env)
}

/// The environment a real Antigravity launch hands the agent process.
///
/// Factored out of [`build_agent`]'s `Binary` branch so the browser-free
/// sign-in flow ([`crate::acp::antigravity_login`]) can spawn the SAME binary
/// with the SAME environment. That identity is the whole point: the sign-in
/// child is the one that writes the OAuth token, and the token's location is
/// decided by `GEMINI_HOME` (via `paths.py`) and its storage backend by
/// `AGY_ACP_FORCE_FILE_STORAGE` — so a child launched with a different
/// environment would faithfully sign the user in and then leave the credential
/// somewhere no session ever reads.
///
/// Deliberately does NOT run [`sync_antigravity_settings_file`]: this returns a
/// value and that writes a file, and the sign-in path wants the report rather
/// than a silently dropped one. Callers run the sync themselves.
///
/// `scratch` is `None` for the settings panel, which builds this env to INSPECT
/// it and spawns nothing; the sign-in/sign-out path passes its own directory.
pub fn antigravity_launch_env(
    runtime_env: &BTreeMap<String, String>,
    scratch: Option<&Path>,
) -> Vec<(String, String)> {
    let registry_env: &[(&'static str, &'static str)] =
        match registry::get_agent_meta(AgentType::Antigravity).distribution {
            AgentDistribution::Binary { env, .. } => env,
            // Unreachable while the registry entry stays `Binary`; an empty
            // base is the correct answer for every other shape anyway, since
            // `runtime_env` carries everything the panel owns.
            _ => &[],
        };
    let mut merged = merge_agent_env(registry_env, runtime_env, scratch);
    apply_antigravity_env_policy(&mut merged, runtime_env);
    merged
}

/// The `auth.type` values Antigravity accepts, for callers that must validate a
/// method id before acting on it.
pub fn is_antigravity_auth_method(method_id: &str) -> bool {
    ANTIGRAVITY_AUTH_METHODS.contains(&method_id)
}

/// What codeg can say about the method the ACP server will authenticate with.
///
/// Three states, and [`Unreadable`](Self::Unreadable) is emphatically not a
/// flavor of [`Absent`](Self::Absent). The server parses Hjson and codeg only
/// strict JSON, so a file codeg cannot read is one the SERVER can — it names a
/// method, codeg just cannot see which. Collapsing the two would let a caller
/// treat "I have no idea" as "there is nothing there", which for the sign-out
/// means aiming `logout` at a flavor with nothing to clear and reporting the
/// `{}` it answers as a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AntigravityAuthType {
    /// The file names this method, in its canonical spelling.
    Declared(String),
    /// No file, or a file that names no method. The server has nothing to
    /// infer from either, and falls back to its own defaults.
    Absent,
    /// codeg could not read or parse it. The server still can.
    Unreadable,
}

/// The `auth.type` the ACP server will actually authenticate with, read from
/// the file the server reads it from.
///
/// Deliberately NOT the method in the stored row. The two normally agree —
/// every launch runs [`sync_antigravity_settings_file`] — but the file is the
/// only thing the server consults (`_infer_auth_state`), so it is also what
/// decides which flavor of credential a sign-out actually clears.
pub fn antigravity_effective_auth_type(
    runtime_env: &BTreeMap<String, String>,
) -> AntigravityAuthType {
    let Ok(acp_dir) = antigravity_acp_dir_for_env(runtime_env) else {
        // The directory itself cannot be named, so neither can the file.
        return AntigravityAuthType::Unreadable;
    };
    let parsed = match read_antigravity_settings(&acp_dir.join("settings.json")) {
        // `Ok(None)` is specifically "no such file", which IS positive
        // knowledge: there is no method there to find.
        Ok(None) => return AntigravityAuthType::Absent,
        Ok(Some(parsed)) => parsed,
        Err(_) => return AntigravityAuthType::Unreadable,
    };
    parsed
        .get("auth")
        .and_then(|auth| auth.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        // The server resolves the legacy spelling before it tests membership,
        // so a caller matching on canonical ids would otherwise miss it.
        .map(|value| AntigravityAuthType::Declared(
            canonical_antigravity_auth_method(value).to_string(),
        ))
        .unwrap_or(AntigravityAuthType::Absent)
}

/// The pre-rebrand `vertex-ai` spelling resolved to the id codeg uses.
fn canonical_antigravity_auth_method(method: &str) -> &str {
    match method {
        "vertex-ai" => "agent-platform",
        other => other,
    }
}

/// `<GEMINI_HOME>/antigravity-acp` for a launch carrying `runtime_env`, for
/// callers outside this module that need to name a file the agent keeps there
/// (its OAuth token, alongside the `settings.json` this module writes).
///
/// Same resolution, same `Err` contract as the private original: see
/// [`antigravity_acp_dir_for_env`].
pub fn antigravity_acp_dir_for_runtime_env(
    runtime_env: &BTreeMap<String, String>,
) -> Result<PathBuf, String> {
    antigravity_acp_dir_for_env(runtime_env)
}

/// Read `settings.json` for editing.
///
/// `Ok(None)` means "no file, safe to create". `Err` means "do not touch it" —
/// unreadable, not JSON codeg can parse, or not a JSON object.
fn read_antigravity_settings(path: &Path) -> Result<Option<serde_json::Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read it ({err})")),
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        format!("it is not strict JSON codeg can rewrite without losing content ({err})")
    })?;
    if !parsed.is_object() {
        return Err("it does not hold a JSON object".to_string());
    }
    Ok(Some(parsed))
}

/// Serialize over `path` through a temp file in the same directory, mirroring
/// the server's own writer: symlinks are resolved first (dotfile managers like
/// Stow and chezmoi symlink settings.json, and replacing the link with a
/// regular file would break their setup — it also keeps the temp file on the
/// same filesystem, without which the rename is not atomic).
fn write_antigravity_settings(
    acp_dir: &Path,
    path: &Path,
    value: &serde_json::Value,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target.parent().unwrap_or(acp_dir);
    std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;

    let temp = parent.join(format!(".settings.json.codeg-{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{body}\n")).map_err(|err| err.to_string())?;
    if let Err(err) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(err.to_string());
    }
    Ok(())
}

/// Merge the panel's choice into a parsed `settings.json`.
///
/// `Ok(None)` means the file already says exactly this, so the caller can skip
/// the write. `Err` means a block codeg would have to edit is not the shape it
/// expects — the same fail-closed rule the read side applies to the whole
/// document, and the same one the server's own `settings_writer` applies here
/// ("not editing %r because `auth` is not an object"). Replacing a non-object
/// `auth` with an object would delete whatever the user meant by it.
///
/// `existing` is `None` only for a file that does not exist — the caller
/// refuses to touch one it could not read or parse — so creating a fresh object
/// here never destroys anything.
fn merge_antigravity_settings(
    existing: Option<serde_json::Value>,
    method: &str,
    gcp_project: GcpField<'_>,
    gcp_location: GcpField<'_>,
) -> Result<Option<serde_json::Value>, String> {
    let mut root = match existing {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        _ => serde_json::json!({}),
    };
    let before = root.clone();

    {
        let obj = root
            .as_object_mut()
            .ok_or_else(|| "it does not hold a JSON object".to_string())?;

        // Absent is fine (create it); present-but-not-an-object is not.
        match obj.get("auth") {
            None | Some(serde_json::Value::Null) => {
                obj.insert("auth".into(), serde_json::json!({}));
            }
            Some(serde_json::Value::Object(_)) => {}
            Some(_) => return Err("`auth` is not an object".to_string()),
        }
        obj.get_mut("auth")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "`auth` is not an object".to_string())?
            .insert("type".into(), serde_json::Value::String(method.to_string()));

        // Only touch the `gcp` block when the panel has something to say about
        // it. `Keep` on both means it is not the panel's — a field the user
        // typed straight into the file, or a method that does not render the
        // inputs at all — so a strange `gcp` is left alone rather than blocking
        // an `auth.type` update that does not depend on it.
        //
        // `Clear` is the case the old two-state signature could not express,
        // and it has to be honored on a block that may not exist yet only in
        // the sense of "there is then nothing to remove": a clear NEVER creates
        // the block.
        let writes =
            matches!(gcp_project, GcpField::Set(_)) || matches!(gcp_location, GcpField::Set(_));
        let clears =
            matches!(gcp_project, GcpField::Clear) || matches!(gcp_location, GcpField::Clear);
        // A clear on a block that is not an object has nothing to remove, so it
        // must not be the thing that REFUSES the write: the same reasoning that
        // keeps a strange `gcp` from blocking an `auth.type` update when there
        // is nothing to say about it at all. Only a `Set` — which really would
        // have to replace that value — earns the refusal.
        let clearable = clears
            && obj
                .get("gcp")
                .is_some_and(serde_json::Value::is_object);
        if writes || clearable {
            match obj.get("gcp") {
                None | Some(serde_json::Value::Null) => {
                    obj.insert("gcp".into(), serde_json::json!({}));
                }
                Some(serde_json::Value::Object(_)) => {}
                Some(_) => return Err("`gcp` is not an object".to_string()),
            }
            let gcp = obj
                .get_mut("gcp")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| "`gcp` is not an object".to_string())?;
            for (name, field) in [("project", &gcp_project), ("location", &gcp_location)] {
                match field {
                    GcpField::Set(value) => {
                        gcp.insert(name.into(), serde_json::Value::String((*value).to_string()));
                    }
                    GcpField::Clear => {
                        gcp.remove(name);
                    }
                    GcpField::Keep => {}
                }
            }
            // An empty block left behind by a clear says nothing; drop it so
            // the file reads the way a fresh one would.
            if gcp.is_empty() {
                obj.remove("gcp");
            }
        }
    }

    Ok((root != before).then_some(root))
}

/// Codex-only launch policy: force codex-acp's MCP name-conflict de-duplication
/// OFF. codeg injects its companion server (`codeg-mcp`) over ACP
/// `session/new.mcpServers`; codex-acp otherwise drops any ACP-passed server
/// whose name collides with a `config.toml` entry — global *or* project layer
/// (the check was widened to project `.codex/config.toml` in codex-acp #322) —
/// silently stripping codeg-mcp and with it ask_user_question / delegation /
/// feedback / session_info. The late `retain` + `push` makes the override win
/// over any user `runtime_env` twin, so the injection is guaranteed to survive.
/// Codex launch env policy. `initial_agent_mode` is the preset resolved from
/// `~/.codex/config.toml` by `codex_launch_initial_agent_mode` — passed in rather
/// than read here so this stays pure (and so its tests don't depend on whatever
/// `~/.codex/config.toml` the developer happens to have).
fn apply_codex_env_policy(
    agent_type: AgentType,
    merged: &mut Vec<(String, String)>,
    initial_agent_mode: Option<&str>,
) {
    if agent_type != AgentType::Codex {
        return;
    }
    let key = "DISABLE_MCP_CONFIG_FILTERING";
    merged.retain(|(k, _)| k != key);
    merged.push((key.to_string(), "true".to_string()));

    // Make `~/.codex/config.toml`'s sandbox/approval choice mean something.
    // codex-acp re-sends its own approvalPolicy + sandboxPolicy every turn from
    // an `AgentMode` seeded once per session, so without this the user's config
    // is silently overridden by the adapter's default `agent` preset (#442).
    // Same shape as Grok's `--permission-mode` launch flag.
    //
    // A pre-existing value in the runtime env WINS: that is an explicit
    // user-set key, a stronger signal than a config-file inference. A
    // `preferred_mode_id` / `config_values["mode"]` still overrides this after
    // connect via `set_config_option` — explicit choice > config inference.
    let mode_key = "INITIAL_AGENT_MODE";
    if merged
        .iter()
        .any(|(k, v)| k == mode_key && !v.trim().is_empty())
    {
        return;
    }
    if let Some(mode) = initial_agent_mode {
        merged.retain(|(k, _)| k != mode_key);
        merged.push((mode_key.to_string(), mode.to_string()));
    }
}

/// Prepend `dir` to the PATH entry of `env`, seeding from `fallback_path` when
/// `env` has no PATH key of its own. Removes any pre-existing PATH key first
/// (case-insensitively when `windows`, since Windows env keys are
/// case-insensitive) so the result has exactly one PATH entry — otherwise a
/// differently-cased duplicate (e.g. an inherited `Path` plus an inserted
/// `PATH`) could clobber the injected value when the child `Command` applies
/// them. Pure (no env/fs access) so it is unit-tested for both platforms.
fn prepend_dir_to_path_env(
    env: &mut BTreeMap<String, String>,
    dir: &str,
    fallback_path: &str,
    windows: bool,
) {
    let sep = if windows { ';' } else { ':' };
    // Collect every PATH-ish key. `BTreeMap` iterates sorted, so when several
    // differently-cased keys exist (e.g. both `Path` and `PATH`), the last is
    // the one the child `Command` applies last — i.e. the effective value under
    // Windows' case-insensitive env. Remove all of them so exactly one PATH
    // entry remains; a stale duplicate could otherwise overwrite the injected
    // value when the child applies them in order.
    let matching: Vec<String> = env
        .keys()
        .filter(|k| {
            if windows {
                k.eq_ignore_ascii_case("PATH")
            } else {
                k.as_str() == "PATH"
            }
        })
        .cloned()
        .collect();
    let mut existing_val: Option<String> = None;
    for k in &matching {
        existing_val = env.remove(k);
    }
    let existing_val = existing_val.unwrap_or_else(|| fallback_path.to_string());
    let new_path = if existing_val.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}{sep}{existing_val}")
    };
    // Reuse the effective (last-sorted) key's casing when present; otherwise
    // default to the platform-conventional name (`Path` on Windows, `PATH` on Unix).
    let key = matching
        .into_iter()
        .next_back()
        .unwrap_or_else(|| if windows { "Path" } else { "PATH" }.to_string());
    env.insert(key, new_path);
}

/// Prepend an agent's own installer directories (see
/// [`registry::binary_system_dirs`]) to a merged env's PATH.
///
/// Operates on the merged `Vec` rather than inside [`merge_agent_env`] because
/// it is the one PATH contributor that depends on WHICH agent is launching,
/// and `merge_agent_env` is deliberately agent-agnostic (it is also called from
/// the settings panel, with no agent process in hand).
fn prepend_agent_install_dirs_path(env: &mut Vec<(String, String)>, agent_type: AgentType) {
    let dirs = registry::binary_system_dirs(agent_type);
    if dirs.is_empty() {
        return;
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let fallback = std::env::var("PATH").unwrap_or_default();
    let mut map: BTreeMap<String, String> = std::mem::take(env).into_iter().collect();
    // Reverse, because each pass prepends: walking `[a, b]` forwards would
    // leave `b` ahead of `a` and quietly invert the registry's declared
    // precedence the first time an agent lists two directories.
    for dir in dirs.iter().rev() {
        let joined = home.join(dir);
        if !joined.is_dir() {
            continue;
        }
        prepend_dir_to_path_env(
            &mut map,
            &joined.to_string_lossy(),
            &fallback,
            cfg!(windows),
        );
    }
    *env = map.into_iter().collect();
}

/// Prepend codeg's known OfficeCLI install dir to `env`'s PATH when officecli is
/// installed there but not yet on the live PATH (see
/// `office_tools::officecli_agent_path_dir`). Applied to both the agent process
/// env (`merge_agent_env`) and the ACP terminal runtime's base env, so an
/// agent-invoked `officecli` resolves whether the agent execs it directly or
/// runs it through the client `terminal/create` tool. PATH-only: never forwards
/// model/API secrets.
fn prepend_officecli_path(env: &mut BTreeMap<String, String>) {
    if let Some(dir) = crate::commands::office_tools::officecli_agent_path_dir() {
        let fallback = std::env::var("PATH").unwrap_or_default();
        prepend_dir_to_path_env(env, &dir.to_string_lossy(), &fallback, cfg!(windows));
    }
}

/// The two actions codex's bespoke `_codex/session/goal_control` request
/// accepts (codex-acp #293, v1.1.4). Start / resume / re-objective are NOT part
/// of this method — those go through the `/goal` prompt (a real slash command;
/// only `/plan`, a config-option state toggle, is suppressed). Serializes to the
/// lowercase wire value codex expects (`"pause"` / `"clear"`) and deserializes
/// from the same string coming off the tauri command / HTTP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalControlAction {
    Pause,
    Clear,
}

/// How the adapter disposed of a `_session/steering` message — the wire
/// `outcome` string, parsed by [`parse_steer_outcome`]. The distinction that
/// matters to callers is CONSUMPTION: `Injected` and `StartedNewTurn` mean the
/// adapter took the content (record it delivered, never resend);
/// `PromptRequired` means it did not (safe to resubmit as a normal prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// Pushed into the RUNNING turn's input — consumed.
    Injected,
    /// The turn settled first and the adapter honored the
    /// `idleBehavior = "promptRequired"` opt-in — NOT consumed, still
    /// host-owned. The caller reroutes it through `session/prompt`.
    PromptRequired,
    /// The adapter ignored the opt-in (pre-0.64 claude adapter, codex-acp)
    /// and spun up a detached turn no host request owns — consumed. The
    /// manager records it delivered, warns, and downgrades
    /// `native_steering_available` for the rest of the session.
    StartedNewTurn,
}

/// Commands sent from Tauri command handlers to the ACP connection loop.
pub enum ConnectionCommand {
    Prompt {
        blocks: Vec<PromptInputBlock>,
        /// Pre-projected cross-client user-message broadcast (`message_id` +
        /// user blocks), computed by the manager under the prompt lock. The
        /// loop emits it as `AcpEvent::UserMessage` right before issuing the
        /// agent request, so its seq strictly precedes the turn's assistant /
        /// status events (viewers apply in seq order) and it only fires for a
        /// prompt actually being processed. `None` for delegation children,
        /// empty prompts, unbound conversations, and non-linked senders.
        user_message: Option<(String, Vec<UserMessageBlock>)>,
    },
    SetMode {
        mode_id: String,
    },
    SetConfigOption {
        config_id: String,
        value_id: String,
    },
    GoalControl {
        action: GoalControlAction,
        /// Did the goal-control request LAND? Attached only by a caller that
        /// intends to follow a successful control with an interrupt
        /// (`ConnectionManager::goal_control`), because aborting the turn is
        /// destructive and must not happen on the strength of a request the
        /// agent rejected. `None` = fire-and-forget, nobody is listening.
        reply: Option<tokio::sync::oneshot::Sender<bool>>,
    },
    Cancel,
    RespondPermission {
        request_id: String,
        option_id: String,
    },
    Fork {
        /// Fork at a chosen message instead of the tail. `None` keeps the
        /// tail-fork the fork-send composer has always done; see
        /// [`crate::acp::fork::ForkPoint`] for how each agent resolves it.
        fork_point: Option<crate::acp::fork::ForkPoint>,
        reply:
            tokio::sync::oneshot::Sender<Result<crate::acp::types::ForkProtocolResult, AcpError>>,
    },
    /// Inject a live-feedback message into the RUNNING turn over the ACP
    /// `_session/steering` extension (native push channel — see
    /// `manager::submit_feedback`). Carries the same `PromptInputBlock`s a
    /// normal prompt does (text plus image attachments), mapped onto the wire
    /// with the same conversion, so a steered draft keeps its attachments.
    /// The loop does the protocol round-trip only and replies the parsed
    /// outcome; recording the note + the `FeedbackSubmitted` broadcast happen
    /// in the manager's cancellation-shielded task, mirroring Fork's
    /// protocol/persistence split. The idle arm replies `Err(NoActiveTurn)`
    /// so the oneshot can never hang.
    Steer {
        blocks: Vec<PromptInputBlock>,
        reply: tokio::sync::oneshot::Sender<Result<SteerOutcome, AcpError>>,
    },
    /// Stop one AIR async task (`_session/async_task/stop`; claude-agent-acp
    /// 0.73+, codex-acp 1.10+ — identical params and response on both).
    /// Handled in BOTH loops on purpose: background work is normally launched
    /// by — and outlives — a turn, so the user is as likely to reach for the
    /// stop button mid-turn as between turns.
    ///
    /// The reply is the adapter's own `stopped` flag, not "did the request
    /// succeed": it answers `false` for a task it will not stop (unknown,
    /// already terminal, or a stop already in flight). The visible result
    /// arrives on the normal channel either way — a successful stop is followed
    /// by an `async_task_state_update` (and, on codex, by the launching tool
    /// call finally settling `failed` with exit code -1) — so this is only what
    /// the caller needs to avoid claiming it stopped something it didn't.
    StopAsyncTask {
        task_id: String,
        reply: tokio::sync::oneshot::Sender<Result<bool, AcpError>>,
    },
    Disconnect,
}

/// Sentinel string embedded in a `agent_client_protocol::Error` when the Initialize
/// handshake times out. Converted back to `AcpError::InitializeTimeout`
/// by the outer `.map_err(...)` in `run_connection`.
const INIT_TIMEOUT_SENTINEL: &str = "__codeg_init_timeout__";

/// Sentinel appended to a `session/new` failure when codeg had just forwarded
/// MCP servers to a *custom* agent, so the outer `.map_err(...)` can raise
/// `AcpError::McpRejectedByAgent` and point the user at the `supports_mcp`
/// switch. Same trick as [`INIT_TIMEOUT_SENTINEL`] — the inner future is typed
/// to `agent_client_protocol::Error`, which has nowhere to carry a codeg error kind.
const MCP_SUSPECT_SENTINEL: &str = "__codeg_mcp_suspect__";

/// Sentinel appended to a `session/new` failure the agent answered with ACP's
/// `authRequired`, so the outer `.map_err(...)` can raise
/// `AcpError::AgentAuthRequired`. Same trick as [`INIT_TIMEOUT_SENTINEL`]: the
/// typed error code does not survive the `agent_client_protocol::Error` the inner future is
/// typed to, and this is the one classification that has to be made while it
/// is still there — the wire text alone is the agent's own wording, which for
/// cursor-agent names a command that does not exist.
const AUTH_REQUIRED_SENTINEL: &str = "__codeg_auth_required__";

/// Sentinel appended to a `session/new` or `session/load` failure that is pi-acp
/// reporting a pi too old for it ([`pi_runtime_is_outdated`]), so the outer
/// `.map_err(...)` can raise `AcpError::AgentRuntimeOutdated`. Same trick as
/// [`INIT_TIMEOUT_SENTINEL`].
const PI_RUNTIME_OUTDATED_SENTINEL: &str = "__codeg_pi_runtime_outdated__";

/// [`pi_runtime_is_outdated`]'s verdict, carried out of the typed inner future
/// on [`PI_RUNTIME_OUTDATED_SENTINEL`].
fn tag_pi_runtime_outdated(
    err: &agent_client_protocol::Error,
    agent_type: AgentType,
) -> Option<agent_client_protocol::Error> {
    if !pi_runtime_is_outdated(err, agent_type) {
        return None;
    }
    tracing::warn!("[ACP][{agent_type}] pi is too old for pi-acp: {err}");
    Some(agent_client_protocol::util::internal_error(format!(
        "{err}{PI_RUNTIME_OUTDATED_SENTINEL}"
    )))
}

/// Classify a `session/new` failure while its typed code is still readable.
///
/// `authRequired` and an outdated pi are checked first and return on their own:
/// each is a diagnosis, where the MCP tag below is only a hint, and running both
/// would leave a message carrying two markers and the weaker reading.
fn tag_new_session_failure(
    err: agent_client_protocol::Error,
    agent_type: AgentType,
    mcp_servers: &[McpServer],
) -> agent_client_protocol::Error {
    if let Some(tagged) = tag_pi_runtime_outdated(&err, agent_type) {
        return tagged;
    }
    if matches!(err.code, agent_client_protocol::schema::v1::ErrorCode::AuthRequired) {
        tracing::warn!("[ACP][{agent_type}] session/new refused with authRequired: {err}");
        return agent_client_protocol::util::internal_error(format!("{err}{AUTH_REQUIRED_SENTINEL}"));
    }
    tag_mcp_suspect(err, agent_type, mcp_servers)
}

/// Mark a `session/new` failure as possibly caused by the MCP servers codeg put
/// on the wire.
///
/// Restricted to custom agents on purpose. A built-in's `supports_mcp` is a
/// repository constant already verified against the real agent, so blaming MCP
/// there would send the user chasing a switch that isn't wrong (and that they
/// cannot flip). A custom agent's is a user declaration about a binary codeg
/// knows nothing about — the case this hint is for. An empty list rules MCP out
/// entirely, since then nothing was forwarded to reject.
fn tag_mcp_suspect(
    err: agent_client_protocol::Error,
    agent_type: AgentType,
    mcp_servers: &[McpServer],
) -> agent_client_protocol::Error {
    if mcp_servers.is_empty() || !matches!(agent_type, AgentType::Custom(_)) {
        return err;
    }
    // Also log it. The coded error reaches the desktop webview through the
    // global `acp://event` emit, but a web/remote client that has not attached
    // yet never sees it — the connection is removed from the manager map as
    // soon as this task unwinds, so there is no state left to snapshot. That
    // loss is not specific to this hint (it costs `initialize_timeout`,
    // `spawn_failed` and every protocol error just the same), and repairing it
    // means changing how long terminal connections are retained. Until then the
    // log is the one channel that survives on every transport.
    tracing::warn!(
        "[ACP][{}] session/new failed with {} MCP server(s) attached; if this agent \
         does not accept MCP, turn off \"MCP support\" for it in settings: {}",
        agent_type,
        mcp_servers.len(),
        err
    );
    agent_client_protocol::util::internal_error(format!("{err}{MCP_SUSPECT_SENTINEL}"))
}

/// RAII guard that removes the `AgentConnection` entry from the manager
/// map when dropped. Runs on both normal task exit AND task panic, so a
/// panic inside `run_connection` can't leak a stale map entry.
///
/// The `Mutex` is async, so we take two paths:
/// - If the lock is immediately available (`try_lock` succeeds), remove
///   the entry synchronously in the current context.
/// - Otherwise, spawn a short-lived cleanup task to acquire the lock
///   and remove the entry asynchronously. The guard must hold owned
///   `Arc<Mutex<_>>` and `String` so the spawned task has `'static`
///   captures.
struct ConnectionCleanupGuard {
    connections: Arc<tokio::sync::Mutex<HashMap<String, AgentConnection>>>,
    connection_id: String,
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.connections.try_lock() {
            guard.remove(&self.connection_id);
            return;
        }
        let connections = self.connections.clone();
        let connection_id = std::mem::take(&mut self.connection_id);
        tokio::spawn(async move {
            connections.lock().await.remove(&connection_id);
        });
    }
}

/// Represents a single active ACP agent connection.
pub struct AgentConnection {
    pub id: String,
    pub agent_type: AgentType,
    pub status: ConnectionStatus,
    pub owner_window_label: String,
    pub cmd_tx: mpsc::Sender<ConnectionCommand>,
    /// 后端权威的会话状态。所有 `emit_with_state` 写入此状态并自增 seq。
    /// 使用 `Arc<RwLock<_>>` 让 spawn 出的连接 task 与外部 snapshot 读取共享。
    pub state: Arc<RwLock<SessionState>>,
    /// 出口侧的事件发射器；管理器层（如 `send_prompt_linked`）需要直接发射
    /// `ConversationLinked` 等带 SessionState 写入的事件。
    pub emitter: EventEmitter,
    /// Serializes prompt sends per connection. Held across the
    /// link-check + DB write + emit + cmd_tx.send sequence so two
    /// concurrent prompts (multiple browser tabs of the same conversation,
    /// chat-channel + UI overlap) can't interleave and produce duplicate
    /// conversation rows or a confused agent that received two prompts
    /// in the same turn.
    pub prompt_lock: Arc<tokio::sync::Mutex<()>>,

    /// Canonical fingerprint of the agent's effective config (env vars + model
    /// provider creds + native config file content) captured at spawn. The
    /// running process is locked to THIS config; comparing it against a freshly
    /// recomputed fingerprint after a settings save tells us whether the session
    /// has drifted onto stale config. Immutable for the connection's lifetime.
    pub config_fingerprint: String,
    /// The most recent fingerprint seen by `refresh_connection_staleness`.
    /// Tracks "did anything change since we last looked" so a second settings
    /// save re-emits `SessionConfigStale` (re-showing a dismissed banner) while a
    /// no-op save (identical values) stays silent. Starts equal to
    /// `config_fingerprint`.
    pub last_observed_fingerprint: String,
    /// OS process id of the spawned agent subprocess, published by the
    /// `acp::agent_process` `on_spawn` callback. `0` until the process has
    /// launched (or if the pid was never observed). Used only as a shutdown
    /// backstop: `disconnect_all` kills this pid's whole process tree
    /// synchronously after the graceful-disconnect grace window, so agents
    /// (and their own child processes, e.g. MCP servers) never leak as orphans
    /// when the host process exits before `ChildGuard::drop` can run on the
    /// connection driver thread.
    ///
    /// Reset to `0` by the paired `on_exit` callback the moment the process is
    /// *reaped* — the only moment its pid stops naming our child and becomes
    /// reassignable. That reset is what keeps the backstop from ever aiming at
    /// a pid the OS has since handed to an unrelated process. Notably it does
    /// NOT fire merely because the connection ended: `ChildGuard::drop` signals
    /// the tree without waiting, so the agent may still be alive and still
    /// needs the backstop.
    pub child_pid: Arc<std::sync::atomic::AtomicU32>,
}

impl AgentConnection {
    pub fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            id: self.id.clone(),
            agent_type: self.agent_type,
            status: self.status.clone(),
        }
    }
}

/// Build an AcpAgent from registry metadata.
/// Directory handed to codex-acp via `APP_SERVER_LOGS` so its adapter-side
/// (ACP ↔ Codex app-server translation) logs land on disk for support.
///
/// Roots under the same `<cache>/app.codeg` tree as
/// [`binary_cache::cache_dir`] for consistency. Returns `None` — and the
/// caller injects nothing — when the system cache dir is unknown or the
/// directory can't be created: diagnostics must never block a connection.
fn codex_app_server_log_dir() -> Option<String> {
    let dir = dirs::cache_dir()?
        .join("app.codeg")
        .join("acp-logs")
        .join("codex-acp");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.to_string_lossy().into_owned())
}

/// Pi runs through pi-acp, which spawns the actual `pi` binary at runtime. If
/// `pi` (or the BYO-pi `PI_ACP_PI_COMMAND` override) isn't resolvable, pi-acp
/// dies mid-connection with a raw ENOENT. This preflight resolves the command
/// the way that spawn will — a bare name on the launch env's `PATH` (which may
/// carry the OfficeCLI prepend), a path against `cwd`, the session workspace pi
/// is started in — and returns a clear message when it can't be found; `None`
/// means launch may proceed.
///
/// Deliberately NOT a version check. The pinned pi-acp needs pi
/// [`registry::PI_MIN_RUNTIME_VERSION`], but asking `pi --version` would cost a
/// process spawn on every connect, and a version string only guesses at the
/// capability (a custom build may report anything, `0.0.0` included). Opening
/// the session reports the real incompatibility instead — see
/// [`pi_runtime_is_outdated`].
///
/// The message contains the literal substring "is not installed", which the
/// frontend matches to show the localized SDK-missing prompt with an "Open Agent
/// Settings" action (see `src/contexts/acp-connections-context.tsx`). Do not
/// change that substring.
fn pi_launch_preflight(launch_env: &BTreeMap<String, String>, cwd: &Path) -> Option<String> {
    let command = crate::commands::acp::pi_command_for_env(launch_env);
    if crate::commands::acp::resolve_pi_command_in(&command, launch_env, cwd).is_some() {
        return None;
    }
    Some(if command == "pi" {
        "Pi is not installed. Install it with: \
         npm install -g @earendil-works/pi-coding-agent \
         (or set a custom pi command in Agent Settings → Pi → Runtime)."
            .to_string()
    } else {
        format!(
            "Pi is not installed: the custom pi command \"{command}\" was not found. \
             Update it in Agent Settings → Pi → Runtime."
        )
    })
}

/// Whether a failed session open is pi-acp finding a pi too old for it.
///
/// pi-acp 0.0.34 asks pi for the current model's thinking levels
/// (`get_available_thinking_levels`, added in pi 0.81.0) while opening every
/// session, and fails the open when that fails. An older pi answers with
/// `Unknown command: get_available_thinking_levels` (its wording for any unknown
/// RPC), which pi-acp relays as `pi get_available_thinking_levels failed: …` —
/// in the error message on `session/new`, and in `data.details` on
/// `session/load`, where the ACP SDK wraps the plain `Error` it throws. The
/// error's `Display` covers both.
///
/// Matching the answer rather than predicting it from `pi --version` is what
/// keeps this exact: it is the one failure that proves the capability is
/// missing, whatever version a custom build reports.
fn pi_runtime_is_outdated(err: &agent_client_protocol::Error, agent_type: AgentType) -> bool {
    agent_type == AgentType::Pi
        && err
            .to_string()
            .contains("Unknown command: get_available_thinking_levels")
}

/// The explanation [`AcpError::AgentRuntimeOutdated`] carries for pi.
fn pi_runtime_outdated_message() -> String {
    format!(
        "Pi is too old for this version of Codeg: opening a session needs Pi {} or newer. \
         Update it with npm install -g @earendil-works/pi-coding-agent, or choose a newer \
         custom command in Agent Settings → Pi → Runtime.",
        registry::PI_MIN_RUNTIME_VERSION
    )
}

/// Transcript directory for an agent that codeg must record itself, or `None`
/// for agents with their own store parser.
///
/// Only custom ACP agents are recorded: every built-in has a dedicated parser
/// reading the agent's native transcript, and recording those too would double
/// the storage while risking two disagreeing histories.
fn transcript_dir_for(agent_type: AgentType) -> Option<&'static str> {
    agent_type
        .custom_id()
        .map(|_| registry::registry_id_for(agent_type))
}

/// Ensure a custom agent's transcript file exists with its header. No-op for
/// built-ins, and idempotent per session (a reconnect keeps the original
/// header, so the session's original cwd/start time survive).
fn record_transcript_header(agent_type: AgentType, session_id: &str, cwd: &str) {
    drop(queue_transcript_header(agent_type, session_id, cwd, None));
}

/// [`record_transcript_header`] for a session that carries an existing
/// conversation forward.
///
/// `continues_from` is set when `session/load` failed and codeg opened a fresh
/// agent session for the same conversation: the earlier turns stay where they
/// are and this header links back to them, so the reader still sees one
/// history. See [`crate::acp_transcript::TranscriptHeader::continues_from`].
async fn record_transcript_header_continuing(
    agent_type: AgentType,
    session_id: &str,
    cwd: &str,
    continues_from: Option<&str>,
) {
    let Some(ack) = queue_transcript_header(agent_type, session_id, cwd, continues_from) else {
        return;
    };
    // Wait for the link to be DURABLE before returning, so it is on disk before
    // the caller emits `SessionStarted`. Same shape and same reason as
    // `record_prompt` below: the writer is a background thread, and readers go
    // to the file.
    //
    // The specific reader here is `acp::continued_session_ids`, which the
    // session-binding guard consults to tell "this conversation continues under
    // a new agent session" from "an unrelated session landed on this row". Emit
    // first and a subscriber can read an empty chain, conclude the sessions are
    // unrelated, and split the conversation in two — and nothing removes that
    // row once the header lands, so the duplicate is permanent.
    //
    // Costs one disk write per continuation, i.e. per restart of a conversation
    // whose agent forgot it. `record_prompt` accepts the same cost per TURN.
    // A stalled writer falls back to the old racy behaviour after the timeout
    // rather than blocking the session: the failure mode there is a duplicate
    // row, never lost history.
    if continues_from.is_some() {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
    }
}

/// Build and enqueue the header write. `None` for agents with their own store
/// (nothing is recorded); otherwise the writer's completion ack.
fn queue_transcript_header(
    agent_type: AgentType,
    session_id: &str,
    cwd: &str,
    continues_from: Option<&str>,
) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let dir = transcript_dir_for(agent_type)?;
    let mut header = crate::acp_transcript::TranscriptHeader::new(
        &agent_type.as_wire(),
        session_id,
        cwd,
        crate::acp_transcript::now_epoch_ms(),
    );
    if let Some(previous) = continues_from.filter(|p| !p.is_empty() && *p != session_id) {
        header = header.continuing(previous);
    }
    Some(crate::acp_transcript::record_header(dir, &header))
}

/// Record an outgoing prompt for a custom agent, and wait (briefly) for it to
/// land. No-op for agents with their own store.
///
/// Bound-waited like [`record_turn_end`], but for a sharper reason. The gate
/// that decides whether a later `session/load` replay may be recorded is
/// `acp_transcript::has_entries`, and it reads the FILE — a queued prompt is
/// invisible to it. Returning before the prompt is durable therefore leaves a
/// window in which a reconnect concludes "this conversation has no transcript",
/// records the agent's replay, and ends up with two copies of the same history.
///
/// The window is small but reachable (the writer can be behind on a slow disk,
/// and a conversation can be torn down between its first prompt and its turn
/// end, which is the other place codeg waits). A prompt happens once per turn,
/// so closing it costs one disk write per turn — nothing the user can perceive,
/// against a failure that is permanent and silent.
async fn record_prompt(agent_type: AgentType, session_id: &str, blocks: &[ContentBlock]) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let Ok(payload) = serde_json::to_value(blocks) else {
        return;
    };
    let ack = crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::Prompt,
        payload,
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
}

/// Record a message delivered into a custom agent's running turn (a native
/// steer), and wait (briefly) for it to land. No-op for agents with their own
/// store.
///
/// The adapter keeps a steer to itself — no `user_message_chunk` echoes it — so
/// codeg's transcript, which IS a custom agent's history, would otherwise never
/// hold it: the reply to the steer would glue onto the reply before it, and the
/// message would vanish once the conversation is reopened. Recorded as the
/// prompt it is, marked so the reader knows the turn did not start there.
async fn record_steer(agent_type: AgentType, session_id: &str, blocks: &[ContentBlock]) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let Ok(payload) = serde_json::to_value(blocks) else {
        return;
    };
    let ack = crate::acp_transcript::record_steer(dir, session_id, payload);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
}

/// Record a turn's completion for a custom agent, and wait (briefly) for it to
/// land. No-op for agents with their own store.
///
/// The bounded wait exists because the frontend refetches conversation detail
/// right after `TurnComplete`; without it, a reopened conversation could be
/// read before the final lines were flushed. The bound means a stalled writer
/// delays nothing more than this.
async fn record_turn_end(
    agent_type: AgentType,
    session_id: &str,
    stop_reason: &str,
    started_at_ms: u64,
    model: Option<String>,
) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let now = crate::acp_transcript::now_epoch_ms();
    let mut payload = serde_json::json!({
        "stopReason": stop_reason,
        "durationMs": now.saturating_sub(started_at_ms),
    });
    // ACP puts no model on the prompt response, so the session's model selector
    // is the only honest answer at turn end — and it is the same value the
    // composer showed while the turn ran. Recorded per turn rather than once in
    // the header because a mid-conversation model switch must not retroactively
    // relabel the turns that ran before it.
    if let (Some(obj), Some(model)) = (payload.as_object_mut(), model.filter(|m| !m.is_empty())) {
        obj.insert("model".to_string(), serde_json::Value::String(model));
    }
    let ack = crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::TurnEnd,
        payload,
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
}

/// The model id a session's selectors currently report. Agent-agnostic: the
/// ACP `category: "model"` selector is the one channel every agent that has a
/// model at all publishes it on. `None` when the agent exposes no model
/// selector — most custom agents don't, and a fabricated label would be worse
/// than an empty field.
/// The value every advertised config option currently holds, keyed by option
/// id, in exactly the shape [`apply_preferred_session_options`] consumes for
/// `preferred_config_values` (a select's value id; `"true"`/`"false"` for a
/// boolean — see `config_option_already_holds`).
///
/// Used to carry a session's selectors across a fork.
fn current_config_option_values(
    opts: &[SessionConfigOptionInfo],
) -> BTreeMap<String, String> {
    opts.iter()
        .map(|opt| {
            let value = match &opt.kind {
                SessionConfigKindInfo::Select(sel) => sel.current_value.clone(),
                SessionConfigKindInfo::Boolean(b) => b.current_value.to_string(),
            };
            (opt.id.clone(), value)
        })
        .collect()
}

fn current_model_id_from_opts(opts: &[SessionConfigOptionInfo]) -> Option<String> {
    opts.iter()
        .find(|o| o.category.as_deref() == Some("model"))
        .and_then(|o| {
            // A model selector is always a `select`; any other kind carries no
            // model id to report.
            let SessionConfigKindInfo::Select(sel) = &o.kind else {
                return None;
            };
            Some(sel.current_value.clone())
        })
        .filter(|m| !m.is_empty())
}

/// [`current_model_id_from_opts`] against the authoritative `SessionState`
/// snapshot.
async fn current_session_model_id(state: &Arc<RwLock<SessionState>>) -> Option<String> {
    let opts = state.read().await.config_options.clone()?;
    current_model_id_from_opts(&opts)
}

/// Queue one raw `session/update` for a custom agent, handing back the ack so
/// the caller decides whether landing it matters.
///
/// `None` when nothing was queued: not a custom agent, an update the history
/// projection never reads back (see
/// [`crate::parsers::acp_native::is_recorded_update`], which owns that call so
/// the filter cannot drift from the reader it exists to serve), or an
/// unserializable payload.
fn queue_transcript_update(
    agent_type: AgentType,
    session_id: &str,
    update: &SessionUpdate,
) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let dir = transcript_dir_for(agent_type)?;
    if !crate::parsers::acp_native::is_recorded_update(update) {
        return None;
    }
    let payload = serde_json::to_value(update).ok()?;
    Some(crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::Update,
        payload,
    ))
}

/// Record one raw `session/update` for a custom agent, fire and forget.
///
/// The ack is dropped: streamed chunks must never make the live read loop wait.
/// Turn boundaries are the only place the live path bound-waits.
fn record_transcript_update(agent_type: AgentType, session_id: &str, update: &SessionUpdate) {
    drop(queue_transcript_update(agent_type, session_id, update));
}

/// How long one hydrated line may take to land before hydration gives up on
/// recording. Only a wedged filesystem can reach it (a line costs tens of
/// microseconds), so it is not a throughput bound — it is the difference
/// between "the conversation opens with a truncated history and a warning" and
/// "opening the conversation hangs forever".
const HYDRATION_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// [`record_transcript_update`] **with backpressure**, for the `session/load`
/// hydration drain. Returns false once the writer has stopped keeping up, after
/// which the caller must stop recording.
///
/// The live path can afford to drop the ack because a lost chunk costs one
/// chunk. Hydration cannot: the replay it is draining is the ONLY copy of that
/// history, and it arrives as fast as it parses while the writer runs at disk
/// speed. Fire-and-forget there fills the bounded queue and then discards from
/// the MIDDLE of the history — silently, leaving a transcript with holes that
/// the `has_entries` gate will never let a later replay repair.
///
/// Awaiting each ack is async-native backpressure (no worker thread is blocked,
/// and one outstanding line cannot overflow a queue of thousands), and it turns
/// the pathological case from "history with random holes" into "history that
/// stops cleanly at a point" — which is what a prefix-honest reader can work
/// with.
async fn record_hydrated_update(
    agent_type: AgentType,
    session_id: &str,
    update: &SessionUpdate,
) -> bool {
    let Some(ack) = queue_transcript_update(agent_type, session_id, update) else {
        return true;
    };
    match tokio::time::timeout(HYDRATION_ACK_TIMEOUT, ack).await {
        // `Err(RecvError)` means the writer thread is gone; there is nothing
        // left to wait for and nothing more will land either.
        Ok(res) => res.is_ok(),
        Err(_) => {
            tracing::warn!(
                "[ACP] transcript writer stalled while hydrating {session_id}; \
                 stopping recording so the replay lands as a clean prefix"
            );
            false
        }
    }
}

/// Build the `with_debug` callback for a spawned agent.
///
/// stderr does double duty: it is tee'd to `tracing::debug!` (historical
/// behavior, invisible at the default `Info` level) AND pushed into
/// `stderr_tail`, an in-memory ring buffer that survives regardless of the log
/// level. The buffer is what a silent-`EndTurn` diagnosis reads from — see
/// [`crate::acp::stderr_tail`]. stdin/stdout are NEVER buffered: they carry
/// JSON-RPC traffic including prompt text and file contents.
fn agent_debug_callback(
    agent_name: String,
    stderr_tail: Arc<StderrTail>,
    stdio_debug_enabled: bool,
) -> impl Fn(&str, agent_client_protocol::LineDirection) + Send + Sync + 'static {
    move |line, dir| {
        let (tag, enabled) = match dir {
            agent_client_protocol::LineDirection::Stderr => {
                stderr_tail.push(line);
                ("stderr", true)
            }
            agent_client_protocol::LineDirection::Stdout => ("stdout", stdio_debug_enabled),
            agent_client_protocol::LineDirection::Stdin => ("stdin", stdio_debug_enabled),
        };
        if !enabled {
            return;
        }
        const MAX: usize = 256;
        if line.len() > MAX {
            let head = line
                .char_indices()
                .take_while(|(i, _)| *i < MAX)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(MAX);
            tracing::debug!(
                "[ACP][{agent_name}][{tag}] {}... <truncated {} bytes>",
                &line[..head],
                line.len() - head
            );
        } else {
            tracing::debug!("[ACP][{agent_name}][{tag}] {line}");
        }
    }
}

async fn build_agent(
    agent_type: AgentType,
    runtime_env: &BTreeMap<String, String>,
    cwd: &Path,
    stderr_tail: &Arc<StderrTail>,
    scratch: Option<&Path>,
) -> Result<AcpAgent, AcpError> {
    // A conversation can outlive the custom-agent definition it was started
    // with (the user deleted it in settings). `get_agent_meta` cannot report
    // that — it is infallible — so it hands back a placeholder with an empty
    // command. Catch it here, before we try to spawn nothing and surface an
    // opaque ENOENT.
    if let Some(id) = agent_type.custom_id() {
        if !crate::acp::custom_registry::is_registered(id) {
            return Err(AcpError::SdkNotInstalled(format!(
                "The custom agent \"{id}\" is no longer registered. Re-add it in Settings → Agents to use this conversation."
            )));
        }
    }
    let meta = registry::get_agent_meta(agent_type);
    debug_assert_eq!(meta.agent_type, agent_type);

    let agent = match meta.distribution {
        AgentDistribution::Npx { cmd, args, env, .. } => {
            let mut merged_env = merge_agent_env(env, runtime_env, scratch);
            // pi-acp spawns the real `pi` binary; fail fast with a clear,
            // install-prompt-routable error if it (or a BYO-pi override) isn't
            // resolvable, rather than letting pi-acp die mid-connection on a raw
            // ENOENT that surfaces as an opaque protocol error.
            if agent_type == AgentType::Pi {
                let launch_env: BTreeMap<String, String> = merged_env.iter().cloned().collect();
                if let Some(message) = pi_launch_preflight(&launch_env, cwd) {
                    return Err(AcpError::SdkNotInstalled(message));
                }
                // NOTE: codeg deliberately does NOT touch pi's `trust.json` here.
                // It used to mark this workspace trusted on every launch, which
                // made pi load the repo's own `.pi/*` — including `.pi/extensions`,
                // JS/TS modules whose top level executes at pi startup with the
                // user's permissions, before any prompt is sent. Because pi-acp
                // spawns `pi --mode rpc` (no UI), pi's own default is to skip those
                // resources, so the seeding was the sole reason they ran. Trust is
                // now an explicit per-workspace decision surfaced in the UI
                // (`acp_pi_set_project_trust`).
                //
                // Dropping the seeding does NOT retract the grants it already
                // wrote: they persist in pi's user-level store and are inherited
                // by every subdirectory, so a repo cloned into a folder some
                // earlier session trusted is trusted before it is ever opened.
                // Since pi resolves trust at startup and runs extensions
                // immediately, an unconfirmed grant has to stop the launch here —
                // a notice shown once the connection is up comes after the code it
                // warns about has already run.
                if let Some(message) =
                    crate::commands::acp::pi_project_trust_launch_block(cwd, runtime_env)
                {
                    return Err(AcpError::PiProjectTrustRequired(message));
                }
            }
            // Resolve the config-derived preset HERE (like Grok's
            // `grok_launch_permission_mode` below) so the policy helper stays a
            // pure function over the env list.
            let codex_initial_mode = if agent_type == AgentType::Codex {
                crate::commands::acp::codex_launch_initial_agent_mode()
            } else {
                None
            };
            apply_codex_env_policy(agent_type, &mut merged_env, codex_initial_mode.as_deref());
            // codex-acp 1.0.0 honors APP_SERVER_LOGS as a directory for its
            // adapter-side logs. Surface it only under CODEG_ACP_DEBUG so
            // default runs are unchanged; a directory-creation failure silently
            // skips injection (diagnostics must never block a connect).
            let want_codex_logs = agent_type == AgentType::Codex
                && std::env::var("CODEG_ACP_DEBUG")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
            if want_codex_logs {
                if let Some(dir) = codex_app_server_log_dir() {
                    merged_env.push(("APP_SERVER_LOGS".to_string(), dir));
                }
            }
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in &merged_env {
                parts.push(format!("{k}={v}"));
            }
            parts.push(
                crate::commands::acp::resolve_npx_command(cmd)
                    .await
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| {
                        crate::process::normalized_program(cmd)
                            .to_string_lossy()
                            .to_string()
                    }),
            );
            // Grok's root-level launch flags go BEFORE its `agent stdio`
            // subcommand (which rejects them):
            //  - `--no-auto-update`: codeg owns the pinned version, so suppress the
            //    CLI's background self-update (it would drift off the pin and can
            //    break the ACP contract). Config twin: `[cli].auto_update = false`.
            //  - `--permission-mode <value>`: grok's real permission enum
            //    (default/acceptEdits/auto/dontAsk/bypassPermissions/plan), read
            //    from the Grok panel's `[ui].permission_mode`. Only passed for a
            //    non-`default` mode; `default`/unset leaves it off so ACP
            //    permission requests still reach codeg's UI. (Grok exposes no ACP
            //    `modes` channel for permission — verified against 0.2.99 — so this
            //    launch flag, not a live `session/set_mode`, is the control point.)
            if agent_type == AgentType::Grok {
                parts.push("--no-auto-update".into());
                if let Some(mode) = crate::commands::acp::grok_launch_permission_mode() {
                    parts.push("--permission-mode".into());
                    parts.push(mode);
                }
            }
            for a in args {
                parts.push((*a).into());
            }
            // Translate OpenClaw-specific env vars to CLI flags
            if agent_type == AgentType::OpenClaw {
                if let Some(url) = runtime_env
                    .get("OPENCLAW_GATEWAY_URL")
                    .filter(|v| !v.is_empty())
                {
                    parts.push("--url".into());
                    parts.push(url.clone());
                }
                if let Some(key) = runtime_env
                    .get("OPENCLAW_SESSION_KEY")
                    .filter(|v| !v.is_empty())
                {
                    parts.push("--session".into());
                    parts.push(key.clone());
                }
                // When creating a new conversation (no session_id to resume),
                // pass --reset-session so OpenClaw mints a fresh transcript
                // instead of appending to the previous one.
                if runtime_env
                    .get("OPENCLAW_RESET_SESSION")
                    .is_some_and(|v| v == "1")
                {
                    parts.push("--reset-session".into());
                }
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            let agent_name = meta.name.to_string();
            let tail = Arc::clone(stderr_tail);
            AcpAgent::from_args(&refs)
                .map(|a| {
                    // `false`: this branch never logged stdin/stdout, and the
                    // shared callback must not quietly widen that. Only the
                    // Binary branch opts into the CODEG_ACP_DEBUG stdio dump.
                    a.with_debug(agent_debug_callback(agent_name, tail, false))
                })
                .map_err(|e| AcpError::SpawnFailed(e.to_string()))
        }
        AgentDistribution::Binary {
            version: registry_version,
            cmd,
            args,
            env,
            platforms,
            ..
        } => {
            let platform = registry::current_platform();
            let _ = platforms
                .iter()
                .find(|p| p.platform == platform)
                .ok_or_else(|| {
                    AcpError::PlatformNotSupported(format!(
                        "{} is not available on {platform}",
                        meta.name
                    ))
                })?;

            // Session-page connect must never trigger a download. Use
            // the best cached version available (tolerates users on
            // older-but-still-working binaries); return SdkNotInstalled
            // only when nothing is cached, so the frontend can prompt
            // the user to install it from the Agent Settings page.
            //
            // With nothing cached, every binary agent falls back to a
            // user-installed CLI on PATH (e.g. `cursor-agent` from the
            // official install script, a brew `opencode`, or the user's
            // own install of a custom tool) before giving up — mirroring
            // the Uvx `system_cmd` fallback.
            //
            // INVARIANT: the substring "is not installed" is matched
            // verbatim by the frontend catch block in
            // `src/contexts/acp-connections-context.tsx` to surface a
            // localized install prompt. Do not change the wording.
            let cached =
                crate::acp::binary_cache::find_best_cached_binary_for_agent(agent_type, cmd)?;
            let (binary_path, cached_version) = match cached {
                Some((path, cached_version)) => {
                    if cached_version == registry_version {
                        tracing::info!(
                            "[ACP][{}] Using cached binary {cached_version}",
                            meta.name
                        );
                    } else {
                        tracing::info!(
                            "[ACP][{}] Using cached binary {cached_version} (registry recommends {registry_version})",
                            meta.name
                        );
                    }
                    (path, Some(cached_version))
                }
                None => {
                    let system =
                        crate::commands::acp::resolve_system_agent_binary_for(agent_type, cmd)
                        .ok_or_else(|| {
                            AcpError::SdkNotInstalled(format!(
                                "{} is not installed. Please install it in Agent Settings.",
                                meta.name
                            ))
                        })?;
                    tracing::info!(
                        "[ACP][{}] No cached binary; using system {} from PATH",
                        meta.name,
                        system.display()
                    );
                    (system, None)
                }
            };

            let binary_str = binary_path.to_string_lossy().to_string();
            let binary_size = std::fs::metadata(&binary_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let mut server = McpServerStdio::new(meta.name, &binary_str);
            let mut cmd_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            // Cursor's ROOT-level `--model <id>` flag precedes the `acp`
            // subcommand and sets the session's default model. Sourced from
            // the Cursor panel's default-model control (env_json key
            // CURSOR_MODEL — a codeg-side launch knob; the CLI itself reads
            // no model env var).
            if agent_type == AgentType::Cursor {
                if let Some(model) = runtime_env
                    .get("CURSOR_MODEL")
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                {
                    cmd_args.insert(0, "--model".to_string());
                    cmd_args.insert(1, model.to_string());
                }
                // Root `--force` = Run Everything: the ACP session swaps its
                // permission prompter for an auto-allow one, so tool calls
                // never reach session/request_permission (deny rules still
                // apply, and an org policy can downgrade it to rule-based
                // approval). Sourced from the panel's permission-mode
                // control (env_json key CURSOR_FORCE — codeg-side knob; the
                // CLI reads no such env var). Unset means Ask; see
                // `cursor_force_enabled`.
                if cursor_force_enabled(runtime_env.get("CURSOR_FORCE").map(String::as_str)) {
                    cmd_args.insert(0, "--force".to_string());
                }
            }
            // `opencode acp` embeds an HTTP server that otherwise takes its
            // port and host from opencode's GLOBAL config (`server.*`, meant
            // for `opencode serve`); a fixed port there fails every concurrent
            // session with `ServeError` (#860). The subcommand's own defaults,
            // passed explicitly, outrank that config — for builds that know
            // the flags (see `opencode_launch`).
            if agent_type == AgentType::OpenCode {
                let listen = crate::acp::opencode_launch::listen_args(
                    &binary_path,
                    cached_version.as_deref(),
                )
                .await;
                cmd_args.extend(listen.iter().map(|a| (*a).to_string()));
            }
            let cmd_args_for_log = cmd_args.clone();
            if !cmd_args.is_empty() {
                server = server.args(cmd_args);
            }
            let mut merged_env = merge_agent_env(env, runtime_env, scratch);
            // codeg launches the binary by absolute path, so this is not about
            // finding it — it is about the agent finding ITSELF. An agent that
            // re-execs its own CLI for a subtask looks it up on PATH, and a
            // desktop launch never inherits the shell rc line the vendor's
            // installer appended.
            prepend_agent_install_dirs_path(&mut merged_env, agent_type);
            if agent_type == AgentType::Cursor {
                apply_cursor_env_policy(&mut merged_env, runtime_env);
            } else if agent_type == AgentType::Grok {
                apply_grok_env_policy(&mut merged_env, runtime_env);
            } else if agent_type == AgentType::Antigravity {
                apply_antigravity_env_policy(&mut merged_env, runtime_env);
                // Kept separate from the env policy above: the other
                // `apply_*_env_policy`s are pure, and this one WRITES the
                // server's settings.json. It has to happen before the spawn —
                // the file is read during `session/new`, and without it that
                // call fails with `Authentication required`.
                //
                // The report is for the settings panel, which runs the same
                // sync at save time; here it is already in the log and must
                // never block a launch, so it is deliberately dropped.
                let _ = sync_antigravity_settings_file(runtime_env);
            }
            let env_key_list: Vec<&str> = merged_env.iter().map(|(k, _)| k.as_str()).collect();
            if !merged_env.is_empty() {
                let env_vars: Vec<agent_client_protocol::schema::v1::EnvVariable> = merged_env
                    .iter()
                    .map(|(k, v)| agent_client_protocol::schema::v1::EnvVariable::new(k, v))
                    .collect();
                server = server.env(env_vars);
            }
            // Spawn-time diagnostic dump: binary identity, args, and env
            // key list (values omitted — they may contain API keys). If
            // the connection hangs later, these lines pin down exactly
            // which binary was invoked and how.
            tracing::info!(
                "[ACP][{}] binary_path={} size={} platform={} args={:?} env_keys={:?}",
                meta.name,
                binary_str,
                binary_size,
                registry::current_platform(),
                cmd_args_for_log,
                env_key_list
            );

            // Stdio logging policy:
            // - stderr is always on: it's the agent's own diagnostic
            //   output (ANSI log lines) and does not contain user data.
            // - stdin / stdout carry JSON-RPC traffic that includes
            //   prompt text, tool-call arguments, file read/write
            //   contents, and permission-response payloads — all of
            //   which may contain API keys pasted by users or file
            //   contents the agent is editing. They are gated behind
            //   the `CODEG_ACP_DEBUG=1` env var so production builds
            //   don't persist user content into OS-level log files
            //   (Console.app on macOS, journald on Linux).
            // - Max line length is kept short so what does get logged
            //   captures the JSON-RPC envelope (method, id) rather
            //   than large payload bodies.
            let stdio_debug_enabled = std::env::var("CODEG_ACP_DEBUG")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let agent_name = meta.name.to_string();
            let tail = Arc::clone(stderr_tail);
            Ok(
                AcpAgent::new(agent_client_protocol::schema::v1::McpServer::Stdio(server)).with_debug(
                    agent_debug_callback(agent_name, tail, stdio_debug_enabled),
                ),
            )
        }
        AgentDistribution::Uvx {
            package,
            cmd,
            args,
            env,
            python,
            system_cmd,
            ..
        } => {
            let merged_env = merge_agent_env(env, runtime_env, scratch);
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in &merged_env {
                parts.push(format!("{k}={v}"));
            }
            if let Some(uvx_path) = crate::commands::acp::resolve_uvx_command() {
                // Primary: `uvx [--python <ver>] --from <pinned package> <entry
                // script>`. uvx fetches + caches the pinned package on first use;
                // the `--python` pin keeps it on an interpreter the agent
                // supports (see the registry `python` field).
                parts.push(uvx_path.to_string_lossy().to_string());
                parts.extend(crate::commands::acp::uvx_python_args(python));
                parts.push("--from".into());
                parts.push(package.to_string());
                parts.push(cmd.to_string());
                for a in args {
                    parts.push((*a).into());
                }
            } else if let Some((sys_path, sys_args)) = system_cmd.and_then(|(c, a)| {
                crate::commands::acp::resolve_command_on_path(c).map(|path| (path, a))
            }) {
                // Fallback: the agent's own CLI is already on PATH, installed
                // via pipx / `uv tool install` / an official installer rather
                // than provisioned through uvx.
                tracing::warn!(
                    "[ACP][{}] uvx unavailable; falling back to system command {:?}",
                    meta.name, sys_path
                );
                // `system_cmd` is a complete launch recipe for the PATH binary;
                // the uvx entry-script `args` don't necessarily apply to it.
                parts.push(sys_path.to_string_lossy().to_string());
                for a in sys_args {
                    parts.push((*a).into());
                }
            } else {
                // INVARIANT: the substring "is not installed" is matched
                // verbatim by the frontend catch block in
                // `src/contexts/acp-connections-context.tsx` to surface a
                // localized install prompt. Do not change the wording.
                return Err(AcpError::SdkNotInstalled(format!(
                    "{} is not installed. Please install it in Agent Settings.",
                    meta.name
                )));
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            let agent_name = meta.name.to_string();
            let tail = Arc::clone(stderr_tail);
            AcpAgent::from_args(&refs)
                .map(|a| {
                    // `false` for the same reason as the Npx branch above.
                    a.with_debug(agent_debug_callback(agent_name, tail, false))
                })
                .map_err(|e| AcpError::SpawnFailed(e.to_string()))
        }
    }?;

    // Run the agent subprocess in the session's working directory rather than
    // codeg's own process cwd (a desktop app launched from the Dock often
    // inherits "/"). A coding agent belongs in its project root. This is
    // required for Hermes, whose local terminal backend force-exports
    // TERMINAL_CWD = os.getcwd() at import (clobbering any inherited value)
    // and reports that as the agent's "Current working directory" in its
    // system prompt — without pinning it would believe it lives in "/". For
    // agents that already use the ACP session/new cwd this is a harmless
    // alignment (process cwd == session cwd). Guard on an existing directory
    // so a not-yet-created working_dir (e.g. a worktree path) can't make the
    // spawn fail.
    Ok(if cwd.is_dir() {
        agent.with_current_dir(cwd)
    } else {
        agent
    })
}

/// Stack size for the dedicated OS thread that drives each ACP connection's
/// `run_connection` future (see the spawn site below). `run_connection` is one
/// colossal async state machine — the full per-connection message loop plus
/// every registered client-request handler — and in DEBUG builds the compiler
/// does not pack async locals, so a single poll of the connection closure needs
/// far more than a default ~2 MiB Tokio worker-thread stack. Left on the worker
/// pool it overflowed the stack and aborted the whole process under `tauri dev`
/// (release builds pack the frame small enough to fit, which is why only debug
/// crashed). 8 MiB matches the macOS main-thread stack — 4x the default that
/// overflowed, generous headroom for the debug frame as ACP features accrete.
/// The stack is reserved address space, lazily committed, so it costs no
/// physical memory beyond the pages actually touched (~the real 2-3 MiB the
/// frame uses), regardless of this cap — so the larger reservation is free. If a
/// future ACP feature ever grows the loop past even this, split `run_connection`
/// into boxed sub-futures rather than raising it further.
const ACP_CONNECTION_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Spawn an ACP agent process and run the connection loop in a background task.
///
/// On success, the newly created `AgentConnection` is inserted into
/// `connections` before this function returns. The background task
/// automatically removes the entry from `connections` once `run_connection`
/// exits (timeout, error, or clean disconnect), so the manager never
/// leaks stale entries after a connection tears down.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_agent_connection(
    connection_id: String,
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
    runtime_env: BTreeMap<String, String>,
    owner_window_label: String,
    emitter: EventEmitter,
    connections: Arc<tokio::sync::Mutex<HashMap<String, AgentConnection>>>,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    terminal_shell_config: TerminalShellRuntimeConfig,
) -> Result<tokio::sync::oneshot::Receiver<()>, AcpError> {
    // Create the authoritative session state up front. Subsequent emit_with_state
    // calls write through this state and increment its seq counter so the first
    // event the frontend sees has seq=1, not the placeholder 0 from Phase 0.
    let mut initial_state = SessionState::new(
        connection_id.clone(),
        agent_type,
        working_dir.clone().map(PathBuf::from),
        owner_window_label.clone(),
        None, // folder_id 由后续 prompt handler 在首次 send 时绑定 (Phase 2)
    );

    // Install the SessionStarted dedup signal BEFORE wrapping into Arc so the
    // first event (StatusChanged{Connecting} below) doesn't race with the
    // installer. The receiver is returned to `spawn_agent`, which holds the
    // per-session dedup lock until this rx fires (or times out / aborts).
    let session_started_rx = initial_state.install_session_started_signal();
    // Record what this launch's environment freezes before the state is shared:
    // the emit path and the preference replay both read it, and both run after
    // this point.
    initial_state.env_pinned_config_option_ids =
        env_pinned_config_option_ids(agent_type, &runtime_env);

    let session_state = Arc::new(RwLock::new(initial_state));

    emit_with_state(
        &session_state,
        &emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Connecting,
        },
    )
    .await;

    // Align ~/.hermes/.env's base-URL var with config.yaml's model.base_url so
    // Hermes' auxiliary tasks (title generation, compression, …) resolve the
    // same endpoint as the main conversation. Best-effort; never blocks launch.
    if agent_type == AgentType::Hermes {
        crate::commands::acp::reconcile_hermes_runtime_env(&runtime_env);
    }

    // Resolve the launch cwd from the same `working_dir` (via the same helper)
    // that run_connection uses for the session/new request, so the process
    // cwd, the ACP session cwd, and any os.getcwd()-derived agent state all
    // agree. Computed here because `working_dir` is moved into run_connection
    // below.
    let launch_cwd = resolve_working_dir(working_dir.as_deref());
    // Shared cell that receives the agent process's OS pid the instant it
    // spawns (via `on_spawn` below). Stored on the `AgentConnection` so the
    // shutdown path can `kill_tree` the process tree synchronously as a
    // backstop when the connection driver thread is torn down by process exit
    // before `ChildGuard::drop` can run. 0 = not spawned yet / unknown.
    let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(0));
    // Connection-scoped ring buffer of the agent's stderr, populated by the
    // `with_debug` callback `build_agent` installs and read at turn end when a
    // turn is diagnosed as silently empty. Created here so both the spawn side
    // and the conversation loop share the same buffer.
    let stderr_tail = Arc::new(StderrTail::new());
    // Per-launch temp directory, created BEFORE the spawn because it has to
    // exist by the time the child looks: a self-extracting agent binary creates
    // only its own leaf under whatever `TMP` names, so pointing at a directory
    // that is not there yet is a failed launch rather than a graceful fallback.
    // `None` (isolation off, or the directory could not be created) means the
    // child inherits the ambient temp dir exactly as it did before.
    let scratch = crate::acp::scratch_dir::create();
    let agent = match build_agent(
        agent_type,
        &runtime_env,
        &launch_cwd,
        &stderr_tail,
        scratch.as_ref().map(|s| s.path()),
    )
    .await
    {
        Ok(agent) => agent,
        Err(e) => {
            // No process was spawned, so nothing will ever fire `on_exit` for
            // it. Hand the directory back here or it waits for a sweep.
            if let Some(scratch) = scratch {
                scratch.release();
            }
            return Err(e);
        }
    };
    let agent = agent
        .on_spawn({
            let child_pid = Arc::clone(&child_pid);
            move |pid| child_pid.store(pid, std::sync::atomic::Ordering::SeqCst)
        })
        // Paired with `on_spawn`: publish 0 again once the process has been
        // reaped, so the shutdown backstop can never `kill_tree` a pid the OS
        // has already handed to someone else. Fires ONLY on a real reap — a
        // connection that merely ended keeps its pid published, because
        // `acp::agent_process`'s `ChildGuard` signals the tree without waiting
        // and the agent may still be running.
        //
        // That "only on a real reap" is also why the scratch directory is
        // released HERE and not from `ConnectionCleanupGuard`: the guard drops
        // when the driver thread unwinds, which can be well before the agent is
        // actually gone. Even so a reaped parent does not prove its descendants
        // let go of the extracted files, so `release` retries on a ladder rather
        // than deleting once and hoping.
        //
        // If this callback is instead DROPPED without ever firing — a
        // connection that never reported a reap — `LaunchScratch`'s own `Drop`
        // performs the same cleanup and logs that it had to. Nothing here is
        // the last line of defence.
        .on_exit({
            let child_pid = Arc::clone(&child_pid);
            // `on_exit` is `Fn`, so the one-shot move needs interior mutability.
            let scratch = std::sync::Mutex::new(scratch);
            move || {
                child_pid.store(0, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut held) = scratch.lock() {
                    if let Some(scratch) = held.take() {
                        scratch.release();
                    }
                }
            }
        });

    // Path policy for the ACP `fs/*` channel. Built HERE rather than inside
    // `run_connection` because it needs the full `runtime_env` (only the git
    // credential keys survive into `terminal_base_env` below), and a per-agent
    // relocation like `GROK_HOME` must move the allowed root along with the
    // agent's state. Uses the same `launch_cwd` the process and ACP session get.
    let fs_policy = FsAccessPolicy::from_env(&launch_cwd, agent_type, &runtime_env);

    // Whether codeg hosts the `fs/*` + `terminal/*` channels at all, or hands
    // them back to the agent so the agent's OWN sandbox covers them (#436).
    // Resolved here for the same reason as `fs_policy`: it reads the full
    // per-agent `runtime_env`, which does not survive into `run_connection`.
    let host_tools = HostToolsPolicy::from_env(&runtime_env);

    // Forward only the codeg git credential helper keys into the terminal
    // runtime — not the agent's API tokens or model provider credentials.
    // This makes `git fetch`/`git push` issued through the ACP
    // `terminal/create` tool authenticate via the same helper path the
    // agent process uses, while keeping unrelated secrets scoped to the
    // agent and out of arbitrary shell commands it runs.
    let mut terminal_base_env: BTreeMap<String, String> = runtime_env
        .iter()
        .filter(|(k, _)| k.starts_with("GIT_CONFIG_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Also surface a codeg-installed OfficeCLI on the terminal's PATH: agents run
    // office skills' `officecli …` through this `terminal/create` tool, not as a
    // child of the agent process, so the agent-env injection alone wouldn't reach
    // them right after install (before install.ps1's User-PATH change lands).
    prepend_officecli_path(&mut terminal_base_env);

    let (cmd_tx, cmd_rx) = mpsc::channel::<ConnectionCommand>(32);
    let conn_id = connection_id.clone();
    let emitter_clone = emitter.clone();
    let cleanup_connections = connections.clone();
    let cleanup_connection_id = connection_id.clone();
    let state_clone = Arc::clone(&session_state);

    // Canonical config fingerprint of what this process is launching with.
    // Derived from the same `runtime_env` we hand the agent (minus per-launch
    // volatile keys) plus the agent's native config file content, so a later
    // settings save can be compared against it to detect a stale running session.
    let config_fingerprint =
        crate::commands::acp::fingerprint_config(agent_type, &runtime_env);

    // Insert the entry BEFORE spawning the background task so that a
    // fast-failing `run_connection` can never remove it before it was
    // inserted (would otherwise leak the entry).
    connections.lock().await.insert(
        connection_id.clone(),
        AgentConnection {
            id: connection_id,
            agent_type,
            status: ConnectionStatus::Connecting,
            owner_window_label,
            cmd_tx,
            state: Arc::clone(&session_state),
            emitter: emitter.clone(),
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            last_observed_fingerprint: config_fingerprint.clone(),
            config_fingerprint,
            child_pid,
        },
    );

    // Drive `run_connection` on a dedicated, large-stack thread (see
    // ACP_CONNECTION_STACK_SIZE) rather than a Tokio worker task: its debug
    // poll frame is too big for a default ~2 MiB worker stack and was aborting
    // the process under `tauri dev`. `Handle::block_on` runs it on the SAME
    // shared runtime, so `tokio::spawn`/timers/IO inside `run_connection` still
    // use the pool — only the giant top-level frame moves to the roomy stack.
    // The connection is fire-and-forget (torn down from within via `cmd_rx` /
    // process exit; no JoinHandle is awaited), so a thread is behaviorally
    // equivalent to the previous task.
    let connection_rt = tokio::runtime::Handle::current();
    // RAII guard built OUTSIDE the thread body and moved in: on a normal exit
    // or panic unwind its Drop removes the manager map entry, AND if the thread
    // fails to spawn the dropped closure runs the same Drop — so the entry is
    // never leaked.
    let cleanup_guard = ConnectionCleanupGuard {
        connections: cleanup_connections,
        connection_id: cleanup_connection_id,
    };
    let connection_thread = std::thread::Builder::new()
        .name(format!("acp-conn-{conn_id}"))
        .stack_size(ACP_CONNECTION_STACK_SIZE)
        .spawn(move || {
            let _cleanup = cleanup_guard;
            connection_rt.block_on(async move {
        let delegation_for_cleanup = delegation_injection.clone();
        let result = run_connection(
            agent,
            conn_id.clone(),
            agent_type,
            working_dir,
            session_id,
            cmd_rx,
            emitter_clone.clone(),
            Arc::clone(&state_clone),
            terminal_base_env,
            terminal_shell_config,
            preferred_mode_id,
            preferred_config_values,
            delegation_injection,
            fs_policy,
            host_tools,
            stderr_tail,
        )
        .await;

        // Revoke the per-launch token + cascade cancel any still-pending
        // delegations AND questions owned by this parent connection. All are
        // best-effort: a missing token entry is a no-op, and both
        // `cancel_by_parent` calls are safe on an empty pending map.
        if let Some(inj) = delegation_for_cleanup {
            let token = {
                let snap = state_clone.read().await;
                snap.delegation_token.clone()
            };
            if let Some(tok) = token {
                inj.tokens.revoke(&tok).await;
            }
            inj.broker.cancel_by_parent(&conn_id).await;
            // Reclaim a parked `ask_user_question` instead of waiting for the
            // companion's ask socket to close (which a reparented/hard-killed
            // agent may never do); the dropped sender declines the tool cleanly.
            inj.questions.cancel_questions_by_parent(&conn_id).await;
            // Likewise reclaim a parked Grok `exit_plan_mode` approval; the
            // dropped sender replies disconnect so grok keeps plan mode active.
            inj.plan_approvals
                .cancel_plan_approvals_by_parent(&conn_id)
                .await;
        }

        if let Err(e) = result {
            let code = e.code().map(String::from);
            emit_with_state(
                &state_clone,
                &emitter_clone,
                AcpEvent::Error {
                    message: e.to_string(),
                    agent_type: agent_type.to_string(),
                    code,
                    details: None,
                    // The only genuinely terminal emit site: `run_connection`
                    // is unwinding and the next event is `Disconnected`.
                    // The lifecycle worker uses this flag to decide whether
                    // to flip the conversation row to Cancelled and to
                    // buffer the detail for the broker's cancel reason.
                    terminal: true,
                },
            )
            .await;
            // Drive the state machine through `Error` before `Disconnected`
            // so the frontend's error-handling effect (cancelled-on-error)
            // engages — without this hop the connection would jump straight
            // to Disconnected and look like a clean shutdown.
            emit_with_state(
                &state_clone,
                &emitter_clone,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Error,
                },
            )
            .await;
        }

        emit_with_state(
            &state_clone,
            &emitter_clone,
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        )
        .await;
                // Connection loop ended; `block_on` returns and `_cleanup`
                // (bound at the top of the thread body) drops next, removing
                // the manager map entry — same as on a panic unwind.
            });
        });
    if let Err(e) = connection_thread {
        // Thread creation only fails on OS resource exhaustion. Dropping the
        // un-spawned closure already ran `cleanup_guard`'s Drop (removing the
        // map entry), so just surface the failure and let the caller abort.
        tracing::error!("[ACP] failed to spawn connection driver thread: {e}");
        return Err(AcpError::SpawnFailed(format!(
            "connection driver thread: {e}"
        )));
    }

    Ok(session_started_rx)
}

/// The agent request a permission card answers. `Acp` is a real ACP
/// `session/request_permission`; `CodexElicitation` is a codex approval-style
/// `elicitation/create` (MCP tool-call approval / message-only confirm) routed
/// through the SAME permission card so approvals look exactly like they did
/// before codeg advertised `elicitation.form` — its chosen option answers the
/// blocked elicitation request instead (see `handle_elicitation_request`).
enum PermissionReply {
    Acp(Responder<RequestPermissionResponse>),
    CodexElicitation {
        responder: Responder<serde_json::Value>,
        approval: crate::acp::question::ElicitationApproval,
    },
}

impl PermissionReply {
    /// The request's `$/cancel_request` marker — see
    /// [`watch_permission_withdrawal`].
    fn cancellation(&self) -> RequestCancellation {
        match self {
            PermissionReply::Acp(responder) => responder.cancellation(),
            PermissionReply::CodexElicitation { responder, .. } => responder.cancellation(),
        }
    }
}

/// A parked permission-card responder, as [`PermissionQueue`] holds it.
struct PendingPermission {
    reply: PermissionReply,
    /// The request's `$/cancel_request` marker, shared with its watcher: the
    /// queue reads it so it never promotes a card the agent already withdrew.
    withdrawn: RequestCancellation,
    /// Never sent on. It is dropped together with the responder — answered,
    /// drained or withdrawn — and that drop is what ends the card's
    /// [`watch_permission_withdrawal`] task.
    _settled: oneshot::Sender<()>,
}

/// What [`PermissionQueue`] needs from a parked responder. Abstracted into a
/// trait ONLY so the queue stays a pure state machine that unit tests can drive:
/// the ACP runtime's `Responder` has private fields and no public constructor, so a real
/// [`PendingPermission`] cannot be built outside a live ACP connection.
trait PermissionResponder {
    /// Resolve with the user's chosen option id.
    fn respond_selected(self, option_id: String);
    /// Resolve as cancelled — the turn ended / connection tore down before the
    /// user chose, or the agent withdrew the request.
    fn respond_cancelled(self);
    /// Whether the agent has already taken the request back, even if its
    /// [`watch_permission_withdrawal`] task has not retired it yet.
    fn is_withdrawn(&self) -> bool;
}

impl PermissionResponder for PendingPermission {
    fn respond_selected(self, option_id: String) {
        match self.reply {
            PermissionReply::Acp(responder) => {
                let outcome =
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id));
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
            }
            PermissionReply::CodexElicitation {
                responder,
                approval,
            } => {
                let response = crate::acp::question::build_elicitation_approval_response(
                    &approval, &option_id,
                );
                let _ = responder.respond(serde_json::to_value(response).unwrap_or_default());
            }
        }
    }

    fn respond_cancelled(self) {
        match self.reply {
            PermissionReply::Acp(responder) => {
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            PermissionReply::CodexElicitation { responder, .. } => {
                let _ = responder.respond(
                    serde_json::to_value(crate::acp::question::elicitation_cancel_response())
                        .unwrap_or_default(),
                );
            }
        }
    }

    fn is_withdrawn(&self) -> bool {
        self.withdrawn.is_cancelled()
    }
}

/// A permission card that is waiting for its turn on screen. Holds exactly the
/// three fields `AcpEvent::PermissionRequest` carries, so promoting a queued
/// entry is a move, not a rebuild.
struct QueuedPermission {
    request_id: String,
    tool_call: serde_json::Value,
    options: Vec<PermissionOptionInfo>,
}

/// What [`PermissionQueue::resolve`] decided.
struct ResolvedPermission {
    /// `false` when `request_id` was not ours (already answered, drained, or a
    /// stale client) — the caller must then emit nothing at all.
    answered: bool,
    /// The card promoted onto the screen by this answer, if any.
    next: Option<QueuedPermission>,
}

/// Where a card stood when it left [`PermissionQueue`].
enum Departure {
    /// It was on screen; `next` is the card promoted into its place, if any.
    Shown { next: Option<QueuedPermission> },
    /// It was still waiting behind the visible card, so the screen is
    /// unchanged — only the "N more waiting" count moved.
    Queued,
}

/// Per-connection permission state: the blocked responders AND the display
/// queue, under ONE lock.
///
/// Why they must share a lock (#442): the responder map used to be the only
/// state, and every `session/request_permission` emitted its card immediately.
/// But a card is a SINGLE slot both in the snapshot
/// (`SessionState.pending_permission`) and in the frontend reducer
/// (`conn.pendingPermission`), so N concurrent requests collapsed to the last
/// one — the earlier `request_id`s were no longer referenced by any client, so
/// no `RespondPermission` could ever arrive for them and their responders sat
/// parked until teardown, blocking the agent's tool calls forever (a work task
/// then sat at `awaiting_input` because its outstanding-request set kept the
/// orphan keys). Codex issues these concurrently: codex-acp forwards each
/// `item/commandExecution/requestApproval` to its own `session/request_permission`
/// and serializes only its notification queue, not approvals.
///
/// So: queue instead of overwrite — one card at a time, FIFO, nothing lost.
/// Splitting the responder map from the queue would leave a real interleaving
/// (the request handler runs on the ACP runtime's dispatch task, `Cancel` on the
/// conversation task, so they interleave at every `.await`):
///
/// ```text
/// handler: insert responder for P
/// cancel : drain responders; clear queue      <- P's responder is gone
/// handler: enqueue P; emit PermissionRequest(P)   <- clickable but inert card,
///                                                   and `showing = P` wedges
///                                                   every later request
/// ```
///
/// Re-checking "is P still in the responder map" before emitting does NOT close
/// that window (a drain can still land between the check and the publish). One
/// lock covering both, with the emit performed INSIDE it (see
/// [`admit_permission`]), makes "responder exists" and "card is on screen"
/// atomic with respect to a drain.
///
/// Invariant, upheld by every method: every id in `showing`/`waiting` has a
/// live entry in `responders`, and every `responders` key is either `showing` or
/// in `waiting`. That makes "queued card whose responder is gone"
/// unrepresentable, so promotion never has to skip dead entries. It does skip
/// WITHDRAWN ones — live responders the agent has already taken back — see
/// [`Self::promote_next`].
///
/// LOCK ORDER: this mutex is always acquired BEFORE `SessionState`'s `RwLock`,
/// never after. `emit_with_state` takes the state lock internally, so publishing
/// while holding this mutex is the sanctioned direction; nothing may acquire
/// this mutex while already holding a `SessionState` guard.
struct PermissionQueue<R = PendingPermission> {
    responders: HashMap<String, R>,
    /// The card currently published to clients. `None` = nothing on screen.
    showing: Option<String>,
    waiting: VecDeque<QueuedPermission>,
    /// Abort handles for permission requests this connection answers on ANOTHER
    /// surface — today only pi's extension-UI `select`, which renders on the
    /// interactive question card (`try_bridge_pi_select_ask`). They publish no
    /// permission card, so they are deliberately outside `responders` /
    /// `showing` / `waiting` and the invariant above.
    ///
    /// They still have to be parked HERE rather than tracked separately,
    /// because a drain is the connection's "everything waiting on the user is
    /// moot now" signal and fires from several places (both turn-completion
    /// paths, an idle cancel, a mid-turn disconnect). pi is the first asker that
    /// can outlive its turn — pi-acp dispatches the dialog detached, and pi
    /// resolves a timed / aborted one locally without telling it — so a missed
    /// drain leaves the agent blocked AND wedges the connection's
    /// one-ask-at-a-time slot, sending every later select back to the raw
    /// approval card this bridge exists to replace. Sitting in the queue makes
    /// that coverage structural: a new drain site cannot forget them.
    ///
    /// Dropping the sender is the signal; the bridged task then cancels its
    /// question and answers the agent `Cancelled`.
    detached: Vec<oneshot::Sender<()>>,
}

// Hand-written rather than derived: `#[derive(Default)]` would demand
// `R: Default`, which a responder never is.
impl<R> Default for PermissionQueue<R> {
    fn default() -> Self {
        Self {
            responders: HashMap::new(),
            showing: None,
            waiting: VecDeque::new(),
            detached: Vec::new(),
        }
    }
}

impl<R: PermissionResponder> PermissionQueue<R> {
    /// Register a blocked responder and its card. Returns the card to publish
    /// NOW — the newcomer itself when the screen was free, otherwise `None`
    /// (it waits its turn).
    fn admit(&mut self, responder: R, card: QueuedPermission) -> Option<QueuedPermission> {
        self.responders.insert(card.request_id.clone(), responder);
        if self.showing.is_none() {
            self.showing = Some(card.request_id.clone());
            Some(card)
        } else {
            self.waiting.push_back(card);
            None
        }
    }

    /// Answer `request_id` and advance the screen.
    ///
    /// Unknown / already-answered ids are an idempotent no-op (`answered:
    /// false`) — two clients racing the same card must not double-respond to a
    /// responder that has already been consumed.
    fn resolve(&mut self, request_id: &str, option_id: String) -> ResolvedPermission {
        let Some((pending, departure)) = self.take(request_id) else {
            return ResolvedPermission {
                answered: false,
                next: None,
            };
        };
        pending.respond_selected(option_id);
        ResolvedPermission {
            answered: true,
            next: match departure {
                Departure::Shown { next } => next,
                Departure::Queued => None,
            },
        }
    }

    /// The agent took `request_id` back (`$/cancel_request`) before the user
    /// chose: answer it `Cancelled` and retire its card, wherever it stood.
    /// `None` when the id is no longer ours — answered or drained first, which
    /// is the common outcome of a withdrawal racing the user's click.
    fn withdraw(&mut self, request_id: &str) -> Option<Departure> {
        let (pending, departure) = self.take(request_id)?;
        pending.respond_cancelled();
        Some(departure)
    }

    /// Remove `request_id`'s responder and card, promoting the next card when
    /// it was the one on screen. `None` when the id is not ours.
    fn take(&mut self, request_id: &str) -> Option<(R, Departure)> {
        let pending = self.responders.remove(request_id)?;
        if self.showing.as_deref() == Some(request_id) {
            let next = self.promote_next();
            self.showing = next.as_ref().map(|c| c.request_id.clone());
            Some((pending, Departure::Shown { next }))
        } else {
            // The card never reached the screen — a stale client answered it,
            // or the agent withdrew it while it waited. Drop its queue entry
            // too, or promoting it later would surface a card with no
            // responder — the state the invariant above exists to forbid.
            self.waiting.retain(|c| c.request_id != request_id);
            Some((pending, Departure::Queued))
        }
    }

    /// Pop the card to show next, answering `Cancelled` — without ever showing
    /// them — any the agent has already withdrawn.
    ///
    /// Withdrawals come in bursts (claude aborting a sub-agent's parallel tool
    /// calls, codex closing a session), and each card's own watcher retires it
    /// only when it gets its turn. Promoting a card whose request is already
    /// gone would flash it on every client, raise its OS notification and push
    /// it to chat channels, only for its watcher to take it down again.
    fn promote_next(&mut self) -> Option<QueuedPermission> {
        while let Some(card) = self.waiting.pop_front() {
            let withdrawn = self
                .responders
                .get(&card.request_id)
                .is_some_and(R::is_withdrawn);
            if !withdrawn {
                return Some(card);
            }
            if let Some(pending) = self.responders.remove(&card.request_id) {
                pending.respond_cancelled();
            }
            // Pairs with its `queued` line; the queue cannot take the state
            // lock the other lines' scope needs, and the id is unique enough.
            tracing::info!(
                "[ACP] permission {} withdrawn by the agent while queued; never shown",
                card.request_id
            );
        }
        None
    }

    /// Cancel every blocked responder and clear the queue. Returns the id of the
    /// card that was on screen, which the caller MUST follow with a compensating
    /// `PermissionResolved` — otherwise that card stays up on every client with
    /// no live responder behind it, and a work task keeps its outstanding-request
    /// key forever (the pre-existing idle-`Cancel` ghost, #442).
    ///
    /// Queued cards need no compensation: they were never published, so no
    /// client rendered them and `track_request` never counted them. Neither do
    /// the `detached` ones — dropping their abort senders lets each bridged
    /// task clear its own surface.
    fn drain(&mut self) -> Option<String> {
        for (_, pending) in self.responders.drain() {
            pending.respond_cancelled();
        }
        self.waiting.clear();
        self.detached.clear();
        self.showing.take()
    }

    /// Park an abort handle for a request answered on another surface — see
    /// [`Self::detached`].
    fn park_detached(&mut self, abort: oneshot::Sender<()>) {
        // A fired/dropped handle is a task that already finished; sweeping them
        // here keeps a long connection from accumulating dead entries.
        self.detached.retain(|tx| !tx.is_closed());
        self.detached.push(abort);
    }

    /// How many cards are waiting BEHIND the one on screen.
    fn waiting_len(&self) -> usize {
        self.waiting.len()
    }
}

/// Shared per-connection permission state (responders + display queue).
type PendingPermissions = Arc<tokio::sync::Mutex<PermissionQueue>>;

/// `conn=<id> conv=<id>` for the permission log lines.
///
/// Whose permission a log line is about is not answerable from the request id
/// alone, and the answer matters most exactly when it is a delegation
/// sub-agent's — that connection has no tab, so a stall there is the one an
/// operator is least able to see (#447). With the connection id here and
/// `child_connection_id` on the `delegation_task` span, an external watchdog can
/// join the two. `conv` is `-` until the row is bound.
///
/// Takes the `SessionState` read lock, so callers must hold the permission queue
/// mutex — never the reverse (see [`PermissionQueue`]'s LOCK ORDER note).
async fn permission_log_scope(state: &Arc<RwLock<SessionState>>) -> String {
    let s = state.read().await;
    match s.conversation_id {
        Some(cid) => format!("conn={} conv={cid}", s.connection_id),
        None => format!("conn={} conv=-", s.connection_id),
    }
}

/// Register a blocked permission responder and publish its card if the screen is
/// free. The emit happens INSIDE the queue lock — see [`PermissionQueue`] for why
/// that is load-bearing rather than incidental.
///
/// Also starts the card's [`watch_permission_withdrawal`] task on `cx`, so an
/// agent that takes the request back retires the card instead of leaving it up.
async fn admit_permission(
    cx: &ConnectionTo<Agent>,
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    reply: PermissionReply,
    card: QueuedPermission,
) {
    let cancellation = reply.cancellation();
    let (settled_tx, settled_rx) = oneshot::channel();
    let request_id = card.request_id.clone();
    admit_card(
        perms,
        state,
        emitter,
        PendingPermission {
            reply,
            withdrawn: cancellation.clone(),
            _settled: settled_tx,
        },
        card,
    )
    .await;
    watch_permission_withdrawal(
        cx,
        cancellation,
        settled_rx,
        perms,
        state,
        emitter,
        request_id,
    );
}

/// The queue half of [`admit_permission`]: admit, then publish the card or the
/// new queue depth while still holding the lock.
async fn admit_card(
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    responder: PendingPermission,
    card: QueuedPermission,
) {
    let mut queue = perms.lock().await;
    let scope = permission_log_scope(state).await;
    let request_id = card.request_id.clone();
    match queue.admit(responder, card) {
        Some(card) => {
            tracing::info!(
                "[ACP] permission {} shown {scope} (waiting={})",
                card.request_id,
                queue.waiting_len()
            );
            // `queued` is 0 by construction here: a card is only published when
            // the screen was free, which means nothing was waiting.
            emit_with_state(
                state,
                emitter,
                AcpEvent::PermissionRequest {
                    request_id: card.request_id,
                    tool_call: card.tool_call,
                    options: card.options,
                    queued: 0,
                },
            )
            .await;
        }
        None => {
            let depth = queue.waiting_len();
            tracing::info!(
                "[ACP] permission {request_id} queued {scope} behind {:?} (waiting={depth})",
                queue.showing,
            );
            // The visible card did not change, so nothing republishes it — send
            // a depth-only update or its "N more waiting" hint goes stale.
            emit_with_state(
                state,
                emitter,
                AcpEvent::PermissionQueueDepth {
                    depth: depth as u32,
                },
            )
            .await;
        }
    }
}

/// Answer a permission card and promote the next one.
///
/// Publish order is deliberate: `PermissionRequest(next)` goes out BEFORE
/// `PermissionResolved(request_id)`. Both clears are id-checked
/// (`SessionState::apply_event`, and the frontend's `PERMISSION_CLEARED`), so the
/// trailing resolve is a no-op against the newly-shown card rather than wiping
/// it. It also keeps a work task's outstanding-request set from transiently
/// emptying, which would otherwise flip the row `awaiting_input → running →
/// awaiting_input` on every single approval.
async fn resolve_permission(
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_id: String,
    option_id: String,
) {
    let mut queue = perms.lock().await;
    let resolved = queue.resolve(&request_id, option_id);
    if !resolved.answered {
        return;
    }
    // Pairs with the `shown` line: a watchdog reading only the raise side can't
    // tell a permission the user answered in seconds from one that has been
    // blocking the agent since. Resolved AFTER the idempotent no-op check, so a
    // stale/duplicate answer costs no state read and logs nothing.
    let scope = permission_log_scope(state).await;
    tracing::info!("[ACP] permission {request_id} answered {scope}");
    if let Some(card) = resolved.next {
        let depth = queue.waiting_len();
        publish_promoted(state, emitter, &scope, &request_id, card, depth).await;
    }
    emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id }).await;
}

/// Publish the card a departing one promoted onto the screen, with `depth`
/// cards still waiting behind it. Called with the queue lock held, like every
/// other publish from the queue.
async fn publish_promoted(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    scope: &str,
    departed: &str,
    card: QueuedPermission,
    depth: usize,
) {
    tracing::info!(
        "[ACP] permission {} promoted {scope} after {departed} (waiting={depth})",
        card.request_id,
    );
    emit_with_state(
        state,
        emitter,
        AcpEvent::PermissionRequest {
            request_id: card.request_id,
            tool_call: card.tool_call,
            options: card.options,
            // Unlike a fresh admit this can be non-zero: promotion happens
            // with the rest of the queue still behind it.
            queued: depth as u32,
        },
    )
    .await;
}

/// Retire the card of a request the agent withdrew, and answer it `Cancelled`.
///
/// Publishes exactly what an answer would have changed — the promoted card
/// first, then `PermissionResolved` (the order [`resolve_permission`] explains)
/// — or, for a card that was still queued, only the new queue depth. A no-op
/// when the card already settled some other way.
async fn withdraw_permission(
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_id: String,
) {
    let mut queue = perms.lock().await;
    let Some(departure) = queue.withdraw(&request_id) else {
        return;
    };
    let scope = permission_log_scope(state).await;
    tracing::info!("[ACP] permission {request_id} withdrawn by the agent {scope}");
    match departure {
        Departure::Shown { next } => {
            if let Some(card) = next {
                let depth = queue.waiting_len();
                publish_promoted(state, emitter, &scope, &request_id, card, depth).await;
            }
            emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id }).await;
        }
        Departure::Queued => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::PermissionQueueDepth {
                    depth: queue.waiting_len() as u32,
                },
            )
            .await;
        }
    }
}

/// Retire a permission card the moment the agent takes its request back.
///
/// The ACP runtime marks a parked request cancelled when the agent sends
/// `$/cancel_request` for it, and the two adapters codeg follows most closely
/// both do: claude-agent-acp (0.81) wires each tool call's abort signal into its
/// `session/request_permission`, so a stopped sub-agent or an interrupted tool
/// withdraws its approval; codex-acp (1.13) wires its prompt's signal into every
/// approval and elicitation it sends, and that signal aborts when the session
/// closes or the `session/prompt` request itself is cancelled (a plain
/// `session/cancel` interrupts the turn without it). claude races the answer
/// against the abort and moves on; codex keeps waiting for one, so the
/// `Cancelled` sent here is what releases it. Left up, such a card could only be
/// answered after the fact — and, the queue being FIFO, every later card would
/// wait behind it.
///
/// Runs as a connection task (`ConnectionTo::spawn`), so it never outlives the
/// connection, and it ends the moment the card settles any other way: `settled`
/// resolves when the sender parked with the responder is dropped.
fn watch_permission_withdrawal(
    cx: &ConnectionTo<Agent>,
    cancellation: RequestCancellation,
    settled: oneshot::Receiver<()>,
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_id: String,
) {
    let perms = Arc::clone(perms);
    let state = Arc::clone(state);
    let emitter = emitter.clone();
    let log_id = request_id.clone();
    let watch = async move {
        if withdrawn_while_parked(settled, cancellation.cancelled()).await {
            withdraw_permission(&perms, &state, &emitter, request_id).await;
        }
        Ok(())
    };
    if let Err(e) = cx.spawn(watch) {
        // Only when the connection is already going down, which drains the
        // card anyway.
        tracing::debug!("[ACP] not watching permission {log_id} for withdrawal: {e}");
    }
}

/// Wait out a parked permission card: `true` when the agent withdrew the
/// request while the card was still queued, `false` once the card left the
/// queue some other way (answered, drained, or skipped by `promote_next`).
///
/// A card that has left is never treated as withdrawn, even when its
/// withdrawal is ready too by the time the watcher runs. The queue key is a
/// fresh UUID per admission, so a late `withdraw_permission` would only find
/// nothing to do; this keeps that from depending on the key.
async fn withdrawn_while_parked(
    settled: oneshot::Receiver<()>,
    withdrawn: impl std::future::Future<Output = ()>,
) -> bool {
    tokio::select! {
        biased;
        _ = settled => false,
        () = withdrawn => true,
    }
}

/// Cancel every pending permission on this connection and clear the on-screen
/// card via a compensating `PermissionResolved`.
///
/// Mirrors `ConnectionManager::compensate_if_question_drained`, which solves the
/// same "broadcast card outlives its backend waiter" problem for questions.
async fn drain_permissions(
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
) {
    let mut queue = perms.lock().await;
    drain_permissions_locked(&mut queue, state, emitter).await;
}

/// Drain, then publish `follow_up`, WITHOUT releasing the queue lock in between.
///
/// The two must be atomic whenever `follow_up` is `TurnComplete`, because
/// `SessionState::apply_event` nulls `pending_permission` on that event
/// unconditionally. With a plain drain followed by a separate emit, an
/// `admit_permission` landing in the gap (the request handler runs on the ACP
/// runtime's dispatch task, this on the conversation task) would publish its card and set
/// `showing`, and then `TurnComplete` would silently un-display it on every
/// client — leaving a live responder behind an id nobody can answer, and wedging
/// every LATER permission behind it for the life of the connection. That is the
/// exact failure mode this queue exists to prevent.
///
/// Holding the lock across both makes an in-flight admit wait until the turn has
/// ended; its card then lands on an empty queue and displays normally as the
/// out-of-turn request it now is.
async fn drain_permissions_then_emit(
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    follow_up: AcpEvent,
) {
    let mut queue = perms.lock().await;
    drain_permissions_locked(&mut queue, state, emitter).await;
    emit_with_state(state, emitter, follow_up).await;
}

/// Shared body of the two drains above. Emits the compensating
/// `PermissionResolved` for whatever was on screen; queued cards were never
/// published, so they need none.
async fn drain_permissions_locked(
    queue: &mut PermissionQueue,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
) {
    if let Some(request_id) = queue.drain() {
        tracing::info!("[ACP] permission {request_id} cancelled by drain");
        emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id }).await;
    }
}

fn map_session_modes(mode_state: &SessionModeState) -> SessionModeStateInfo {
    SessionModeStateInfo {
        current_mode_id: mode_state.current_mode_id.to_string(),
        available_modes: mode_state
            .available_modes
            .iter()
            .map(|mode| SessionModeInfo {
                id: mode.id.to_string(),
                name: mode.name.clone(),
                description: mode.description.clone(),
            })
            .collect(),
    }
}

async fn emit_session_modes(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    modes: &Option<SessionModeState>,
) {
    if let Some(mode_state) = modes {
        emit_with_state(
            state,
            emitter,
            AcpEvent::SessionModes {
                modes: map_session_modes(mode_state),
            },
        )
        .await;
    }
}

fn map_session_config_category(category: &SessionConfigOptionCategory) -> String {
    match category {
        SessionConfigOptionCategory::Mode => "mode".to_string(),
        SessionConfigOptionCategory::Model => "model".to_string(),
        SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
        SessionConfigOptionCategory::Other(value) => value.clone(),
        _ => "unknown".to_string(),
    }
}

fn map_session_config_select_option(
    option: &SessionConfigSelectOption,
) -> SessionConfigSelectOptionInfo {
    SessionConfigSelectOptionInfo {
        value: option.value.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
    }
}

fn map_session_config_select_group(
    group: &SessionConfigSelectGroup,
) -> SessionConfigSelectGroupInfo {
    SessionConfigSelectGroupInfo {
        group: group.group.to_string(),
        name: group.name.clone(),
        options: group
            .options
            .iter()
            .map(map_session_config_select_option)
            .collect(),
    }
}

/// The `recommendedValue` an agent attached to ONE config option, out of its
/// `_meta.jetbrains.air` envelope.
///
/// Envelope validation mirrors [`air_session_failure`] (integer `version >= 1`,
/// the same check the adapters run on codeg's own advertisement), so a
/// future-incompatible envelope yields `None` rather than a half-understood
/// hint. The payload must be a non-blank string: codex-acp only ever writes a
/// model id or a reasoning-effort id there, and anything else is not something
/// a select's values could match.
///
/// Deliberately NOT validated against the option's own value list. The frontend
/// marks the recommendation by equality, so a stale or unknown value marks
/// nothing — re-deriving the membership rule here would only duplicate the
/// adapter's own filter and could reject a shape (e.g. a grouped select) it
/// grows later.
fn air_recommended_value(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    let air = meta?.get("jetbrains")?.get("air")?;
    let version = air.get("version").and_then(serde_json::Value::as_i64)?;
    if version < 1 {
        return None;
    }
    let value = air
        .get("recommendedValue")
        .and_then(serde_json::Value::as_str)?
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn map_session_config_option(option: &SessionConfigOption) -> Option<SessionConfigOptionInfo> {
    let recommended_value = air_recommended_value(option.meta.as_ref());
    match &option.kind {
        SessionConfigKind::Select(select) => {
            let (flat_options, groups) = match &select.options {
                SessionConfigSelectOptions::Ungrouped(options) => (
                    options
                        .iter()
                        .map(map_session_config_select_option)
                        .collect::<Vec<_>>(),
                    Vec::new(),
                ),
                SessionConfigSelectOptions::Grouped(grouped) => (
                    grouped
                        .iter()
                        .flat_map(|group| {
                            group.options.iter().map(map_session_config_select_option)
                        })
                        .collect::<Vec<_>>(),
                    grouped
                        .iter()
                        .map(map_session_config_select_group)
                        .collect::<Vec<_>>(),
                ),
                _ => (Vec::new(), Vec::new()),
            };

            Some(SessionConfigOptionInfo {
                id: option.id.to_string(),
                name: option.name.clone(),
                description: option.description.clone(),
                category: option.category.as_ref().map(map_session_config_category),
                kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                    current_value: select.current_value.to_string(),
                    options: flat_options,
                    groups,
                }),
                recommended_value,
            })
        }
        // A toggle has no value list to recommend INTO — its two states are
        // already spelled out by `current_value` — so the hint is dropped here
        // rather than carried to a frontend with nowhere to put it.
        SessionConfigKind::Boolean(toggle) => Some(SessionConfigOptionInfo {
            id: option.id.to_string(),
            name: option.name.clone(),
            description: option.description.clone(),
            category: option.category.as_ref().map(map_session_config_category),
            kind: SessionConfigKindInfo::Boolean(SessionConfigBooleanInfo {
                current_value: toggle.current_value,
            }),
            recommended_value: None,
        }),
        _ => None,
    }
}

fn map_session_config_options(
    config_options: &[SessionConfigOption],
) -> Vec<SessionConfigOptionInfo> {
    config_options
        .iter()
        .filter_map(map_session_config_option)
        .collect()
}

/// Config-option ids a launch carrying `runtime_env` has pinned, so the agent
/// will reject any attempt to change them (see
/// [`SessionState::env_pinned_config_option_ids`]).
///
/// Only cline has one today: its `provider` selector is hard-refused whenever
/// `CLINE_PROVIDER` is exported, which codeg does for every bring-your-own
/// provider. The check is on the variable codeg actually ships, not on the
/// configured provider id, because the pin is what the agent tests.
fn env_pinned_config_option_ids(
    agent_type: AgentType,
    runtime_env: &BTreeMap<String, String>,
) -> Vec<String> {
    if agent_type != AgentType::Cline {
        return Vec::new();
    }
    // Emptiness is judged the way the agent judges it. cline's guard is a bare
    // `if (process.env.CLINE_PROVIDER)`, so only the empty string is falsy —
    // whitespace is a pin, and `buildConfig`'s `?? ` would go on to use it
    // verbatim as the provider id. Trimming first would leave the selector on
    // screen for a session that refuses every choice in it.
    let pinned = runtime_env
        .get("CLINE_PROVIDER")
        .is_some_and(|value| !value.is_empty());
    if pinned {
        vec![CLINE_PROVIDER_CONFIG_OPTION_ID.to_string()]
    } else {
        Vec::new()
    }
}

/// Cline's provider selector id — the one `CLINE_PROVIDER` freezes.
const CLINE_PROVIDER_CONFIG_OPTION_ID: &str = "provider";

/// The advertised options minus the ones this launch pinned through the
/// environment. Applied on the way out so all three producers of a
/// `SessionConfigOptions` event share one rule.
fn visible_config_options(
    pinned: &[String],
    config_options: Vec<SessionConfigOption>,
) -> Vec<SessionConfigOption> {
    if pinned.is_empty() {
        return config_options;
    }
    config_options
        .into_iter()
        .filter(|option| !pinned.iter().any(|id| *id == option.id.to_string()))
        .collect()
}

async fn emit_session_config_options_values(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    config_options: Vec<SessionConfigOption>,
) {
    // Drop what this launch froze BEFORE mapping, so every producer of this
    // event — establishment, the answer to a set, and the agent's own
    // `config_option_update` push — is filtered by one rule.
    let pinned = state.read().await.env_pinned_config_option_ids.clone();
    let visible = visible_config_options(&pinned, config_options);
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionConfigOptions {
            config_options: map_session_config_options(&visible),
        },
    )
    .await;
}

async fn emit_selectors_ready(state: &Arc<RwLock<SessionState>>, emitter: &EventEmitter) {
    emit_with_state(state, emitter, AcpEvent::SelectorsReady).await;
}

/// The conventional id of a model selector. ACP reserves none — `category:
/// "model"` is the spec-level signal — but every agent codeg drives spells the
/// id this way, and the frontend's `isModelConfigOption` accepts either.
const MODEL_CONFIG_OPTION_ID: &str = "model";

/// Synthesized config-option id for Grok's model picker (drives the composer's
/// grouped model selector via the frontend's `isModelConfigOption`).
const GROK_MODEL_OPTION_ID: &str = MODEL_CONFIG_OPTION_ID;

/// Synthesized config-option id for Grok's per-session reasoning-effort selector.
/// Grok ships effort choices in `x.ai/sessionConfig` under `category:"mode"`
/// (ids `low`/`medium`/`high`), and applies a live override via the
/// `session/set_model` request's `_meta.reasoningEffort` — so effort is a live
/// composer control, not just a global config.toml default.
const GROK_EFFORT_OPTION_ID: &str = "reasoning_effort";

/// Stable `AcpEvent::Error` code the frontend localizes when a Grok model switch
/// is rejected because the conversation is already bound to a different agent
/// type (see `is_grok_incompatible_agent_switch`). Recoverable, not terminal.
const GROK_INCOMPATIBLE_AGENT_ERROR_CODE: &str = "grok_model_switch_incompatible_agent";

/// Stable `AcpEvent::Error` codes for the other recoverable failures, so the
/// frontend can localize them and treat each as what it is (see
/// `lib/acp-error-presentation.ts`): the verdict on something the user just
/// did — a mode, option or goal change, an attached image — is a notification
/// and nothing more, not the session's standing error until the next prompt;
/// and a session restored as a NEW one is a warning, not an error.
/// The English `message` is unchanged: it stays the fallback for a client that
/// doesn't know the code, and it is what the chat channels forward.
const SESSION_LOAD_FALLBACK_ERROR_CODE: &str = "session_load_fallback";
const SET_MODE_FAILED_ERROR_CODE: &str = "set_mode_failed";
const SET_CONFIG_OPTION_FAILED_ERROR_CODE: &str = "set_config_option_failed";
const GOAL_CONTROL_FAILED_ERROR_CODE: &str = "goal_control_failed";
const IMAGE_DROPPED_ERROR_CODE: &str = "image_dropped";
/// Accompanies a grok FAILED compaction card (see `compaction_failure_error`).
/// The frontend draws nothing for it — the card already shows the failure.
const COMPACTION_FAILED_ERROR_CODE: &str = "compaction_failed";

/// Grok partitions its models by `agentType` (e.g. `grok-4.5` → `grok-build-plan`,
/// `grok-composer-2.5-fast` → `cursor`). A session may switch models freely until
/// its first turn, after which it is locked to the agent type it started with;
/// a later cross-agent-type `session/set_model` is then rejected with a stable
/// `data.code` of `MODEL_SWITCH_INCOMPATIBLE_AGENT` (`suggestion: start_new_session`).
/// Grok's own `x.ai/sessionConfig` still lists every model regardless of type, so
/// the composer offers them all and we detect this specific rejection to handle
/// it gracefully rather than leaking a raw JSON-RPC error.
fn is_grok_incompatible_agent_switch(e: &agent_client_protocol::Error) -> bool {
    e.data
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        == Some("MODEL_SWITCH_INCOMPATIBLE_AGENT")
}

/// Canonical, composer-facing label for a Grok reasoning-effort tier id. Aligns
/// the composer with the settings panel's `grok.effort*` wording
/// (Low/Medium/High/Max); unknown ids fall back to the id itself. Grok's own
/// richer per-tier text (e.g. "Highest implementation quality…") is kept as the
/// option *description*, not the name.
fn grok_effort_label(id: &str) -> &str {
    match id {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Max",
        other => other,
    }
}

/// Canonical composer-facing *description* (sub-text) for a Grok reasoning-effort
/// tier. Grok ships its own per-tier `description` only for the models switchable
/// `reasoningEfforts`; the model default that lives OUTSIDE that list — grok-4.5's
/// `xhigh`/Max — carries none, so the front-injected option would otherwise be the
/// only tier with no sub-text. This supplies a fitting one (and doubles as a
/// fallback if grok ever omits a switchable tier's description). Unknown ids get
/// `None`. Grok's own, more specific text always takes precedence over this.
fn grok_effort_description(id: &str) -> Option<&'static str> {
    match id {
        "low" => Some("Quick, fast responses"),
        "medium" => Some("Balanced speed and quality"),
        "high" => Some("Extensive reasoning for high quality"),
        "xhigh" => Some("Maximum reasoning for the most complex tasks"),
        _ => None,
    }
}

/// Parse Grok's raw top-level `models` (from a session-establishment response)
/// into a per-`modelId` spec map: reasoning-effort capability plus the model's
/// own context window. Absent `models` / `availableModels` → empty map (caller
/// falls back to the flat `x.ai/sessionConfig` effort list). Missing `_meta`
/// fields degrade gracefully (`supports=false` / `default=None` / `options=[]` /
/// `context_window=None`).
fn parse_grok_model_specs(models: Option<&serde_json::Value>) -> HashMap<String, GrokModelSpec> {
    let mut out = HashMap::new();
    let Some(list) = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|v| v.as_array())
    else {
        return out;
    };
    for m in list {
        let Some(model_id) = m.get("modelId").and_then(|v| v.as_str()) else {
            continue;
        };
        let meta = m.get("_meta");
        let supports = meta
            .and_then(|x| x.get("supportsReasoningEffort"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let default = meta
            .and_then(|x| x.get("reasoningEffort"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut options = Vec::new();
        if let Some(efforts) = meta
            .and_then(|x| x.get("reasoningEfforts"))
            .and_then(|v| v.as_array())
        {
            for e in efforts {
                let Some(id) = e.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let label = e
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id)
                    .to_string();
                let description = e
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                options.push((id.to_string(), label, description));
            }
        }
        out.insert(
            model_id.to_string(),
            GrokModelSpec {
                options,
                default,
                supports,
                context_window: meta
                    .and_then(|x| x.get("totalContextTokens"))
                    .and_then(|v| v.as_u64())
                    .filter(|window| *window > 0),
            },
        );
    }
    out
}

/// Build the reasoning-effort selector for `model_id` from the per-model spec
/// map, or `None` if the model is absent from the map or does not support
/// effort. Options are the model's switchable `reasoningEfforts` (relabeled via
/// [`grok_effort_label`], keeping grok's own copy as the description); the model
/// default is injected at the FRONT when it isn't already listed, so a default
/// that lives OUTSIDE the switchable set — grok-4.5's `xhigh` — stays selectable
/// and the current value is always representable. `current_value` = the model
/// default (or the first option).
fn build_grok_effort_option(
    model_id: &str,
    specs: &HashMap<String, GrokModelSpec>,
) -> Option<SessionConfigOptionInfo> {
    let spec = specs.get(model_id)?;
    if !spec.supports {
        return None;
    }
    let mut options: Vec<SessionConfigSelectOptionInfo> = spec
        .options
        .iter()
        .map(|(id, _grok_label, desc)| SessionConfigSelectOptionInfo {
            value: id.clone(),
            name: grok_effort_label(id).to_string(),
            // Grok's own per-tier text wins; canonical fallback fills any gap.
            description: desc
                .clone()
                .or_else(|| grok_effort_description(id).map(str::to_string)),
        })
        .collect();
    if let Some(def) = &spec.default {
        if !options.iter().any(|o| &o.value == def) {
            options.insert(
                0,
                SessionConfigSelectOptionInfo {
                    value: def.clone(),
                    name: grok_effort_label(def).to_string(),
                    // The injected default (grok-4.5's `xhigh`) is absent from grok's
                    // switchable list, so it has no grok description — supply ours.
                    description: grok_effort_description(def).map(str::to_string),
                },
            );
        }
    }
    if options.is_empty() {
        return None;
    }
    let current_value = spec
        .default
        .clone()
        .unwrap_or_else(|| options[0].value.clone());
    Some(SessionConfigOptionInfo {
        id: GROK_EFFORT_OPTION_ID.to_string(),
        name: "Reasoning effort".to_string(),
        description: None,
        category: Some("mode".to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value,
            options,
            groups: Vec::new(),
        }),
        // Grok's per-model default IS `current_value` here, so a recommendation
        // would only repeat the checkmark.
        recommended_value: None,
    })
}

/// Re-point the effort selector in `opts` at `model_id`: drop any existing
/// effort selector, then append a freshly-built one iff the model supports
/// effort. The model selector is untouched and effort stays LAST (matching
/// `synthesize_grok_config_options`' ordering). Used on a mid-session model
/// switch, where grok never re-sends per-model effort data.
fn set_grok_effort_selector_for_model(
    opts: &mut Vec<SessionConfigOptionInfo>,
    model_id: &str,
    specs: &HashMap<String, GrokModelSpec>,
) {
    opts.retain(|o| o.id != GROK_EFFORT_OPTION_ID);
    if let Some(effort) = build_grok_effort_option(model_id, specs) {
        opts.push(effort);
    }
}

/// `_x.ai/models/update` — Grok's model catalog broadcast. Connection-level: it
/// carries NO `sessionId` (see [`GrokModelCatalogBroadcasts`]).
const GROK_MODELS_UPDATE_METHOD: &str = "_x.ai/models/update";

/// Read a `_x.ai/models/update` broadcast. As grok 1.0.40 sends it (captured
/// live):
///
///   {"currentModelId": "grok-4.7",
///    "availableModels": [{"modelId", "name", "description",
///                         "_meta": {"totalContextTokens", "agentType",
///                                   "supportsReasoningEffort", "reasoningEffort",
///                                   "reasoningEfforts": [{"id", "value", "label",
///                                                         "description",
///                                                         "default"}]}}]}
///
/// — the entry shape of a handshake's `models`, custom `[model.<id>]` endpoints
/// included. `None` when it names no usable model (`availableModels` missing,
/// malformed or empty, or no entry with a model id), so a bad frame can never
/// wipe the picker. `currentModelId` is deliberately not read; see
/// [`fold_grok_catalog_into_picker`].
fn parse_grok_model_catalog(params: &serde_json::Value) -> Option<GrokModelCatalog> {
    let entries = params.get("availableModels")?.as_array()?;
    let mut models: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    for entry in entries {
        let Some(model_id) = entry
            .get("modelId")
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if models.iter().any(|row| row.value == model_id) {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(model_id);
        models.push(SessionConfigSelectOptionInfo {
            value: model_id.to_string(),
            name: name.to_string(),
            description: None,
        });
    }
    if models.is_empty() {
        return None;
    }
    Some(GrokModelCatalog {
        models,
        specs: parse_grok_model_specs(Some(params)),
    })
}

/// Fold a catalog broadcast's specs into the session's.
///
/// What a model OFFERS — its efforts, whether it takes one, its window — comes
/// from the broadcast: it is the newer catalog, and the only source for a model
/// the handshake did not know. A `default` the session already had for a model
/// is kept, though: a handshake's per-model `reasoningEffort` folds in the
/// session's own effort (`default_reasoning_effort` included — a live 1.0.40
/// handshake said `xhigh` where the broadcast after it said `high`), while the
/// broadcast's is the bare catalog default. Models the broadcast no longer
/// lists keep their spec: the session may still be running on one.
fn fold_grok_catalog_specs(
    specs: &mut HashMap<String, GrokModelSpec>,
    catalog: &HashMap<String, GrokModelSpec>,
) {
    for (model_id, fresh) in catalog {
        let default = specs
            .get(model_id)
            .and_then(|known| known.default.clone())
            .or_else(|| fresh.default.clone());
        specs.insert(
            model_id.clone(),
            GrokModelSpec {
                default,
                ..fresh.clone()
            },
        );
    }
}

/// Fold a catalog broadcast into a Grok picker: the model rows become the
/// catalog's, and nothing the session chose moves.
///
/// * The checkmark stays on the model the picker shows, and nothing here sends
///   `session/set_model`. The broadcast's `currentModelId` is grok's
///   process-wide pick, not a switch of this session; grok's own pager does not
///   move its selection on it either. A session whose model the catalog no
///   longer lists is still running on it, so its row is kept, first, rather
///   than dropped — the pager keeps it displayed too.
/// * The effort selector is rebuilt from the current model's refreshed spec but
///   keeps the value it shows — the user's pick, or the session's own effort —
///   even if the refreshed list no longer offers it, the way
///   [`build_grok_effort_option`] keeps an out-of-list default. The catalog's
///   default only fills in where there was no effort selector to keep a value
///   from. A model with no spec keeps its effort selector as it is.
///
/// A list with no model selector is left alone: a broadcast refreshes a
/// picker, it never invents one.
fn fold_grok_catalog_into_picker(
    opts: &mut Vec<SessionConfigOptionInfo>,
    catalog: &GrokModelCatalog,
    specs: &HashMap<String, GrokModelSpec>,
) {
    let Some(model) = opts
        .iter_mut()
        .filter(|o| o.id == GROK_MODEL_OPTION_ID)
        .find_map(|o| match &mut o.kind {
            SessionConfigKindInfo::Select(sel) => Some(sel),
            SessionConfigKindInfo::Boolean(_) => None,
        })
    else {
        return;
    };
    let current = model.current_value.clone();
    let mut rows = catalog.models.clone();
    if !rows.iter().any(|row| row.value == current) {
        rows.insert(
            0,
            kept_select_row(&model.options, &current, || current.clone(), None),
        );
    }
    model.options = rows;

    if !specs.contains_key(&current) {
        return;
    }
    let kept_effort = opts
        .iter()
        .filter(|o| o.id == GROK_EFFORT_OPTION_ID)
        .find_map(|o| match &o.kind {
            SessionConfigKindInfo::Select(sel) => Some((
                sel.current_value.clone(),
                kept_select_row(
                    &sel.options,
                    &sel.current_value,
                    || grok_effort_label(&sel.current_value).to_string(),
                    grok_effort_description(&sel.current_value),
                ),
            )),
            SessionConfigKindInfo::Boolean(_) => None,
        });
    opts.retain(|o| o.id != GROK_EFFORT_OPTION_ID);
    let Some(mut effort) = build_grok_effort_option(&current, specs) else {
        return;
    };
    if let (Some((value, row)), SessionConfigKindInfo::Select(sel)) =
        (kept_effort, &mut effort.kind)
    {
        if !sel.options.iter().any(|o| o.value == value) {
            sel.options.insert(0, row);
        }
        sel.current_value = value;
    }
    opts.push(effort);
}

/// The row `value` already has in `rows`, or a fresh one named by `name` — for
/// a value a refreshed list no longer offers but the picker still shows.
fn kept_select_row(
    rows: &[SessionConfigSelectOptionInfo],
    value: &str,
    name: impl FnOnce() -> String,
    description: Option<&str>,
) -> SessionConfigSelectOptionInfo {
    rows.iter()
        .find(|row| row.value == value)
        .cloned()
        .unwrap_or_else(|| SessionConfigSelectOptionInfo {
            value: value.to_string(),
            name: name(),
            description: description.map(str::to_string),
        })
}

/// Grok does not emit the standard ACP `config_options` / `modes` channels that
/// codeg's generic composer-selector pipeline reads (which is why the composer
/// showed no selectors for Grok). Instead it ships its selectors in a
/// non-standard `_meta["x.ai/sessionConfig"].options` list — a flat array of
/// `{id, category, label, description?, selected}` covering both model choices
/// (`category:"model"`) and reasoning-effort choices (`category:"mode"`). Fold
/// that list into the same `SessionConfigOptionInfo` shape every other agent's
/// selectors flow through, so Grok reaches selector parity with zero new
/// frontend code. Returns `None` when there is no usable sessionConfig.
fn synthesize_grok_config_options(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    specs: &HashMap<String, GrokModelSpec>,
) -> Option<Vec<SessionConfigOptionInfo>> {
    let options = meta?
        .get("x.ai/sessionConfig")?
        .get("options")?
        .as_array()?;

    let mut model_opts: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    let mut model_current: Option<String> = None;
    let mut effort_opts: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    let mut effort_current: Option<String> = None;

    for opt in options {
        let Some(id) = opt.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        // Grok ships two composer selectors here: the MODEL list
        // (`category:"model"`) and the reasoning-EFFORT list (`category:"mode"`,
        // ids low/medium/high). Both are live over ACP — model via
        // `session/set_model`, effort via that request's `_meta.reasoningEffort`
        // (see `set_grok_model` / `set_grok_config_option`). Effort options only
        // appear when the current model advertises `supportsReasoningEffort`, so
        // the selector self-gates. Anything else is ignored.
        let (opts_vec, current) = match opt.get("category").and_then(|v| v.as_str()) {
            Some("model") => (&mut model_opts, &mut model_current),
            Some("mode") => (&mut effort_opts, &mut effort_current),
            _ => continue,
        };
        let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or(id);
        if opt
            .get("selected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            *current = Some(id.to_string());
        }
        opts_vec.push(SessionConfigSelectOptionInfo {
            value: id.to_string(),
            name: label.to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }

    let mut result: Vec<SessionConfigOptionInfo> = Vec::new();
    // Current model id (the `selected` one, else the first) — needed both for
    // the model selector's `current_value` and to pick the per-model effort spec.
    let current_model = model_current
        .clone()
        .or_else(|| model_opts.first().map(|o| o.value.clone()));
    if !model_opts.is_empty() {
        let current = current_model
            .clone()
            .unwrap_or_else(|| model_opts[0].value.clone());
        result.push(SessionConfigOptionInfo {
            id: GROK_MODEL_OPTION_ID.to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current,
                options: model_opts,
                groups: Vec::new(),
            }),
            // `x.ai/sessionConfig` names no default beyond `selected`.
            recommended_value: None,
        });
    }
    // Effort selector. With per-model `specs` (parsed from the response's
    // top-level `models`), it follows the CURRENT model's advertised capability
    // — present/absent, its option set, and an `xhigh`-style out-of-list default
    // (see `build_grok_effort_option`). Without specs (no `models` in the
    // response) fall back to today's flat `x.ai/sessionConfig` "mode" list so
    // nothing regresses.
    if !specs.is_empty() {
        if let Some(effort) = current_model
            .as_deref()
            .and_then(|m| build_grok_effort_option(m, specs))
        {
            result.push(effort);
        }
    } else if !effort_opts.is_empty() {
        let current = effort_current.unwrap_or_else(|| effort_opts[0].value.clone());
        result.push(SessionConfigOptionInfo {
            id: GROK_EFFORT_OPTION_ID.to_string(),
            name: "Reasoning effort".to_string(),
            description: None,
            category: Some("mode".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current,
                options: effort_opts,
                groups: Vec::new(),
            }),
            recommended_value: None,
        });
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Emit the picker a Grok session establishment built (grok's selectors are
/// synthesized as `Info` directly rather than mapped from schema
/// `SessionConfigOption`s), folded with the model catalog broadcast that came in
/// while it was being built, if one did.
///
/// The broadcast can land anywhere in an establishment — after the handshake
/// answered but before this emit, for one, when applying the saved preferences
/// awaits `session/set_model` — and the handshake may predate the catalog it
/// brings, so it waits in `SessionState::grok_catalog_broadcast`. It is taken
/// under the emit's own lock, so a broadcast either lands before (and is folded
/// in here) or after (and [`GrokModelCatalogBroadcasts`] folds it into this
/// picker), never in between.
async fn emit_grok_established_picker(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    mut opts: Vec<SessionConfigOptionInfo>,
) {
    emit_with_state_built(state, emitter, move |s| {
        if let Some(catalog) = s.grok_catalog_broadcast.take() {
            let specs = s.grok_model_specs.get_or_insert_with(HashMap::new);
            fold_grok_catalog_specs(specs, &catalog.specs);
            fold_grok_catalog_into_picker(&mut opts, &catalog, specs);
        }
        Some(AcpEvent::SessionConfigOptions {
            config_options: opts,
        })
    })
    .await;
}

/// Switch Grok's active model — and, optionally, its reasoning effort — via the
/// `session/set_model`, a method the stable ACP schema no longer carries (the
/// unstable session-model API was dropped in favour of config options), so it
/// goes out as an `UntypedMessage` — grok still implements it.
///
/// Reasoning effort IS live-settable (verified against grok 0.2.99): a
/// `reasoning_effort` value carried in the request's `_meta.reasoningEffort`
/// (string `low`/`medium`/`high`) is applied on top of the model — grok logs
/// `applying reasoning_effort override from meta` and emits a `model_changed`
/// session notification echoing the effort. Passing `None` leaves the current
/// effort untouched (e.g. a pure model switch). The `~/.grok/config.toml`
/// `default_reasoning_effort` remains the at-birth global default this overrides.
async fn set_grok_model(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    model_id: String,
    reasoning_effort: Option<String>,
) -> Result<(), agent_client_protocol::Error> {
    let params = build_grok_set_model_params(
        session_id.0.as_ref(),
        &model_id,
        reasoning_effort.as_deref(),
    );
    let untyped_req = UntypedMessage::new("session/set_model", params).map_err(|e| {
        agent_client_protocol::util::internal_error(format!("Failed to build set_model request: {e}"))
    })?;
    cx.send_request_to(Agent, untyped_req).block_task().await?;
    Ok(())
}

/// Build the `session/set_model` params. A reasoning-effort override rides in
/// `_meta.reasoningEffort` (the exact key grok's sampling layer reads — verified
/// against 0.2.99); `None` omits `_meta` for a pure model switch.
fn build_grok_set_model_params(
    session_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "sessionId": session_id,
        "modelId": model_id,
    });
    if let Some(effort) = reasoning_effort {
        params["_meta"] = serde_json::json!({ "reasoningEffort": effort });
    }
    params
}

/// Send `_session/steering` (the ACP steering extension) to inject a message
/// into the RUNNING turn. Untyped because it is an extension method the schema
/// has no typed request for. Always opts into the 0.64.0
/// `promptRequired` idle contract; codeg only enables native steering for
/// adapters proven to honor it AND to keep the owning prompt in flight across
/// the steered work (claude-agent-acp 0.65.0 / #958 — see
/// [`synthesize_native_steering`] and `registry::steering_prompt_required_min_version`),
/// but the caller still handles every outcome in case the proof was wrong.
async fn send_steer_request(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    blocks: &[PromptInputBlock],
) -> Result<SteerOutcome, AcpError> {
    let params = build_steer_params(session_id.0.as_ref(), blocks);
    let untyped_req = UntypedMessage::new("_session/steering", params).map_err(|e| {
        AcpError::protocol(format!("Failed to build steering request: {e}"))
    })?;
    let raw = cx
        .send_request_to(Agent, untyped_req)
        .block_task()
        .await
        .map_err(|e| AcpError::protocol(format!("Steering request failed: {e}")))?;
    parse_steer_outcome(&raw)
}

/// Build the `_session/steering` params. The prompt carries the caller's
/// blocks through [`map_prompt_blocks`] — the SAME conversion a
/// `session/prompt` uses — so a steered draft's image attachments reach the
/// adapter in the exact encoding its prompt path already accepts (a plain
/// note is still a single text block, as before). `_meta.steering
/// .idleBehavior = "promptRequired"` opts into the turn-end-race contract: a
/// turn that settled first yields `{outcome:"promptRequired"}` WITHOUT
/// consuming the content, so the host resubmits it through a normal
/// `session/prompt`.
fn build_steer_params(session_id: &str, blocks: &[PromptInputBlock]) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session_id,
        "prompt": map_prompt_blocks(blocks.to_vec()),
        "_meta": { "steering": { "idleBehavior": "promptRequired" } },
    })
}

/// Send `_session/async_task/stop` (the AIR async-task control) to stop one
/// background task. Untyped for the same reason as `_session/steering`: an
/// extension method the schema has no typed request for.
///
/// Returns the adapter's `stopped` flag. A missing/non-boolean field reads as
/// `false` rather than raising: the adapter answers `{stopped:false}` for a task
/// it declines to stop, so "no clear yes" and "no" call for the same handling,
/// and a transport-level failure is already an `Err` on the path above.
async fn send_stop_async_task_request(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    task_id: &str,
) -> Result<bool, AcpError> {
    let params = serde_json::json!({
        "sessionId": session_id.0.as_ref(),
        "asyncTaskId": task_id,
    });
    let untyped_req = UntypedMessage::new("_session/async_task/stop", params).map_err(|e| {
        AcpError::protocol(format!("Failed to build async task stop request: {e}"))
    })?;
    let raw = cx
        .send_request_to(Agent, untyped_req)
        .block_task()
        .await
        .map_err(|e| AcpError::protocol(format!("Async task stop request failed: {e}")))?;
    Ok(raw
        .get("stopped")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

/// Parse a `_session/steering` response's top-level `outcome`. Strict on
/// unknowns: a missing or unrecognized outcome is a protocol error, NOT a
/// silent success — the caller must know whether the content was consumed
/// before it decides between "record delivered" and "safe to resend".
fn parse_steer_outcome(raw: &serde_json::Value) -> Result<SteerOutcome, AcpError> {
    match raw.get("outcome").and_then(serde_json::Value::as_str) {
        Some("injected") => Ok(SteerOutcome::Injected),
        Some("promptRequired") => Ok(SteerOutcome::PromptRequired),
        Some("startedNewTurn") => Ok(SteerOutcome::StartedNewTurn),
        other => Err(AcpError::protocol(format!(
            "unexpected _session/steering outcome: {other:?}"
        ))),
    }
}

/// On reconnect, re-apply the user's last-picked Grok model AND reasoning effort
/// (both saved per agent by the frontend and shipped back as preferred config
/// values), reflecting each in its selector's `current_value`. Model is applied
/// first (a pure switch, effort untouched); effort is then re-applied on top of
/// the now-current model via `set_model`'s `_meta.reasoningEffort`.
async fn apply_grok_preferred_options(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    opts: &mut Vec<SessionConfigOptionInfo>,
    preferred_config_values: &BTreeMap<String, String>,
    specs: &HashMap<String, GrokModelSpec>,
) {
    // Model preference — a pure `set_model` (no effort override). On success we
    // also re-point the effort selector at the newly-preferred model (grok ships
    // per-model effort only at birth, never on set_model).
    if let Some(pref) = preferred_config_values.get(GROK_MODEL_OPTION_ID).cloned() {
        // Split the eligibility read (immutable) from the rebuild (mutable) so we
        // never hold a `&mut opts` borrow across `set_grok_effort_selector_for_model`.
        let eligible = opts
            .iter()
            .find(|o| o.id == GROK_MODEL_OPTION_ID)
            .is_some_and(|o| {
                // Grok's selectors are synthesized as selects; any other kind
                // has no model value to compare against.
                let SessionConfigKindInfo::Select(sel) = &o.kind else {
                    return false;
                };
                // Skip if already current, or the saved model is no longer offered.
                sel.current_value != pref && sel.options.iter().any(|x| x.value == pref)
            });
        if eligible {
            match set_grok_model(cx, session_id, pref.clone(), None).await {
                Ok(()) => {
                    if let Some(SessionConfigKindInfo::Select(sel)) = opts
                        .iter_mut()
                        .find(|o| o.id == GROK_MODEL_OPTION_ID)
                        .map(|o| &mut o.kind)
                    {
                        sel.current_value = pref.clone();
                    }
                    if !specs.is_empty() {
                        set_grok_effort_selector_for_model(opts, &pref, specs);
                    }
                }
                Err(e) => tracing::error!(
                    "[ACP] failed to apply preferred grok model '{pref}' on connect: {e}"
                ),
            }
        }
    }
    // Effort preference — re-applied on top of the (possibly just-switched)
    // current model. The effort selector was rebuilt above for that model, so an
    // unsupported model (no selector) or an unoffered value is skipped here.
    if let Some(pref) = preferred_config_values.get(GROK_EFFORT_OPTION_ID) {
        let model_id = current_grok_model_id_from_opts(opts);
        if let Some(SessionConfigKindInfo::Select(sel)) = opts
            .iter_mut()
            .find(|o| o.id == GROK_EFFORT_OPTION_ID)
            .map(|o| &mut o.kind)
        {
            if &sel.current_value != pref && sel.options.iter().any(|o| &o.value == pref) {
                if let Some(model_id) = model_id {
                    match set_grok_model(cx, session_id, model_id, Some(pref.clone())).await {
                        Ok(()) => sel.current_value = pref.clone(),
                        Err(e) => tracing::error!(
                            "[ACP] failed to apply preferred grok effort '{pref}' on connect: {e}"
                        ),
                    }
                }
            }
        }
    }
}

/// The Grok model selector's current value, read from an in-memory options list.
fn current_grok_model_id_from_opts(opts: &[SessionConfigOptionInfo]) -> Option<String> {
    opts.iter()
        .find(|o| o.id == GROK_MODEL_OPTION_ID)
        .and_then(|o| {
            let SessionConfigKindInfo::Select(sel) = &o.kind else {
                return None;
            };
            Some(sel.current_value.clone())
        })
}

/// The Grok model selector's current value, read from the authoritative
/// `SessionState.config_options` snapshot — needed to carry a reasoning-effort
/// override on `session/set_model` (effort is applied relative to a model).
async fn current_grok_model_id(state: &Arc<RwLock<SessionState>>) -> Option<String> {
    let opts = state.read().await.config_options.clone()?;
    current_grok_model_id_from_opts(&opts)
}

/// Context window for the session's CURRENT Grok model. One state read, done at
/// turn start — the answer only changes on a model switch, which cannot happen
/// mid-turn.
///
/// Resolution order mirrors `parsers::grok::build_detail` so the live ring and
/// the re-parsed history never disagree about the denominator: what Grok
/// reported for the model at session establishment, then its on-disk catalog
/// (which is where a BYO endpoint's declared window lives — that one is often
/// absent from the wire), then the id-shaped heuristic.
///
/// `None` is a real outcome, not just "no model selector": a BYO endpoint is
/// keyed by whatever id the user typed (`[model.<id>]` holds the upstream
/// model's name), so an id like `deepseek-chat` can miss the wire, the catalog,
/// and every heuristic family at once. [`grok_window_change_usage`] is what
/// keeps that from stranding a previous model's window on screen.
async fn grok_current_model_context_window(state: &Arc<RwLock<SessionState>>) -> Option<u64> {
    let (model, reported) = {
        let st = state.read().await;
        let model = current_grok_model_id_from_opts(st.config_options.as_deref()?)?;
        let reported = st
            .grok_model_specs
            .as_ref()
            .and_then(|specs| specs.get(&model))
            .and_then(|spec| spec.context_window);
        (model, reported)
    };
    // Lock released: the catalog lookup below touches the filesystem.
    reported.or_else(|| {
        grok_offline_context_window(&model, &crate::parsers::grok::resolve_grok_home_dir())
    })
}

/// The window for `model` when Grok didn't report one on the wire: its on-disk
/// catalog first, then the id-shaped heuristic. Split out of
/// [`grok_current_model_context_window`] so the resolution order is testable
/// against a fixture home instead of the host's real `~/.grok`.
fn grok_offline_context_window(model: &str, grok_home: &Path) -> Option<u64> {
    crate::parsers::grok::grok_catalog_context_window(grok_home, model)
        .or_else(|| crate::parsers::infer_context_window_max_tokens(Some(model)))
}

/// The live pair to re-emit at turn start because the DENOMINATOR moved — the
/// user switched model between turns — or `None` when nothing needs re-keying.
///
/// Two cases, and the second is why this exists at all:
///   * the new model has a window → carry the cumulative count forward onto it,
///     so the ring doesn't keep dividing by the previous model's window until
///     this turn's first token-bearing update;
///   * the new model has NO resolvable window (a BYO id that the wire, the
///     catalog and the heuristic all decline to size) → hand the denominator
///     back with `size: 0`, the frontend's own "unknown window" sentinel
///     (`rawLiveSize > 0 ? … : null`), which drops the ring to the re-parsed
///     session stats. This is what stops the previous model's window from
///     surviving indefinitely on a turn that never reports a token count.
///
/// Note `size: 0` and not `used: 0` — the frontend's `USAGE_UPDATE` reducer
/// *drops* a zero-`used` update whenever it already holds a positive one (a rule
/// that exists for Claude Code's synthetic `/context` responses), so clearing
/// via zeros would strand every attached client on the stale value while the
/// backend snapshot moved on.
fn grok_window_change_usage(window: Option<u64>, last: Option<(u64, u64)>) -> Option<(u64, u64)> {
    let (used, last_size) = last?;
    let size = window.unwrap_or(0);
    (size != last_size).then_some((used, size))
}

/// The `(used, size)` a Grok update moves the context ring to, or `None` when
/// there is nothing new to report.
///
/// Grok emits no ACP `usage_update` at all; it reports a CUMULATIVE token count
/// for the session in the OUTER `params._meta.totalTokens` of ordinary
/// `session/update` notifications (the same number
/// `parsers::grok::GrokTurnMeta` folds into each turn's `usage.input_tokens`).
/// That count IS the context "used", so pairing it with the model's window fills
/// the composer ring live — previously it only moved once a turn had been
/// written to disk and re-parsed.
///
/// `last` suppresses the repeats: the count rides nearly every chunk, but steps
/// only a handful of times per turn. It keys on the WINDOW too, so a model
/// switch still re-emits while the count sits still.
///
/// An unresolvable window reports `size: 0` rather than going silent — the
/// frontend reads that as "unknown window" and sizes the ring from the re-parsed
/// session stats, while the live count still tracks. Going silent instead would
/// freeze whatever pair was last emitted (see [`grok_window_change_usage`]).
fn grok_live_usage_step(
    dispatch: &Dispatch,
    agent_type: AgentType,
    context_window: Option<u64>,
    last: Option<(u64, u64)>,
) -> Option<(u64, u64)> {
    if agent_type != AgentType::Grok {
        return None;
    }
    let size = context_window.unwrap_or(0);
    let Dispatch::Notification(notification) = dispatch else {
        return None;
    };
    let used = notification
        .params
        .get("_meta")?
        .get("totalTokens")?
        .as_u64()
        .filter(|used| *used > 0)?;
    (Some((used, size)) != last).then_some((used, size))
}

/// Route a composer config-option change for Grok. Both live selectors go
/// through `session/set_model`: the model selector switches the model, and the
/// reasoning-effort selector re-sends the current model with an
/// `_meta.reasoningEffort` override (the `~/.grok/config.toml`
/// `default_reasoning_effort` stays the at-birth global default). Re-emits the
/// options with the new `current_value` so the backend snapshot stays authoritative.
///
/// A cross-agent-type switch rejected on an established conversation
/// (`is_grok_incompatible_agent_switch`) is handled in-band: re-emit the
/// authoritative options to revert the composer's optimistic pick and surface a
/// friendly, recoverable `AcpEvent::Error` (localized by the frontend via
/// `GROK_INCOMPATIBLE_AGENT_ERROR_CODE`), returning `Ok` so the caller does not
/// also emit the raw JSON-RPC error. The saved model preference is left intact,
/// so the suggested "start a new session" actually lands on the picked model
/// (a fresh session applies the preference pre-turn, where the switch succeeds).
async fn set_grok_config_option(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    config_id: String,
    value_id: String,
) -> Result<(), agent_client_protocol::Error> {
    // Resolve the `set_model` args for whichever selector changed. A model pick
    // is the model itself (no effort override); an effort pick re-sends the
    // current model carrying the new `_meta.reasoningEffort`. Any other id is a
    // no-op (defensive — the composer only offers these two).
    let (model_id, effort) = if config_id == GROK_MODEL_OPTION_ID {
        (value_id.clone(), None)
    } else if config_id == GROK_EFFORT_OPTION_ID {
        match current_grok_model_id(state).await {
            Some(model_id) => (model_id, Some(value_id.clone())),
            // No model known yet — nothing to carry the effort override on.
            None => return Ok(()),
        }
    } else {
        return Ok(());
    };
    match set_grok_model(cx, session_id, model_id, effort).await {
        Ok(()) => {
            // Built under the emit's own lock, from the picker as it is now: a
            // model catalog broadcast refreshes it from the connection's
            // dispatch task, and re-emitting a list read before that would
            // undo the refresh (see `GrokModelCatalogBroadcasts`).
            emit_with_state_built(state, emitter, |s| {
                let mut opts = s.config_options.clone()?;
                if let Some(SessionConfigKindInfo::Select(sel)) = opts
                    .iter_mut()
                    .find(|o| o.id == config_id)
                    .map(|o| &mut o.kind)
                {
                    sel.current_value = value_id.clone();
                }
                // A MODEL switch must re-point the effort selector at the new
                // model — grok never re-sends per-model effort data on
                // set_model. An EFFORT change leaves the list shape alone; no
                // specs ⇒ leave as-is (flat-fallback session).
                if config_id == GROK_MODEL_OPTION_ID {
                    if let Some(specs) = &s.grok_model_specs {
                        set_grok_effort_selector_for_model(&mut opts, &value_id, specs);
                    }
                }
                Some(AcpEvent::SessionConfigOptions {
                    config_options: opts,
                })
            })
            .await;
            Ok(())
        }
        Err(e) if is_grok_incompatible_agent_switch(&e) => {
            emit_grok_incompatible_agent_switch(state, emitter).await;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Recover from a Grok cross-agent-type model-switch rejection: revert the
/// composer's optimistic selection by re-emitting the authoritative (unchanged)
/// options, then surface a friendly, recoverable error the frontend localizes
/// via `GROK_INCOMPATIBLE_AGENT_ERROR_CODE`. Split out of `set_grok_config_option`
/// so it can be unit-tested without a live ACP connection.
async fn emit_grok_incompatible_agent_switch(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
) {
    // Re-emit the options as they stand, read under the emit's own write lock:
    // a separate read first would race a model catalog broadcast refreshing the
    // picker in between, and re-emit the list from before it. `current_value`
    // is unchanged because the switch never took effect.
    emit_with_state_built(state, emitter, |s| {
        s.config_options
            .clone()
            .map(|config_options| AcpEvent::SessionConfigOptions { config_options })
    })
    .await;
    emit_with_state(
        state,
        emitter,
        AcpEvent::Error {
            message: "Cannot switch to that model in an existing conversation. \
                      Start a new session to use it."
                .to_string(),
            agent_type: AgentType::Grok.to_string(),
            code: Some(GROK_INCOMPATIBLE_AGENT_ERROR_CODE.to_string()),
            details: None,
            // Recoverable: the conversation continues on its current model.
            terminal: false,
        },
    )
    .await;
}

/// Emit the composer's session config-option selectors. For Grok this reads the
/// synthesized `x.ai/sessionConfig` (parity path); for every other agent it runs
/// the standard preference-application + schema-mapping pipeline unchanged.
#[allow(clippy::too_many_arguments)]
async fn apply_and_emit_session_config_options(
    cx: &ConnectionTo<Agent>,
    session: &mut AgentSession,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    grok_meta: Option<&serde_json::Map<String, serde_json::Value>>,
    grok_model_specs: Option<&HashMap<String, GrokModelSpec>>,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
    initial_config_options: Vec<SessionConfigOption>,
) {
    // Every establishment starts from an empty ledger. `SessionState` outlives a
    // fork transition, so without this the child would inherit — and defend —
    // whatever the parent asserted, on paths that never write it back: Grok's
    // dedicated branch below, and the empty-preferences early return inside
    // `apply_preferred_session_options`. See `SessionState::asserted_config_values`.
    state.write().await.asserted_config_values.clear();
    if agent_type == AgentType::Grok {
        let specs = grok_model_specs.cloned().unwrap_or_default();
        if let Some(mut opts) = synthesize_grok_config_options(grok_meta, &specs) {
            // Cache the per-model effort map so a later model switch can rebuild
            // the effort selector for the target model (grok ships it only at
            // session birth, and on a catalog broadcast). `None` when empty
            // keeps the switch path on the flat-fallback branch.
            state.write().await.grok_model_specs = (!specs.is_empty()).then(|| specs.clone());
            // Grok's picker comes from its handshake, not a config-option list:
            // its values are what the agent picked on its own, read before the
            // replay below rewrites them (see
            // `SessionState::agent_chosen_config_values`).
            state.write().await.agent_chosen_config_values = current_config_option_values(&opts);
            let session_id = session.session_id().clone();
            apply_grok_preferred_options(
                cx,
                &session_id,
                &mut opts,
                preferred_config_values,
                &specs,
            )
            .await;
            emit_grok_established_picker(state, emitter, opts).await;
            return;
        }
        // No x.ai/sessionConfig (unexpected): fall through to the standard path,
        // which for Grok emits an empty list (no selectors) — same as before.
    }
    // What the agent picked on its own, read before the replay below rewrites
    // any of it (see `SessionState::agent_chosen_config_values`).
    state.write().await.agent_chosen_config_values =
        current_config_option_values(&map_session_config_options(&initial_config_options));
    let updated = apply_preferred_session_options(
        cx,
        session,
        state,
        emitter,
        preferred_mode_id,
        preferred_config_values,
        initial_config_options,
    )
    .await;
    emit_session_config_options_values(state, emitter, updated).await;
}

/// Grok's initialize still advertises `image: false` — the coding model
/// cannot see pixels. Native ACP `image` blocks nevertheless run its
/// image-describe sidecar. An image-mime `resource` blob does not: grok dumps
/// it as `<file_contents type="binary">` and the model only gets a path.
/// Advertise `image: true` so the composer sends Image blocks.
///
/// Measured live against 0.2.112, 1.0.0 and 1.0.3: every one of them still
/// advertises `image: false`, accepts a native `image` block anyway, and answers
/// correctly about the pixels — while the same bytes as a resource blob make the
/// model invent an answer. The advertisement has simply been wrong for as long
/// as codeg has supported grok, across every version tested, so this is
/// deliberately NOT version-gated (and stays correct if grok ever starts
/// advertising the truth: `image` is already true then). Deliberately no pinned
/// version named here either — `registry.rs` moves on its own schedule, and a
/// claim about "the pinned version" rots at the next bump.
///
/// This bit says only that grok takes image blocks AT ALL — it decodes just some
/// formats, which is a per-mime question a single capability cannot answer, so
/// [`normalize_grok_image_blocks`] settles that separately at dispatch.
fn effective_prompt_capabilities(
    agent_type: AgentType,
    capabilities: &agent_client_protocol::schema::v1::PromptCapabilities,
) -> PromptCapabilitiesInfo {
    PromptCapabilitiesInfo {
        image: capabilities.image || agent_type == AgentType::Grok,
        audio: capabilities.audio,
        embedded_context: capabilities.embedded_context,
    }
}

async fn emit_prompt_capabilities(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    capabilities: &agent_client_protocol::schema::v1::PromptCapabilities,
    agent_type: AgentType,
) {
    emit_with_state(
        state,
        emitter,
        AcpEvent::PromptCapabilities {
            prompt_capabilities: effective_prompt_capabilities(agent_type, capabilities),
        },
    )
    .await;
}

fn resolve_working_dir(working_dir: Option<&str>) -> PathBuf {
    match working_dir {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            }
        }
        None => std::env::current_dir()
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))),
    }
}

fn claude_raw_sdk_session_meta(
    agent_type: AgentType,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }

    let mut claude_code = serde_json::Map::new();
    claude_code.insert(
        "emitRawSDKMessages".to_string(),
        serde_json::Value::Bool(true),
    );

    let mut meta = serde_json::Map::new();
    meta.insert(
        "claudeCode".to_string(),
        serde_json::Value::Object(claude_code),
    );
    Some(meta)
}

/// The client capabilities codeg advertises on Initialize, with per-agent
/// gates. Extracted for testability — each gate is a documented product
/// decision:
///
/// - Everyone, unless `host_tools` says otherwise: filesystem read/write +
///   terminal, for ACP tool execution. Under
///   [`HostToolsPolicy::Agent`] BOTH are withheld, which is not a narrowing of
///   what the agent may do — it moves the doing back inside the agent's own
///   process, where the agent's own OS sandbox and permission rules already
///   apply (#436). Withholding one without the other buys nothing: the agent
///   just reaches the same file through the channel it kept.
/// - Codex only: form elicitation, so codex's native Plan-mode
///   `request_user_input` is delivered as `elicitation/create` (handled by
///   `handle_elicitation_request`) instead of being silently answered `{}`.
///   NOTE this reroutes codex's WHOLE form-elicitation surface — MCP
///   tool-call approvals and MCP-server forms included — so the handler must
///   cover every shape (`classify_elicitation`). URL elicitation is
///   deliberately NOT advertised: codex-acp then falls back to
///   `session/request_permission`, which codeg already handles. Scoped to
///   Codex to keep the blast radius off other agents (e.g. Claude's native
///   AskUserQuestion, which would otherwise un-gate and duplicate the
///   codeg-mcp ask tool).
/// - Claude Code only: `_meta["subagent-transcript"] = true` — opt into
///   claude-agent-acp ≥0.63's subagent transcript forwarding (#881, SDK
///   `forwardSubagentText`). Subagent text/thought chunks then stream with
///   update-level `_meta.claudeCode.parentToolUseId` instead of being
///   filtered; codeg routes them into the live Agent capsule (see
///   `claude_chunk_parent_tool_use_id`). The adapter checks strictly
///   `=== true`, and a pre-0.63 binary ignores the unknown key, so this is
///   inert everywhere it isn't understood.
/// - Any agent that launches `cursor-agent … acp` (the built-in Cursor entry
///   and custom agents wrapping the same binary — see
///   `registry::uses_cursor_acp_backend`): `_meta["parameterizedModelPicker"]
///   = true`. cursor-agent's ACP layer reads exactly this key
///   (`clientSupportsParameterizedModelPicker`, strictly `=== true`) to pick
///   between its two model-picker shapes. Without it the `model` select is the
///   EXPLODED variant list — one row per model×parameter combination, valued
///   by the variant string — and `set_config_option` accepts no other id. With
///   it the picker splits into a `model` select over model names plus one
///   option per model parameter (`fast`, thinking level), which is the only
///   way Composer's Fast switch is reachable. A build that predates the key
///   ignores it and stays on variants, so this is inert where it isn't
///   understood.
fn build_client_capabilities(
    agent_type: AgentType,
    host_tools: HostToolsPolicy,
) -> ClientCapabilities {
    let mut client_capabilities = ClientCapabilities::new();
    if host_tools.hosts_channels() {
        client_capabilities = client_capabilities.terminal(true).fs(
            FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true),
        );
    }
    // Form elicitation is advertised only to agents that are KNOWN to send
    // spec-conformant `elicitation/create` forms `classify_elicitation` can
    // bridge: codex-acp (Plan-mode `request_user_input`, MCP forms/approvals)
    // and deepseek-acp (its `ask_user_question` + plan-review both build
    // standard oneOf/anyOf forms, and decode a free-text answer that is not in
    // the option set as a custom answer — the card's "Other" input round-trips
    // cleanly). Agents without the bit fall back to their own
    // `request_permission` button path.
    if matches!(agent_type, AgentType::Codex | AgentType::DeepSeek) {
        client_capabilities = client_capabilities
            .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::new()));
    }
    // Client `_meta` extensions, per agent. `jetbrains.air` opts codeg into
    // JetBrains AIR typed session-failure records (claude-agent-acp 0.67+,
    // codex-acp 1.2+) — both adapters gate publication on EXACTLY this
    // advertisement (integer version >= 1 and "sessionFailure" in the
    // capabilities array). Advertising REPLACES codex's legacy failure
    // surfaces for the connection (`_meta.codex.error` → TurnRetrying,
    // warning/config-warning text chunks), so this ships together with the
    // `SessionFailure` consumer (`air_session_failure` + the frontend
    // banner). Only the two known AIR speakers get it: the capability-gate
    // convention is to advertise nothing an agent hasn't implemented.
    let mut meta = serde_json::Map::new();
    if agent_type == AgentType::ClaudeCode {
        meta.insert("subagent-transcript".to_string(), serde_json::Value::Bool(true));
    }
    // claude-agent-acp 0.73.0 added "asyncTasks", and codex-acp 1.10.0 joined
    // it, so BOTH are advertised. It publishes the lifecycle of an agent's
    // NON-AGENT background work as `async_task_spawned` / `_progress` /
    // `_state_update`, all on the parent session id. Unlike the two capabilities
    // below, this one adds something codeg cannot get anywhere else: the
    // transcript watcher (`background_watch`) can see that a task was launched
    // but explicitly CANNOT tell a still-running task from one whose CLI died,
    // it never sees workflow/monitor tasks at all (they produce no tool call),
    // and there is no way to stop a task from outside. This channel carries a
    // real terminal edge, a liveness boundary, an output file path, and the
    // `_session/async_task/stop` control. Sub-agent tasks stay out of it by the
    // adapter's own filter (`taskType: "local_agent"` is marked ignored), so
    // advertising this does not disturb the sub-agent surfaces.
    //
    // For claude those tasks are background shells, workflows and monitors; for
    // codex they are BACKGROUND TERMINALS — a shell process the model leaves
    // running past the tool call, which codex core tracks under
    // `thread/backgroundTerminals/*`. codex-acp only announces a terminal that
    // its own `thread/backgroundTerminals/list` still reports as alive, so an
    // ordinary foreground command never appears here.
    //
    // The codex half was captured off a live 1.10.0 over stdio, and the control
    // run (identical prompt, capability withheld) is what settles the trade:
    //
    //   WITHOUT the advertisement — one frame, then silence for the rest of the
    //   connection:
    //     tool_call        {toolCallId:"exec-…", status:"in_progress",
    //                       kind:"execute", title:"sleep 400"}
    //   The turn ends `end_turn` with that call still `in_progress`. There is no
    //   completion, no terminal edge, and nothing that says why.
    //
    //   WITH it, the same run adds:
    //     tool_call_update {toolCallId:"exec-…",
    //                       _meta.jetbrains.air.asyncTasks.backgrounded:true}
    //     async_task_spawned {asyncTaskId:"exec-…", name:"sleep 400",
    //                         taskType:"shell", showInTranscript:false,
    //                         canStop:true, toolCallId:"exec-…"}
    //     …and on `_session/async_task/stop` → {stopped:true}:
    //     async_task_state_update {asyncTaskId:"exec-…", state:"stopped",
    //                              toolCallId:"exec-…"}
    //     tool_call_update {toolCallId:"exec-…", status:"failed", exit_code:-1}
    //
    // Three details of the codex shape that the shared reader already absorbs:
    // `asyncTaskId` EQUALS the `toolCallId` for a root-session task (the adapter
    // only prefixes `<threadId>:` for a sub-agent thread); there is no
    // `description`, `usage` or `outputFilePath`, so the strip row is name-only;
    // and the `backgrounded` marker rides a `_meta` block with NO `version` key,
    // unlike its `sessionFailure` sibling — the frontend's
    // `toolCallMovedToBackground` must not gate on one.
    //
    // The remaining two AIR capabilities are deliberately still out.
    // claude-agent-acp 0.69.0 and codex-acp 1.4.0 added
    // "agentFileChangeReport": advertise it and every prompt may
    // carry `_meta.jetbrains.air.agentFileChangeReportRequest = {version: 1,
    // requestId}`, which the agent answers at the end of the turn on
    // `session_info_update._meta.jetbrains.air.agentFileChangeReport`.
    //
    // The COST half of that decision has now expired on BOTH sides, and the
    // record should say so. 1.4.0–1.11.0 / 0.69.0–0.77.0 answered by running an
    // extra model round-trip (claude: a Stop hook plus a hidden continuation
    // calling `mcp__claude_agent_acp__report_changed_files`; codex: an ephemeral
    // read-only `thread/fork` with a 30s budget). codex-acp 1.12.0 deleted its
    // half — it now buffers the `turn/diff/updated` unified diff for the turn
    // and parses the paths out of it — and claude-agent-acp 0.78.0 deleted the
    // whole audit (server, tool, both hooks) for a checkpoint read,
    // `query.rewindFiles(promptUuid, { dryRun: true })` under a 2s budget. So
    // neither adapter spends a model turn on it any more.
    //
    // It stays out anyway, because the reason that mattered was never cost.
    // Both adapters CLAMP the reported paths to `cwd` + `additionalDirectories`
    // (anything outside a root is dropped as truncated), which is exactly the
    // tree `workspace_state` already watches recursively via `notify`. So the
    // report can only ever name a SUBSET of what the watcher sees, and it
    // truncates at 1024 paths / 256KB. Both deterministic rewrites made that gap
    // WIDER, not narrower, and each says so in its own words: codex 1.12.0
    // hard-codes its `uncertainty` to "Codex turn diffs may omit same-content
    // renames and changes made outside apply_patch, including shell commands,
    // version-control commands, generators, and child processes", and claude
    // 0.78.0 hard-codes `declaredComplete: false` because checkpoints "cover
    // Claude file tools, but not every mutation source (notably Bash and most
    // subagents)" — precisely the changes the model audit they replaced was
    // instructed to go find.
    //
    // Claude's rewrite also introduced two costs the audit never had, both paid
    // by any client that negotiates the report. The adapter flips
    // `enableFileCheckpointing: true` on the SDK for the whole session, so every
    // turn pays snapshot I/O whether or not a report was asked for. And its
    // `settleActive` became async purely to await that bounded preview BEFORE
    // settling the prompt response: on every prompt the client stamps with an
    // `agentFileChangeReportRequest` — which is the point of advertising, so in
    // practice every prompt — the turn's completion now waits on the checkpoint
    // read, up to 2s, including on turns that changed no file at all.
    //
    // The report exists for clients with no filesystem watcher; codeg is not
    // one. Nothing else in either release depends on it, and both adapters no-op
    // without the advertisement, so staying out costs us nothing.
    //
    // codex-acp 1.7.0 and claude-agent-acp 0.73.0 have a third,
    // "nativeSubagentSessions" (the draft ACP subagent RFD; the canonical gate
    // is a `clientCapabilities.subagents: {}` field, with this AIR key as the
    // fallback for SDKs that strip it). It stays out — and the reason is no
    // longer "the schema crate can't deserialize it". `air_async_task_delta`
    // proves a raw pre-dispatch reader gets around that for any variant. The
    // reason is that advertising DELETES the frame codeg renders subagents
    // from, and replaces it with strictly less:
    //
    // * The adapter suppresses the `Agent`/`Task` tool call outright once the
    //   capability is on — `NativeSubagentRuntime.route()` returns null for it,
    //   commented "Native Agent/Task control calls are intentionally not
    //   transcript tools". codex does the same to its `subAgentActivity` item.
    // * That tool call is codeg's whole anchor. `conversation-runtime-store`
    //   registers its id as an "agent" capsule, nests the child's tool calls
    //   under it by `meta.claudeCode.parentToolUseId`, and attaches the child's
    //   prose to it as `agent_transcript`.
    // * `subagent_spawned` cannot replace it: it carries `subagentSessionId`,
    //   `name`, `task` and an empty `capabilities` — and NO `parentToolUseId`.
    //   The child's own updates keep theirs, but it names the tool call that
    //   was just suppressed, so every one of them would arrive as an orphan.
    // * The child's output is not new information either. `route()` rewrites
    //   only `sessionId`; the `_meta.claudeCode.parentToolUseId` codeg already
    //   routes on rides through untouched. So opting in would move the same
    //   content onto a session id codeg would then have to map back — to a
    //   capsule it no longer receives.
    //
    // Net: the advertisement trades a precise rendering for a poorer one plus a
    // connection-level session router, against a spec that is still an
    // UNMERGED draft (agent-client-protocol#1992) whose adapter file calls
    // itself "Temporary typed surface … replaced by SDK exports when the draft
    // ships". Revisit when the draft lands and the announcement carries enough
    // to rebuild the capsule — a parent tool-use id, or the child tool calls
    // arriving with one codeg has seen.
    //
    // "recommendedValue" is the last AIR capability, and codeg takes it from
    // BOTH speakers — claude-agent-acp 0.76.0 and codex-acp 1.11.0 each
    // implement it, so the "advertise nothing an agent hasn't built" rule is
    // satisfied on both sides. With it, `session/new`'s `model` and
    // `effort`/`reasoning_effort` options each gain
    // `_meta.jetbrains.air = {version: 1, recommendedValue: <value id>}`, read
    // by [`air_recommended_value`] and rendered as a "recommended" marker
    // beside the matching row.
    //
    // It is inert without the advertisement (live runs against both adapters
    // over stdio, the capability withheld and then sent: WITHOUT it every
    // option's `_meta` is `null`, byte-identical to the prior pin; WITH it both
    // options carry the block) and purely additive with it — `currentValue` is
    // untouched, so nothing about what is SELECTED changes.
    //
    // The two adapters are worth it for different reasons, and both hold:
    //
    // * codex marks the model it calls `isDefault` and the CURRENT model's
    //   `defaultReasoningEffort`, so the effort recommendation re-derives on a
    //   model switch. That matters most where codeg's own persisted per-agent
    //   preference pins an option: a user who once pinned
    //   `reasoning_effort: max` otherwise has no signal that the model they
    //   just switched to defaults somewhere else.
    // * claude additionally DROPS the ambiguous `default` row from both
    //   selectors, and codeg wants that row gone more than it wants the
    //   marker: `current_model_id_from_opts` reads the model selector's
    //   `current_value`, and that string is what `record_turn_end` stamps on
    //   every journaled turn — on the `default` row it is the literal
    //   `"default"`, a model id no consumer can resolve.
    //
    // See the claude entry in `registry.rs` (k) for the live capture of both
    // shapes, the effort trade that drop accepts, and why the stale-`"default"`
    // preference is healed on the frontend rather than here; and the codex
    // entry (a) for the 1.11.0 wire trace.
    //
    // ⚠️ From claude-agent-acp 0.82.0 / codex-acp 2.0.0 this object does more
    // than name capabilities: its mere PRESENCE makes codeg an "AIR client",
    // and both adapters then send AIR's own tool-call contract (each `_meta`
    // key once, keys moved under `_meta.jetbrains.air`, edit text only in the
    // diff, read/search results withheld). Everything that makes that wire
    // read like the old one lives in `crate::acp::air_contract`; staying out
    // of AIR instead would cost every capability above plus the goal, the
    // permission presentation and the compaction record, which those releases
    // send to AIR clients only.
    //
    // codex alone also gets "diffPatch" (2.0.0): each changed file as ONE
    // exact Git patch. Without it 2.0.0 sends a file change as one ACP diff
    // per hunk — context and changed lines only, no line numbers — where
    // every earlier codex-acp sent the whole old and new file; the patch
    // gives the edit card its real hunk positions back. Taken for codex only:
    // claude sends an Edit/Write patch in the PERMISSION REQUEST, whose
    // `oldText: null` placeholders the permission card would read as a new
    // empty file, and in patch mode it shows a live Write no diff at all until
    // approval. codex's approval request carries no diff (the started tool
    // call already does). Patch blocks are read by `diff_block_payload`.
    //
    // claude-agent-acp 0.89.0 and codex-acp 2.2.0 add "sessionIndex": an
    // indexed `session/list` with archive and rename, and a pushed list
    // subscription. It stays out. It serves a list from inside a running
    // adapter, while codeg reads its list offline from the agents' own
    // transcripts; undeclared, both adapters keep it inert (see the claude
    // entry in `registry.rs`, (lll)). The same releases' "customInstructions",
    // and codex's "codexHooks", are capabilities the AGENT announces; a client
    // uses them without declaring anything, and codeg uses neither (claude
    // (mmm), codex (ff)).
    if matches!(agent_type, AgentType::ClaudeCode | AgentType::Codex) {
        let capabilities: &[&str] = if agent_type == AgentType::Codex {
            &["sessionFailure", "asyncTasks", "recommendedValue", "diffPatch"]
        } else {
            &["sessionFailure", "asyncTasks", "recommendedValue"]
        };
        meta.insert(
            "jetbrains".to_string(),
            serde_json::json!({
                "air": {
                    "version": 1,
                    "capabilities": capabilities,
                }
            }),
        );
    }
    // codex-acp 1.13.0 (#528): with `terminal_output_delta` advertised, a
    // command's completion frame stops repeating its whole aggregated output
    // as `rawOutput`. codeg already takes that output from the deltas it
    // streams (the `hosted_terminal_*` bridge, for EVERY command — codex
    // forwards `outputDelta` for search/listFiles/read too), so the repeat was
    // parsed only to be thrown away. The key it streams under does not change:
    // `resolveTerminalOutputMode` lands on `terminal_output_delta` either way.
    //
    // What it costs, and why that is paid: the same flag drops the
    // `{formatted_output, exit_code}` envelope from the NON-terminal commands
    // as well, and nothing replaces their exit code (`terminal_exit` is only
    // sent for real shell commands). A command that printed something is
    // unaffected — its text already arrived through the bridge. One that
    // printed nothing now completes as a bare status, and the only reader
    // that cared is grep's "No matches": rg exits 1 when nothing matched, so
    // that arrives as a silent `failed`. `isGrepNoMatchResult` (frontend
    // adapter) reads that shape — live `failed`, grep, no output at all — as
    // "no matches", since a real rg failure prints a diagnostic that streams
    // in like any other output.
    //
    // claude-agent-acp 0.81.0 honours the same key (#1150), but there it is the
    // gate for claude's whole terminal `_meta` presentation, which moves shell
    // output off `content` onto a channel codeg does not bridge for claude
    // (see `hosted_terminal_output_key`) — so it stays codex-only.
    if agent_type == AgentType::Codex {
        meta.insert(
            "terminal_output_delta".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    // Cursor ACP gates Composer 2.5's `fast` parameter behind this client
    // capability. Without it the agent advertises only the default variant
    // (Fast); with it the model picker splits into separate `model` and
    // `fast` config options that `session/set_config_option` can set.
    if registry::uses_cursor_acp_backend(agent_type) {
        meta.insert(
            "parameterizedModelPicker".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if !meta.is_empty() {
        client_capabilities = client_capabilities.meta(meta);
    }
    // `clientCapabilities.session`: the Session Notices and Session
    // Compaction RFDs, advertised to the two agents that BUILT them
    // (claude-agent-acp 0.81.0, codex-acp 1.13.0). Both read the members off
    // the raw object (`clientSupportsNotices`: `typeof session.notices ===
    // "object"`; codex's `clientSupportsCompaction`: `session.compaction !=
    // null`), which is exactly what the empty capability structs serialize to.
    //
    // ⚠️ Both members REPLACE a surface codeg already consumes, so neither may
    // be advertised without its consumer:
    // * `notices` outranks the AIR advisory lane (claude publishes its model
    //   fallback advisory only `if (!supportsNotices &&
    //   supportsAirSessionFailures)`; codex's readme-dev states the same
    //   precedence). The consumer shows notices as notifications — see
    //   `session_notice` and the frontend's `lib/session-notices.ts`.
    // * `compaction` makes both adapters STOP sending the
    //   `_meta.contextCompaction` synthetic tool call that
    //   `<ContextCompactionCard>` renders from. The consumer translates
    //   `compaction_update` back into that exact shape, so the card, the
    //   timeline's `"compaction"` render kind and every history parser keep
    //   working unchanged — see `session_compaction_event`.
    //
    // One accepted loss, recorded because it is silent: with `notices` on,
    // claude DROPS its `informational` frames at `level === "info"` (`if
    // (message.level === "info") break;`). Those were plain transcript text.
    // Upstream's reason is that they only show in Claude Code's own transcript
    // mode; `warning`, `notice` and `suggestion` all still arrive.
    if matches!(agent_type, AgentType::ClaudeCode | AgentType::Codex) {
        client_capabilities = client_capabilities.session(
            ClientSessionCapabilities::new()
                .notices(NoticeCapabilities::new())
                .compaction(CompactionCapabilities::new()),
        );
    }
    // Two more client capabilities are deliberately NOT advertised, because each
    // would change a surface codeg reads today with nothing ready to take it
    // over:
    // * `session.configOptions.boolean`. Both adapters answer it by turning
    //   their "Fast mode" option from an On/Off `select` into a `boolean`. The
    //   composer renders boolean toggles (cline sends them unasked), but other
    //   selector surfaces only read selects — the grouped model picker, the
    //   automation and delegation-default editors — and a saved `"on"`
    //   preference would reach `encode_config_option_value` as a boolean
    //   `"on" == "true"`, i.e. `false`: Fast mode silently switched off.
    // * `plan` (`unstable_plan_operations`, not enabled in Cargo.toml either).
    //   codex answers it by streaming a Plan-mode proposal as `plan_update`
    //   instead of the final-answer message chunk the transcript renders the
    //   plan from now; without the feature that variant does not even
    //   deserialize, so the plan would vanish.
    client_capabilities
}

fn build_new_session_request(
    agent_type: AgentType,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
) -> NewSessionRequest {
    let mut req = NewSessionRequest::new(cwd.to_path_buf());
    if let Some(meta) = claude_raw_sdk_session_meta(agent_type) {
        req = req.meta(meta);
    }
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    req
}

fn build_load_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
) -> LoadSessionRequest {
    let mut req = LoadSessionRequest::new(session_id, cwd.to_path_buf());
    if let Some(meta) = claude_raw_sdk_session_meta(agent_type) {
        req = req.meta(meta);
    }
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    req
}

/// Build a `session/resume` request. Mirrors `build_load_session_request`
/// (same fields + ClaudeCode raw-SDK meta + non-empty mcp_servers); the only
/// wire difference is that `ResumeSessionRequest.mcp_servers` is
/// `skip_serializing_if = Vec::is_empty`, so an empty list is omitted from the
/// payload rather than emitted as `[]`.
fn build_resume_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
) -> ResumeSessionRequest {
    let mut req = ResumeSessionRequest::new(session_id, cwd.to_path_buf());
    if let Some(meta) = claude_raw_sdk_session_meta(agent_type) {
        req = req.meta(meta);
    }
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    req
}

/// Send a session-establishing request UNTYPED and parse the reply into its
/// typed response, handing back the raw top-level `models` alongside it.
///
/// That field is the one reason these requests are untyped: grok puts its
/// per-model reasoning-effort data there, and the typed responses have no field
/// for it (the stable schema carries no session-model API), so it has to be read
/// off the raw JSON before the typed parse drops it. Everything else is exactly
/// the typed exchange — `UntypedMessage::new` serializes the very same request,
/// the reply goes through the same serde impl (whose `configOptions` already
/// skips an option kind this build does not know, one entry at a time), and a
/// JSON-RPC error comes back as the same `Err(agent_client_protocol::Error)`
/// with `.code` / `.to_string()` intact. `session/load` needs none of this and is
/// a plain typed send.
///
/// `models` is `None` when the reply carries none. Other agents can send one too
/// (codex-acp reports its model state there), but only grok's is ever read:
/// every caller checks the agent before parsing it.
pub(crate) async fn send_capturing_models<Resp: serde::de::DeserializeOwned>(
    cx: &ConnectionTo<Agent>,
    method: &str,
    params: impl serde::Serialize,
) -> Result<(Resp, Option<serde_json::Value>), agent_client_protocol::Error> {
    let request = UntypedMessage::new(method, params).map_err(|e| {
        agent_client_protocol::util::internal_error(format!(
            "Failed to build {method} request: {e}"
        ))
    })?;
    let raw_response = cx.send_request_to(Agent, request).block_task().await?;
    let models = raw_response.get("models").cloned();
    let response = serde_json::from_value(raw_response).map_err(|e| {
        agent_client_protocol::util::internal_error(format!(
            "Failed to parse {method} response: {e}"
        ))
    })?;
    Ok((response, models))
}

/// Wire-level half of `session/resume`; see [`send_capturing_models`].
async fn send_resume_session(
    cx: &ConnectionTo<Agent>,
    req: ResumeSessionRequest,
) -> Result<(ResumeSessionResponse, Option<serde_json::Value>), agent_client_protocol::Error> {
    send_capturing_models(cx, AGENT_METHOD_NAMES.session_resume, req).await
}

/// Wire-level half of `session/new`; see [`send_capturing_models`].
/// `AgentSession::attach` only consumes `session_id` / `modes` off the result.
async fn send_new_session_capturing_models(
    cx: &ConnectionTo<Agent>,
    req: NewSessionRequest,
) -> Result<(NewSessionResponse, Option<serde_json::Value>), agent_client_protocol::Error> {
    send_capturing_models(cx, AGENT_METHOD_NAMES.session_new, req).await
}

/// Whether MCP servers forwarded over the ACP wire (`session/new.mcpServers`)
/// actually reach the agent's model. Almost all adapters deliver them; pi-acp
/// (0.0.31) accepts the `mcpServers` field but DROPS it — it never forwards MCP
/// to the inner `pi --mode rpc` process, and pi has no native MCP. So forwarding
/// either user servers or the built-in codeg-mcp companion to pi is futile, and
/// injecting codeg-mcp would falsely mark delegation/feedback/ask as available
/// (`feedback_tool_available`, a registered delegation token pi can never use).
/// `supports_mcp` stays `true` for pi (session/new tolerates the field), so this
/// is a separate, narrower gate. Gate codeg-mcp injection on it.
fn agent_delivers_wire_mcp(agent_type: AgentType) -> bool {
    !matches!(agent_type, AgentType::Pi)
}

/// Load MCP servers configured for `agent_type` and convert them into the
/// ACP wire format. Errors and unsupported entries are logged and skipped so
/// a single malformed entry never blocks a session from starting.
fn load_mcp_servers_for_agent(agent_type: AgentType) -> Vec<McpServer> {
    // Hermes, Kimi Code, Grok, and Cursor each read their own native MCP
    // config at launch — Hermes from `~/.hermes/config.yaml` (`mcp_servers`,
    // registered as `mcp-<name>` toolsets), Kimi Code from
    // `~/.kimi-code/mcp.json` (`mcpServers`), Grok from `~/.grok/config.toml`
    // (`[mcp_servers.<name>]`), Cursor from `~/.cursor/mcp.json`
    // (`mcpServers`, shared with the IDE). codeg manages those files directly
    // via the MCP settings UI, so forwarding the same servers over the ACP
    // wire here would double-register them — skip it. (The built-in
    // `codeg-mcp` companion is injected separately by `inject_codeg_mcp`, so
    // it still reaches them.)
    //
    // DeepSeek is deliberately NOT in this set: deepseek-acp reads no MCP file
    // at all, so `$DSH_HOME/mcp.json` (codeg's own store) reaches it ONLY
    // through the wire — skipping it would silently drop every user server.
    //
    // Qoder joins the skip set: the CLI reads `mcpServers` out of its own
    // `~/.qoder/settings.json` (gemini-schema settings file) at startup, which
    // codeg's MCP settings UI manages directly — forwarding the same servers
    // over the wire would double-mount them.
    //
    // Antigravity joins it too, for the same reason with one nuance: its ACP
    // server reads `<GEMINI_HOME>/config/mcp_config.json` (which codeg's MCP
    // settings UI manages) and MERGES it with the wire list BY NAME, wire
    // winning — so forwarding would not actually double-mount. It is skipped
    // anyway because defining one server through two channels is noise, and
    // because the `codeg-mcp` companion is injected separately regardless.
    if matches!(
        agent_type,
        AgentType::Hermes
            | AgentType::KimiCode
            | AgentType::Grok
            | AgentType::Cursor
            | AgentType::Qoder
            | AgentType::Antigravity
    ) {
        return Vec::new();
    }
    let entries = match crate::commands::mcp::read_servers_for_agent_type(agent_type) {
        Ok(map) => map,
        Err(err) => {
            tracing::error!(
                "[ACP][{}] failed to read MCP servers from local config: {err}",
                agent_type
            );
            return Vec::new();
        }
    };

    let mut out = Vec::with_capacity(entries.len());
    for (name, spec) in entries {
        match canonical_spec_to_mcp_server(&name, &spec) {
            Ok(server) => out.push(server),
            Err(err) => {
                tracing::warn!(
                    "[ACP][{}] skip MCP server '{name}' (cannot map to ACP schema): {err}",
                    agent_type
                );
            }
        }
    }
    out
}

/// Context the connection layer needs to inject the built-in `codeg-mcp`
/// MCP entry. Built once per `run_connection` from the live AppState pieces
/// (broker config, token registry, UDS path) and passed through.
///
/// Optional because some test paths spin up `run_connection` without a
/// full delegation stack — those just skip injection.
/// Injection-time lookup of which agents the user has disabled in settings.
///
/// `delegate_to_agent`'s advertised targets must track the live toggle: a
/// disabled agent cannot spawn anyway (`build_session_runtime_env` rejects it
/// inside the delegation spawner), so listing it would only invite doomed
/// calls. Read fresh on every injection — sessions launched before a toggle
/// flip keep their launch-time list, and the spawn-time check stays the hard
/// gate for those.
#[async_trait::async_trait]
pub trait AgentAvailabilityLookup: Send + Sync {
    /// Wire slugs (`AgentType::as_wire`) of the agents disabled in settings.
    async fn disabled_agent_wire_slugs(&self) -> Vec<String>;
}

/// [`AgentAvailabilityLookup`] over the live `AppDatabase`: `agent_setting`
/// rows with `enabled = false`. An absent row means enabled (the settings
/// default). A read error fails OPEN — the enum then lists everything rather
/// than taking the whole companion injection down, and the spawn-time
/// disabled check still enforces.
pub struct DbAgentAvailabilityLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

#[async_trait::async_trait]
impl AgentAvailabilityLookup for DbAgentAvailabilityLookup {
    async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
        match crate::db::service::agent_setting_service::list(&self.db.conn).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| !row.enabled)
                .filter_map(|row| serde_json::from_str::<AgentType>(&row.agent_type).ok())
                .map(|agent_type| agent_type.as_wire().into_owned())
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "[delegation] reading agent settings failed ({e}); \
                     delegate targets will not be filtered this launch"
                );
                Vec::new()
            }
        }
    }
}

#[derive(Clone)]
pub struct DelegationInjection {
    pub broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    pub tokens: Arc<crate::acp::delegation::listener::TokenRegistry>,
    pub socket_path: PathBuf,
    /// Which agents are currently disabled in settings, read at injection
    /// time so `delegate_to_agent` only advertises launchable targets.
    pub agent_availability: Arc<dyn AgentAvailabilityLookup>,
    /// Hot-swappable "is live-feedback enabled?" flag. Read at injection time
    /// alongside the broker's delegation flag so `codeg-mcp` is injected when
    /// EITHER feature is on, and the companion is told which tool groups to
    /// expose. Shares the same `tokens` registry and UDS socket as delegation.
    pub feedback: crate::acp::feedback::FeedbackRuntimeConfig,
    /// Hot-swappable "is ask-user-question enabled?" flag. Read at injection
    /// time alongside delegation + feedback so `codeg-mcp` is injected when ANY
    /// of the three is on, and the companion's `--features` lists `ask` to expose
    /// the `ask_user_question` tool.
    pub ask: crate::acp::question::QuestionRuntimeConfig,
    /// Hot-swappable "is get-session-info enabled?" flag. Read at injection time
    /// alongside the other three so `codeg-mcp` is injected when ANY of the four
    /// is on, and the companion's `--features` lists `sessions` to expose the
    /// `get_session_info` tool. No teardown handle (the lookup is stateless).
    pub sessions: crate::acp::session_info::SessionInfoRuntimeConfig,
    /// Hot-swappable chat-authoring flags (`create_automation` /
    /// `create_work_task`). Read at injection time like the others so the
    /// companion's `--features` lists `automations` / `taskboard`. Unlike the
    /// read-only groups these are ALSO re-read at call time by the authoring
    /// access impl — see [`crate::acp::chat_authoring::ChatAuthoringRuntimeConfig`].
    pub authoring: crate::acp::chat_authoring::ChatAuthoringRuntimeConfig,
    /// Hot-swappable "may agents see the built-in browser?" flag. Read here to
    /// decide whether to advertise the `browser` group, and re-read at call
    /// time by the access impl so switching it off stops the agent that is
    /// already running — see
    /// [`crate::acp::browser_tools::BrowserToolsRuntimeConfig`].
    pub browser: crate::acp::browser_tools::BrowserToolsRuntimeConfig,
    /// Hot-swappable computer-use settings. Read here to decide whether to
    /// advertise the `computer` group, and re-read at call time by the access
    /// impl, like the browser's — see
    /// [`crate::acp::computer_tools::ComputerToolsRuntimeConfig`].
    pub computer: crate::acp::computer_tools::ComputerToolsRuntimeConfig,
    /// Question registry handle for the teardown cascade. The `run_connection`
    /// cleanup guard calls `cancel_questions_by_parent` through this so a pending
    /// `ask_user_question` is reclaimed synchronously on disconnect, mirroring
    /// the delegation `broker.cancel_by_parent` cleanup. Shares the same backing
    /// `ConnectionManager` as the listener's question lookup.
    pub questions: Arc<dyn crate::acp::question::SessionQuestionAccess>,
    /// Plan-approval registry handle for Grok's `exit_plan_mode` ext bridge.
    /// Unlike delegation / ask / feedback this is NOT a codeg-mcp feature — it is
    /// Grok's native plan mode — so it has no runtime on/off flag and is always
    /// wired. Shares the same backing `ConnectionManager` as the question lookup.
    /// The `run_connection` handler registers approvals + parks on the reply
    /// through this, and the cleanup guard calls `cancel_plan_approvals_by_parent`
    /// on disconnect (mirroring the question teardown cascade).
    pub plan_approvals: Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
}

/// Locate the `codeg-mcp` companion binary across the supported deployment
/// shapes:
///
/// 1. `CODEG_MCP_BIN` env override — explicit absolute path. Lets dev shells,
///    custom installs, and integration tests point at a freshly compiled
///    binary without touching the install layout.
/// 2. Sibling of the running executable — the production layout for every
///    shipping target. Tauri sidecar (`Contents/MacOS/codeg-mcp` on macOS,
///    next to `codeg.exe` on Windows, next to the unix binary on Linux
///    deb/rpm), `install.sh`/`install.ps1` (drops `codeg-mcp` next to
///    `codeg-server`), Docker image (`/usr/local/bin/codeg-mcp` next to
///    `codeg-server`), and `cargo build` dev output
///    (`target/<profile>/codeg-mcp`).
/// 3. `PATH` lookup — last-resort for atypical layouts where ops moved the
///    two binaries apart but kept both reachable on `PATH`.
///
/// Returns `None` when no candidate is an executable file. Callers MUST
/// treat `None` as "delegation is unavailable at this site" and skip
/// injection — never paper over with a phantom path, because that fails
/// inside the agent's MCP spawn loop and may take the entire ACP session
/// down on stricter agents.
pub fn locate_codeg_mcp_binary() -> Option<PathBuf> {
    let filename = if cfg!(windows) {
        "codeg-mcp.exe"
    } else {
        "codeg-mcp"
    };

    if let Some(raw) = std::env::var_os("CODEG_MCP_BIN") {
        let candidate = PathBuf::from(raw);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let candidate = dir.join(filename);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    which::which(filename)
        .ok()
        .filter(|p| is_executable_file(p))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Append the built-in `codeg-mcp` MCP entry if delegation is enabled
/// AND the companion binary is present on disk. Returns the per-launch token
/// that was registered, or `None` when injection was skipped (disabled by
/// config, or binary missing).
///
/// When the binary is missing we log a single-line warning and skip
/// injection rather than register the token + emit a phantom McpServerStdio
/// pointing at a non-existent path. Phantom injection would have made every
/// new ACP session ship a guaranteed-to-fail MCP server entry: stricter
/// agents (Claude Code) refuse the whole session; lax agents lose the
/// delegate tool silently. Skipping leaves the agent fully functional minus
/// `delegate_to_agent`, which is the right degradation when codeg-mcp didn't
/// make it into the install.
/// Which tool groups a companion launch should expose. A struct rather than a
/// positional bool list: the groups keep growing and seven adjacent `bool`s at a
/// call site is a silent argument-swap waiting to happen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CompanionFeatureFlags {
    delegation: bool,
    feedback: bool,
    ask: bool,
    sessions: bool,
    /// Per-spawn (task-engine launches only), not a settings toggle — on its own
    /// it still injects the companion so a task session always has its reporting
    /// tools.
    tasks: bool,
    /// `create_automation`, gated by the chat-authoring setting.
    automations: bool,
    /// `create_work_task`, gated by the chat-authoring setting.
    taskboard: bool,
    /// `browser_list_tabs` / `browser_snapshot`, gated by the browser-tools
    /// setting AND by there being a built-in browser at all — the tabs are
    /// native webviews this process owns, which server mode has none of.
    browser: bool,
    /// `browser_eval`, gated by a second setting on top of `browser`. Its own
    /// flag so that turning it on or off does not disturb the rest of the
    /// group, and so that the group being on never implies it.
    browser_eval: bool,
    /// The read-only `computer_*` tools, gated by the computer-use setting AND
    /// by there being a desktop at all — the windows are the user's screen,
    /// which server mode does not have.
    computer: bool,
    /// `computer_launch_app` / `computer_set_window_frame`, on the person's
    /// own switch on top of `computer`.
    computer_launch: bool,
    /// `computer_clipboard_read` / `computer_clipboard_write`, the same.
    computer_clipboard: bool,
}

/// The `--features` value for a companion launch, or `None` when no group is
/// enabled (the companion isn't injected at all). Pulled out as a pure function
/// so the inject/skip decision is unit-testable without a real binary on disk or
/// a live broker. The order here is the order the companion's
/// `CompanionFeatures::parse` recognizes.
fn companion_features_arg(flags: CompanionFeatureFlags) -> Option<String> {
    let mut features: Vec<&str> = Vec::new();
    if flags.delegation {
        features.push("delegation");
    }
    if flags.feedback {
        features.push("feedback");
    }
    if flags.ask {
        features.push("ask");
    }
    if flags.sessions {
        features.push("sessions");
    }
    if flags.tasks {
        features.push("tasks");
    }
    if flags.automations {
        features.push("automations");
    }
    if flags.taskboard {
        features.push("taskboard");
    }
    if flags.browser {
        features.push("browser");
    }
    // Only ever alongside `browser`: the companion requires both, and a
    // `--features browser_eval` on its own would be a line in an agent's MCP
    // config that reads as if it granted something.
    if flags.browser && flags.browser_eval {
        features.push("browser_eval");
    }
    if flags.computer {
        features.push("computer");
    }
    // Only ever alongside `computer`, as `browser_eval` with `browser`.
    if flags.computer && flags.computer_launch {
        features.push("computer_launch");
    }
    if flags.computer && flags.computer_clipboard {
        features.push("computer_clipboard");
    }
    if features.is_empty() {
        return None;
    }
    Some(features.join(","))
}

/// Outcome of injecting the `codeg-mcp` companion: the per-launch token to
/// stash for revocation, plus whether the `check_user_feedback` tool was exposed
/// to this agent (so the session can gate submit + UI on its real capability).
struct CompanionInjection {
    token: String,
    feedback_available: bool,
    /// Whether the `delegate_to_agent` tool group was exposed this launch.
    delegation_enabled: bool,
}

async fn inject_codeg_mcp(
    servers: &mut Vec<McpServer>,
    injection: &DelegationInjection,
    parent_connection_id: &str,
    working_dir: &Path,
    tasks_enabled: bool,
    host_tools: HostToolsPolicy,
) -> Option<CompanionInjection> {
    inject_codeg_mcp_with_binary_locator(
        servers,
        injection,
        parent_connection_id,
        working_dir,
        tasks_enabled,
        host_tools,
        locate_codeg_mcp_binary,
    )
    .await
}

async fn inject_codeg_mcp_with_binary_locator<F>(
    servers: &mut Vec<McpServer>,
    injection: &DelegationInjection,
    parent_connection_id: &str,
    working_dir: &Path,
    tasks_enabled: bool,
    host_tools: HostToolsPolicy,
    locate_binary: F,
) -> Option<CompanionInjection>
where
    F: FnOnce() -> Option<PathBuf>,
{
    // codeg-mcp carries BOTH the delegation tools and the live-feedback tool.
    // Inject it when EITHER feature is enabled; the `--features` arg tells the
    // companion which tool groups to expose so a disabled feature's tools never
    // surface to the LLM. (Historically this was gated on delegation alone.)
    // `tasks_enabled` is per-spawn: true only for task-engine launches, which
    // must get their reporting tools regardless of the settings toggles.
    let feedback_enabled = injection.feedback.is_enabled().await;
    let authoring = injection.authoring.snapshot().await;
    // Delegation is a THIRD door into the same room as `fs/*` and `terminal/*`:
    // `delegate_to_agent` has codeg spawn a second agent — in codeg's process
    // tree, under ITS own (by default `Default`) policy — and hand its output
    // back. A sandboxed agent that cannot read `.env` itself would just ask a
    // sibling to read it. That defeats the boundary this switch advertises, for
    // the same reason withholding only one of fs/terminal would, so `agent`
    // withholds this group too. The other groups stay: they surface codeg's own
    // state (feedback, ask, session info, task reporting), not arbitrary file
    // or command execution on the user's machine.
    let delegation_configured = injection.broker.config_snapshot().await.enabled;
    let delegation_enabled = delegation_configured && host_tools.hosts_channels();
    if delegation_configured && !delegation_enabled {
        // The one combination that looks like a bug from the settings UI: the
        // multi-agent switch reads "on" and the tools are still absent. Say so
        // once per launch, naming the knob, so the log answers the question the
        // user actually asks ("why did delegate_to_agent disappear?").
        tracing::info!(
            "[delegation] multi-agent delegation is enabled in settings but withheld from \
             connection {parent_connection_id}: {HOST_TOOLS_ENV}=agent hands fs/terminal back \
             to this agent, and delegate_to_agent would route the same work through codeg \
             anyway. Turn that per-agent switch off to restore the delegation tools."
        );
    }
    // Which agents the user switched off, so the companion's advertised enum
    // tracks the live toggle. One indexed query, skipped outright when
    // delegation is off, and it fails open: the spawn-time disabled check is
    // the hard gate either way.
    let disabled = if delegation_enabled {
        injection.agent_availability.disabled_agent_wire_slugs().await
    } else {
        Vec::new()
    };
    let flags = CompanionFeatureFlags {
        delegation: delegation_enabled,
        feedback: feedback_enabled,
        ask: injection.ask.is_enabled().await,
        sessions: injection.sessions.is_enabled().await,
        tasks: tasks_enabled,
        automations: authoring.automations_enabled,
        taskboard: authoring.work_tasks_enabled,
        // `cfg!` rather than a runtime probe: a browser tab is a native
        // webview owned by this process, and the server binary has no such
        // thing — what a web user sees in a "browser tab" is an iframe their
        // own browser renders, which nothing here can read. Advertising the
        // tools there would promise a capability that cannot exist, and the
        // agent would find out by being told "no tabs" forever.
        browser: cfg!(feature = "tauri-runtime") && injection.browser.is_enabled().await,
        browser_eval: cfg!(feature = "tauri-runtime") && injection.browser.is_eval_enabled().await,
        // Only where this process serves computer use: the desktop app, and
        // codeg-server where the person who runs it has let it share the
        // screen it runs on (`CODEG_COMPUTER_USE`). Elsewhere there are no
        // windows to offer.
        computer: injection.computer.is_served() && injection.computer.is_enabled().await,
        computer_launch: injection.computer.is_served()
            && injection.computer.is_launch_enabled().await,
        computer_clipboard: injection.computer.is_served()
            && injection.computer.is_clipboard_enabled().await,
    };
    // `None` (no feature enabled) short-circuits BEFORE the binary lookup, the
    // token registration and the server append: there is no companion to launch,
    // and looking for a binary we would never use would also emit the "binary
    // not found" warning below for a connection that asked for nothing.
    let features_arg = companion_features_arg(flags)?;
    let Some(binary_path) = locate_binary() else {
        tracing::warn!(
            "[delegation][WARN] codeg-mcp companion binary not found (checked CODEG_MCP_BIN, \
             exe sibling, and PATH); skipping delegate_to_agent / check_user_feedback / \
             ask_user_question / get_session_info tool injection for connection \
             {parent_connection_id}. Reinstall codeg or set CODEG_MCP_BIN to fix."
        );
        return None;
    };
    // Registered-and-enabled custom agents become extra `delegate_to_agent`
    // targets; disabled BUILT-INS are subtracted companion-side
    // (`--disabled-agents`) so the embedded schema stays the single source of
    // truth for the builtin list and its order. Either flag is omitted when
    // empty, which also keeps an older codeg-mcp binary — one that rejects
    // unknown flags at startup — working for installations needing neither.
    let (custom_slugs, disabled_builtins) = delegate_target_args(&disabled);
    let token = uuid::Uuid::new_v4().to_string();
    injection
        .tokens
        .register(
            token.clone(),
            crate::acp::delegation::listener::TokenEntry {
                parent_connection_id: parent_connection_id.to_string(),
                working_dir: working_dir.to_path_buf(),
            },
        )
        .await;
    let mut server = McpServerStdio::new("codeg-mcp", binary_path.clone());
    let mut args = vec![
        "--parent-connection-id".to_string(),
        parent_connection_id.to_string(),
        "--socket-path".to_string(),
        injection.socket_path.to_string_lossy().to_string(),
        "--token".to_string(),
        token.clone(),
        // Self-cleanup watchdog: codeg-mcp exits when this PID is gone so
        // orphaned companions can't keep the binary file locked across an
        // installer upgrade (Windows) or hold a stale broker connection
        // (any platform).
        "--parent-pid".to_string(),
        std::process::id().to_string(),
        // Tool groups to expose this launch (see `CompanionFeatureFlags`).
        "--features".to_string(),
        features_arg,
    ];
    if !custom_slugs.is_empty() {
        args.push("--custom-agents".to_string());
        args.push(custom_slugs.join(","));
    }
    if !disabled_builtins.is_empty() {
        args.push("--disabled-agents".to_string());
        args.push(disabled_builtins.join(","));
    }
    server = server.args(args);
    servers.push(McpServer::Stdio(server));
    Some(CompanionInjection {
        token,
        feedback_available: feedback_enabled,
        delegation_enabled: flags.delegation,
    })
}

/// Split the delegate-target adjustments into the two companion flags:
/// custom agents to APPEND to `delegate_to_agent`'s enum (the registered set
/// minus the disabled ones), and disabled built-ins for the companion to
/// SUBTRACT from its embedded list. Disabled customs need no subtraction
/// entry — they are simply never appended. The subtraction list is sorted so
/// the arg string is deterministic regardless of settings-row order.
fn delegate_target_args(disabled_wire_slugs: &[String]) -> (Vec<String>, Vec<String>) {
    let disabled: HashSet<&str> = disabled_wire_slugs.iter().map(String::as_str).collect();
    let custom_slugs: Vec<String> = crate::acp::custom_registry::all()
        .iter()
        .map(|a| a.as_wire().into_owned())
        .filter(|slug| !disabled.contains(slug.as_str()))
        .collect();
    let mut disabled_builtins: Vec<String> = disabled_wire_slugs
        .iter()
        .filter(|slug| !slug.starts_with(crate::models::agent::CUSTOM_AGENT_WIRE_PREFIX))
        .cloned()
        .collect();
    disabled_builtins.sort();
    disabled_builtins.dedup();
    (custom_slugs, disabled_builtins)
}

/// Resolve an MCP server `command` to an absolute path.
///
/// The ACP spec requires `McpServerStdio.command` to be an absolute path.
/// Users typically configure bare names like `npx` / `node` / `bunx`; if we
/// forwarded those verbatim, agents would fail to spawn the server. We try
/// `which` first, fall back to the platform-normalized form (which adds
/// `.exe`/`.cmd` on Windows), and finally to the raw input as last resort.
fn resolve_mcp_command(command: &str) -> PathBuf {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Ok(found) = which::which(command) {
        return found;
    }
    PathBuf::from(crate::process::normalized_program(command))
}

fn canonical_spec_to_mcp_server(name: &str, spec: &serde_json::Value) -> Result<McpServer, String> {
    let obj = spec
        .as_object()
        .ok_or_else(|| "spec must be a JSON object".to_string())?;
    let typ = obj
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("stdio");

    match typ {
        "stdio" => {
            let command = obj
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "stdio MCP entry missing 'command'".to_string())?;
            // ACP spec requires an absolute path. If users wrote a bare
            // command (e.g. "npx"), resolve it via PATH so the agent can
            // actually spawn the server. Fall back to the raw value when
            // resolution fails — the agent will surface a clearer error.
            let command_path = resolve_mcp_command(command);
            let mut server = McpServerStdio::new(name, command_path);
            if let Some(args) = obj.get("args").and_then(serde_json::Value::as_array) {
                let args: Vec<String> = args
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !args.is_empty() {
                    server = server.args(args);
                }
            }
            if let Some(env_obj) = obj.get("env").and_then(serde_json::Value::as_object) {
                let env_vars: Vec<agent_client_protocol::schema::v1::EnvVariable> = env_obj
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| agent_client_protocol::schema::v1::EnvVariable::new(k, s)))
                    .collect();
                if !env_vars.is_empty() {
                    server = server.env(env_vars);
                }
            }
            Ok(McpServer::Stdio(server))
        }
        "http" | "sse" => {
            let url = obj
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "remote MCP entry missing 'url'".to_string())?;
            let headers: Vec<HttpHeader> = obj
                .get("headers")
                .and_then(serde_json::Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| HttpHeader::new(k, s)))
                        .collect()
                })
                .unwrap_or_default();
            if typ == "http" {
                let mut server = McpServerHttp::new(name, url);
                if !headers.is_empty() {
                    server = server.headers(headers);
                }
                Ok(McpServer::Http(server))
            } else {
                let mut server = McpServerSse::new(name, url);
                if !headers.is_empty() {
                    server = server.headers(headers);
                }
                Ok(McpServer::Sse(server))
            }
        }
        other => Err(format!("unsupported MCP transport type '{other}'")),
    }
}

/// The main ACP connection loop.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "connection",
    skip_all,
    fields(
        connection_id = %connection_id,
        agent_type = ?agent_type,
        working_dir = ?working_dir,
        session_id = ?session_id,
    )
)]
async fn run_connection(
    agent: AcpAgent,
    connection_id: String,
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
    mut cmd_rx: mpsc::Receiver<ConnectionCommand>,
    emitter: EventEmitter,
    state: Arc<RwLock<SessionState>>,
    terminal_base_env: BTreeMap<String, String>,
    terminal_shell_config: TerminalShellRuntimeConfig,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    fs_policy: FsAccessPolicy,
    host_tools: HostToolsPolicy,
    // Connection-scoped agent stderr buffer, shared with the `with_debug`
    // callback installed by `build_agent`. Read only when a turn ends without
    // agent output, to attach evidence to the synthesized error.
    stderr_tail: Arc<StderrTail>,
) -> Result<(), AcpError> {
    let pending_perms: PendingPermissions =
        Arc::new(tokio::sync::Mutex::new(PermissionQueue::default()));
    // `terminal_base_env` already filtered to just the credential helper
    // keys upstream — see `spawn_agent_connection` for the rationale and
    // why we don't forward the full agent runtime_env here.
    let cwd = resolve_working_dir(working_dir.as_deref());
    // Default terminals to the session working directory so an agent that calls
    // `terminal/create` without a `cwd` (e.g. CodeBuddy) runs in the folder the
    // conversation runs in rather than codeg's own process cwd.
    // An agent that runs `pnpm dev` through `terminal/create` has started a
    // local server the same way a person in the terminal panel has, and that
    // output is the only place its address appears. A connection with no real
    // window behind it (`work_task`, the delegation probe) still watches; the
    // event it emits names that window and no workspace answers to it.
    let terminal_runtime = Arc::new(
        TerminalRuntime::with_base_env(terminal_base_env)
            .with_default_cwd(Some(cwd.clone()))
            .with_default_shell_config(terminal_shell_config)
            .with_service_watch(Some(crate::browser::services::ServiceWatch::new(
                emitter.clone(),
                state.read().await.owner_window_label.clone(),
                crate::browser::types::ServiceSource::Agent,
            ))),
    );
    let cwd_string = cwd.to_string_lossy().to_string();
    // The connection's security posture in one place, so what a live session
    // actually enforces is readable from the log rather than inferred.
    tracing::info!(
        "[ACP] fs policy {} | host tools {}",
        fs_policy.describe(),
        host_tools.describe()
    );
    // `strict` reads as a containment boundary and is not one while codeg also
    // advertises `terminal`: an agent refused a read just `cat`s the file
    // through the shell codeg runs for it (empirically what grok does). Say so
    // rather than letting the knob's name do the promising.
    if fs_policy.confines_reads() && host_tools.hosts_channels() {
        tracing::warn!(
            "[ACP] {FS_POLICY_ENV} confines the fs channel but codeg still advertises \
             `terminal`, so an agent reaches the same paths through a shell — this is \
             not a containment boundary. Set {HOST_TOOLS_ENV}=agent to hand file access \
             and commands back to the agent, where its own sandbox applies."
        );
    }
    let file_system_runtime = Arc::new(FileSystemRuntime::with_policy(fs_policy));

    let conn_id = connection_id.clone();
    let emitter_clone = emitter.clone();
    let perms = pending_perms.clone();
    let state_outer = Arc::clone(&state);

    // Shared question access + feature toggle (both live on the delegation
    // injection) for the agents that ask NATIVELY, over a blocking request of
    // their own rather than the codeg-mcp tool: grok's `_x.ai/ask_user_question`
    // ext request (verified against 0.2.101) and pi's extension-UI `select`,
    // which pi-acp folds into `session/request_permission`. Both handlers
    // register the questions through the SAME interactive-card pipeline and
    // answer the blocked request once the user submits. `None` when the
    // companion isn't injected — the handlers then leave the agent on its
    // pre-bridge rendering.
    let native_ask_access = delegation_injection
        .as_ref()
        .map(|inj| (Arc::clone(&inj.questions), inj.ask.clone()));
    let grok_ask_conn_id = connection_id.clone();
    // Grok `exit_plan_mode` bridge access — always wired in production (native
    // plan mode, no feature flag). `None` only on the test paths that spin up
    // `run_connection` without a delegation stack; the handler then replies
    // disconnect and grok keeps plan mode active.
    let grok_plan_access = delegation_injection
        .as_ref()
        .map(|inj| Arc::clone(&inj.plan_approvals));
    let grok_plan_conn_id = connection_id.clone();
    // The ext handler emits the answered in-stream card (`AskQuestionResultCard`)
    // itself once the user submits — grok never emits a completed tool result into
    // the ACP stream — so it needs this connection's session state + emitter.
    let grok_ask_state = Arc::clone(&state);
    let grok_ask_emitter = emitter.clone();

    // Claude-only: tail this connection's session transcript for OUT-OF-TURN
    // activity (async sub-agent / background-shell completions, the agent's
    // continued work after them, cron//loop autonomous turns — none of which
    // the wire reliably represents) and surface it as `BackgroundActivity`
    // events; also feeds the keep-alive accounting that exempts the
    // connection from the idle sweeps while such work is pending. Created
    // HERE — per CONNECTION, not per conversation loop — so ONE watcher (and
    // one prompt ledger) spans fork restarts: `run_watch` observes the
    // session-id change and re-arms in place, carrying still-outstanding
    // tasks and settled ids across the fork (a post-fork `SendMessage`
    // resume must re-arm the keep-alive). The guard aborts the watcher when
    // this connection ends. Its spawn epoch (captured before the session
    // exists) is what lets the first arm process records written before the
    // transcript file is discovered.
    let prompt_ledger = background_watch::PromptLedger::shared();
    let _bg_watch = background_watch::spawn_if_claude(
        &connection_id,
        agent_type,
        Arc::clone(&state),
        emitter.clone(),
        cwd_string.clone(),
        Arc::clone(&prompt_ledger),
    );

    Client
        .builder()
        .name("codeg")
        // First in the chain on purpose: it has to claim a null-`sessionId`
        // message before the runtime can park it for retry. See the type docs.
        .with_handler(ClaimNullSessionIds)
        // Grok's model catalog broadcast names no session, so no session
        // router ever claims it. See the type docs.
        .with_handler(GrokModelCatalogBroadcasts {
            agent_type,
            state: Arc::clone(&state),
            emitter: emitter.clone(),
        })
        .on_receive_request(
            {
                let emitter_inner = emitter_clone.clone();
                let perms = perms.clone();
                let perm_cwd = cwd_string.clone();
                let state_inner = Arc::clone(&state);
                // pi routes its `ctx.ui.select` questions through this channel;
                // diverting them needs the shared question access (and the ask
                // feature toggle).
                let perm_ask_access = native_ask_access.clone();
                let perm_conn_id = connection_id.clone();
                async move |req: RequestPermissionRequest,
                            responder: Responder<RequestPermissionResponse>,
                            cx: ConnectionTo<Agent>| {
                    // An approval gating codeg's OWN ask tool is a dialog asking
                    // permission to show a dialog; allow it so the user sees only
                    // the interactive question card (see
                    // `codeg_ask_auto_allow_option`).
                    let responder =
                        match try_auto_allow_codeg_ask(&perm_ask_access, &req, responder).await {
                            Ok(()) => return Ok(()),
                            Err(responder) => responder,
                        };
                    // pi asks the user a question THROUGH this channel (see
                    // `try_bridge_pi_select_ask`); route it to the interactive
                    // question card instead of an approval card. Every reject
                    // path hands the responder back, so the request continues
                    // down the normal permission path untouched.
                    let responder = match try_bridge_pi_select_ask(
                        &perm_ask_access,
                        &perm_conn_id,
                        &state_inner,
                        &emitter_inner,
                        &perms,
                        &req,
                        responder,
                    )
                    .await
                    {
                        Ok(()) => return Ok(()),
                        Err(responder) => responder,
                    };
                    handle_permission_request(
                        &cx,
                        &state_inner,
                        &emitter_inner,
                        &perms,
                        &perm_cwd,
                        agent_type,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = file_system_runtime.clone();
                async move |req: ReadTextFileRequest,
                            responder: Responder<ReadTextFileResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "fs/read_text_file");
                    }
                    respond_file_system_request(responder, runtime.read_text_file(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = file_system_runtime.clone();
                async move |req: WriteTextFileRequest,
                            responder: Responder<WriteTextFileResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "fs/write_text_file");
                    }
                    respond_file_system_request(responder, runtime.write_text_file(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: CreateTerminalRequest,
                            responder: Responder<CreateTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "terminal/create");
                    }
                    respond_terminal_request(responder, runtime.create_terminal(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: TerminalOutputRequest,
                            responder: Responder<TerminalOutputResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "terminal/output");
                    }
                    respond_terminal_request(responder, runtime.terminal_output(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: WaitForTerminalExitRequest,
                            responder: Responder<WaitForTerminalExitResponse>,
                            cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        // Refuse INLINE, before the spawn below: there is no
                        // terminal to wait on, so answering immediately is
                        // correct and keeps the refusal off the spawn path.
                        return refuse_unadvertised_channel(responder, "terminal/wait_for_exit");
                    }
                    // `terminal/wait_for_exit` blocks until the command exits,
                    // and the ACP runtime awaits request handlers INSIDE its single
                    // dispatch loop ("the loop awaits the handler to completion
                    // before processing the next message"). Answering inline
                    // therefore freezes the ENTIRE connection for a command
                    // that never exits — an agent that backgrounds a dev server
                    // and then monitors it (grok does exactly this) would stall
                    // the turn forever, with every later session/update stuck
                    // unprocessed in the transport queue.
                    //
                    // Answer from a spawned task instead — the runtime's own sanctioned
                    // escape hatch. `cx.spawn` rather than `tokio::spawn` so the
                    // wait is connection-scoped and torn down with it.
                    let runtime = runtime.clone();
                    cx.spawn(async move {
                        let result = runtime.wait_for_terminal_exit(req).await;
                        if let Err(err) = respond_terminal_request(responder, result) {
                            // Propagating this would tear down the whole
                            // connection, and a failed send only means the peer
                            // is already gone.
                            tracing::warn!(
                                "[ACP] failed to answer terminal/wait_for_exit: {err}"
                            );
                        }
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: KillTerminalRequest,
                            responder: Responder<KillTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "terminal/kill");
                    }
                    respond_terminal_request(responder, runtime.kill_terminal(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: ReleaseTerminalRequest,
                            responder: Responder<ReleaseTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    if !host_tools.hosts_channels() {
                        return refuse_unadvertised_channel(responder, "terminal/release");
                    }
                    respond_terminal_request(responder, runtime.release_terminal(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = native_ask_access.clone();
                let conn_id = grok_ask_conn_id.clone();
                let card_state = Arc::clone(&grok_ask_state);
                let card_emitter = grok_ask_emitter.clone();
                async move |req: GrokAskUserQuestionRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_grok_ask_user_question(
                        &access,
                        &conn_id,
                        &card_state,
                        &card_emitter,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = grok_plan_access.clone();
                let conn_id = grok_plan_conn_id.clone();
                async move |req: GrokExitPlanModeRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_grok_exit_plan_mode(&access, &conn_id, req, responder).await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                // Codex `elicitation/create`: question-style requests (Plan
                // mode `request_user_input`, generic MCP forms) bridge into
                // the same ask card as the codeg-mcp ask tool (reusing the ask
                // access + kill switch); approval-style requests (MCP
                // tool-call approvals, message-only confirms) route through
                // the permission card via `pending_perms`.
                let access = native_ask_access.clone();
                let conn_id = grok_ask_conn_id.clone();
                let perms = perms.clone();
                let state_inner = Arc::clone(&state);
                let emitter_inner = emitter_clone.clone();
                async move |req: CodexElicitationRequest,
                            responder: Responder<serde_json::Value>,
                            cx: ConnectionTo<Agent>| {
                    handle_elicitation_request(
                        &cx,
                        &access,
                        &perms,
                        &state_inner,
                        &emitter_inner,
                        &conn_id,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = native_ask_access.clone();
                let conn_id = grok_ask_conn_id.clone();
                let card_state = Arc::clone(&grok_ask_state);
                let card_emitter = grok_ask_emitter.clone();
                async move |req: CursorAskQuestionRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_cursor_ask_question(
                        &access,
                        &conn_id,
                        &card_state,
                        &card_emitter,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = grok_plan_access.clone();
                let conn_id = grok_plan_conn_id.clone();
                async move |req: CursorCreatePlanRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_cursor_create_plan(&access, &conn_id, req, responder).await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CursorUpdateTodosRequest,
                        responder: Responder<serde_json::Value>,
                        _cx: ConnectionTo<Agent>| {
                handle_cursor_update_todos(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CursorTaskRequest,
                        responder: Responder<serde_json::Value>,
                        _cx: ConnectionTo<Agent>| {
                handle_cursor_task(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CursorGenerateImageRequest,
                        responder: Responder<serde_json::Value>,
                        _cx: ConnectionTo<Agent>| {
                handle_cursor_generate_image(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            async move |notif: AuthStatusUpdateNotification, _cx: ConnectionTo<Agent>| {
                handle_auth_status_update(agent_type, notif);
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(agent, async move |cx| -> Result<(), agent_client_protocol::Error> {
            let state = state_outer;
            let agent_name_for_log = registry::get_agent_meta(agent_type).name;

            let init_request = InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(build_client_capabilities(agent_type, host_tools));
            // Bound the Initialize handshake so an outdated / incompatible
            // cached binary that never responds can't leave the frontend
            // stuck on "Connecting...". A healthy agent answers in <1s; we
            // give 60s headroom for cold process startup on slow machines.
            //
            // We cannot carry a structured error code through the ACP Error
            // type, so we tag the timeout with `INIT_TIMEOUT_SENTINEL` and
            // convert it back to `AcpError::InitializeTimeout` in the
            // outer `.map_err(...)` below. The outer layer attaches a
            // stable `code` to the frontend event so it can be localized.
            tracing::info!(
                "[ACP][{agent_name_for_log}] Sending Initialize (protocol={}, timeout=60s)",
                ProtocolVersion::V1
            );
            let init_started = std::time::Instant::now();
            let init_resp = match tokio::time::timeout(
                std::time::Duration::from_secs(60),
                cx.send_request_to(Agent, init_request).block_task(),
            )
            .await
            {
                Ok(Ok(resp)) => {
                    tracing::info!(
                        "[ACP][{agent_name_for_log}] Initialize responded in {:?}",
                        init_started.elapsed()
                    );
                    resp
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        "[ACP][{agent_name_for_log}] Initialize failed in {:?}: {e}",
                        init_started.elapsed()
                    );
                    // An agent that dies on launch fails the handshake with the
                    // runtime's bare EOF error; its exit report says why.
                    if lost_the_connection(&e) {
                        return Err(defer_to_connection_report(e).await);
                    }
                    return Err(e);
                }
                Err(_) => {
                    tracing::error!(
                        "[ACP][{agent_name_for_log}] Initialize TIMED OUT after {:?} \
                         — the agent never answered the handshake. Check the \
                         [stderr] lines above for agent-side errors. For a full \
                         JSON-RPC trace, re-launch with CODEG_ACP_DEBUG=1.",
                        init_started.elapsed()
                    );
                    return Err(agent_client_protocol::util::internal_error(INIT_TIMEOUT_SENTINEL));
                }
            };
            emit_prompt_capabilities(
                &state,
                &emitter_clone,
                &init_resp.agent_capabilities.prompt_capabilities,
                agent_type,
            )
            .await;

            let supports_fork = init_resp
                .agent_capabilities
                .session_capabilities
                .fork
                .is_some();
            let supports_resume = init_resp
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some();
            let supports_close = init_resp
                .agent_capabilities
                .session_capabilities
                .close
                .is_some();
            tracing::info!(
                "[ACP] Agent capabilities: load_session={}, fork={}, resume={}, close={}",
                init_resp.agent_capabilities.load_session,
                supports_fork,
                supports_resume,
                supports_close
            );

            // Native live-feedback steering, synthesized ONCE from three gates
            // so every consumer (the submit split, the snapshot, the frontend)
            // reads a single authoritative bool: (1) the adapter advertises
            // the extension (top-level `_meta`), (2) the registry says this
            // agent type honors the `promptRequired` idle opt-in, and (3) the
            // RUNNING binary proves it via `agent_info.version` — launch
            // prefers a PATH-resolved install over the pinned package, so (2)
            // alone can't vouch for the process on the other end of the pipe.
            // The raw advertisement is deliberately NOT stored: exposing it
            // would tempt the frontend to re-derive eligibility and show the
            // instant channel for adapters (codex) that advertise steering but
            // would detach a turn on the idle race.
            let steering_advertised = init_advertises_steering(init_resp.meta.as_ref());
            let native_steering_available = synthesize_native_steering(
                agent_type,
                init_resp.meta.as_ref(),
                init_resp.agent_info.as_ref(),
            );
            // Same `agent_info.version` proof, for a different shape decision:
            // which generation of codex's `request_user_input` form this
            // connection will receive.
            let codex_user_input_shape =
                codex_user_input_shape(agent_type, init_resp.agent_info.as_ref());
            let steering_policy =
                steering_policy_agent(agent_type, init_resp.agent_info.as_ref());
            tracing::info!(
                "[ACP][{}] steering: advertised={}, agent={:?}, agent_version={:?}, policy={}{}, native={}",
                agent_type,
                steering_advertised,
                init_resp.agent_info.as_ref().map(|i| i.name.as_str()),
                init_resp.agent_info.as_ref().map(|i| i.version.as_str()),
                steering_policy,
                if steering_policy == agent_type { "" } else { " (by agent name)" },
                native_steering_available
            );

            // Goal channel selection, pinned for the whole connection: an
            // adapter advertising the provider-neutral goal extension
            // (`_meta.goal`, claude-agent-acp 0.66+/codex-acp 1.2+) publishes
            // goal snapshots ONLY there — and codex dropped the legacy
            // `_meta.codex.goal` key in the same release, so honoring the
            // advertisement is what keeps GoalCard alive across the bump (see
            // `session_info_goal_value`).
            let neutral_goal_channel = init_advertises_goal(init_resp.meta.as_ref());
            if neutral_goal_channel {
                tracing::info!("[ACP][{}] goal channel: provider-neutral (_meta.goal)", agent_type);
            }
            // Advertised goal-control surface (method + action vocabulary);
            // None keeps the legacy codex method and resolves the vocabulary to
            // the legacy pair — see the SessionState field docs.
            let goal_control = goal_advertised_control(init_resp.meta.as_ref());

            // Whether this agent accepts MCP server entries over the ACP wire
            // (`session/new`'s `mcpServers`). Almost all do; OpenClaw rejects
            // any server entry and fails session creation, so it must receive
            // NONE — neither user-configured servers nor the built-in codeg-mcp
            // companion. A custom agent carries the same flag as a stored
            // declaration the user flips in settings, for the same reason:
            // codeg cannot know whether an arbitrary ACP agent tolerates the
            // field, and one that doesn't fails to connect at all until it is
            // turned off. (The `mcpServers` key itself is always serialized as
            // `[]` by the ACP schema and OpenClaw tolerates the empty list; the
            // gate only guarantees the list stays empty for it.) This is the
            // single chokepoint feeding session/new, session/load, and the
            // load→new fallback, so gating here keeps server entries off the
            // wire on every path. See `AcpAgentMeta::supports_mcp`.
            let agent_supports_mcp = registry::get_agent_meta(agent_type).supports_mcp;

            // Load MCP servers configured for this agent and filter by the
            // capabilities the agent just declared. Stdio is mandatory per
            // ACP spec; HTTP/SSE are gated on `mcp_capabilities.{http,sse}`.
            let mut mcp_servers: Vec<McpServer> = if agent_supports_mcp {
                let mcp_caps = &init_resp.agent_capabilities.mcp_capabilities;
                load_mcp_servers_for_agent(agent_type)
                    .into_iter()
                    .filter(|s| match s {
                        McpServer::Stdio(_) => true,
                        McpServer::Http(server) => {
                            if mcp_caps.http {
                                true
                            } else {
                                tracing::warn!(
                                    "[ACP][{}] skip HTTP MCP server '{}': agent does not advertise mcpCapabilities.http",
                                    agent_type, server.name
                                );
                                false
                            }
                        }
                        McpServer::Sse(server) => {
                            if mcp_caps.sse {
                                true
                            } else {
                                tracing::warn!(
                                    "[ACP][{}] skip SSE MCP server '{}': agent does not advertise mcpCapabilities.sse",
                                    agent_type, server.name
                                );
                                false
                            }
                        }
                        _ => false,
                    })
                    .collect()
            } else {
                tracing::info!(
                    "[ACP][{}] supports_mcp=false: skipping all MCP wire forwarding (user servers + codeg-mcp companion)",
                    agent_type
                );
                Vec::new()
            };

            // Inject the built-in `codeg-mcp` MCP server. Stdio is
            // unconditionally supported by the ACP wire — no `mcp_caps`
            // filter needed. The returned token is stashed on the session
            // state so connection teardown can revoke it. Skipped entirely
            // for agents that don't accept MCP over the wire (above).
            let delegate_injection = if agent_supports_mcp && agent_delivers_wire_mcp(agent_type) {
                if let Some(inj) = delegation_injection.as_ref() {
                    // Task-engine launches (owner label "work_task") carry the
                    // task_progress / task_complete tool group.
                    let tasks_enabled =
                        { state.read().await.owner_window_label == "work_task" };
                    inject_codeg_mcp(
                        &mut mcp_servers,
                        inj,
                        &conn_id,
                        &cwd,
                        tasks_enabled,
                        host_tools,
                    )
                    .await
                } else {
                    None
                }
            } else {
                None
            };
            {
                let mut s = state.write().await;
                // Native steering is independent of the MCP companion — set it
                // even when no codeg-mcp is injected (it's exactly the channel
                // that needs no tool; OpenClaw-style `supports_mcp: false`
                // agents could ship it someday).
                s.native_steering_available = native_steering_available;
                s.codex_user_input_shape = codex_user_input_shape;
                s.neutral_goal_channel = neutral_goal_channel;
                // The vocabulary is decided HERE for every adapter, advertising
                // or not — this assignment is what flips it from "unknown" to
                // known, and a client reading the snapshot before it lands must
                // see `None` rather than a legacy pair it would latch (a claude
                // session offering a Pause the adapter rejects).
                let (goal_method, goal_actions) = resolve_goal_control(goal_control);
                if let Some(method) = goal_method {
                    s.goal_control_method = method;
                }
                s.goal_actions = Some(goal_actions);
                if let Some(ref injected) = delegate_injection {
                    s.delegation_token = Some(injected.token.clone());
                    s.delegation_enabled = injected.delegation_enabled;
                    // The agent's actual feedback capability for this session
                    // — the authoritative gate for submit + UI, fixed at
                    // launch.
                    s.feedback_tool_available = injected.feedback_available;
                } else {
                    // Keep a reused/test state fail-closed if companion
                    // injection was skipped; no stale token or delegation
                    // capability may survive.
                    s.delegation_token = None;
                    s.delegation_enabled = false;
                    s.feedback_tool_available = false;
                }
            }

            // Emit fork support capability
            emit_with_state(
                &state,
                &emitter_clone,
                AcpEvent::ForkSupported {
                    supported: supports_fork,
                },
            )
            .await;

            // Emit connected status early so the frontend can show cached
            // selectors and enable sending while the session initialises.
            // Prompts sent before run_conversation_loop are buffered in
            // the cmd_rx channel and processed as soon as the loop starts.
            emit_with_state(
                &state,
                &emitter_clone,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Connected,
                },
            )
            .await;

            if let Some(sid) = session_id {
                // Prefer session/resume when the agent advertises the
                // capability: it restores session context WITHOUT replaying
                // history (which session/load does only for us to drain and
                // discard — the transcript the user sees comes from the disk
                // parser, not the ACP wire). On any non-terminal resume failure
                // we fall through to the session/load block below, so the
                // effective chain is resume → load → new.
                if supports_resume {
                    let resume_req = build_resume_session_request(
                        agent_type,
                        SessionId::new(sid.clone()),
                        &cwd,
                        mcp_servers.clone(),
                    );
                    match send_resume_session(&cx, resume_req).await {
                        Ok((resume_resp, grok_models_raw)) => {
                            let initial_config_options = resume_resp.config_options.clone();
                            let new_resp = NewSessionResponse::new(SessionId::new(sid.clone()))
                                .modes(resume_resp.modes)
                                .config_options(resume_resp.config_options)
                                .meta(resume_resp.meta);
                            let grok_meta = if agent_type == AgentType::Grok {
                                new_resp.meta.clone()
                            } else {
                                None
                            };
                            // Opportunistic: grok may include per-model effort data
                            // on resume; absent ⇒ empty specs ⇒ flat fallback.
                            let grok_model_specs = (agent_type == AgentType::Grok)
                                .then(|| parse_grok_model_specs(grok_models_raw.as_ref()));
                            let mut session = AgentSession::attach(&cx, new_resp)?;

                            // No drain: session/resume does not replay history,
                            // so there is nothing to discard. Any buffered
                            // notification (e.g. an early AvailableCommandsUpdate)
                            // is consumed and forwarded by run_conversation_loop.

                            record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::SessionStarted {
                                    session_id: sid.clone(),
                                },
                            )
                            .await;
                            emit_session_modes(&state, &emitter_clone, session.modes()).await;
                            apply_and_emit_session_config_options(
                                &cx,
                                &mut session,
                                &state,
                                &emitter_clone,
                                agent_type,
                                grok_meta.as_ref(),
                                grok_model_specs.as_ref(),
                                preferred_mode_id.as_deref(),
                                &preferred_config_values,
                                initial_config_options.unwrap_or_default(),
                            )
                            .await;
                            emit_selectors_ready(&state, &emitter_clone).await;

                            let loop_result = run_conversation_loop(
                                &mut session,
                                &conn_id,
                                &emitter_clone,
                                &state,
                                agent_type,
                                &perms,
                                &mut cmd_rx,
                                terminal_runtime.clone(),
                                &cwd_string,
                                supports_fork,
                                &prompt_ledger,
                                delegation_injection.as_ref(),
                                &stderr_tail,
                            )
                            .await;
                            terminal_runtime.release_all_for_session(&sid).await;
                            drop(session);
                            // Explicit return: this arm is NOT in tail position
                            // (the session/load block follows it), so without
                            // `return` a successful resume would fall into
                            // session/load.
                            return handle_fork_or_exit(
                                loop_result,
                                &conn_id,
                                &emitter_clone,
                                &state,
                                agent_type,
                                &perms,
                                &mut cmd_rx,
                                terminal_runtime.clone(),
                                &cwd,
                                &cwd_string,
                                supports_resume,
                                supports_close,
                                &mcp_servers,
                                &prompt_ledger,
                                delegation_injection.as_ref(),
                                &stderr_tail,
                            )
                            .await;
                        }
                        Err(e) => {
                            // resume is unstable and NOT guaranteed equivalent to
                            // session/load, so a resume-specific failure must
                            // never deny a load that might still succeed. EVERY
                            // resume error — ResourceNotFound, "Authentication
                            // required", "Method not found", or anything else —
                            // falls through to the session/load block below,
                            // which already owns all terminal decisions
                            // (SessionLoadFailed for not-found, silent stop for
                            // auth, fallback to session/new otherwise). No
                            // user-facing event is emitted here: load re-derives
                            // the same outcome a moment later, so emitting now
                            // would double up (not-found) or flash a transient
                            // error that self-heals when load succeeds.
                            tracing::warn!(
                                "[ACP] session/resume failed ({e}); falling back to session/load"
                            );
                            // fall through to the session/load block below
                        }
                    }
                }

                // Load existing session via session/load.
                //
                // ACP is explicit that a client MUST NOT send `session/load` to
                // an agent that has not advertised `loadSession` (Zed enforces
                // the same gate). Skipping the RPC lands on exactly the
                // recovery its wire error would have taken — `session/new` plus
                // a `continues_from` link, so a custom agent's conversation
                // still reads as one history — without putting an unsupported
                // method on the wire.
                //
                // Only a declared **false** is trusted. A declared true is not:
                // agents that advertise `loadSession: true` and then answer
                // "Method not found" are real, so the whole error ladder below
                // stays exactly as it was.
                let attempted_load = init_resp.agent_capabilities.load_session;
                let load_result = if attempted_load {
                    let load_req = build_load_session_request(
                        agent_type,
                        SessionId::new(sid.clone()),
                        &cwd,
                        mcp_servers.clone(),
                    );
                    cx.send_request_to(Agent, load_req).block_task().await
                } else {
                    Err(agent_client_protocol::Error::method_not_found()
                        .data("agent does not advertise the loadSession capability"))
                };

                match load_result {
                    Ok(load_resp) => {
                        let initial_config_options = load_resp.config_options.clone();
                        let new_resp = NewSessionResponse::new(SessionId::new(sid.clone()))
                            .modes(load_resp.modes)
                            .config_options(load_resp.config_options)
                            .meta(load_resp.meta);
                        let grok_meta = if agent_type == AgentType::Grok {
                            new_resp.meta.clone()
                        } else {
                            None
                        };
                        let mut session = AgentSession::attach(&cx, new_resp)?;

                        // Drain historical replay notifications from session/load,
                        // but forward AvailableCommandsUpdate to the frontend.
                        //
                        // For a custom agent with no transcript yet — a session
                        // created outside codeg, or one whose recording was
                        // lost — this replay is the ONLY source of its history,
                        // so capture it instead of discarding it. When codeg
                        // already recorded the session live, the replay is a
                        // duplicate and stays drained.
                        let hydrate_from_replay = transcript_dir_for(agent_type).is_some_and(|dir| {
                            !crate::acp_transcript::has_recorded_history(dir, &sid)
                        });
                        if hydrate_from_replay {
                            tracing::info!(
                                "[ACP] hydrating custom agent transcript for {sid} from session/load replay"
                            );
                        }
                        // The header must land BEFORE any replayed entry:
                        // `record_header` is a no-op once the file is non-empty,
                        // so writing it after the drain would leave a hydrated
                        // transcript permanently headerless (no cwd, no start
                        // time, hence no folder in the conversation list).
                        record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                        let mut drained = 0u32;
                        // Cleared if the writer ever stalls: from then on the
                        // drain still runs to completion (the session is not
                        // usable until the replay is consumed) but records
                        // nothing more, so the transcript ends at a line
                        // boundary instead of growing holes.
                        let mut recording = hydrate_from_replay;
                        while let Ok(Ok(dispatch)) = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            session.read_update(),
                        )
                        .await
                        {
                            drained += 1;
                            let h = emitter_clone.clone();
                            let st = Arc::clone(&state);
                            let dispatch = fix_usage_update_nulls(dispatch);
                            // Historical replay: a task announced in a past
                            // session is not running now, and its terminal
                            // edge may never have been recorded. Drop rather
                            // than seed the live strip with zombie rows —
                            // but drop HERE, so it isn't counted as an
                            // update codeg failed to read. Grok persists its
                            // workflow frames too, so they replay the same way.
                            if air_async_task_delta(&dispatch).is_some()
                                || grok_workflow_task_delta(&dispatch, agent_type).is_some()
                            {
                                continue;
                            }
                            // A notice is a live event with no history
                            // position — the RFD says outright that it is
                            // "not part of session history" and that
                            // repeated notices stay independent. Replaying
                            // one would raise a toast for something that
                            // already happened, so drop it on the same
                            // terms as the task deltas above.
                            if session_notice(&dispatch).is_some() {
                                continue;
                            }
                            // Compaction, by contrast, IS history: codex
                            // 1.13.0 replays each persisted compaction as
                            // one completed update in its history position,
                            // which is exactly where the divider belongs.
                            if let Some(event) = session_compaction_event(&dispatch) {
                                emit_with_state(&st, &h, event).await;
                                continue;
                            }
                            let _ = MatchDispatch::new(dispatch)
                                .if_notification(async |notif: SessionNotification| {
                                    if recording {
                                        recording = record_hydrated_update(
                                            agent_type,
                                            &sid,
                                            &notif.update,
                                        )
                                        .await;
                                    }
                                    if matches!(
                                        notif.update,
                                        SessionUpdate::AvailableCommandsUpdate(_)
                                    ) {
                                        // Historical-replay path only
                                        // forwards AvailableCommandsUpdate,
                                        // which never carries tool output or
                                        // tool-call titles — throwaway state
                                        // is fine.
                                        let mut replay_cache =
                                            ToolCallOutputCache::default();
                                        let mut replay_cb_state =
                                            CodeBuddyLiveState::default();
                                        emit_conversation_update(
                                            &st,
                                            &h,
                                            agent_type,
                                            notif.update,
                                            None,
                                            &mut replay_cache,
                                            &mut replay_cb_state,
                                        )
                                        .await;
                                    }
                                    Ok(())
                                })
                                .await
                                .otherwise(async |dispatch| {
                                    // Historical replay: throwaway state,
                                    // mirroring the sibling closure above.
                                    // grok's own ext outcomes are skipped,
                                    // though — a dropped image recorded in a
                                    // past session is not happening now, and
                                    // a compaction card would linger in the
                                    // live state as in-flight content (see
                                    // `grok_ext_notification_skipped_on_replay`).
                                    // So is a claude frame reporting plugin
                                    // failures, which only the live loop can
                                    // tell from a repeat (see
                                    // `claude_plugin_failures_skipped_on_replay`).
                                    // The typed closure above draws the same
                                    // line by forwarding only AvailableCommands.
                                    let mut replay_cb_state =
                                        CodeBuddyLiveState::default();
                                    if !grok_ext_notification_skipped_on_replay(&dispatch, agent_type)
                                        && !claude_plugin_failures_skipped_on_replay(&dispatch)
                                    {
                                        maybe_emit_ext_notification(&st, &h, agent_type, dispatch, &mut replay_cb_state).await;
                                    }
                                    Ok(())
                                })
                                .await;
                        }
                        if drained > 0 {
                            tracing::info!("[ACP] Drained {drained} historical replay notifications");
                        }

                        emit_with_state(
                            &state,
                            &emitter_clone,
                            AcpEvent::SessionStarted {
                                session_id: sid.clone(),
                            },
                        )
                        .await;
                        emit_session_modes(&state, &emitter_clone, session.modes()).await;
                        apply_and_emit_session_config_options(
                            &cx,
                            &mut session,
                            &state,
                            &emitter_clone,
                            agent_type,
                            grok_meta.as_ref(),
                            // `session/load` is a typed send with no raw `models`
                            // capture, so effort stays on the flat fallback.
                            None,
                            preferred_mode_id.as_deref(),
                            &preferred_config_values,
                            initial_config_options.unwrap_or_default(),
                        )
                        .await;
                        emit_selectors_ready(&state, &emitter_clone).await;

                        let loop_result = run_conversation_loop(
                            &mut session,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            terminal_runtime.clone(),
                            &cwd_string,
                            supports_fork,
                            &prompt_ledger,
                            delegation_injection.as_ref(),
                            &stderr_tail,
                        )
                        .await;
                        terminal_runtime.release_all_for_session(&sid).await;
                        drop(session);
                        handle_fork_or_exit(
                            loop_result,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            terminal_runtime.clone(),
                            &cwd,
                            &cwd_string,
                            supports_resume,
                            supports_close,
                            &mcp_servers,
                            &prompt_ledger,
                            delegation_injection.as_ref(),
                            &stderr_tail,
                        )
                        .await
                    }
                    Err(e) => {
                        // session/load failed. Classify it: an unrecoverable
                        // historical session — the agent has no record of it
                        // (ResourceNotFound, -32002) or the agent process/session
                        // died mid-load (Claude 0.58.1 reports this as a -32603
                        // Internal error, not -32002) — is surfaced to the
                        // frontend as SessionLoadFailed so the user can choose
                        // Reload vs New conversation. It is NOT auto-fallen-back
                        // to session/new, which would silently orphan the
                        // historical context (and, on a dead process, fail anyway
                        // and leak a raw protocol error). Every other failure
                        // keeps the session/new fallback below.
                        //
                        // Custom agents are the exception: their history is
                        // codeg's own transcript, not the agent's store, so
                        // "the agent forgot this session" costs nothing the
                        // user can see. Many custom agents keep sessions in
                        // memory only, which would make the banner appear on
                        // every restart of every conversation. They fall
                        // through to session/new instead, and the new
                        // transcript links back to the old one so the history
                        // reads as one conversation.
                        // A pi too old for pi-acp fails every open the same way,
                        // so neither the banner (the session is fine) nor the
                        // session/new fallback (it would fail identically, after
                        // announcing a fresh start) is the right answer.
                        if let Some(tagged) = tag_pi_runtime_outdated(&e, agent_type) {
                            return Err(tagged);
                        }
                        let err_str = e.to_string();
                        let forgotten_session = classify_session_load_error(&e);
                        let recovers_locally =
                            recovers_load_failure_locally(agent_type, forgotten_session);
                        if let Some(code) = forgotten_session.filter(|_| !recovers_locally) {
                            tracing::warn!(
                                "[ACP] session/load failed ({err_str}); surfacing as session_load_failed={code}"
                            );
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::SessionLoadFailed {
                                    session_id: sid.clone(),
                                    message: err_str,
                                    code: code.to_string(),
                                },
                            )
                            .await;
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::StatusChanged {
                                    status: ConnectionStatus::Error,
                                },
                            )
                            .await;
                            return Ok(());
                        }
                        if attempted_load {
                            tracing::warn!(
                                "[ACP] session/load failed ({err_str}), falling back to session/new"
                            );
                        } else {
                            tracing::info!(
                                "[ACP] agent declares no loadSession support; opening a new session \
                                 for {sid} and linking its history instead of calling session/load"
                            );
                        }
                        // Only emit a visible error for unexpected failures;
                        // "Method not found" is expected for agents that don't
                        // support session resume (e.g. Cline).
                        // "Authentication required" is expected for agents whose
                        // credentials have expired (e.g. Gemini CLI) — skip
                        // session/new too since it will also fail.
                        if err_str.contains("Authentication required") {
                            return Ok(());
                        }
                        // An agent that simply forgot a session codeg recorded
                        // itself is the expected steady state after a restart,
                        // not an incident — an error toast on every reopen
                        // would be pure noise.
                        // A load codeg deliberately never sent is not a failure
                        // to report — the capability gate above is the expected
                        // path for agents that don't implement it.
                        if attempted_load && !err_str.contains("Method not found") && !recovers_locally
                        {
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::Error {
                                    message: format!("Failed to load session, starting new: {e}"),
                                    agent_type: agent_type.to_string(),
                                    code: Some(SESSION_LOAD_FALLBACK_ERROR_CODE.to_string()),
                                    details: None,
                                    // Recoverable: we fall through to `session/new`
                                    // below. Connection stays alive.
                                    terminal: false,
                                },
                            )
                            .await;
                        }
                        let (new_resp, grok_models_raw) = send_new_session_capturing_models(
                            &cx,
                            build_new_session_request(agent_type, &cwd, mcp_servers.clone()),
                        )
                        .await
                        .map_err(|e| tag_new_session_failure(e, agent_type, &mcp_servers))?;
                        let fallback_sid = new_resp.session_id.0.to_string();
                        let initial_config_options = new_resp.config_options.clone();
                        let grok_meta = if agent_type == AgentType::Grok {
                            new_resp.meta.clone()
                        } else {
                            None
                        };
                        let grok_model_specs = (agent_type == AgentType::Grok)
                            .then(|| parse_grok_model_specs(grok_models_raw.as_ref()));
                        // Read BEFORE `AgentSession::attach` consumes the response.
                        state.write().await.pi_startup_banner =
                            pi_startup_banner(agent_type, new_resp.meta.as_ref());
                        let mut session = AgentSession::attach(&cx, new_resp)?;
                        // Same conversation, new agent session: link the fresh
                        // transcript to the one the failed load was for, so the
                        // turns codeg already recorded keep rendering.
                        // Awaited: the link must be on disk before
                        // `SessionStarted` goes out, or the session-binding
                        // guard can read an empty chain and split this
                        // conversation into a permanent duplicate.
                        record_transcript_header_continuing(
                            agent_type,
                            &fallback_sid,
                            &cwd.to_string_lossy(),
                            Some(sid.as_str()),
                        )
                        .await;
                        emit_with_state(
                            &state,
                            &emitter_clone,
                            AcpEvent::SessionStarted {
                                session_id: fallback_sid.clone(),
                            },
                        )
                        .await;
                        emit_session_modes(&state, &emitter_clone, session.modes()).await;
                        apply_and_emit_session_config_options(
                            &cx,
                            &mut session,
                            &state,
                            &emitter_clone,
                            agent_type,
                            grok_meta.as_ref(),
                            grok_model_specs.as_ref(),
                            preferred_mode_id.as_deref(),
                            &preferred_config_values,
                            initial_config_options.unwrap_or_default(),
                        )
                        .await;
                        emit_selectors_ready(&state, &emitter_clone).await;

                        let loop_result = run_conversation_loop(
                            &mut session,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            terminal_runtime.clone(),
                            &cwd_string,
                            supports_fork,
                            &prompt_ledger,
                            delegation_injection.as_ref(),
                            &stderr_tail,
                        )
                        .await;
                        terminal_runtime
                            .release_all_for_session(&fallback_sid)
                            .await;
                        drop(session);
                        handle_fork_or_exit(
                            loop_result,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            terminal_runtime.clone(),
                            &cwd,
                            &cwd_string,
                            supports_resume,
                            supports_close,
                            &mcp_servers,
                            &prompt_ledger,
                            delegation_injection.as_ref(),
                            &stderr_tail,
                        )
                        .await
                    }
                }
            } else {
                // Create new session
                let (new_resp, grok_models_raw) = send_new_session_capturing_models(
                    &cx,
                    build_new_session_request(agent_type, &cwd, mcp_servers.clone()),
                )
                .await
                .map_err(|e| tag_new_session_failure(e, agent_type, &mcp_servers))?;
                let sid = new_resp.session_id.0.to_string();
                let initial_config_options = new_resp.config_options.clone();
                let grok_meta = if agent_type == AgentType::Grok {
                    new_resp.meta.clone()
                } else {
                    None
                };
                let grok_model_specs = (agent_type == AgentType::Grok)
                    .then(|| parse_grok_model_specs(grok_models_raw.as_ref()));
                // Read BEFORE `AgentSession::attach` consumes the response.
                state.write().await.pi_startup_banner =
                    pi_startup_banner(agent_type, new_resp.meta.as_ref());
                let mut session = AgentSession::attach(&cx, new_resp)?;
                record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                emit_with_state(
                    &state,
                    &emitter_clone,
                    AcpEvent::SessionStarted {
                        session_id: sid.clone(),
                    },
                )
                .await;
                emit_session_modes(&state, &emitter_clone, session.modes()).await;
                apply_and_emit_session_config_options(
                    &cx,
                    &mut session,
                    &state,
                    &emitter_clone,
                    agent_type,
                    grok_meta.as_ref(),
                    grok_model_specs.as_ref(),
                    preferred_mode_id.as_deref(),
                    &preferred_config_values,
                    initial_config_options.unwrap_or_default(),
                )
                .await;
                emit_selectors_ready(&state, &emitter_clone).await;

                let loop_result = run_conversation_loop(
                    &mut session,
                    &conn_id,
                    &emitter_clone,
                    &state,
                    agent_type,
                    &perms,
                    &mut cmd_rx,
                    terminal_runtime.clone(),
                    &cwd_string,
                    supports_fork,
                    &prompt_ledger,
                    delegation_injection.as_ref(),
                    &stderr_tail,
                )
                .await;
                terminal_runtime.release_all_for_session(&sid).await;
                drop(session);
                handle_fork_or_exit(
                    loop_result,
                    &conn_id,
                    &emitter_clone,
                    &state,
                    agent_type,
                    &perms,
                    &mut cmd_rx,
                    terminal_runtime.clone(),
                    &cwd,
                    &cwd_string,
                    supports_resume,
                    supports_close,
                    &mcp_servers,
                    &prompt_ledger,
                    delegation_injection.as_ref(),
                    &stderr_tail,
                )
                .await
            }
        })
        .await
        .map_err(connection_failure)
}

/// The codeg error a failed connection ends with: the sentinel an inner step
/// tagged the ACP error with picks the kind (and never reaches the user);
/// anything untagged is a protocol error.
fn connection_failure(err: agent_client_protocol::Error) -> AcpError {
    let raw = err.to_string();
    if raw.contains(INIT_TIMEOUT_SENTINEL) {
        AcpError::InitializeTimeout
    } else if raw.contains(PI_RUNTIME_OUTDATED_SENTINEL) {
        AcpError::AgentRuntimeOutdated(pi_runtime_outdated_message())
    } else if raw.contains(AUTH_REQUIRED_SENTINEL) {
        AcpError::agent_auth_required(raw.replace(AUTH_REQUIRED_SENTINEL, ""))
    } else if raw.contains(MCP_SUSPECT_SENTINEL) {
        // Strip the marker so the user sees the agent's own words, then
        // let the frontend append the `supports_mcp` suggestion.
        AcpError::mcp_rejected(raw.replace(MCP_SUSPECT_SENTINEL, ""))
    } else {
        AcpError::protocol(raw)
    }
}

/// Grok's native `ask_user_question` tool issues this ACP ext request
/// (`_x.ai/ask_user_question`) and BLOCKS on the reply — it does NOT go through
/// the codeg-mcp ask tool. Transparent over the raw params object
/// (`{sessionId, toolCallId, questions, mode}`); the fields codeg needs are read
/// by [`crate::acp::question::parse_grok_ext_questions`]. The runtime routes typed
/// handlers on the RAW wire method, so the derive keeps the leading `_`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "_x.ai/ask_user_question", response = serde_json::Value)]
#[serde(transparent)]
struct GrokAskUserQuestionRequest(serde_json::Value);

/// Store the plan-approval responder and render the approval card. Grok's native
/// `exit_plan_mode` tool issues this ACP ext request (`_x.ai/exit_plan_mode`) and
/// BLOCKS on the reply — the agent won't leave plan mode until the user acts.
/// Transparent over the raw params object (`{sessionId, toolCallId, planContent}`);
/// the fields codeg needs are read by
/// [`crate::acp::plan_approval::parse_grok_exit_plan_request`]. The runtime routes typed
/// handlers on the RAW wire method, so the derive keeps the leading `_`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "_x.ai/exit_plan_mode", response = serde_json::Value)]
#[serde(transparent)]
struct GrokExitPlanModeRequest(serde_json::Value);

/// Every codex `elicitation/create` request — `request_user_input` (Plan
/// mode), generic MCP-server forms, MCP tool-call approvals, message-only
/// confirms — arrives here once codeg advertises `elicitation.form`. Like the
/// grok bridge, this takes the raw params object and replies with a raw JSON
/// value (the serialized `CreateElicitationResponse`): the form schemas codex
/// sends carry `_meta` markers (`codex.isSecret`, …) that `question.rs` reads
/// straight off the raw JSON. The runtime has no built-in elicitation handling,
/// so this handler claims the method with no dispatch conflict.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "elicitation/create", response = serde_json::Value)]
#[serde(transparent)]
struct CodexElicitationRequest(serde_json::Value);

/// Bridge grok's native `_x.ai/ask_user_question` ext request into codeg's
/// interactive question card. Grok blocks on the reply, so codeg registers the
/// questions through the shared [`crate::acp::question::SessionQuestionAccess`] —
/// the SAME path the codeg-mcp ask tool uses (it sets `pending_question`,
/// broadcasts `QuestionRequest`, and the `AskQuestionCard` renders) — then answers
/// the ext request with the user's choice, serialized to grok's own format, once
/// they submit. Every early return responds with an error, which makes grok fall
/// back to its inert fire-and-forget rendering — exactly the pre-bridge behavior,
/// so no path here can regress it.
async fn handle_grok_ask_user_question(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    connection_id: &str,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    req: GrokAskUserQuestionRequest,
    responder: Responder<serde_json::Value>,
) {
    let Some((questions, ask_cfg)) = access else {
        let _ = responder.respond_with_internal_error("ask_user_question bridge unavailable");
        return;
    };
    // Same kill switch as the codeg-mcp ask tool: when off, let grok fall back.
    if !ask_cfg.is_enabled().await {
        let _ = responder.respond_with_internal_error("ask_user_question is disabled");
        return;
    }
    let specs = match crate::acp::question::parse_grok_ext_questions(&req.0) {
        Ok(specs) => specs,
        Err(e) => {
            tracing::warn!("[grok ask] rejecting malformed ext request: {e}");
            let _ =
                responder.respond_with_internal_error(format!("invalid ask_user_question: {e}"));
            return;
        }
    };
    // Grok's tool_call_id correlates this ext ask with the (suppressed) native
    // tool_call in the live stream; reuse it so the synthesized result card is the
    // single card for that id. Absent → still answer grok, just skip the card.
    let tool_call_id = req
        .0
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // register_question consumes the specs; keep a copy to render the answered
    // in-stream card once the user submits.
    let card_specs = specs.clone();
    let Some(registered) = questions.register_question(connection_id, specs).await else {
        // Connection gone, or an ask is already pending on this connection.
        let _ = responder.respond_with_internal_error("could not register ask_user_question");
        return;
    };
    // The user answers out-of-band (the HTTP `answer_question` endpoint resolves
    // the one-shot below), so await it on a task — keeping the ACP dispatch loop
    // free — then reply to grok's blocked ext request.
    let state = Arc::clone(state);
    let emitter = emitter.clone();
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(outcome) => {
                // Surface the answered "提问回答" capsule in-stream — the codeg-mcp
                // ask parity grok's native tool never emits into the ACP stream (it
                // resolves the answer over THIS ext round-trip). Emit BEFORE
                // unblocking grok so the card lands ahead of grok's follow-up text;
                // grok is blocked on this reply, so nothing races the emit. The
                // matching raw ask tool_call/updates are suppressed in the live loop
                // (see `grok_ask_tool_ids`), so this synthesized event — keyed by the
                // same id — is the only card for the ask.
                if let Some(tool_call_id) = tool_call_id {
                    emit_with_state(
                        &state,
                        &emitter,
                        AcpEvent::ToolCall {
                            tool_call_id,
                            title: "ask_user_question".to_string(),
                            kind: "other".to_string(),
                            status: "completed".to_string(),
                            content: None,
                            raw_input: Some(
                                crate::acp::question::grok_result_card_input(&card_specs)
                                    .to_string(),
                            ),
                            raw_output: Some(
                                crate::acp::question::grok_result_card_output(&outcome).to_string(),
                            ),
                            locations: None,
                            meta: None,
                            images: None,
                        },
                    )
                    .await;
                }
                let _ = responder.respond(crate::acp::question::build_grok_ext_response(&outcome));
            }
            // Sender dropped: the ask was canceled or the connection tore down —
            // nothing to render; let grok fall back via skip_interview.
            Err(_) => {
                let _ = responder.respond(crate::acp::question::grok_ext_skip_response());
            }
        }
    });
}

/// The option id that silently allows a `session/request_permission` which is
/// only gating codeg's OWN `ask_user_question` companion tool, or `None` to
/// leave the request on the ordinary approval-card path.
///
/// An agent whose permission mode consults the user before every MCP tool call
/// (claude-agent-acp's default) gates the ask tool too, so asking the user a
/// question used to cost TWO dialogs: a raw "run mcp__codeg-mcp__ask_user_question?"
/// approval dumping the questions as JSON, and only after "Yes" the real
/// interactive card. The first one carries no decision the second doesn't — see
/// [`crate::acp::question::is_codeg_ask_tool_name`] for why answering it is the
/// user's consent either way.
///
/// The tool is identified by NAME, read from the two places a host puts it:
/// `toolCall.title` (claude-agent-acp's `toolInfoFromToolUse` falls through to
/// the raw tool name for MCP tools) and the request-level
/// `_meta.permission.title` it pairs with (claude-agent-acp 0.73+ / codex-acp
/// 1.7+, the same block [`hoist_request_permission_meta`] forwards to the card).
/// The ACP `toolCall.name` field would be the exact answer but is UNSTABLE and
/// dropped by the schema crate codeg pins, so it is not available here.
///
/// Only an `allow_once` option is ever selected. An `allow_always` writes a
/// durable permission rule into the user's own agent settings — a decision that
/// outlives this turn and this connection, so it stays theirs to make. With no
/// such option (an agent that offers only "always"), `None` keeps today's card.
fn codeg_ask_auto_allow_option(req: &RequestPermissionRequest) -> Option<String> {
    let permission_title = crate::acp::air_contract::permission_record(req.meta.as_ref())
        .and_then(|p| p.get("title"))
        .and_then(serde_json::Value::as_str);
    let is_ask = [req.tool_call.fields.title.as_deref(), permission_title]
        .into_iter()
        .flatten()
        .any(crate::acp::question::is_codeg_ask_tool_name);
    if !is_ask {
        return None;
    }
    req.options
        .iter()
        .find(|opt| opt.kind == PermissionOptionKind::AllowOnce)
        .map(|opt| opt.option_id.to_string())
}

/// Answer a permission request that is merely gating codeg's own ask tool, so
/// the interactive question card is the only thing the user ever sees.
///
/// `Err(responder)` hands the request back for the ordinary permission path —
/// the outcome for every other tool, and the deliberate fallback whenever the
/// auto-allow cannot be taken (the ask feature is off, or the agent offered no
/// allow-once option).
// `result_large_err`: agent-client-protocol 2.x's `Responder` is past clippy's
// size limit, but this `Err` is the responder handed back once per permission
// request, not an error bubbled through `?` — boxing it would only add an
// allocation.
#[allow(clippy::result_large_err)]
async fn try_auto_allow_codeg_ask(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    req: &RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) -> Result<(), Responder<RequestPermissionResponse>> {
    let Some(option_id) = codeg_ask_auto_allow_option(req) else {
        return Err(responder);
    };
    // Same kill switch as the ask tool itself (and as `try_bridge_pi_select_ask`):
    // with the feature off codeg-mcp never exposed `ask_user_question`, so a
    // request naming it is not the tool this shortcut is allowed to speak for.
    let Some((_, ask_cfg)) = access else {
        return Err(responder);
    };
    if !ask_cfg.is_enabled().await {
        return Err(responder);
    }
    tracing::debug!(
        "[ACP] auto-allowing the permission request for codeg's own ask_user_question tool \
         (option {option_id}); the interactive question card is the actual prompt"
    );
    let _ = responder.respond(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ));
    Ok(())
}

/// Bridge pi's extension-UI `select` — the way a pi extension asks the user a
/// multiple-choice question (`ctx.ui.select`) — into codeg's interactive question
/// card.
///
/// pi has no dedicated ask channel: pi-acp turns the dialog into a plain
/// `session/request_permission` (see
/// [`crate::acp::question::parse_pi_select_ask`] for the wire shape), so codeg
/// used to render it as a generic approval card that dumped the synthetic tool
/// call as raw JSON, and — because pi never emits a `session/update` for that
/// `pi-ui-*` id — left NO record of the answer once the card was dismissed
/// (#644). This registers the choices through the shared
/// [`crate::acp::question::SessionQuestionAccess`] (the SAME path the codeg-mcp
/// ask tool uses), answers the blocked permission request with the option the
/// user picked, and emits the answered `AskQuestionResultCard` into the stream.
///
/// `Err(responder)` hands the request back for the ordinary permission path,
/// which is the outcome for everything that isn't a pi select, for a select the
/// card cannot represent faithfully, and for a connection with an ask already
/// pending — none of those are worse than the pre-bridge behavior.
///
/// A bridged select still parks an abort handle on `perms`, so every permission
/// drain reclaims it exactly as it reclaimed the approval card this replaces —
/// see [`PermissionQueue::detached`] for why that matters for pi specifically.
// Same `Err` hand-back as `try_auto_allow_codeg_ask`; see its allow note.
#[allow(clippy::result_large_err)]
async fn try_bridge_pi_select_ask(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    connection_id: &str,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    req: &RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) -> Result<(), Responder<RequestPermissionResponse>> {
    // Gated on pi's OWN marker rather than `AgentType::Pi`: the id prefix +
    // `method: "select"` + `choice-*` option ids are pi-acp's fingerprint, and
    // that also covers a pi registered under a custom agent id (which does not
    // resolve to `AgentType::Pi`).
    let tool_call_id = req.tool_call.tool_call_id.to_string();
    if !tool_call_id.starts_with(crate::acp::question::PI_EXTENSION_UI_ID_PREFIX) {
        return Err(responder);
    }
    let Some((questions, ask_cfg)) = access else {
        return Err(responder);
    };
    // Same kill switch as the codeg-mcp ask tool: when the feature is off, the
    // approval card stays the way to answer.
    if !ask_cfg.is_enabled().await {
        return Err(responder);
    }
    let tool_call = serde_json::to_value(&req.tool_call).unwrap_or_default();
    let options: Vec<(String, String)> = req
        .options
        .iter()
        .map(|o| (o.option_id.to_string(), o.name.clone()))
        .collect();
    let Some(ask) = crate::acp::question::parse_pi_select_ask(&tool_call, &options) else {
        return Err(responder);
    };
    // Park the abort handle BEFORE registering, never after: both are await
    // points, so a drain can land between them, and a handle parked afterwards
    // would be one the drain never saw — leaving the ask alive across it, which
    // is the wedge this exists to prevent. Ordering it first is what closes the
    // window rather than narrowing it: dropping the sender is STICKY, so a drain
    // in the gap is still observed by the task below on its first poll, which
    // then cancels the question it just registered. (A re-check after parking
    // would not close it — the same reasoning as `PermissionQueue`'s emit-inside-
    // the-lock note.) An early return from here leaves the handle behind with its
    // receiver dropped; `park_detached` sweeps those, and a drain clears them.
    let (abort_tx, abort_rx) = oneshot::channel();
    perms.lock().await.park_detached(abort_tx);
    // `register_question` consumes the spec; keep the ask to map the answer back
    // to pi's option id and to render the answered in-stream card.
    let Some(registered) = questions
        .register_question(connection_id, vec![ask.spec.clone()])
        .await
    else {
        return Err(responder);
    };
    let state = Arc::clone(state);
    let emitter = emitter.clone();
    let questions = Arc::clone(questions);
    let connection_id = connection_id.to_string();
    let question_id = registered.question_id;
    // The user answers out-of-band (the `answer_question` endpoint resolves the
    // one-shot below), so await it on a task — keeping the ACP dispatch loop free
    // — then unblock pi.
    tokio::spawn(async move {
        // `biased` so an answer that lands in the same instant as a drain still
        // counts: the user clicked, and pi can still be told what they picked.
        let answer = tokio::select! {
            biased;
            answered = registered.answer_rx => answered.ok(),
            _ = abort_rx => None,
        };
        match answer {
            Some(outcome) => {
                let option_id = crate::acp::question::pi_select_option_id(&outcome, &ask);
                // Keep the record honest: an answer pi could not be given (a
                // decline, or free text typed into the card's always-present
                // "Other" box) is reported as a decline, which is exactly what
                // pi is about to be told.
                let recorded = if option_id.is_some() {
                    outcome
                } else {
                    crate::acp::question::QuestionOutcome {
                        answers: Vec::new(),
                        declined: true,
                    }
                };
                // The in-stream "提问回答" capsule. pi resolves the answer over
                // THIS permission round-trip and never emits a tool_call for the
                // `pi-ui-*` id, so without this the pick vanishes with the card.
                // Emitted BEFORE unblocking pi so it lands ahead of pi's
                // follow-up output; pi is blocked on the reply, so nothing races.
                emit_with_state(
                    &state,
                    &emitter,
                    AcpEvent::ToolCall {
                        // pi's own id for the dialog, so the one card is keyed
                        // to the request it answers.
                        tool_call_id,
                        title: "ask_user_question".to_string(),
                        kind: "other".to_string(),
                        status: "completed".to_string(),
                        content: None,
                        raw_input: Some(
                            crate::acp::question::grok_result_card_input(std::slice::from_ref(
                                &ask.spec,
                            ))
                            .to_string(),
                        ),
                        raw_output: Some(
                            crate::acp::question::grok_result_card_output(&recorded).to_string(),
                        ),
                        locations: None,
                        meta: None,
                        images: None,
                    },
                )
                .await;
                let outcome = match option_id {
                    Some(option_id) => {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
                    }
                    None => RequestPermissionOutcome::Cancelled,
                };
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
            }
            // Either the ask was canceled outright (turn cancel, teardown) or a
            // permission drain reclaimed it — the connection's "everything
            // waiting on the user is moot now" signal, which for pi also means
            // pi has abandoned the dialog on its side. Nothing to render; clear
            // the ask so the NEXT select can register, and unblock pi with the
            // cancel it would have gotten from the drained permission queue.
            None => {
                questions.cancel_question(&connection_id, &question_id).await;
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
        }
    });
    Ok(())
}

/// Bridge grok's native `_x.ai/exit_plan_mode` ext request into codeg's
/// interactive plan-approval card. Grok BLOCKS on the reply — it won't leave plan
/// mode until the user acts — so codeg registers the approval through the shared
/// [`crate::acp::plan_approval::SessionPlanApprovalAccess`] (which sets
/// `pending_plan_approval`, broadcasts `PlanApprovalRequest`, and renders the card
/// above the composer), then answers the ext request with the user's decision once
/// they submit. Unlike the ask bridge there is no synthesized in-stream card:
/// grok's own `exit_plan_mode` tool_call renders the plan in the transcript (via
/// `PlanModeCard`), mirroring how the permission dialog coexists with the tool
/// call. Every early return replies with the disconnect-shaped response so grok
/// keeps plan mode active — it can never be read as a silent approval.
async fn handle_grok_exit_plan_mode(
    access: &Option<Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>>,
    connection_id: &str,
    req: GrokExitPlanModeRequest,
    responder: Responder<serde_json::Value>,
) {
    // Log the wire SHAPE (top-level field names), not the raw request — the plan
    // body can be large and carry file paths / source. The keys are what
    // wire-format verification needs (confirm `sessionId`/`toolCallId`/`planContent`
    // on the first real run); the malformed path below logs more if parsing fails.
    tracing::info!(
        "[grok exit_plan] received _x.ai/exit_plan_mode ext request: keys={:?}",
        req.0
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>())
    );
    let Some(access) = access else {
        let _ =
            responder.respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
        return;
    };
    let (plan_markdown, tool_call_id) =
        match crate::acp::plan_approval::parse_grok_exit_plan_request(&req.0) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!("[grok exit_plan] rejecting malformed ext request: {e}");
                let _ = responder
                    .respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
                return;
            }
        };
    tracing::info!(
        "[grok exit_plan] toolCallId={tool_call_id:?} plan_chars={}",
        plan_markdown.chars().count()
    );
    let Some(registered) = access
        .register_plan_approval(connection_id, tool_call_id, plan_markdown)
        .await
    else {
        // Connection gone, or an approval is already pending on this connection.
        let _ =
            responder.respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
        return;
    };
    // The user answers out-of-band (the HTTP `answer_plan_approval` endpoint
    // resolves the one-shot below), so await it on a task — keeping the ACP
    // dispatch loop free — then reply to grok's blocked ext request. The manager's
    // `answer_plan_approval` / teardown emit `PlanApprovalResolved` to clear the
    // card; this task only unblocks grok.
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(answer) => {
                let _ = responder.respond(
                    crate::acp::plan_approval::build_grok_exit_plan_response(&answer),
                );
            }
            // Sender dropped: the approval was canceled or the connection tore
            // down — reply disconnect so grok keeps plan mode active.
            Err(_) => {
                let _ = responder
                    .respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
            }
        }
    });
}

/// Bridge codex's `elicitation/create` requests into codeg's interactive
/// surfaces. Codex only sends these when codeg declares `elicitation.form`
/// (see `connect_with`), then BLOCKS on the reply, so every shape must resolve
/// to something the user can act on (see
/// [`crate::acp::question::classify_elicitation`] for the full taxonomy):
///
///   * Question-style (Plan-mode `request_user_input`, generic MCP forms) →
///     the shared [`crate::acp::question::SessionQuestionAccess`] path — the
///     SAME one the codeg-mcp ask tool and the grok bridge use (it sets
///     `pending_question`, broadcasts `QuestionRequest`, and `AskQuestionCard`
///     renders) — answered once the user submits.
///   * Approval-style (MCP tool-call approvals, message-only confirms) → the
///     permission card via `pending_perms`, exactly like the
///     `session/request_permission` fallback codex-acp used before the
///     capability was advertised. Auto-declining these would reject the tool
///     call (including codeg-mcp's own tools in consent-requiring modes).
///
/// Question-path early returns DECLINE, which makes codex proceed with its own
/// judgment — no worse than the pre-bridge `{answers:{}}`, so nothing here can
/// regress it. Codex delivers `request_user_input` ONLY as this elicitation and
/// never puts a completed tool_call on the stream, so — like the grok bridge —
/// the question path synthesizes the answered result card itself once the user
/// submits (keyed by the elicitation's tool_call_id).
/// Bridge Cursor's blocking `cursor/ask_question` into the shared ask card.
/// Same path as [`handle_grok_ask_user_question`]: register, wait off the
/// dispatch loop, reply in Cursor's option-id envelope. Early returns use
/// `skipped` so the agent continues instead of hanging on `-32601`.
async fn handle_cursor_ask_question(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    connection_id: &str,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    req: CursorAskQuestionRequest,
    responder: Responder<serde_json::Value>,
) {
    tracing::info!(
        "[cursor ask] received cursor/ask_question: keys={:?}",
        crate::acp::cursor_ext::param_keys(&req.0)
    );
    // Every early return is `skipped` WITH a reason: cursor's own `skipped`
    // doubles as "the user dismissed the card", and an agent that can't tell
    // that apart from "this host never showed it" will proceed as if the user
    // had a say. The reason text is structural — never any of the payload.
    let Some((questions, ask_cfg)) = access else {
        let _ = responder.respond(crate::acp::cursor_ext::cursor_ask_skip_response_with_reason(
            "the host's question bridge is unavailable; the user was not asked",
        ));
        return;
    };
    if !ask_cfg.is_enabled().await {
        let _ = responder.respond(crate::acp::cursor_ext::cursor_ask_skip_response_with_reason(
            "the host's interactive question card is disabled; the user was not asked",
        ));
        return;
    }
    let parsed = match crate::acp::cursor_ext::parse_cursor_ask_questions(&req.0) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!("[cursor ask] rejecting malformed ext request: {e}");
            let _ = responder.respond(crate::acp::cursor_ext::cursor_ask_skip_response_with_reason(
                &format!("the host could not render this ask: {e}"),
            ));
            return;
        }
    };
    let tool_call_id = req
        .0
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let specs: Vec<_> = parsed.iter().map(|q| q.spec.clone()).collect();
    let card_specs = specs.clone();
    let Some(registered) = questions.register_question(connection_id, specs).await else {
        let _ = responder.respond(crate::acp::cursor_ext::cursor_ask_skip_response_with_reason(
            "the host could not open a question card (one is already pending, or the session is gone)",
        ));
        return;
    };
    let state = Arc::clone(state);
    let emitter = emitter.clone();
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(outcome) => {
                if let Some(tool_call_id) = tool_call_id {
                    emit_with_state(
                        &state,
                        &emitter,
                        AcpEvent::ToolCall {
                            tool_call_id,
                            title: "ask_user_question".to_string(),
                            kind: "other".to_string(),
                            status: "completed".to_string(),
                            content: None,
                            raw_input: Some(
                                crate::acp::question::grok_result_card_input(&card_specs)
                                    .to_string(),
                            ),
                            raw_output: Some(
                                crate::acp::question::grok_result_card_output(&outcome).to_string(),
                            ),
                            locations: None,
                            meta: None,
                            images: None,
                        },
                    )
                    .await;
                }
                let _ = responder.respond(crate::acp::cursor_ext::build_cursor_ask_response(
                    &parsed, &outcome,
                ));
            }
            Err(_) => {
                let _ = responder.respond(crate::acp::cursor_ext::cursor_ask_skip_response());
            }
        }
    });
}

/// Bridge Cursor's blocking `cursor/create_plan` into the shared plan-approval
/// card. Disconnect / malformed → `cancelled`, never a silent `accepted`.
async fn handle_cursor_create_plan(
    access: &Option<Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>>,
    connection_id: &str,
    req: CursorCreatePlanRequest,
    responder: Responder<serde_json::Value>,
) {
    tracing::info!(
        "[cursor plan] received cursor/create_plan: keys={:?}",
        crate::acp::cursor_ext::param_keys(&req.0)
    );
    let Some(access) = access else {
        let _ = responder.respond(crate::acp::cursor_ext::cursor_create_plan_disconnect_response());
        return;
    };
    let (plan_markdown, tool_call_id) = match crate::acp::cursor_ext::parse_cursor_create_plan(&req.0)
    {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!("[cursor plan] rejecting malformed ext request: {e}");
            let _ = responder.respond(crate::acp::cursor_ext::cursor_create_plan_disconnect_response());
            return;
        }
    };
    let Some(registered) = access
        .register_plan_approval(connection_id, tool_call_id, plan_markdown)
        .await
    else {
        let _ = responder.respond(crate::acp::cursor_ext::cursor_create_plan_disconnect_response());
        return;
    };
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(answer) => {
                let _ = responder.respond(
                    crate::acp::cursor_ext::build_cursor_create_plan_response(&answer),
                );
            }
            Err(_) => {
                let _ = responder
                    .respond(crate::acp::cursor_ext::cursor_create_plan_disconnect_response());
            }
        }
    });
}

fn handle_cursor_update_todos(
    req: CursorUpdateTodosRequest,
    responder: Responder<serde_json::Value>,
) {
    tracing::debug!(
        "[cursor todos] cursor/update_todos keys={:?}",
        crate::acp::cursor_ext::param_keys(&req.0)
    );
    let _ = responder.respond(crate::acp::cursor_ext::build_cursor_update_todos_response(
        &req.0,
    ));
}

fn handle_cursor_task(req: CursorTaskRequest, responder: Responder<serde_json::Value>) {
    tracing::debug!(
        "[cursor task] cursor/task keys={:?}",
        crate::acp::cursor_ext::param_keys(&req.0)
    );
    let _ = responder.respond(crate::acp::cursor_ext::build_cursor_task_response(&req.0));
}

fn handle_cursor_generate_image(
    req: CursorGenerateImageRequest,
    responder: Responder<serde_json::Value>,
) {
    tracing::debug!(
        "[cursor image] cursor/generate_image keys={:?}",
        crate::acp::cursor_ext::param_keys(&req.0)
    );
    let _ = responder.respond(crate::acp::cursor_ext::build_cursor_generate_image_response(
        &req.0,
    ));
}

#[allow(clippy::too_many_arguments)]
async fn handle_elicitation_request(
    cx: &ConnectionTo<Agent>,
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    connection_id: &str,
    req: CodexElicitationRequest,
    responder: Responder<serde_json::Value>,
) {
    // The wire reply is the serialized `CreateElicitationResponse` (see the
    // newtype above). `Decline` makes codex proceed with its own judgment.
    fn decline() -> serde_json::Value {
        serde_json::to_value(crate::acp::question::elicitation_decline_response())
            .unwrap_or_default()
    }
    let raw = req.0;
    // Who is on the other end, from the CONNECTION rather than from the frame.
    // This handler is registered for every agent and codeg advertises
    // `elicitation.form` to DeepSeek as well as Codex, so the parser must be
    // told the peer's identity instead of inferring it from `_meta.codex` —
    // `_meta` is an open namespace and another speaker (or an MCP server behind
    // it) may use the same keys for something else entirely. For a codex peer
    // this also carries the running adapter's `request_user_input` generation,
    // pinned at initialize: it settles which of a codex question's
    // `title`/`description` holds the question, which 1.12.0 swapped and the
    // wire cannot disambiguate.
    let peer = {
        let s = state.read().await;
        if s.agent_type == AgentType::Codex {
            crate::acp::question::ElicitationPeer::Codex(s.codex_user_input_shape)
        } else {
            crate::acp::question::ElicitationPeer::Other
        }
    };
    // Everything codex-acp can send once `elicitation.form` is advertised
    // resolves to a plan here — an unhandled shape would silently reject the
    // agent's blocked request (an MCP tool-call approval, most damagingly).
    let plan = match crate::acp::question::classify_elicitation(&raw, peer) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("[codex elicitation] declining unrenderable request: {e}");
            let _ = responder.respond(decline());
            return;
        }
    };
    match plan {
        // Approval-style (MCP tool-call approval / message-only confirm):
        // render through the permission card — the exact surface these used
        // before codeg advertised `elicitation.form` (codex-acp then sent
        // `session/request_permission`). Deliberately NOT gated by the
        // ask_user_question toggle: this is consent, not an agent question,
        // and auto-declining would reject the tool call outright.
        crate::acp::question::ElicitationPlan::Approval(approval) => {
            let request_id = uuid::Uuid::new_v4().to_string();
            // Mirror codex-acp's own `request_permission` fallback tool_call
            // shape (`buildPermissionRequest`) so the frontend permission card
            // renders it identically. When codex correlated the approval to an
            // already-rendered mcpToolCall item, reuse that id so the card
            // attaches to it.
            let tool_call = serde_json::json!({
                "toolCallId": approval
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| format!("elicitation-{request_id}")),
                "title": approval.message,
                "kind": "execute",
                "status": "pending",
                "content": [{
                    "type": "content",
                    "content": {"type": "text", "text": approval.message},
                }],
            });
            let options: Vec<PermissionOptionInfo> = approval
                .options
                .iter()
                .map(|o| PermissionOptionInfo {
                    option_id: o.option_id.clone(),
                    name: o.label.clone(),
                    kind: o.kind.to_string(),
                    // Synthesized from an elicitation form, not from ACP
                    // `PermissionOption`s — there is no wire `_meta` to forward.
                    meta: None,
                })
                .collect();
            admit_permission(
                cx,
                perms,
                state,
                emitter,
                PermissionReply::CodexElicitation {
                    responder,
                    approval,
                },
                QueuedPermission {
                    request_id,
                    tool_call,
                    options,
                },
            )
            .await;
        }
        // Question-style (codex `request_user_input`, generic MCP forms):
        // bridge into the same ask card as the codeg-mcp ask tool.
        crate::acp::question::ElicitationPlan::Questions(questions) => {
            let Some((question_access, ask_cfg)) = access else {
                let _ = responder.respond(decline());
                return;
            };
            // Same kill switch as the codeg-mcp ask tool and the grok bridge:
            // when the user has turned ask_user_question off, decline so codex
            // proceeds.
            if !ask_cfg.is_enabled().await {
                let _ = responder.respond(decline());
                return;
            }
            // register_question consumes the specs; `questions` keeps its copy
            // to correlate the answer back to each field when building the
            // response.
            let Some(registered) = question_access
                .register_question(connection_id, questions.specs.clone())
                .await
            else {
                // Connection gone, or an ask is already pending on this connection.
                let _ = responder.respond(decline());
                return;
            };
            // The user answers out-of-band (the `answer_question` endpoint
            // resolves the one-shot below), so await it on a task — keeping
            // the ACP dispatch loop free — then reply to codex's blocked
            // elicitation request. Cloned here so the synthesized card below can
            // write through the session state from the spawned task.
            let card_state = Arc::clone(state);
            let card_emitter = emitter.clone();
            // codex may take the question back before anyone answers: it races a
            // `request_user_input` against `_meta.codex.autoResolutionMs` and
            // answers `{answers: {}}` itself on expiry, and a closed session or a
            // cancelled `session/prompt` request aborts it too. Every one of
            // those arrives as `$/cancel_request` for this request, so the card
            // is retired the moment codex stops waiting for it, rather than
            // lingering as a question whose answer would reach nobody.
            let cancellation = responder.cancellation();
            let withdraw_access = Arc::clone(question_access);
            let withdraw_conn = connection_id.to_string();
            let question_id = registered.question_id;
            let answer_rx = registered.answer_rx;
            tokio::spawn(async move {
                let answer = tokio::select! {
                    // Withdrawal wins a tie: an answer that crosses it would
                    // reach nobody, so it must not render as a delivered one.
                    biased;
                    () = cancellation.cancelled() => None,
                    answer = answer_rx => Some(answer),
                };
                let response = match answer {
                    None => {
                        tracing::info!(
                            "[codex elicitation] question {question_id} withdrawn by the agent"
                        );
                        withdraw_access
                            .cancel_question(&withdraw_conn, &question_id)
                            .await;
                        crate::acp::question::elicitation_cancel_response()
                    }
                    Some(Ok(outcome)) => {
                        // Surface the answered "提问回答" capsule in-stream — the
                        // parity codex's `request_user_input` never emits itself: it
                        // resolves the answer over THIS elicitation round-trip and
                        // puts no completed tool_call on the ACP stream (so the live
                        // message has nothing to render otherwise). Emit BEFORE
                        // unblocking codex so the card lands ahead of its follow-up
                        // text; codex is blocked on this reply, so nothing races the
                        // emit. Keyed by the elicitation's tool_call_id so this is
                        // the single card for the ask and the reloaded history card
                        // (`codex.rs`, same id) replaces rather than duplicates it.
                        if let Some(tool_call_id) = questions.tool_call_id.clone() {
                            emit_with_state(
                                &card_state,
                                &card_emitter,
                                AcpEvent::ToolCall {
                                    tool_call_id,
                                    title: "request_user_input".to_string(),
                                    kind: "other".to_string(),
                                    status: "completed".to_string(),
                                    content: None,
                                    raw_input: Some(
                                        crate::acp::question::grok_result_card_input(
                                            &questions.specs,
                                        )
                                        .to_string(),
                                    ),
                                    raw_output: Some(
                                        crate::acp::question::grok_result_card_output(&outcome)
                                            .to_string(),
                                    ),
                                    locations: None,
                                    meta: None,
                                    images: None,
                                },
                            )
                            .await;
                        }
                        crate::acp::question::build_elicitation_response(&questions, &outcome)
                    }
                    // Sender dropped: canceled or the connection tore down.
                    // Decline so codex proceeds with its own judgment.
                    Some(Err(_)) => crate::acp::question::elicitation_decline_response(),
                };
                let _ = responder.respond(serde_json::to_value(response).unwrap_or_default());
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_permission_request(
    cx: &ConnectionTo<Agent>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    cwd: &str,
    agent_type: AgentType,
    req: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Codex Plan-mode review gate: seed the tool call codex never announced, so
    // its follow-up `tool_call_update` (status + rawOutput only) merges into a
    // card that has an identity instead of creating an untitled orphan. See
    // `codex_plan_review_meta`. `raw_input` is deliberately omitted: the plan text
    // is already in the transcript (codex emits the plan as an
    // `agent_message_chunk` before this) and the permission card renders it from
    // the request's own `rawInput.plan` — a third copy would be noise.
    if let Some(review_meta) = codex_plan_review_meta(
        agent_type,
        &req.tool_call.tool_call_id.to_string(),
        matches!(req.tool_call.fields.kind, Some(ToolKind::SwitchMode)),
        req.meta.as_ref(),
    ) {
        emit_with_state(
            state,
            emitter,
            AcpEvent::ToolCall {
                tool_call_id: req.tool_call.tool_call_id.to_string(),
                title: req.tool_call.fields.title.clone().unwrap_or_default(),
                kind: "switch_mode".to_string(),
                status: "pending".to_string(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: Some(serde_json::Value::Object(review_meta)),
                images: None,
            },
        )
        .await;
    }

    let options: Vec<PermissionOptionInfo> = req
        .options
        .iter()
        .map(|opt| PermissionOptionInfo {
            option_id: opt.option_id.to_string(),
            name: opt.name.clone(),
            kind: match opt.kind {
                PermissionOptionKind::AllowOnce => "allow_once".into(),
                PermissionOptionKind::AllowAlways => "allow_always".into(),
                PermissionOptionKind::RejectOnce => "reject_once".into(),
                PermissionOptionKind::RejectAlways => "reject_always".into(),
                _ => "unknown".into(),
            },
            // Opaque passthrough — the frontend reads codex-acp ≥1.1.8's
            // `_meta.permission.changes[].description` off this. codex-acp
            // 2.0.0 moved an option's record under `_meta.jetbrains.air`; it is
            // copied back to the key the card reads.
            meta: crate::acp::air_contract::normalize_permission_option_meta(opt.meta.as_ref()),
        })
        .collect();

    let mut tool_call_value = serde_json::to_value(&req.tool_call).unwrap_or_default();

    // Resolve line numbers in rawInput for edit tool permission requests
    if let Some(obj) = tool_call_value.as_object_mut() {
        let key = ["rawInput", "raw_input"]
            .into_iter()
            .find(|k| obj.contains_key(*k));
        if let Some(key) = key {
            match obj.get_mut(key) {
                // rawInput is a JSON object: inject _start_line in place
                Some(v) if v.is_object() => {
                    inject_start_line(v, Some(cwd));
                }
                // rawInput is a JSON string: parse, inject, write back as object
                Some(serde_json::Value::String(text)) => {
                    let text = text.clone();
                    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                        if inject_start_line(&mut parsed, Some(cwd)) {
                            obj.insert(key.to_string(), parsed);
                        }
                    } else if text.contains("@@\n") || text.contains("@@\r\n") {
                        if let Some(resolved) = crate::parsers::resolve_patch_text(&text, Some(cwd))
                        {
                            obj.insert(key.to_string(), serde_json::Value::String(resolved));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    hoist_request_permission_meta(&mut tool_call_value, req.meta.as_ref());

    admit_permission(
        cx,
        perms,
        state,
        emitter,
        PermissionReply::Acp(responder),
        QueuedPermission {
            request_id,
            tool_call: tool_call_value,
            options,
        },
    )
    .await;
}

fn respond_terminal_request<T: agent_client_protocol::JsonRpcResponse>(
    responder: Responder<T>,
    result: Result<T, TerminalRuntimeError>,
) -> Result<(), agent_client_protocol::Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error.into_rpc_error()),
    }
}

/// Refuse a channel this launch never advertised
/// ([`HostToolsPolicy::Agent`], #436). Withholding the capability is a
/// DECLARATION; a non-conforming agent can still call the method, and if codeg
/// then served it the whole switch would be a suggestion — the operation would
/// land back in codeg's process, outside the agent's sandbox, which is exactly
/// the bug. `method_not_found` is the honest wire answer: as far as this
/// connection is concerned the method does not exist, which is what the agent
/// was told on Initialize.
fn refuse_unadvertised_channel<T: agent_client_protocol::JsonRpcResponse>(
    responder: Responder<T>,
    method: &str,
) -> Result<(), agent_client_protocol::Error> {
    tracing::warn!(
        "[ACP] refusing {method}: {HOST_TOOLS_ENV}=agent, so this channel was never \
         advertised — the agent must use its own (sandboxable) tools"
    );
    responder.respond_with_error(unadvertised_channel_error(method))
}

/// The error [`refuse_unadvertised_channel`] answers with. Split out because a
/// `Responder` cannot be built outside a live connection, so this is the part
/// of the refusal a unit test can pin; the wiring itself is covered end-to-end.
fn unadvertised_channel_error(method: &str) -> agent_client_protocol::Error {
    agent_client_protocol::Error::method_not_found().data(format!(
        "codeg does not host {method} for this agent ({HOST_TOOLS_ENV}=agent)"
    ))
}

fn respond_file_system_request<T: agent_client_protocol::JsonRpcResponse>(
    responder: Responder<T>,
    result: Result<T, FileSystemRuntimeError>,
) -> Result<(), agent_client_protocol::Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error.into_rpc_error()),
    }
}

async fn set_session_mode(
    session: &mut AgentSession,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    mode_id: String,
) -> Result<(), agent_client_protocol::Error> {
    let req = SetSessionModeRequest::new(session.session_id().clone(), mode_id.clone());
    session
        .connection()
        .send_request_to(Agent, req)
        .block_task()
        .await?;

    emit_with_state(state, emitter, AcpEvent::ModeChanged { mode_id }).await;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn set_session_config_option(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    config_id: String,
    value_id: String,
) -> Result<(), agent_client_protocol::Error> {
    // An explicit set means someone has taken ownership of this option — the
    // user in the composer, or the post-establishment re-assert (already
    // drained). Either way the establishment-time value stops being the one to
    // defend, or the user's own pick would be reverted by the next push.
    // Establishment itself goes through `..._inner` and is unaffected.
    state
        .write()
        .await
        .asserted_config_values
        .remove(&config_id);
    // The whole selector transport carries values as opaque strings; only here,
    // at the wire, does the option's advertised kind decide how to encode it.
    let is_boolean = state
        .read()
        .await
        .config_options
        .as_ref()
        .and_then(|opts| opts.iter().find(|o| o.id == config_id))
        .is_some_and(|o| matches!(o.kind, SessionConfigKindInfo::Boolean(_)));
    let value = encode_config_option_value(is_boolean, &value_id);
    let updated =
        set_session_config_option_inner(cx, session_id, config_id.clone(), value).await?;
    // Compare BEFORE emitting: the agent's answer is the only place a request and
    // its outcome are correlated. Once the option list is broadcast it is
    // indistinguishable from an unsolicited update.
    if let Some(rejection) =
        config_option_rejection(&map_session_config_options(&updated), &config_id, &value_id)
    {
        emit_with_state(state, emitter, rejection).await;
    }
    emit_session_config_options_values(state, emitter, updated).await;
    Ok(())
}

/// Build a [`AcpEvent::ConfigOptionRejected`] when the agent's answer settled the
/// option somewhere other than where the request asked.
///
/// Takes the mapped form so grouped and ungrouped selects are already flattened
/// into one value list. Returns `None` when the pick was honoured, when the agent
/// didn't advertise the option at all (nothing to compare against), or for a
/// non-select kind — silence is the right default, since a spurious "your pick was
/// changed" notice is worse than none.
fn config_option_rejection(
    updated: &[SessionConfigOptionInfo],
    config_id: &str,
    requested: &str,
) -> Option<AcpEvent> {
    let option = updated.iter().find(|o| o.id == config_id)?;
    let SessionConfigKindInfo::Select(select) = &option.kind else {
        return None;
    };
    if select.current_value == requested {
        return None;
    }
    // Labels, not ids: the composer's dropdown shows names, so the notice has to
    // name the same things the user was looking at.
    let label = |value: &str| {
        select
            .options
            .iter()
            .find(|item| item.value == value)
            .map(|item| item.name.clone())
            .unwrap_or_else(|| value.to_string())
    };
    Some(AcpEvent::ConfigOptionRejected {
        config_id: config_id.to_string(),
        option_name: option.name.clone(),
        requested: label(requested),
        actual: label(&select.current_value),
        requested_value: requested.to_string(),
        actual_value: select.current_value.clone(),
    })
}

/// Encode a selector value for `session/set_config_option`.
///
/// codeg keeps config values as opaque `String`s end to end (Tauri command, web
/// handler, and the per-agent preference store all use `Record<string, string>`
/// semantics), so the boolean round-trip is `"true"` ⇄ `true` and happens only
/// here. A `select` value stays a bare `{"value": "…"}`, byte-for-byte what
/// codeg sent before boolean options existed — every non-cline agent's wire
/// traffic is unchanged.
fn encode_config_option_value(is_boolean: bool, value: &str) -> SessionConfigOptionValue {
    if is_boolean {
        SessionConfigOptionValue::boolean(value == "true")
    } else {
        SessionConfigOptionValue::value_id(value.to_string())
    }
}

/// Whether an advertised option already holds `value`, so applying a saved
/// preference at connect can skip the round-trip. Reads the same `"true"` ⇄
/// `true` encoding [`encode_config_option_value`] writes; an option kind this
/// build cannot interpret is treated as "does not match" so the agent, not
/// codeg, decides.
fn config_option_already_holds(option: &SessionConfigOption, value: &str) -> bool {
    match &option.kind {
        SessionConfigKind::Select(s) => s.current_value.to_string() == value,
        SessionConfigKind::Boolean(b) => b.current_value == (value == "true"),
        _ => false,
    }
}

/// Whether the option's OWN advertisement proves `value` is not selectable, so
/// replaying a saved preference for it at connect is a guaranteed error.
///
/// This exists because a saved preference outlives the value it names.
/// claude-agent-acp 0.76.0 is the case that forced it: once codeg advertises
/// the AIR `recommendedValue` capability, the adapter REMOVES the `default` row
/// from the model and effort selectors, and a user who had picked it keeps
/// re-sending `"default"` on every connect forever — the option they would have
/// to re-pick to overwrite it no longer exists. Nothing tells the user, so the
/// preference cannot heal itself.
///
/// Decided from the agent's own answer, never from an agent id or a pinned
/// version. That distinction is load-bearing: the registry pin only governs
/// what codeg INSTALLS, while `resolve_npx_command` launches whatever
/// `claude-agent-acp` is on PATH — so "this is the built-in Claude Code agent"
/// says nothing about which release is actually running, and pruning on that
/// assumption would silently discard a still-valid pick on an older adapter.
///
/// Deliberately narrow, in three ways:
///
/// * Only a `select`. A boolean takes both values by construction.
/// * Only a NON-EMPTY value list. Some agents announce an empty
///   `SessionConfigOptions` first and push the real one later (see
///   `AcpManager::probe_agent_options`), and an option kind this build cannot
///   decode also flattens to nothing — neither proves anything.
/// * Only an option the agent ADVERTISED. An id that never appeared is left to
///   the agent, exactly as before: codex answers `set_config_option` for ids it
///   does not advertise (see the call site in
///   [`apply_preferred_session_options`]).
fn config_option_rejects_value(option: &SessionConfigOption, value: &str) -> bool {
    let SessionConfigKind::Select(select) = &option.kind else {
        return false;
    };
    // Grouped and ungrouped are one flat namespace here, the same way
    // `config_option_rejection` reads them.
    let mut advertised = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => {
            options.iter().map(|o| o.value.to_string()).collect::<Vec<_>>()
        }
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(|o| o.value.to_string()))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if advertised.is_empty() {
        return false;
    }
    advertised.sort_unstable();
    advertised.binary_search(&value.to_string()).is_err()
}

/// The plain twin of a saved model pick whose `[1m]` spelling the model
/// selector no longer lists — `opus[1m]` → `opus` — when the selector lists
/// the twin.
///
/// Claude Code CLI 2.1.283 (claude-agent-acp 0.83.0+) is what makes this
/// reachable. The adapter's own live test (`model-presentation.test.ts`)
/// records the Opus row going from `opus[1m]` "Opus (1M context)" to plain
/// `opus` "Opus" — Opus is natively 1M-context, so it is the same model on the
/// same window, renamed; the CLI builds the row either way depending on the
/// account. A user who had picked the old row has `opus[1m]` saved, and
/// [`config_option_rejects_value`] rightly refuses to replay a value the list
/// no longer offers, so every connect would silently land on the account's
/// default model instead. The adapter would have resolved the old spelling
/// itself (its `resolveModelPreference` maps `opus[1m]` onto the `opus` row),
/// but codeg's screen runs first.
///
/// Healed here, off the agent's answer, rather than rewritten in the saved
/// preferences, because only the answer says which spelling is current — and
/// it moves with the CLI the user runs. A gateway account (`ANTHROPIC_BASE_URL`)
/// on 2.1.284 still lists `opus[1m]` next to `sonnet` and `sonnet[1m]`, with no
/// plain `opus` row at all; on 2.1.285+ (claude-agent-acp 0.85.0), which gives a
/// gateway session the 1M window of the models that have one, the same gateway
/// lists only `opus`, `sonnet`, Fable and `haiku` (both measured), so a saved
/// `sonnet[1m]` heals too. Rewriting the stored value would break whichever
/// side the user is not on today.
///
/// Deliberately narrow:
///
/// * Only the model selector — `[1m]` is a model-id spelling — and only for
///   Claude Code (see [`heal_retired_model_picks`]).
/// * Only when [`config_option_rejects_value`] proves the exact value gone AND
///   the twin is listed, so it inherits that function's refusals (a non-select
///   or an empty list proves nothing) and a pick the agent still offers is
///   never touched: where `sonnet` and `sonnet[1m]` are both listed, as on the
///   2.1.284 gateway above, `sonnet[1m]` replays as it is.
/// * Only the direction the rename took; a saved plain pick is never moved
///   onto a `[1m]` row.
///
/// So it never trades away a lane the session offers. When the `[1m]` row is
/// gone, the choice is between the same model on the lane the account does
/// offer and the account's default model — on the measured gateway that
/// default is Opus, listed at twice Sonnet's per-token price.
fn heal_retired_context_lane_pick(option: &SessionConfigOption, value: &str) -> Option<String> {
    if !is_model_config_option(option) {
        return None;
    }
    let twin = strip_context_lane_suffix(value)?;
    (config_option_rejects_value(option, value) && !config_option_rejects_value(option, twin))
        .then(|| twin.to_string())
}

/// `value` without its trailing `[1m]`, matched case-insensitively — the same
/// suffix the CLI drops when it compares model aliases. `None` when there is
/// no such suffix or nothing would be left.
fn strip_context_lane_suffix(value: &str) -> Option<&str> {
    const SUFFIX: &str = "[1m]";
    let split = value.len().checked_sub(SUFFIX.len())?;
    let (twin, suffix) = (value.get(..split)?, value.get(split..)?);
    (!twin.is_empty() && suffix.eq_ignore_ascii_case(SUFFIX)).then_some(twin)
}

/// The row a saved CONCRETE Claude model id is now spelled as, when the model
/// selector no longer lists the id itself: the one row whose label names that
/// exact model — `claude-fable-5-1` → the `fable` row, labelled `Fable 5.1`.
///
/// Claude Code CLI 2.1.287 (claude-agent-acp 0.86.0) is what makes this
/// reachable. Its picker spells the Fable row by the family alias, as it
/// already spelled Opus, Sonnet and Haiku: both row builders in the 2.1.287
/// binary now give a Fable row the value `fable` where 2.1.286 gave it the
/// concrete id, for every kind of account. Measured live through an
/// `ANTHROPIC_BASE_URL` gateway, the row goes from
/// `{value: "claude-fable-5-1", name: "Fable 5.1"}` to
/// `{value: "fable", name: "Fable 5.1"}`, description unchanged; the CLI's
/// changelog says a claude.ai login's Fable pick now follows the newest Fable
/// the way Opus and Sonnet do. Picking that row in codeg saved its value, so a
/// user who chose Fable has `claude-fable-5-1` stored, and
/// [`config_option_rejects_value`] rightly refuses a value the list no longer
/// offers: every connect would silently land on the default model — Opus on
/// the measured gateway. The adapter would have resolved the old spelling
/// itself (its `set_config_option` falls back to `resolveModelPreference`,
/// which finds the row by the concrete model it resolves to), but codeg's
/// screen runs first, and that resolved model never reaches a client.
///
/// What a client does see is how the row names its model, and the CLI names it
/// in one of three ways, all read from the binary: the catalog row is labelled
/// with the model's catalog name (`Fable 5.1`); the built-in Fable row is
/// labelled `Fable` and leads its description with the catalog name
/// (`Fable 5.1 · Most capable for …`); a model pinned through
/// `ANTHROPIC_DEFAULT_*_MODEL` is labelled with the pinned id itself. The
/// adapter's own resolver matches a preference against row labels too.
/// Deliberately narrow:
///
/// * Only the model selector, and only for Claude Code (see
///   [`heal_retired_model_picks`]).
/// * Only when [`config_option_rejects_value`] proves the exact value gone, so
///   a pick the agent still offers is never touched.
/// * Only onto exactly ONE row that names the model. The catalog name carries
///   the version, so a pick is never moved onto another model: once the
///   `fable` row moves on to a newer Fable, a saved `claude-fable-5-1` stops
///   healing and is skipped like any other retired pick.
/// * Only the catalog's `claude-<family>-<version>` form has a catalog name to
///   look for (see [`claude_catalog_label`]); any other value can only match a
///   row labelled with that value itself.
fn heal_respelled_model_pick(option: &SessionConfigOption, value: &str) -> Option<String> {
    if !is_model_config_option(option) || !config_option_rejects_value(option, value) {
        return None;
    }
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let rows: Vec<&SessionConfigSelectOption> = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .collect(),
        _ => Vec::new(),
    };
    let catalog_label = claude_catalog_label(value);
    let names_the_pick = |row: &&SessionConfigSelectOption| {
        let label = row.name.trim();
        if label.eq_ignore_ascii_case(value) {
            return true;
        }
        let Some(catalog) = catalog_label.as_deref() else {
            return false;
        };
        label.eq_ignore_ascii_case(catalog)
            || row
                .description
                .as_deref()
                .is_some_and(|description| description_leads_with(description, catalog))
    };
    let mut matching = rows.into_iter().filter(names_the_pick);
    let row = matching.next()?;
    matching.next().is_none().then(|| row.value.to_string())
}

/// Whether a model row's description opens with `name` as its own segment, the
/// way the CLI writes one: `Fable 5.1 · Most capable for …`.
fn description_leads_with(description: &str, name: &str) -> bool {
    let description = description.trim_start();
    description
        .get(..name.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(name))
        && description[name.len()..].starts_with(" \u{b7} ")
}

/// The name Claude Code's model catalog gives a concrete model id, which is
/// the label its picker shows for that model's row: `claude-fable-5-1` →
/// `Fable 5.1`, `claude-sonnet-5` → `Sonnet 5`, `claude-haiku-4-5-20251001` →
/// `Haiku 4.5` (a snapshot date is not part of the name). `None` for an id not
/// in that `claude-<family>-<version>` form — an alias, a gateway id, or the
/// retired `claude-3-7-sonnet-…` order.
fn claude_catalog_label(id: &str) -> Option<String> {
    let mut parts = id.strip_prefix("claude-")?.split('-');
    let family = parts
        .next()
        .filter(|family| !family.is_empty() && family.bytes().all(|b| b.is_ascii_lowercase()))?;
    let mut version: Vec<&str> = parts.collect();
    if version
        .last()
        .is_some_and(|part| part.len() == 8 && part.bytes().all(|b| b.is_ascii_digit()))
    {
        version.pop();
    }
    let numeric =
        |part: &&str| (1..=2).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    if version.is_empty() || !version.iter().all(numeric) {
        return None;
    }
    let (initial, tail) = family.split_at(1);
    Some(format!(
        "{}{tail} {}",
        initial.to_ascii_uppercase(),
        version.join(".")
    ))
}

/// The spelling the model selector lists now for a saved pick it no longer
/// lists, by the two renames Claude Code has made: a retired `[1m]` lane
/// ([`heal_retired_context_lane_pick`]), a concrete id re-spelled as its family
/// alias ([`heal_respelled_model_pick`]), or both at once — a gateway's
/// `claude-fable-5-1[1m]` lost its lane on 2.1.285 and the plain row was
/// re-spelled `fable` on 2.1.287.
fn heal_retired_model_pick(option: &SessionConfigOption, value: &str) -> Option<String> {
    heal_retired_context_lane_pick(option, value)
        .or_else(|| heal_respelled_model_pick(option, value))
        .or_else(|| {
            let twin = strip_context_lane_suffix(value)?;
            if !config_option_rejects_value(option, value) {
                return None;
            }
            heal_respelled_model_pick(option, twin)
        })
}

/// [`heal_retired_model_pick`] over a whole preference set, against the
/// options the session was established with. Every other entry is returned
/// unchanged, and so is every entry for an agent other than Claude Code: both
/// renames are claude-agent-acp's spellings, and it is the only agent whose
/// renames of them have been observed.
fn heal_retired_model_picks(
    agent_type: AgentType,
    options: &[SessionConfigOption],
    preferred: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    if agent_type != AgentType::ClaudeCode {
        return preferred.clone();
    }
    preferred
        .iter()
        .map(|(config_id, value_id)| {
            let healed = options
                .iter()
                .find(|o| o.id.to_string() == *config_id)
                .and_then(|o| heal_retired_model_pick(o, value_id));
            match healed {
                Some(twin) => {
                    tracing::info!(
                        "[ACP] preferred config '{config_id}'='{value_id}' is no longer \
                         offered; replaying '{twin}', the spelling the agent lists now"
                    );
                    (config_id.clone(), twin)
                }
                None => (config_id.clone(), value_id.clone()),
            }
        })
        .collect()
}

/// [`config_option_rejects_value`] for the mode channel: whether the session's
/// OWN mode list proves a saved `preferred_mode_id` cannot be selected, so
/// `session/set_mode` for it at connect is a guaranteed error.
///
/// claude-agent-acp 0.81.1 (#1165) is what makes this reachable: it withdraws
/// `bypassPermissions` from `availableModes` whenever any settings tier sets
/// `permissions.disableBypassPermissionsMode: "disable"` (the CLI refuses the
/// mode then too), and answers a `set_mode` for it with `Mode bypassPermissions
/// is not available in this session` — measured live, with the control run
/// (setting absent) listing and accepting the mode. The same happens to a root
/// user outside a sandbox and to any other mode an agent retires. A preference
/// saved before would otherwise fail on every connect, for good.
///
/// Same judgment as for config options, and the same reasons: decided off the
/// agent's answer rather than an agent id or the registry pin (an older adapter
/// on PATH may still list the mode), and an EMPTY list proves nothing.
fn session_modes_reject_id(modes: &SessionModeState, mode_id: &str) -> bool {
    !modes.available_modes.is_empty()
        && !modes
            .available_modes
            .iter()
            .any(|mode| mode.id.to_string() == mode_id)
}

/// Whether an advertised option IS the agent's model selector. ACP reserves no
/// id for it, so match either signal — the `category` every agent that has a
/// model publishes it under (see [`current_model_id_from_opts`]) or the
/// conventional `model` id, the same pair the frontend's `isModelConfigOption`
/// checks.
fn is_model_config_option(option: &SessionConfigOption) -> bool {
    matches!(option.category, Some(SessionConfigOptionCategory::Model))
        || option.id.to_string() == MODEL_CONFIG_OPTION_ID
}

/// Saved preferences in application order: the model selector first, then every
/// other id in its natural (sorted) order.
///
/// Order is load-bearing because a model switch RE-SCOPES the options hanging
/// off it. Cursor's parameterized picker is the case that forced this: it
/// answers `set_config_option("model", …)` by reloading THAT model's own saved
/// (or default) parameter values, and rejects a parameter id the model in
/// effect does not define. Replaying by raw key order would put `fast` before
/// `model` and lose it to the switch — or hard-fail it against the outgoing
/// model. Grok's dedicated path (`apply_grok_preferred_options`) already
/// hard-codes the same order for the same reason; this is the generic half.
///
/// A preferred id the agent never advertised is ordered as a non-model option
/// unless it is literally `model` — the fallback stays deliberately narrow
/// because an unadvertised id is still sent (see `apply_preferred_session_options`).
fn order_preferred_config_values<'a>(
    options: &[SessionConfigOption],
    preferred: &'a BTreeMap<String, String>,
) -> Vec<(&'a String, &'a String)> {
    let (model_first, rest): (Vec<_>, Vec<_>) = preferred.iter().partition(|(config_id, _)| {
        options
            .iter()
            .find(|o| o.id.to_string() == **config_id)
            .map_or(config_id.as_str() == MODEL_CONFIG_OPTION_ID, |o| {
                is_model_config_option(o)
            })
    });
    model_first.into_iter().chain(rest).collect()
}

/// Wire-level half of `set_session_config_option`: send the JSON-RPC request and
/// return the agent's new config-options list, without touching SessionState or
/// emitting events. Used at session-init to apply saved preferences before the
/// single emit_session_config_options call so the frontend never sees an
/// "agent default → user preference" flicker.
async fn set_session_config_option_inner(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    config_id: String,
    value: SessionConfigOptionValue,
) -> Result<Vec<SessionConfigOption>, agent_client_protocol::Error> {
    let req = SetSessionConfigOptionRequest::new(session_id.clone(), config_id, value);
    let response = cx.send_request_to(Agent, req).block_task().await?;
    Ok(response.config_options)
}

/// Send the connection's goal-control extension request to pause or clear the
/// session's active goal — the advertised `_session/goal` (claude 0.66+ / codex
/// 1.2+) or codex's bespoke `_codex/session/goal_control` (#293, v1.1.4) it
/// still accepts as an alias. Start / resume / re-objective are NOT this method
/// — they go through the `/goal` prompt.
///
/// The agent replies with an empty object and then pushes the resulting goal
/// snapshot as a normal `session_info_update` (`_meta.goal`, the legacy
/// `_meta.codex.goal`, or `null` for a clear), which the existing goal-card
/// path renders — so the response value carries nothing to parse and is
/// intentionally discarded.
///
/// This request does NOT stop a running turn on either adapter; that is the
/// manager's call (see `ConnectionManager::goal_control`), because whether an
/// interrupt is safe depends on how the adapter delivers the control.
///
/// Sent via `UntypedMessage` because both method names are extensions with no
/// typed request in the schema — the same escape hatch `_session/steering` and
/// `_session/async_task/stop` use.
async fn send_goal_control(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    action: GoalControlAction,
    method: &str,
) -> Result<(), agent_client_protocol::Error> {
    // `method` is the connection's stored `goal_control_method`: the
    // advertised provider-neutral `_session/goal` (claude 0.66+/codex 1.2+)
    // or the legacy `_codex/session/goal_control` default. Both take the
    // same `{sessionId, action}` request shape.
    let params = serde_json::json!({
        "sessionId": session_id,
        "action": action,
    });
    let untyped_req = UntypedMessage::new(method, params).map_err(|e| {
        agent_client_protocol::util::internal_error(format!("Failed to build goal_control request: {e}"))
    })?;
    cx.send_request_to(Agent, untyped_req).block_task().await?;
    Ok(())
}

/// Apply user-saved mode and config-option preferences to a freshly-attached
/// session BEFORE the initial `session_modes` / `session_config_options`
/// events are emitted to the frontend.
///
/// This is the single ownership point for "preference → agent state" — the
/// frontend stores the user's last selections per agent_type and ships them
/// to the backend on connect; we then call `session/set_mode` and
/// `session/set_config_option` to align the agent process so the snapshot
/// the frontend will see (whether via WS `snapshot` frame or fetched HTTP
/// snapshot) already reflects the user's choices. No client-side
/// "intercept event and rewrite then sync back" hack — single source of truth.
///
/// Returns the (possibly updated) list of config options that the caller
/// should emit. Mode preferences trigger a `ModeChanged` event from
/// `set_session_mode`, which the caller's `emit_session_modes` immediately
/// precedes — so the frontend sees `SessionModes{default}` then
/// `ModeChanged{preferred}` and converges to the preferred value before
/// `SelectorsReady` fires. Failures on individual preferences are logged
/// and skipped so a stale/invalid preference can't block session startup.
#[allow(clippy::too_many_arguments)]
async fn apply_preferred_session_options(
    cx: &ConnectionTo<Agent>,
    session: &mut AgentSession,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
    initial_config_options: Vec<SessionConfigOption>,
) -> Vec<SessionConfigOption> {
    if let Some(pref_mode) = preferred_mode_id {
        let needs_apply = session
            .modes()
            .as_ref()
            .map(|m| m.current_mode_id.to_string() != pref_mode)
            .unwrap_or(false);
        // Same rule as `config_option_rejects_value` below, for the mode
        // channel: a saved mode the session no longer lists can only fail.
        let withdrawn = session
            .modes()
            .as_ref()
            .is_some_and(|m| session_modes_reject_id(m, pref_mode));
        if needs_apply && withdrawn {
            tracing::info!(
                "[ACP] skipping preferred mode '{pref_mode}' on connect: \
                 the agent no longer offers that mode"
            );
        } else if needs_apply {
            if let Err(e) = set_session_mode(session, state, emitter, pref_mode.to_string()).await {
                tracing::error!(
                    "[ACP] failed to apply preferred mode '{pref_mode}' on connect: {e}"
                );
            }
        }
    }

    if preferred_config_values.is_empty() {
        return initial_config_options;
    }

    let session_id = session.session_id().clone();
    let mut options = initial_config_options;
    // Ids this launch must not replay a saved preference for. Two rules:
    //
    //   * what this launch's environment froze — a set can only fail, and it is
    //     reachable, because the composer saves the pick BEFORE the set is
    //     awaited, so anyone who clicked cline's provider dropdown while it was
    //     still offered has one stored;
    //   * cline's `provider` unconditionally, because the settings panel owns
    //     it (it writes providers.json and decides the launch env). Pinning
    //     alone is not enough: the preference saved during a BYO launch is
    //     merely SKIPPED there, and would then be replayed on the next sign-in
    //     launch — which is not pinned — switching the user's billing account
    //     away from the one the panel shows. Switching accounts mid-session is
    //     still allowed; it just does not outlive the session.
    let (pinned, agent_type) = {
        let guard = state.read().await;
        (
            guard.env_pinned_config_option_ids.clone(),
            guard.agent_type,
        )
    };
    let never_replayed = |config_id: &str| {
        pinned.iter().any(|id| id == config_id)
            || (agent_type == AgentType::Cline && config_id == CLINE_PROVIDER_CONFIG_OPTION_ID)
    };
    // A model pick saved under a spelling the agent has since renamed — a
    // retired `[1m]` lane, or a concrete id re-spelled as its family alias —
    // replays as the spelling it lists now (see `heal_retired_model_pick`).
    // Healed once, against the same INITIAL list the order below is taken
    // from, so the replay, the screen and the ledger all see one value.
    let preferred_config_values =
        &heal_retired_model_picks(agent_type, &options, preferred_config_values);
    // Model first — see `order_preferred_config_values`. Ordered once against
    // the INITIAL list: every later list is the same agent's answer to a set,
    // so the model selector cannot move between ids mid-replay.
    let ordered = order_preferred_config_values(&options, preferred_config_values);
    for (config_id, value_id) in ordered {
        if never_replayed(config_id) {
            tracing::info!(
                "[ACP] skipping preferred config '{config_id}'='{value_id}' on connect: \
                 codeg owns this option rather than the composer"
            );
            continue;
        }
        // Skip the round-trip when the agent's current value already matches.
        // Note: codex-acp advertises "mode" as a config option (so the match
        // check below normally fires), but we still do NOT skip when a
        // requested config_id is absent from the advertised options — an agent
        // may accept `set_config_option` for an id it never advertised. codex
        // does: its `applySessionConfigOption` switches on `configId` alone,
        // with no advertised-list check (verified in the 1.7.0 bundle). So let
        // the agent decide.
        let advertised = options.iter().find(|o| o.id.to_string() == *config_id);
        let already_matches =
            advertised.is_some_and(|o| config_option_already_holds(o, value_id.as_str()));
        if already_matches {
            continue;
        }
        // …and skip a value the option's own advertisement rules out. Sending
        // it can only fail, and a saved preference for a value an agent
        // retired would otherwise fail on EVERY connect for good — see
        // `config_option_rejects_value` for why this is decided here, off the
        // agent's answer, rather than from the agent id or the registry pin.
        if advertised.is_some_and(|o| config_option_rejects_value(o, value_id.as_str())) {
            tracing::info!(
                "[ACP] skipping preferred config '{config_id}'='{value_id}' on connect: \
                 the agent no longer offers that value"
            );
            continue;
        }
        // Encode against what the agent advertised for this id. An id the agent
        // never advertised falls back to the select form — the same value shape
        // codeg has always sent (see the note above on unadvertised "mode").
        let is_boolean =
            advertised.is_some_and(|o| matches!(o.kind, SessionConfigKind::Boolean(_)));
        let value = encode_config_option_value(is_boolean, value_id);
        match set_session_config_option_inner(cx, &session_id, config_id.clone(), value).await {
            Ok(updated) => options = updated,
            Err(e) => tracing::error!(
                "[ACP] failed to apply preferred config '{config_id}'='{value_id}' \
                 on connect: {e}"
            ),
        }
    }

    // Record what the agent CONFIRMED, not what we asked for, so a rejected or
    // rewritten pick is never re-asserted against the agent's own verdict
    // (`config_option_rejection` already tells the user about those). See
    // `SessionState::asserted_config_values` for why this is retained at all.
    let settled = current_config_option_values(&map_session_config_options(&options));
    //
    // Assigned unconditionally, empty result included: establishment OWNS the
    // ledger. `SessionState` spans fork transitions, so merging into whatever
    // the previous session left behind would defend values this session never
    // asserted — and, when the agent rejected them here, values it has already
    // refused once.
    state.write().await.asserted_config_values = preferred_config_values
        .iter()
        .filter(|(config_id, value_id)| settled.get(*config_id) == Some(*value_id))
        .map(|(config_id, value_id)| (config_id.clone(), value_id.clone()))
        .collect();

    options
}

/// Compare an agent-pushed config-option list against the values codeg asserted
/// at session establishment and return the ones the push contradicts, removing
/// each from the ledger as it is returned, **in application order**.
///
/// Removing on read is what bounds this: an option can be re-asserted at most
/// once per session, so an agent that re-pins unconditionally costs one extra
/// round-trip instead of an endless ping-pong. An empty ledger (the state after
/// the first prompt, and for every agent that doesn't push) makes this a cheap
/// read of a `BTreeMap` that is almost always empty.
///
/// The order is not cosmetic, and a `BTreeMap`'s alphabetical one is wrong.
/// Reverting a model re-pin is precisely the case where a single push drifts
/// BOTH the model and the effort hanging off it, and replaying `effort` before
/// `model` lets the model switch re-scope effort right back — with both ledger
/// entries already spent. `order_preferred_config_values` is the same
/// model-first rule the establishment replay uses, for the same reason.
async fn take_asserted_config_drift(
    state: &Arc<RwLock<SessionState>>,
    pushed: &[SessionConfigOption],
) -> Vec<(String, String)> {
    if state.read().await.asserted_config_values.is_empty() {
        return Vec::new();
    }
    let pushed_values = current_config_option_values(&map_session_config_options(pushed));
    let drifted: BTreeMap<String, String> = {
        let mut snapshot = state.write().await;
        let drifted: BTreeMap<String, String> = snapshot
            .asserted_config_values
            .iter()
            .filter(|(config_id, asserted)| {
                pushed_values
                    .get(*config_id)
                    .is_some_and(|pushed| pushed != *asserted)
            })
            .map(|(config_id, asserted)| (config_id.clone(), asserted.clone()))
            .collect();
        for config_id in drifted.keys() {
            snapshot.asserted_config_values.remove(config_id);
        }
        drifted
    };
    order_preferred_config_values(pushed, &drifted)
        .into_iter()
        .map(|(config_id, value_id)| (config_id.clone(), value_id.clone()))
        .collect()
}

const TERMINAL_POLL_INTERVAL_MS: u64 = 200;
const TERMINAL_POLL_MISSING_LIMIT: u8 = 10;

/// Hard cap on the size of a single ACP event's `raw_output` payload.
///
/// Agents (e.g. Claude Code, Codex) frequently send `tool_call_update`
/// notifications where `raw_output` is the **full accumulated** tool output
/// rather than an incremental delta. For long-running terminal tools this
/// leads to O(N²) bytes flowing through the event pipeline and multi-GB
/// transient allocations (serde_json Value trees, IPC buffers, broadcast
/// channel backlog). This constant caps any single emitted chunk so the
/// pipeline never sees a multi-MB event.
const MAX_SINGLE_EMIT_BYTES: usize = 64 * 1024;

/// Byte length of the tail we retain per tool-call to verify that the next
/// incoming snapshot is a cumulative extension of the previous one. Small
/// enough to keep the cache bounded even in pathological sessions, large
/// enough that a matching tail is an extremely unlikely coincidence.
const MAX_CACHED_TAIL_BYTES: usize = 8 * 1024;

/// Hard cap on the number of tool-call entries the cache retains. Prevents
/// unbounded growth in long sessions where agents forget to mark tool calls
/// as completed. Entries are evicted FIFO by generation counter.
const MAX_CACHE_ENTRIES: usize = 256;

/// Prefix used when an emitted chunk had to be truncated.
const TRUNCATION_MARKER: &str = "[...truncated...]\n";

#[derive(Debug)]
struct CachedOutput {
    /// Total byte length of the last observed `raw_output`.
    total_len: usize,
    /// Tail of the last observed `raw_output`, up to `MAX_CACHED_TAIL_BYTES`
    /// bytes. Always aligned to a UTF-8 character boundary at the start.
    tail: String,
    /// Monotonic insertion/update tick used for FIFO eviction.
    generation: u64,
}

/// Per-session cache of the last `raw_output` fingerprint emitted for each
/// tool call. Enables delta detection: when an agent sends cumulative
/// snapshots, we forward only the suffix (with `raw_output_append=true`)
/// and keep the fingerprint bounded so it works even when the full output
/// grows into the multi-MB range.
#[derive(Debug, Default)]
struct ToolCallOutputCache {
    entries: HashMap<String, CachedOutput>,
    next_generation: u64,
}

impl ToolCallOutputCache {
    /// Diff an incoming full `raw_output` snapshot for `tool_call_id` against
    /// the cache and return what should be emitted downstream.
    ///
    /// Returns `None` when the incoming snapshot is identical to the
    /// previously emitted one (nothing to send). Otherwise returns
    /// `(payload, append)` where:
    /// - `append=true` — `payload` is a (possibly truncated) suffix delta;
    ///   the frontend should append it to the existing chunks.
    /// - `append=false` — `payload` is a (possibly truncated) replacement
    ///   for the full tool output; the frontend should reset chunks.
    fn consume(&mut self, tool_call_id: &str, curr: &str) -> Option<(String, bool)> {
        let curr_len = curr.len();

        let decision: Option<(String, bool)> = match self.entries.get(tool_call_id) {
            Some(prev) if curr_len >= prev.total_len && self.is_extension_of(prev, curr) => {
                if curr_len == prev.total_len {
                    // Identical output — nothing to emit. Cache stays fresh.
                    return None;
                }
                let suffix = &curr[prev.total_len..];
                Some(build_emit_payload(suffix, true))
            }
            _ => Some(build_emit_payload(curr, false)),
        };

        // Update cache snapshot to current state so the next update can
        // still detect a prefix extension.
        let tail =
            trim_partial_ansi_tail(truncate_tail_at_char_boundary(curr, MAX_CACHED_TAIL_BYTES))
                .to_string();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.entries.insert(
            tool_call_id.to_string(),
            CachedOutput {
                total_len: curr_len,
                tail,
                generation,
            },
        );
        self.enforce_entry_cap();
        decision
    }

    /// Seed the cache with an initial snapshot for `tool_call_id`, WITHOUT
    /// attempting to diff against any prior state. Used for the initial
    /// `SessionUpdate::ToolCall` notification, whose frontend reducer
    /// treats `raw_output` as a full replacement.
    fn seed(&mut self, tool_call_id: &str, curr: &str) -> Option<String> {
        let (payload, _append) = build_emit_payload(curr, false);
        let tail =
            trim_partial_ansi_tail(truncate_tail_at_char_boundary(curr, MAX_CACHED_TAIL_BYTES))
                .to_string();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.entries.insert(
            tool_call_id.to_string(),
            CachedOutput {
                total_len: curr.len(),
                tail,
                generation,
            },
        );
        self.enforce_entry_cap();
        if payload.is_empty() {
            None
        } else {
            Some(payload)
        }
    }

    /// Drop cached state for a tool call that has finished. Keeps the
    /// session-scoped cache bounded in long-running sessions.
    fn remove_if_final(&mut self, tool_call_id: &str, status: Option<&str>) {
        if matches!(status, Some("completed" | "failed" | "cancelled" | "error")) {
            self.entries.remove(tool_call_id);
        }
    }

    /// Returns true when the cached fingerprint matches `curr` at the
    /// expected offset — i.e. `curr` is a prefix extension (or identity)
    /// of the previously observed snapshot.
    fn is_extension_of(&self, prev: &CachedOutput, curr: &str) -> bool {
        let tail_start = prev.total_len.saturating_sub(prev.tail.len());
        curr.get(tail_start..prev.total_len)
            .is_some_and(|slice| slice == prev.tail.as_str())
    }

    /// Evict oldest entries (by `generation`) once the cache exceeds the
    /// entry cap. Linear scan over a bounded map, so O(MAX_CACHE_ENTRIES)
    /// per eviction — acceptable at this size.
    fn enforce_entry_cap(&mut self) {
        while self.entries.len() > MAX_CACHE_ENTRIES {
            let Some(oldest_id) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.generation)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest_id);
        }
    }
}

/// Apply the per-event size cap + truncation marker. Returns `(payload,
/// append)`. An empty `text` yields an empty `payload`; callers should
/// decide whether to suppress the emission in that case.
fn build_emit_payload(text: &str, append: bool) -> (String, bool) {
    let truncated =
        trim_partial_ansi_tail(truncate_tail_at_char_boundary(text, MAX_SINGLE_EMIT_BYTES));
    let out = if truncated.len() < text.len() {
        format!("{TRUNCATION_MARKER}{truncated}")
    } else {
        truncated.to_string()
    };
    (out, append)
}

/// Return a substring of `s` whose byte length is `<= max_bytes`, aligned to
/// a UTF-8 character boundary and taken from the TAIL of `s` (so the most
/// recent output is preserved when truncation is required).
fn truncate_tail_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// If the very end of `s` contains a partial ANSI escape sequence, trim it
/// so downstream ANSI parsers (e.g. the frontend `ansi-to-react` renderer)
/// don't see a half-emitted escape.
///
/// Handles the three common ACP-stream cases:
/// - CSI (`ESC [ ... final`): terminator is a byte in 0x40..=0x7E after
///   the `[` introducer.
/// - OSC (`ESC ] ... ST|BEL`): terminator is BEL (0x07) or `ESC \`.
/// - Simple two-byte escape (`ESC <byte>`): complete as soon as the byte
///   following ESC is present.
///
/// ESC is ASCII (1 byte), always a valid UTF-8 char boundary, so slicing
/// at `esc_pos` cannot produce an invalid UTF-8 string.
fn trim_partial_ansi_tail(s: &str) -> &str {
    let bytes = s.as_bytes();
    let Some(esc_pos) = bytes.iter().rposition(|&b| b == 0x1B) else {
        return s;
    };
    let after = &bytes[esc_pos + 1..];
    if after.is_empty() {
        return &s[..esc_pos];
    }
    let terminated = match after[0] {
        b'[' => after[1..].iter().any(|&b| (0x40..=0x7E).contains(&b)),
        b']' => {
            after[1..].contains(&0x07)
                || after[1..].windows(2).any(|w| w[0] == 0x1B && w[1] == b'\\')
        }
        // Two-byte escape sequences (ESC M, ESC D, …) are complete as
        // soon as the second byte is present.
        _ => true,
    };
    if terminated {
        s
    } else {
        &s[..esc_pos]
    }
}

#[derive(Debug, Default)]
struct TrackedTerminalToolCall {
    terminal_ids: Vec<String>,
    status: Option<String>,
    terminal_offsets: HashMap<String, u64>,
    terminal_exit_reported: HashSet<String>,
    has_emitted_output: bool,
    missing_polls: u8,
}

#[derive(Debug, Default)]
struct TerminalPollResult {
    output: Option<String>,
    append: bool,
    any_found: bool,
    all_exited: bool,
}

fn is_final_tool_call_status(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed"))
}

fn merge_terminal_ids(existing: &mut Vec<String>, incoming: Vec<String>) -> bool {
    let mut changed = false;
    for terminal_id in incoming {
        if !existing.iter().any(|id| id == &terminal_id) {
            existing.push(terminal_id);
            changed = true;
        }
    }
    changed
}

fn extract_terminal_ids(content: &[ToolCallContent]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terminal_ids = Vec::new();
    for item in content {
        if let ToolCallContent::Terminal(terminal) = item {
            let terminal_id = terminal.terminal_id.to_string();
            if seen.insert(terminal_id.clone()) {
                terminal_ids.push(terminal_id);
            }
        }
    }
    terminal_ids
}

/// Register the terminals a tool call names so `poll_tracked_terminal_tool_calls`
/// can stream their output, returning whether the poller should run now.
///
/// A terminal the AGENT hosts itself is excluded: pi and codex-acp both name it
/// by their own tool-call id (see `hosted_terminal_meta_marks_shell`), so it can
/// never resolve against `TerminalRuntime`. Tracking one buys a map entry plus
/// ten 200 ms polls per shell call that could only ever miss — the misses are
/// swallowed as `InvalidParams` in `poll_terminal_tool_call_output`, so the
/// entry just ages out silently at `TERMINAL_POLL_MISSING_LIMIT`. Their output
/// arrives on a `_meta` channel instead and is bridged in
/// `emit_conversation_update`.
///
/// Keyed off the adapters' own marker rather than off the agent type wholesale,
/// so a future release that DOES delegate `terminal/*` is polled normally.
fn track_terminal_tool_calls(
    agent_type: AgentType,
    update: &SessionUpdate,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) -> bool {
    let meta = match update {
        SessionUpdate::ToolCall(tc) => tc.meta.as_ref(),
        SessionUpdate::ToolCallUpdate(tcu) => tcu.meta.as_ref(),
        _ => None,
    };
    if hosted_terminal_meta_marks_shell(agent_type, meta) {
        return false;
    }
    match update {
        SessionUpdate::ToolCall(tc) => {
            let terminal_ids = extract_terminal_ids(&tc.content);
            if terminal_ids.is_empty() {
                return false;
            }

            let status = format!("{:?}", tc.status).to_lowercase();
            let entry = tracked.entry(tc.tool_call_id.to_string()).or_default();
            let changed = merge_terminal_ids(&mut entry.terminal_ids, terminal_ids);
            entry.status = Some(status);
            changed
        }
        SessionUpdate::ToolCallUpdate(tcu) => {
            let mut changed = false;
            let mut should_track = false;

            let terminal_ids = tcu
                .fields
                .content
                .as_ref()
                .map(|content| extract_terminal_ids(content))
                .unwrap_or_default();
            if !terminal_ids.is_empty() {
                should_track = true;
            }

            if tracked.contains_key(&tcu.tool_call_id.to_string()) {
                should_track = true;
            }

            if !should_track {
                return false;
            }

            let entry = tracked.entry(tcu.tool_call_id.to_string()).or_default();
            if !terminal_ids.is_empty() {
                changed = merge_terminal_ids(&mut entry.terminal_ids, terminal_ids);
            }

            if let Some(status) = tcu.fields.status {
                let status_str = format!("{:?}", status).to_lowercase();
                if entry.status.as_deref() != Some(status_str.as_str()) {
                    changed = true;
                }
                entry.status = Some(status_str);
            }

            changed
        }
        _ => false,
    }
}

/// The body of the `[terminal exited: …]` line every terminal-backed tool call
/// ends with. The frontend reads that line back: a search that matched nothing
/// ends `[terminal exited: exit code: 1]`, and `isGrepNoMatchCommandResult`
/// (`src/lib/grep-no-match.ts`) matches it verbatim, so rewording it here needs
/// the same change there.
fn format_terminal_exit_status(exit_status: &TerminalExitStatus) -> String {
    let mut parts = Vec::new();
    if let Some(code) = exit_status.exit_code {
        parts.push(format!("exit code: {code}"));
    }
    if let Some(signal) = &exit_status.signal {
        parts.push(format!("signal: {signal}"));
    }
    if parts.is_empty() {
        "finished".to_string()
    } else {
        parts.join(", ")
    }
}

async fn poll_terminal_tool_call_output(
    terminal_runtime: &TerminalRuntime,
    session_id: &SessionId,
    tracked: &mut TrackedTerminalToolCall,
) -> Result<TerminalPollResult, TerminalRuntimeError> {
    let mut chunks: Vec<String> = Vec::new();
    let mut any_found = false;
    let mut all_exited = true;
    let include_headers = tracked.terminal_ids.len() > 1;

    for terminal_id in &tracked.terminal_ids {
        let from_offset = tracked.terminal_offsets.get(terminal_id).copied();
        let response = match terminal_runtime
            .terminal_output_delta(session_id.0.as_ref(), terminal_id, from_offset)
            .await
        {
            Ok(response) => response,
            Err(TerminalRuntimeError::InvalidParams(_)) => continue,
            Err(err) => return Err(err),
        };

        any_found = true;
        tracked
            .terminal_offsets
            .insert(terminal_id.clone(), response.next_offset);

        if response.exit_status.is_none() {
            all_exited = false;
        }

        let mut chunk = String::new();
        if include_headers {
            chunk.push_str(&format!("[Terminal: {terminal_id}]\n"));
        }

        if response.had_gap {
            chunk.push_str("[output truncated]\n");
        }

        if !response.output.is_empty() {
            chunk.push_str(&response.output);
            if !chunk.ends_with('\n') {
                chunk.push('\n');
            }
        }

        if response.truncated && from_offset.is_none() {
            chunk.push_str("[output truncated]\n");
        }

        if let Some(exit_status) = response.exit_status {
            if tracked.terminal_exit_reported.insert(terminal_id.clone()) {
                chunk.push_str(&format!(
                    "[terminal exited: {}]\n",
                    format_terminal_exit_status(&exit_status)
                ));
            }
        }

        if chunk.ends_with('\n') {
            chunk.pop();
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
    }

    if !any_found {
        all_exited = false;
    }

    let append = tracked.has_emitted_output;
    if !chunks.is_empty() {
        tracked.has_emitted_output = true;
    }

    Ok(TerminalPollResult {
        output: if chunks.is_empty() {
            None
        } else {
            Some(chunks.join("\n\n"))
        },
        append,
        any_found,
        all_exited,
    })
}

async fn emit_terminal_output_update(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tool_call_id: &str,
    output: String,
    append: bool,
) {
    // Safety cap: when a subprocess writes very fast between poll ticks,
    // the delta produced by `poll_terminal_tool_call_output` can still be
    // up to ~1 MB (the terminal buffer limit). Enforce the pipeline-wide
    // single-event cap (with ANSI-safe truncation) before emission so the
    // WS/IPC fanout never carries a multi-MB payload.
    let (payload, _append) = build_emit_payload(&output, append);
    emit_with_state(
        state,
        emitter,
        AcpEvent::ToolCallUpdate {
            tool_call_id: tool_call_id.to_string(),
            title: None,
            status: None,
            content: None,
            raw_input: None,
            raw_output: Some(payload),
            raw_output_append: Some(append),
            locations: None,
            meta: None,
            images: None,
        },
    )
    .await;
}

async fn poll_tracked_terminal_tool_calls(
    terminal_runtime: &TerminalRuntime,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) {
    if tracked.is_empty() {
        return;
    }

    let tool_call_ids: Vec<String> = tracked.keys().cloned().collect();
    let mut remove_ids: Vec<String> = Vec::new();

    for tool_call_id in tool_call_ids {
        let Some(entry) = tracked.get_mut(&tool_call_id) else {
            continue;
        };
        if entry.terminal_ids.is_empty() {
            remove_ids.push(tool_call_id.clone());
            continue;
        }

        let poll_result =
            match poll_terminal_tool_call_output(terminal_runtime, session_id, entry).await {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!(
                        "[ACP] Failed to poll terminal output for tool call {}: {:?}",
                        tool_call_id, err
                    );
                    continue;
                }
            };

        if poll_result.any_found {
            entry.missing_polls = 0;
        } else {
            entry.missing_polls = entry.missing_polls.saturating_add(1);
        }

        if let Some(output) = poll_result.output {
            emit_terminal_output_update(state, emitter, &tool_call_id, output, poll_result.append)
                .await;
        }

        if (is_final_tool_call_status(entry.status.as_deref())
            && (!poll_result.any_found || poll_result.all_exited))
            || entry.missing_polls >= TERMINAL_POLL_MISSING_LIMIT
        {
            remove_ids.push(tool_call_id.clone());
        }
    }

    for tool_call_id in remove_ids {
        tracked.remove(&tool_call_id);
    }
}

/// Append the just-ended turn's observed span to the timing journal (see
/// `crate::turn_timings`). `probe` is `Some((send_stamp, prompt_hash))` only
/// on agents codeg journals for (Cursor) and is consumed on the first
/// journaling terminal path, so a turn appends at most one line.
///
/// ONLY cleanly completed turns are journaled — callers gate on the
/// NORMALIZED stop reason (`reason_str == "end_turn"`, which a raw
/// `end_turn` with no agent output does NOT satisfy: it reclassifies to
/// `"empty"` and is excluded). A canceled or empty turn may never have been
/// persisted by Cursor at all, and journaling such a phantom re-opens the
/// misassignment the parser's guards exist to prevent: a later same-hash
/// store turn could pair with the phantom's line even across non-contiguous
/// positions (Codex review R4-2). An unjournaled-but-persisted turn
/// mid-session merely stops the reverse walk (older turns lose their
/// clocks); when such turns make up the session's TAIL, the second accepted
/// residual in `turn_timings`' module docs applies (a stale journal tail can
/// hash-collide with the store's newest turn).
///
/// The append is queued to the journal's single-writer thread and awaited
/// with a short timeout: the normal case lands in microseconds BEFORE the
/// TurnComplete emit (so the post-turn reparse deterministically sees it),
/// while a hung filesystem blocks neither the turn loop nor any Tokio pool —
/// the queued job just lands late (still in order; the FIFO queue is what
/// makes overtaking structurally impossible) or is dropped at the queue cap.
async fn journal_turn_span(
    probe: &mut Option<(u64, String, u64)>,
    connection_id: &str,
    session_id: &str,
) {
    let Some((started_at_ms, prompt_sha, ord)) = probe.take() else {
        return;
    };
    let ack = crate::turn_timings::enqueue_turn_timing(
        crate::paths::codeg_turn_timings_root(),
        crate::turn_timings::CURSOR_JOURNAL_AGENT.to_string(),
        session_id.to_string(),
        crate::turn_timings::TurnTiming {
            v: crate::turn_timings::TURN_TIMING_SCHEMA_VERSION,
            ord,
            conn: connection_id.to_string(),
            prompt_sha,
            started_at_ms,
            ended_at_ms: crate::turn_timings::now_epoch_ms(),
        },
    );
    // Determinism window only — a timeout (or a dropped job's closed channel)
    // means the entry lands late or not at all, degrading that turn to a
    // missing footer clock. (See `turn_timings`' module docs for the two
    // narrow accepted residuals where missing lines can shift alignment.)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ack).await;
}

/// The image mime types grok's normalizer actually decodes — verbatim the set
/// its own clipboard reader accepts (`xai-grok-shared/src/clipboard.rs`), and
/// the boundary between the two carriages in [`normalize_grok_image_blocks`].
///
/// Measured against grok 1.0.0: png / webp / bmp / tiff round-trip through the
/// describe sidecar (jpeg is one of the two formats grok itself re-encodes to,
/// gif rides the same table). `image/svg+xml` does NOT — its validator answers
/// `unsupported or unrecognised image format` and the image never reaches the
/// model. Deliberately an allow-list, not a `image/*` prefix test: a mime we
/// have no evidence for keeps the resource carriage, which is where it already
/// was, so a wrong guess here can never be a regression.
fn grok_decodes_image_mime(mime: &str) -> bool {
    matches!(
        mime.trim().to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff"
    )
}

/// Whether a mime names an image at all — the outer guard on the demotion, so a
/// malformed `Image` block carrying something else entirely is left untouched.
/// Normalized exactly like [`grok_decodes_image_mime`]: if the two guards
/// disagreed about the same string (`IMAGE/SVG+XML` reads as "not an image" to a
/// case-sensitive test), an undecodable format would slip through as native.
fn is_image_mime(mime: &str) -> bool {
    mime.trim().to_ascii_lowercase().starts_with("image/")
}

/// Put every attached image on the carriage grok can actually read.
///
/// Two encodings carry the same bytes and grok treats them very differently:
///
/// * A native `Image` block runs its describe sidecar — the ONLY path where the
///   model sees pixels (see [`effective_prompt_capabilities`]). But grok
///   validates the format first and DROPS anything it cannot decode.
/// * A `Resource` blob is saved into the session's `assets/` and announced to
///   the model as a file path. No pixels, but for a text-shaped image (svg) the
///   model can just read the source — measurably better than a drop.
///
/// So decodable images are promoted (queued drafts and work-task prompts
/// composed before codeg advertised `image:true` still carry the old shape),
/// and undecodable ones are demoted back — the composer only sees the single
/// `image` capability bit and cannot make this call per mime.
fn normalize_grok_image_blocks(blocks: Vec<PromptInputBlock>) -> Vec<PromptInputBlock> {
    blocks
        .into_iter()
        .enumerate()
        .map(|(index, block)| match block {
            PromptInputBlock::Resource {
                uri,
                mime_type: Some(mime),
                text: None,
                blob: Some(blob),
            } if grok_decodes_image_mime(&mime) && !blob.is_empty() => PromptInputBlock::Image {
                data: blob,
                mime_type: mime,
                uri: Some(uri),
            },
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } if is_image_mime(&mime_type)
                && !grok_decodes_image_mime(&mime_type)
                && !data.is_empty() =>
            {
                PromptInputBlock::Resource {
                    // A pasted image has no path; its position in this prompt is
                    // the stable identifier, same as the work-task engine's.
                    uri: uri.unwrap_or_else(|| format!("clipboard://grok-image-{index}")),
                    mime_type: Some(mime_type),
                    text: None,
                    blob: Some(data),
                }
            }
            other => other,
        })
        .collect()
}

/// `pub(crate)` for one reader beyond this module: the ACP-native history
/// parser's parity test, which needs the EXACT wire bytes `record_prompt`
/// writes in order to assert its projection equals the live one
/// ([`crate::acp::types::user_blocks_from_prompt`]). Rebuilding those bytes by
/// hand in the test would let the two drift without failing anything.
pub(crate) fn map_prompt_blocks(blocks: Vec<PromptInputBlock>) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            PromptInputBlock::Text { text } => ContentBlock::Text(TextContent::new(text)),
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } => ContentBlock::Image(ImageContent::new(data, mime_type).uri(uri)),
            PromptInputBlock::Resource {
                uri,
                mime_type,
                text,
                blob,
            } => {
                let resource = match (text, blob) {
                    (Some(text_value), _) => {
                        let content =
                            TextResourceContents::new(text_value, uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::TextResourceContents(content)
                    }
                    (None, Some(blob_value)) => {
                        let content =
                            BlobResourceContents::new(blob_value, uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::BlobResourceContents(content)
                    }
                    (None, None) => {
                        let content =
                            TextResourceContents::new("", uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::TextResourceContents(content)
                    }
                };
                ContentBlock::Resource(EmbeddedResource::new(resource))
            }
            PromptInputBlock::ResourceLink {
                uri,
                name,
                mime_type,
                description,
            } => {
                let mut link = ResourceLink::new(name, uri);
                link.mime_type = mime_type;
                link.description = description;
                ContentBlock::ResourceLink(link)
            }
        })
        .collect()
}

/// The single final agent boundary. `delegation_enabled` is this connection's
/// launch verdict, so routing is appended for EVERY agent that received the
/// companion's delegation group — that flag is the only gate, and it is already
/// the injection gate's own verdict (`supports_mcp` + `agent_delivers_wire_mcp`
/// + the delegation feature being on).
fn prepare_agent_bound_prompt(
    agent_type: AgentType,
    mut blocks: Vec<PromptInputBlock>,
    delegation_enabled: bool,
    role: Option<&crate::models::agent_role::AgentRole>,
    team_members: &[TeamMember],
) -> Vec<ContentBlock> {
    // [crewchat] Role frame (Phase 2.2) FIRST: the persona is the most
    // system-like block, and it is delegation-independent — a team
    // conversation keeps its leader's persona even with codeg-mcp absent.
    if let Some(role) = role {
        append_role_frame(&mut blocks, role);
    }
    append_agent_routes(&mut blocks, delegation_enabled);
    // [crewchat] Team frame (Phase 1.3) — appended after the routing frame so
    // the member list is the last internal block the agent reads. Gated on
    // the same launch verdict: without the codeg-mcp delegation group there
    // is nothing the member list could route to.
    if delegation_enabled {
        append_team_frame(&mut blocks, team_members);
    }
    if agent_type == AgentType::Grok {
        blocks = normalize_grok_image_blocks(blocks);
    }
    map_prompt_blocks(blocks)
}

/// The mode a fork should carry into its child: the event-tracked mode of the
/// live parent, but only when that parent actually advertises modes.
///
/// `SessionState` spans nested fork transitions, so `current_mode` alone can
/// belong to an ancestor. `parent_modes` is the capability gate that discards
/// it — see [`ForkExitInfo::inherited_mode_id`].
fn live_mode_for_fork(
    state: &SessionState,
    parent_modes: Option<&SessionModeState>,
) -> Option<String> {
    parent_modes.and(state.current_mode.clone())
}

/// Result when the conversation loop exits due to a fork request.
struct ForkExitInfo {
    fork_response: agent_client_protocol::schema::v1::ForkSessionResponse,
    /// Raw top-level `models` from the fork response (Grok per-model effort data),
    /// captured before the typed deserialize drops it. `None` when absent.
    fork_models_raw: Option<serde_json::Value>,
    /// The parent's live mode, or `None` when the parent advertised no modes.
    ///
    /// Captured here rather than read off `SessionState` in the fork handler
    /// because `emit_session_modes` is a no-op for a modes-less session
    /// (`connection.rs`, `if let Some(mode_state) = modes`), so
    /// `current_mode` survives a transition into one and would hand an
    /// ancestor's mode to a child that does advertise modes. The parent's
    /// `AgentSession` is the only capability answer that can't go stale.
    /// Its `current_mode_id` is NOT used — codeg tracks mode changes through
    /// events, and the attach-time snapshot never sees them.
    inherited_mode_id: Option<String>,
    original_session_id: String,
    reply: tokio::sync::oneshot::Sender<Result<crate::acp::types::ForkProtocolResult, AcpError>>,
    connection: ConnectionTo<Agent>,
}

/// After `run_conversation_loop` returns, handle normal exit or fork transition.
///
/// When fork is requested, the original session has already been dropped by the
/// caller. The forked session (S2) is then RE-ESTABLISHED with `session/resume`
/// before it is attached — see the comment at the resume call for why the
/// `ForkSessionResponse` alone is not enough to prompt on.
#[allow(clippy::too_many_arguments)]
async fn handle_fork_or_exit(
    loop_result: Result<Option<ForkExitInfo>, agent_client_protocol::Error>,
    conn_id: &str,
    emitter: &EventEmitter,
    state: &Arc<RwLock<SessionState>>,
    agent_type: AgentType,
    perms: &PendingPermissions,
    cmd_rx: &mut mpsc::Receiver<ConnectionCommand>,
    terminal_runtime: Arc<TerminalRuntime>,
    cwd: &std::path::Path,
    cwd_string: &str,
    // `session_capabilities.resume` from initialize, plus the connection's MCP
    // server list: together they are everything `build_resume_session_request`
    // needs to make the forked session real on the agent.
    supports_resume: bool,
    // `session_capabilities.close` from initialize: whether the session the
    // fork leaves can be released on the agent (see `close_forked_parent`).
    supports_close: bool,
    mcp_servers: &[McpServer],
    // Threaded through from run_connection: the connection-scoped prompt
    // ledger (the forked session's loop keeps fingerprinting into the SAME
    // ledger the still-running watcher consumes from).
    prompt_ledger: &background_watch::PromptLedger,
    // Threaded through from run_connection so the forked session's
    // run_conversation_loop call has the same delegation cascade
    // capability as the original.
    delegation_injection: Option<&DelegationInjection>,
    // Same rationale: the forked session keeps writing into (and reading from)
    // the SAME connection-scoped stderr buffer — the agent process is unchanged
    // across a fork, so its stderr history stays relevant.
    stderr_tail: &Arc<StderrTail>,
) -> Result<(), agent_client_protocol::Error> {
    let fork_info = match loop_result {
        Ok(Some(info)) => info,
        Ok(None) => return Ok(()),
        Err(e) => return Err(e),
    };

    let cx = fork_info.connection;
    let fork_resp = fork_info.fork_response;
    let fork_models_raw = fork_info.fork_models_raw;
    let new_sid = fork_resp.session_id.0.to_string();

    // Carry the parent session's selectors across the fork, read BEFORE any
    // emit below replaces them with the new session's.
    //
    // A fork continues the same conversation, so its mode and model must
    // continue too — but nothing on the agent side arranges that. `session/new`
    // semantics apply: claude builds the resumed session from its own defaults,
    // and codex's fork response describes the thread as freshly configured. The
    // gap only became visible once the fork stopped attaching claude's
    // (empty, modes-less) fork response, because an empty response overwrote
    // nothing and the composer simply kept showing the parent's selectors; a
    // populated one resets them. Restoring them here is the same machinery a
    // reconnect uses, and it is a no-op per option when the value already
    // matches, so an agent that does inherit pays nothing.
    //
    // Mode comes pre-gated from the fork request (`ForkExitInfo::
    // inherited_mode_id`); config values are read here because
    // `emit_session_config_options_values` always writes the list — an agent
    // with no config options leaves an empty one, not a stale one.
    let inherited_mode_id = fork_info.inherited_mode_id;
    let inherited_config_values = state
        .read()
        .await
        .config_options
        .as_deref()
        .map(current_config_option_values)
        .unwrap_or_default();

    tracing::info!(
        "[ACP] Fork transition: attaching to forked session {} (original: {})",
        new_sid, fork_info.original_session_id
    );
    tracing::info!(
        "[ACP] Fork inheriting selectors: mode={:?} config={:?}",
        inherited_mode_id,
        inherited_config_values
    );

    // Reply protocol-level result to manager.fork_session, which will combine
    // it with the freshly-created sibling row id to produce the wire ForkResultInfo.
    let original_session_id = fork_info.original_session_id;
    let _ = fork_info
        .reply
        .send(Ok(crate::acp::types::ForkProtocolResult {
            forked_session_id: new_sid.clone(),
            original_session_id: original_session_id.clone(),
        }));

    // Make the forked session REAL on the agent before anything prompts on it.
    //
    // `session/fork` hands back a session id, but on both adapters that
    // implement it that id is not yet usable — in two different, equally silent
    // ways:
    //
    //   * claude-agent-acp 0.73.0's `unstable_forkSession` returns the SDK's
    //     `{ sessionId }` verbatim and never inserts it into its own `sessions`
    //     map, so the first `session/prompt` hits the `if (!session) throw new
    //     Error("Session not found")` guard at the top of `prompt()`. It also
    //     returns no modes and no config options at all.
    //   * codex-acp 1.8.0–2.1.x's `SessionFork` calls `threadUnsubscribe` on
    //     the freshly forked thread to release its writer lock. A prompt on it
    //     then runs to completion inside codex — the rollout file grows — but
    //     the core streams no `turn/*` notifications to an unsubscribed thread,
    //     so `runTurn` awaits a completion event that never arrives: the turn
    //     hangs forever and not one token reaches the transcript. 2.2.0 keeps
    //     the fork subscribed, so a prompt on it works at once and the resume
    //     below is redundant there, but not harmful: measured live, fork →
    //     resume → close the parent → prompt the fork works on 2.1.1 and 2.2.1
    //     alike. It stays for the older adapters a PATH or a custom version
    //     can still resolve.
    //
    // `session/resume` repairs both: claude's `getOrCreateSession` creates the
    // session under the SAME id (`createSession` uses `resume` as the id), and
    // codex's `resumeSession` re-subscribes the thread. Both adapters advertise
    // it. Resume — not load — because load would replay the whole forked history
    // for us to drain and discard; the transcript the user sees comes from the
    // disk parser (same reasoning as the reconnect ladder above).
    //
    // Degradation is deliberate and total: an agent that forks WITHOUT
    // advertising resume, or whose resume fails, falls back to attaching the
    // fork response exactly as before. Neither is worse off than it was before
    // this call existed.
    let resumed = if supports_resume {
        let resume_req = build_resume_session_request(
            agent_type,
            SessionId::new(new_sid.clone()),
            cwd,
            mcp_servers.to_vec(),
        );
        match send_resume_session(&cx, resume_req).await {
            Ok(pair) => Some(pair),
            Err(e) => {
                tracing::warn!(
                    "[ACP] session/resume on the forked session failed ({e}); \
                     attaching to the fork response as-is"
                );
                None
            }
        }
    } else {
        None
    };

    // Release the session the fork left. codeg stopped routing it when the
    // caller dropped its `AgentSession`, and the sibling row
    // `persist_fork_outcome` keeps for its history re-opens it on a connection
    // of its own. Left open on the agent, it is not free:
    //
    //   * codex holds a thread's writer lock for as long as an app-server has
    //     the thread loaded, and `session/fork` never lets the PARENT go (up to
    //     2.1.x it unsubscribes only the child; 2.2.0 not even that).
    //     An open parent stays loaded for the life of this connection, and
    //     opening the sibling row fails with `session_busy`. Closing is
    //     `thread/unsubscribe`, which is also what codex's own TUI does to the
    //     thread it forks from. The app-server then unloads the thread, and
    //     with it the lock, once it has gone `thread_unload_delay_secs` (60 by
    //     default) without a subscriber or a running turn.
    //   * claude-agent-acp runs each session in a CLI process of its own, so
    //     an open parent is one more process per fork, still able to start a
    //     turn by itself (a background task finishing does) where nothing
    //     shows it.
    //
    // This is the agent-side half of what the caller just did to the parent's
    // codeg-hosted terminals (`release_all_for_session`). It goes out after the
    // resume step, whether that resumed the child, failed, or was skipped for
    // an agent without resume, so making the child usable never overlaps the
    // parent's teardown. It is not awaited: a slow close (deepseek-acp waits
    // for the session's MCP servers to exit) must not hold up the child's
    // first prompt.
    if supports_close {
        close_forked_parent(&cx, &original_session_id, &new_sid);
    }

    // Prefer the resume response: it describes the session as the agent holds it
    // right now, and for claude it is the ONLY source of modes/config options.
    let (initial_config_options, new_resp, models_raw) = match resumed {
        Some((resume_resp, resume_models_raw)) => (
            resume_resp.config_options.clone(),
            NewSessionResponse::new(SessionId::new(new_sid.clone()))
                .modes(resume_resp.modes)
                .config_options(resume_resp.config_options)
                .meta(resume_resp.meta),
            resume_models_raw,
        ),
        None => (
            fork_resp.config_options.clone(),
            NewSessionResponse::new(fork_resp.session_id)
                .modes(fork_resp.modes)
                .config_options(fork_resp.config_options)
                .meta(fork_resp.meta),
            fork_models_raw,
        ),
    };
    let grok_meta = if agent_type == AgentType::Grok {
        new_resp.meta.clone()
    } else {
        None
    };
    // Opportunistic: grok may carry per-model effort data on a fork (or, when
    // the fork was re-established above, a resume) response.
    let grok_model_specs =
        (agent_type == AgentType::Grok).then(|| parse_grok_model_specs(models_raw.as_ref()));
    let mut session = AgentSession::attach(&cx, new_resp)?;

    // A fork is a new session id, hence a new transcript file. Its history
    // starts empty and accumulates from the fork point — the pre-fork turns
    // stay in the parent's transcript, which is what forking means.
    record_transcript_header(agent_type, &new_sid, cwd_string);
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionStarted {
            session_id: new_sid.clone(),
        },
    )
    .await;
    emit_session_modes(state, emitter, session.modes()).await;
    apply_and_emit_session_config_options(
        &cx,
        &mut session,
        state,
        emitter,
        agent_type,
        grok_meta.as_ref(),
        grok_model_specs.as_ref(),
        inherited_mode_id.as_deref(),
        &inherited_config_values,
        initial_config_options.unwrap_or_default(),
    )
    .await;
    tracing::info!(
        "[ACP] Fork selectors after restore: mode={:?} model={:?}",
        state.read().await.current_mode,
        state
            .read()
            .await
            .config_options
            .as_deref()
            .and_then(current_model_id_from_opts)
    );
    emit_selectors_ready(state, emitter).await;

    let loop_result = run_conversation_loop(
        &mut session,
        conn_id,
        emitter,
        state,
        agent_type,
        perms,
        cmd_rx,
        terminal_runtime.clone(),
        cwd_string,
        true, // fork already succeeded on this process
        prompt_ledger,
        delegation_injection,
        stderr_tail,
    )
    .await;
    terminal_runtime.release_all_for_session(&new_sid).await;
    drop(session);

    // Recursively handle nested forks
    Box::pin(handle_fork_or_exit(
        loop_result,
        conn_id,
        emitter,
        state,
        agent_type,
        perms,
        cmd_rx,
        terminal_runtime,
        cwd,
        cwd_string,
        supports_resume,
        supports_close,
        mcp_servers,
        prompt_ledger,
        delegation_injection,
        stderr_tail,
    ))
    .await
}

/// Send `session/close` for `parent`, the session a fork just left, without
/// waiting for the answer (see the call in [`handle_fork_or_exit`]).
///
/// The request runs on a task the connection owns, so it ends with the
/// connection. Its outcome is only logged: codeg routes nothing to `parent` any
/// more, and a close that fails leaves the agent where it was before codeg sent
/// one.
///
/// Skipped when the fork answered with the parent's own id, since closing it
/// would close the session about to be attached.
fn close_forked_parent(cx: &ConnectionTo<Agent>, parent: &str, child: &str) {
    if parent == child {
        return;
    }
    let request = CloseSessionRequest::new(SessionId::new(parent.to_string()));
    let request_cx = cx.clone();
    let closed = parent.to_string();
    let close = async move {
        match request_cx
            .send_request_to(Agent, request)
            .block_task()
            .await
        {
            Ok(_) => tracing::info!("[ACP] Closed session {closed}, left by a fork"),
            Err(e) => tracing::warn!(
                "[ACP] session/close for session {closed}, left by a fork, failed: {e}"
            ),
        }
        // Never an error: a spawned task that fails takes the connection down.
        Ok(())
    };
    if let Err(e) = cx.spawn(close) {
        // Only when the connection is already going down, which ends the
        // session on the agent anyway.
        tracing::debug!("[ACP] not closing session {parent}, left by a fork: {e}");
    }
}

/// Main conversation command loop: wait for frontend commands and process them.
///
/// Map ACP `StopReason` to a stable lowercase string carried in the
/// `TurnComplete` event. Covers all 5 spec variants so non-success reasons
/// (`Refusal`/`MaxTokens`/`MaxTurnRequests`) keep their semantics instead of
/// collapsing to `"unknown"` — the lifecycle subscriber and frontend rely on
/// this distinction. The wildcard arm exists because the upstream enum is
/// `#[non_exhaustive]`.
fn stop_reason_to_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::Cancelled => "cancelled",
        StopReason::Refusal => "refusal",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        _ => "unknown",
    }
}

/// Classify a `session/load` failure into a stable frontend `code` when the
/// historical session cannot be restored — either the agent has no record of
/// it (`ResourceNotFound`, -32002) or the agent process/session died mid-load.
/// Claude 0.58.1 surfaces the latter as a -32603 Internal error whose message
/// contains "process exited with code N" (its `getOrCreateSession` only maps
/// "Query closed…"/"No conversation found…" to `ResourceNotFound`), so the
/// crash/ended family is matched on the wire message. Both codes route to the
/// same `SessionLoadFailed` banner (Reload / New conversation) instead of a raw
/// protocol error.
///
/// A third case is archived rather than lost: `codex archive <id>` parks a
/// rollout, and a later `session/load` answers -32603 with a body naming both
/// the session and the command that brings it back. That one is a *recoverable*
/// state, so it earns its own code — the banner can name the fix — but it takes
/// the same banner rather than the silent `session/new` fallback, which would
/// orphan a history the user is one command away from restoring.
///
/// Returns `None` for failures that must keep the existing behavior:
/// "Method not found" (agent lacks resume → silent `session/new` fallback),
/// "Authentication required" (silent stop), and any other error (emit
/// "starting new" then fall through to `session/new`).
fn classify_session_load_failure(
    code: agent_client_protocol::schema::v1::ErrorCode,
    message: &str,
) -> Option<&'static str> {
    if matches!(code, agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound) {
        return Some("resource_not_found");
    }
    // codex-acp on an archived rollout: the -32603 body reads
    // "session <id> is archived. Run `codex unarchive <id>` …". Matched on the
    // wire message for the same reason as the family below — the code is a
    // generic Internal error. Checked BEFORE that family so the more specific
    // (and recoverable) verdict wins if a body ever carries both signals.
    if message.contains("is archived") {
        return Some("session_archived");
    }
    // codex holds a per-thread writer lock for as long as an app-server has the
    // thread loaded. Two holders reach this arm:
    //  * another Codex client — the Codex app, the CLI or an IDE extension —
    //    with the same session open (its lock can outlive the closed tab until
    //    that process unloads the thread);
    //  * codeg itself, for about a minute after a fork: `session/fork`
    //    never releases the PARENT's lock (codex-acp 1.8.0–2.1.x unsubscribe
    //    only the child, `threadUnsubscribe({threadId: response.thread.id})`,
    //    and 2.2.0 keeps the child subscribed too), so the sibling row codeg
    //    creates to keep the pre-fork history hits the lock until the forking
    //    connection lets the parent go. It closes the parent right after the
    //    step that resumes the child (`close_forked_parent`), and the
    //    app-server unloads it `thread_unload_delay_secs` later (60 by
    //    default).
    // codex-acp ≤2.0.x passes codex's raw "thread <id> already has an active
    // writer" through as a -32603 `data.details`; 2.1.0 (#564) answers -32600
    // with `data.reason: "thread_active_writer"`, which
    // [`classify_session_load_error`] reads before this text — and keeps the
    // raw text in `details`, so this match still holds for both.
    //
    // Nothing is lost and nothing is broken: the session is busy, not gone. That
    // is why it must never fall through to `session/new` — doing so rebinds that
    // row to a fresh empty session and destroys the only pointer to the history
    // it exists to preserve. Releasing the other holder frees the lock, and the
    // banner's Reload then opens the session (verified live on both versions:
    // a load answered while another adapter process held the thread succeeds
    // once that process exits).
    if message.contains("already has an active writer") {
        return Some("session_busy");
    }
    if SESSION_GONE_MARKERS.iter().any(|s| message.contains(s)) {
        return Some("session_unavailable");
    }
    None
}

/// [`classify_session_load_failure`] for the error itself: the structured
/// signals in `data` first, then the code and the rendered message.
///
/// codex-acp 2.1.0 (#564) answers a `session/load` / `session/resume` of a
/// thread another app-server holds with
/// `{code: -32600, message: "Invalid request: This Codex session is in use by
/// another Codex client …", data: {reason: "thread_active_writer", threadId,
/// details: "<codex's raw message>"}}`, and documents `reason` as the stable
/// way to recognise it "without parsing the message". `details` is codex's
/// own wording, which has no such promise, so the message match in
/// [`classify_session_load_failure`] stays only as the fallback for older
/// adapters (≤2.0.x send `details` alone, under -32603).
fn classify_session_load_error(e: &agent_client_protocol::Error) -> Option<&'static str> {
    let reason = e
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str);
    if reason == Some("thread_active_writer") {
        return Some("session_busy");
    }
    // codex-acp 2.2: the app-server died under the open (see
    // [`codex_app_server_lost`]). Never the `session/new` fallback, whether or
    // not the adapter will restart it. Up to 2.1.x a dead app-server took the
    // connection with it, so that `session/new` failed as well; from 2.2.0 the
    // adapter restarts the app-server for the next request, and the
    // `session/new` SUCCEEDS — rebinding the row to a fresh empty session and
    // orphaning the history the row exists for. Measured live on 2.2.1 with
    // the app-server killed during each open: resume and load both answer
    // 1001, and a `session/new` right after them opens. A session whose open
    // crashes the app-server every time (a rollout too large to load, say)
    // would take that path on every reopen. The banner keeps the history, and
    // its Reload opens a new connection, which is also the adapter's own
    // remedy once it refuses ("Restart the agent").
    if codex_app_server_lost(e).is_some() {
        return Some("session_unavailable");
    }
    classify_session_load_failure(e.code, &e.to_string())
}

/// codex-acp 2.2's word that the Codex app-server behind the adapter died:
/// JSON-RPC code 1001 with `data: {exitCode, signal, restartable,
/// retryAfterMs?}` (`app-server-recovery/AppServerExit.ts`). `Some` carries
/// `restartable`; `None` is any other error.
///
/// From 2.2.0 the adapter supervises the app-server for every client, not only
/// for the session-index ones: a crash no longer ends the connection, and the
/// next request starts a new app-server and reopens the session on it. A turn
/// running at the crash settles at once (`end_turn` with a `connection`
/// `sessionFailure`, its open tool calls failed, its permission requests
/// withdrawn by `$/cancel_request`), which codeg's existing consumers already
/// render. What reaches a REQUEST is this error, two ways:
///  - `restartable: true` — "The Codex app-server was killed by SIGKILL, which
///    usually means it ran out of memory. The agent starts it again on the
///    next request." Only that request failed.
///  - `restartable: false` — the session cannot come back on this connection:
///    it never reached disk and the restart lost it ("Session <id> had no
///    messages yet and was lost when the Codex app-server restarted. Start a
///    new session.", #604), the crash-loop guard stopped restarting ("crashed
///    5 times in the last 5 min … Restart the agent, or try again in 5 min."),
///    or the thread is refused because opening it crashed the app-server twice
///    (`retryAfterMs` says for how long).
///
/// Every message above was captured live over stdio (2.2.1, the app-server
/// SIGKILLed), bar the refused thread, which is read from the source.
fn codex_app_server_lost(e: &agent_client_protocol::Error) -> Option<bool> {
    if !matches!(
        e.code,
        agent_client_protocol::schema::v1::ErrorCode::Other(1001)
    ) {
        return None;
    }
    e.data.as_ref()?.get("restartable")?.as_bool()
}

/// Wire-message markers for "the session behind this request no longer exists"
/// (claude-agent-acp 0.58.1):
///  - "process exited" → "Claude Code process exited with code 1", "The Claude
///    Agent process exited unexpectedly…"
///  - "session has ended" → SESSION_ENDED_MESSAGE
///  - "Session not found" → a plain Error rethrown as an Internal error
///
/// Matched on the message because the code that carries them is a generic
/// -32603. Shared by the two places that must agree on the verdict:
/// [`classify_session_load_failure`] (a `session/load` that can't be retried)
/// and [`prompt_rejection_is_terminal`] (a `session/prompt` rejection that no
/// later prompt on this connection could survive either).
const SESSION_GONE_MARKERS: &[&str] =
    &["process exited", "session has ended", "Session not found"];

/// Whether a `session/prompt` rejection means the CONNECTION is dead, or only
/// this turn.
///
/// This is the difference between the two exits in the prompt-response arm of
/// [`run_conversation_loop`]: `true` propagates the error, which unwinds
/// `run_connection` into a terminal `Error` → `Disconnected` (and the lifecycle
/// worker flips the conversation row to `Cancelled`); `false` ends the TURN and
/// leaves the session addressable, so the composer stays usable and the next
/// prompt just works.
///
/// Turn-scoped is the DEFAULT, because an agent that answered at all is an
/// agent that is still there. Every ACP agent codeg drives rejects some prompts
/// it is perfectly healthy to keep talking to: qwen-code answers -32603
/// `Slash command not supported in ACP integration: …` for a `/mcp` its ACP
/// surface doesn't implement (issue #797) and keeps the session in its map;
/// opencode wraps any provider-side turn error (rate limit, quota, invalid
/// request) into -32603 and its very next prompt ends with `end_turn` (issue
/// #659). Tearing the agent down for those is pure self-harm — the user waits
/// out a full respawn for a turn that merely failed.
///
/// Only four families stay terminal:
///  - `ResourceNotFound` — the agent has no record of the session id codeg
///    just prompted on, so the handle this connection holds is void.
///  - [`SESSION_GONE_MARKERS`] — the agent answered to say its session or
///    process is gone. Keeping the connection would leave an entry whose every
///    future prompt fails the same way.
///  - [`codex_app_server_lost`] with `restartable: false` — codex-acp 2.2's
///    word that this session cannot come back on this connection (lost before
///    it reached disk, the crash-loop guard, a refused thread). Every later
///    prompt fails the same way, and the remedies the adapter names ("Start a
///    new session", "Restart the agent") both take a new connection. A
///    restartable loss stays turn-scoped: the next prompt restarts the
///    app-server and goes through.
///  - [`lost_the_connection`] — the ACP runtime's own word that no answer can
///    arrive at all: the transport, not the turn, is what died.
///
/// `AuthRequired` short-circuits ahead of all of them: it was unconditionally
/// turn-scoped before this classifier existed, and the message checks below
/// read an agent-controlled string (`Display` renders `data` too), so without
/// the early return a sign-out prompt that happened to quote one of the markers
/// would start tearing connections down.
fn prompt_rejection_is_terminal(e: &agent_client_protocol::Error) -> bool {
    if matches!(e.code, agent_client_protocol::schema::v1::ErrorCode::AuthRequired) {
        return false;
    }
    if matches!(e.code, agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound) {
        return true;
    }
    if codex_app_server_lost(e) == Some(false) {
        return true;
    }
    if lost_the_connection(e) {
        return true;
    }
    let text = e.to_string();
    SESSION_GONE_MARKERS.iter().any(|s| text.contains(s))
}

/// Whether `e` is the runtime saying no answer can arrive on this connection
/// any more.
///
/// It says so two ways. `Incoming transport closed`
/// ([`agent_client_protocol::is_incoming_transport_closed`]) is what every
/// request still in flight gets when the agent's output reaches EOF — it exited
/// or closed stdout — and what a request sent after that gets at once.
/// "response to … never received" is the response channel dropped under
/// `SentRequest::block_task`: the connection's driver itself went away.
fn lost_the_connection(e: &agent_client_protocol::Error) -> bool {
    if agent_client_protocol::is_incoming_transport_closed(e) {
        return true;
    }
    // Both halves of the runtime's own sentence, so an agent quoting "never received"
    // about its own upstream doesn't read as a dead transport.
    let text = e.to_string();
    text.contains("response to ") && text.contains("never received")
}

/// How long `run_connection` holds back the error for a connection it has lost
/// — see [`defer_to_connection_report`].
const CONNECTION_LOSS_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Hold a lost-connection error back for [`CONNECTION_LOSS_GRACE`] before
/// `run_connection`'s main future returns it.
///
/// Losing the connection is not the main future's news to break. The
/// connection's own driver is ending too, and it carries the reason: for an
/// agent that died, the exit status and the tail of its stderr, from
/// `agent_process`'s child monitor. `connect_with` returns whichever of the two
/// finishes first, and the main future can hear first — the runtime fails the
/// in-flight request (the handshake, a prompt) as soon as the agent's output
/// reaches EOF, and nothing makes that wait for the monitor to reap the process
/// and collect its stderr. Returning at once would put a bare `Incoming
/// transport closed` where that report belongs. Waiting lets the driver's
/// report win whenever there is one; the wait only runs out on an agent that
/// exited cleanly (or left stderr open behind it), and then this error is all
/// there is.
async fn defer_to_connection_report(
    e: agent_client_protocol::Error,
) -> agent_client_protocol::Error {
    tokio::time::sleep(CONNECTION_LOSS_GRACE).await;
    e
}

/// Whether codeg can absorb a "the agent forgot this session" load failure by
/// itself, rather than stopping and asking the user to Reload or start over.
///
/// It can exactly when codeg — not the agent — owns the conversation's history:
/// custom ACP agents, whose turns are recorded to
/// [`crate::acp_transcript`]. There the failure costs nothing visible — the
/// history still renders, and a fresh agent session (linked by
/// `continues_from`) continues the same conversation. Agents whose history is
/// read back out of their own store keep the banner: for them the session
/// really is gone, and silently starting a new one would orphan it.
///
/// `classified` is [`classify_session_load_failure`]'s verdict; `None` (an
/// unexpected failure) is never recovered here — it keeps the existing
/// emit-then-fall-back-to-`session/new` behaviour.
///
/// `session_busy` is excluded outright, for ANY agent. Every other verdict means
/// the session is gone, so opening a fresh one and linking the history forward
/// loses nothing; a busy session is still there, and starting over would replace
/// a live history with an empty session for a lock that clears on its own.
fn recovers_load_failure_locally(agent_type: AgentType, classified: Option<&'static str>) -> bool {
    if classified == Some("session_busy") {
        return false;
    }
    classified.is_some() && transcript_dir_for(agent_type).is_some()
}

/// True when a `SessionUpdate` represents actual agent-produced output for
/// the current turn. Used to detect "silent EndTurn" cases where an agent
/// (notably OpenCode) reports the turn ended successfully but never emitted
/// any reply or tool call — in practice this means the model-side request
/// was swallowed and the user would otherwise see a blank conversation
/// transition silently to `PendingReview`. Metadata-only updates
/// (`UserMessageChunk`, `Plan`, `*ModeUpdate`, `ConfigOptionUpdate`,
/// `SessionInfoUpdate`, `AvailableCommandsUpdate`, `UsageUpdate`) do not
/// count.
///
/// `agent_type` is here for pi, whose lifecycle announcements ride the
/// `agent_message_chunk` channel (issue #525). Those chunks are not rendered, so
/// counting them as output would let a status-only turn end BOTH blank and
/// "successful" — the exact silent-blank-turn failure this predicate exists to
/// catch. Asking [`pi_message_chunk_route`] — the same classifier the renderer
/// uses — is what keeps the two from disagreeing; a dropped or retry chunk falls
/// through to `saw_metadata_update`, so such a turn reports
/// `turn_failed_empty_metadata` ("pi sent only status updates this turn and no
/// reply"). In practice this is a backstop rather than a common path: pi's retry
/// and compaction both continue into prose or tools.
fn is_agent_output_update(agent_type: AgentType, update: &SessionUpdate) -> bool {
    if let SessionUpdate::AgentMessageChunk(ContentChunk {
        content: ContentBlock::Text(text),
        meta,
        ..
    }) = update
    {
        if pi_message_chunk_route(agent_type, &text.text, meta.as_ref()) != PiChunkRoute::Prose {
            return false;
        }
    }
    matches!(
        update,
        SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)
    )
}

/// Handle one typed `SessionNotification` inside an active turn.
///
/// Extracted from the `if_notification` closure it is called from, and
/// **returns `()` rather than `Result` on purpose**: the closure's only
/// remaining statement is `Ok(())`, which makes it structurally impossible for
/// downstream handling to contribute an `Err` to `MatchDispatch`. That is what
/// lets the caller read a `MatchDispatch` failure as a params/schema mismatch
/// ([`TurnOutputProbe::note_dropped`]) — without it, a future `?` added here
/// would silently be counted as a protocol mismatch, sending triage in the
/// wrong direction with no compile-time signal. A genuine "downstream handling
/// failed" channel must be surfaced from this function's return type and
/// counted separately.
#[allow(clippy::too_many_arguments)]
async fn handle_turn_notification(
    notif: SessionNotification,
    agent_type: AgentType,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    terminal_runtime: &TerminalRuntime,
    session_id: &SessionId,
    cwd: Option<&str>,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
    raw_output_cache: &mut ToolCallOutputCache,
    cb_state: &mut CodeBuddyLiveState,
    probe: &mut TurnOutputProbe,
) {
    let should_poll_now =
        track_terminal_tool_calls(agent_type, &notif.update, tracked_terminal_tool_calls);
    probe.note_update(agent_type, &notif.update);
    // Custom agents have no store of their own to parse later.
    record_transcript_update(agent_type, &session_id.0, &notif.update);
    emit_conversation_update(
        state,
        emitter,
        agent_type,
        notif.update,
        cwd,
        raw_output_cache,
        cb_state,
    )
    .await;
    if should_poll_now {
        poll_tracked_terminal_tool_calls(
            terminal_runtime,
            session_id,
            state,
            emitter,
            tracked_terminal_tool_calls,
        )
        .await;
    }
}

/// Minimum spacing between "dropped an unreadable update" WARN lines. Chosen to
/// match [`crate::logging::throttle::LAG_LOG_WINDOW`]: the first drop still
/// surfaces instantly, and a sustained mismatch keeps reporting itself roughly
/// every 10s instead of once per streaming chunk.
const DROPPED_UPDATE_LOG_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// The WARN text for a dropped update. `where_` names the loop that dropped it:
/// `"turn"` (the drop is also counted by the turn's [`TurnOutputProbe`]) or
/// `"idle"` (no turn in flight, so there is no probe to count it).
///
/// A drop means `MatchDispatch` matched `session/update` but could not
/// deserialize its params into the typed `SessionNotification` — an ACP schema
/// drift. It is the only way an update gets lost: the transport hands over
/// every message it could frame, and `AgentSession::read_update` has no decode
/// step of its own.
///
/// `coalesced` is the throttle's occurrence count: suppressed hits are never
/// lost, the tally rides on the next emitted line. Pure so the "and the
/// suppressed count is reported" contract has a test that doesn't need a
/// subscriber.
fn dropped_update_log_line(where_: &str, error: &impl std::fmt::Display, coalesced: u64) -> String {
    let head = format!("[ACP] Ignoring unreadable session update ({where_}): {error}");
    if coalesced <= 1 {
        head
    } else {
        format!(
            "{head} (+{} more in the last {}s)",
            coalesced - 1,
            DROPPED_UPDATE_LOG_WINDOW.as_secs()
        )
    }
}

/// Emit the throttled "codeg dropped an update it could not read" WARN.
fn log_dropped_update(
    throttle: &mut LeadingEdgeThrottle,
    where_: &str,
    error: &impl std::fmt::Display,
) {
    if let Some(summary) = throttle.record(1) {
        tracing::warn!(
            "{}",
            dropped_update_log_line(where_, error, summary.occurrences)
        );
    }
}

/// What a single turn was observed to produce. Scoped to one turn and reset at
/// each turn start.
///
/// This exists because `EndTurn` with no output is ambiguous: it can be a real
/// agent-side failure, a turn whose output codeg failed to parse, or a
/// legitimately output-free command turn. Distinguishing them needs more than
/// the single "did we see output" bit this replaces.
#[derive(Debug, Default)]
struct TurnOutputProbe {
    /// Real agent output: reply text, thinking, or a tool call.
    saw_agent_output: bool,
    /// A `SessionUpdate` arrived, but a metadata-only one (plan, mode, usage,
    /// user echo, …).
    saw_metadata_update: bool,
    /// `session/update`s whose params did not deserialize (schema drift) — the
    /// output may well have been there; codeg could not read it.
    dropped: u32,
    /// First drop's *already-redacted* summary. Redaction happens here, at
    /// capture time, so nothing downstream can hold plaintext — parser errors
    /// inline the offending value, and that value comes off the
    /// `session/update` channel (prompt text, file contents, tool args).
    first_drop: Option<String>,
    /// `StderrTail` write position at turn start, so the diagnosis can scope
    /// stderr to this turn.
    stderr_mark: u64,
}

impl TurnOutputProbe {
    fn new(stderr_mark: u64) -> Self {
        Self {
            stderr_mark,
            ..Default::default()
        }
    }

    fn note_update(&mut self, agent_type: AgentType, update: &SessionUpdate) {
        if is_agent_output_update(agent_type, update) {
            self.saw_agent_output = true;
        } else {
            self.saw_metadata_update = true;
        }
    }

    fn note_dropped(&mut self, error: &impl std::fmt::Display) {
        self.dropped += 1;
        if self.first_drop.is_none() {
            self.first_drop = Some(summarize_parser_error(&error.to_string()));
        }
    }
}

/// Why a turn ended with `EndTurn` but no agent output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyTurnCause {
    /// codeg dropped updates it could not parse — the agent may well have
    /// replied; we just couldn't read it.
    ProtocolMismatch,
    /// Only metadata arrived. Observational only: this is NOT proof the turn
    /// was harmless — a real failure can follow a plan or usage update.
    MetadataOnly,
    /// Nothing arrived at all.
    NoOutput,
}

impl EmptyTurnCause {
    fn code(self) -> &'static str {
        match self {
            // Unchanged from before this split, so old clients and replayed
            // snapshots still localize the common case.
            EmptyTurnCause::NoOutput => "turn_failed_empty",
            EmptyTurnCause::ProtocolMismatch => "turn_failed_empty_protocol",
            EmptyTurnCause::MetadataOnly => "turn_failed_empty_metadata",
        }
    }

    fn message(self, agent_type: AgentType) -> String {
        match self {
            EmptyTurnCause::NoOutput => {
                format!("{agent_type} ended the turn without producing any response.")
            }
            EmptyTurnCause::ProtocolMismatch => format!(
                "{agent_type} produced output that codeg could not parse — \
                 the agent version may not match the protocol."
            ),
            EmptyTurnCause::MetadataOnly => format!(
                "{agent_type} sent only status updates this turn and no reply."
            ),
        }
    }
}

/// A diagnosed empty turn: its cause plus the redacted evidence behind it.
#[derive(Debug, Clone)]
struct EmptyTurnReport {
    cause: EmptyTurnCause,
    details: Option<String>,
}

/// Classify an empty turn. `ProtocolMismatch` wins over `MetadataOnly`:
/// "we couldn't read the output" is a stronger signal than "we only saw
/// metadata", and it changes where the user should look.
fn diagnose_empty_turn(probe: &TurnOutputProbe) -> EmptyTurnCause {
    if probe.dropped > 0 {
        EmptyTurnCause::ProtocolMismatch
    } else if probe.saw_metadata_update {
        EmptyTurnCause::MetadataOnly
    } else {
        EmptyTurnCause::NoOutput
    }
}

/// Max stderr lines quoted in an error's `details`.
const EMPTY_TURN_STDERR_LINES: usize = 12;
/// Max bytes of stderr quoted in an error's `details`.
const EMPTY_TURN_STDERR_BYTES: usize = 900;
/// Overall cap on `details`.
const MAX_DETAILS_BYTES: usize = 1200;

/// Assemble the user-facing evidence for an empty turn: what we failed to
/// parse, and what the agent printed to stderr.
///
/// Every fragment is already redacted at its source (`TurnOutputProbe` for
/// parser errors, `StderrTail` for stderr), so this only formats.
fn build_empty_turn_details(probe: &TurnOutputProbe, stderr_tail: &StderrTail) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    if probe.dropped > 0 {
        let mut line = format!("dropped {} unreadable update(s)", probe.dropped);
        if let Some(summary) = &probe.first_drop {
            line.push_str(&format!("; first: {summary}"));
        }
        sections.push(line);
    }

    let tail = stderr_tail.tail_since(
        probe.stderr_mark,
        EMPTY_TURN_STDERR_LINES,
        EMPTY_TURN_STDERR_BYTES,
    );
    if !tail.is_empty() {
        let scope = match tail.scope {
            TailScope::ThisTurn => "this turn",
            TailScope::Recent => "recent",
        };
        let mut block = format!("stderr ({scope}, last {} lines):", tail.lines.len());
        for line in &tail.lines {
            block.push_str("\n  ");
            block.push_str(line);
        }
        sections.push(block);
    }

    if sections.is_empty() {
        return None;
    }
    let joined = sections.join("\n");
    if joined.len() <= MAX_DETAILS_BYTES {
        return Some(joined);
    }
    let end = joined
        .char_indices()
        .take_while(|(i, c)| i + c.len_utf8() <= MAX_DETAILS_BYTES)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    Some(format!("{}…", &joined[..end]))
}

/// Resolve a turn's final stop reason and, when it is `"empty"`, the diagnosis
/// behind it.
///
/// **Pure.** It emits nothing, records nothing, and cancels nothing — the turn
/// exit keeps its side effects (the error toast, the journal, `record_turn_end`)
/// in place, in the order it needs them.
fn finish_turn_reason<'a>(
    probe: &TurnOutputProbe,
    raw_reason_str: &'a str,
    stderr_tail: &StderrTail,
) -> (&'a str, Option<EmptyTurnReport>) {
    if raw_reason_str != "end_turn" || probe.saw_agent_output {
        return (raw_reason_str, None);
    }
    let cause = diagnose_empty_turn(probe);
    let details = build_empty_turn_details(probe, stderr_tail);
    ("empty", Some(EmptyTurnReport { cause, details }))
}

/// Build an `AcpEvent::Error` for a non-success stop reason so the user gets a
/// toast instead of a silent transition to `PendingReview`. Returns `None` for
/// `end_turn` (success) and `cancelled` (already user-driven).
///
/// `Refusal` is included because OpenCode (and similar agents) map backend /
/// gateway errors to `Refusal` per the ACP spec gap — see
/// <https://shashikantjagtap.net/openclaw-acp-what-coding-agent-users-need-to-know-about-protocol-gaps/>.
/// `empty` is a synthesized reason emitted by `run_conversation_loop` when the
/// agent reports `EndTurn` without producing any agent output; `empty` carries
/// an `EmptyTurnReport` that refines the code and attaches redacted evidence.
/// `auth_required` is the other synthesized reason: the agent REJECTED the
/// prompt with ACP's -32000 instead of ending the turn, which asks the user to
/// sign in and retry rather than reporting anything wrong with the turn itself.
fn turn_failure_error_event(
    reason_str: &str,
    agent_type: AgentType,
    empty: Option<&EmptyTurnReport>,
) -> Option<AcpEvent> {
    let (code, message, details) = match reason_str {
        "refusal" => (
            "turn_failed_refusal",
            format!("{agent_type} refused to continue this turn."),
            None,
        ),
        "max_tokens" => (
            "turn_failed_max_tokens",
            format!("{agent_type} reached the maximum token limit for this turn."),
            None,
        ),
        "max_turn_requests" => (
            "turn_failed_max_turn_requests",
            format!("{agent_type} reached the maximum number of allowed requests for this turn."),
            None,
        ),
        "unknown" => (
            "turn_failed_unknown",
            format!("{agent_type} ended the turn with an unrecognized stop reason."),
            None,
        ),
        "auth_required" => (
            "turn_failed_auth_required",
            format!("{agent_type} needs you to sign in again before it can run this turn."),
            None,
        ),
        "empty" => {
            // A missing report can only happen on a path that didn't run the
            // diagnosis; fall back to the pre-split behavior rather than
            // dropping the error entirely.
            let cause = empty.map(|r| r.cause).unwrap_or(EmptyTurnCause::NoOutput);
            (
                cause.code(),
                cause.message(agent_type),
                empty.and_then(|r| r.details.clone()),
            )
        }
        _ => return None,
    };
    Some(AcpEvent::Error {
        message,
        agent_type: agent_type.to_string(),
        code: Some(code.to_string()),
        details,
        // Non-terminal: this Error is paired with a `TurnComplete`
        // carrying the same stop reason. The connection stays alive and
        // the broker's pending entry is drained by `complete_call` with
        // the correct child-side mapping (`ChildRefusal` /
        // `ChildMaxTokens` / …). See F1 in the v0.14.3 sub-agent
        // delegation post-mortem.
        terminal: false,
    })
}

/// Returns `Ok(None)` on normal exit (disconnect / command channel closed),
/// `Ok(Some(ForkExitInfo))` when the loop should be restarted on a forked
/// session, and `Err` when the connection itself failed — a rejected prompt the
/// agent cannot recover from, or the session's update router going away.
#[allow(clippy::too_many_arguments)]
async fn run_conversation_loop(
    session: &mut AgentSession,
    conn_id: &str,
    emitter: &EventEmitter,
    state: &Arc<RwLock<SessionState>>,
    agent_type: AgentType,
    perms: &PendingPermissions,
    cmd_rx: &mut mpsc::Receiver<ConnectionCommand>,
    terminal_runtime: Arc<TerminalRuntime>,
    cwd: &str,
    supports_fork: bool,
    // Connection-scoped (created once in `run_connection`, shared across fork
    // restarts of this loop): outgoing prompts are fingerprinted here so the
    // transcript watcher can classify their turns as wire-rendered foreground.
    prompt_ledger: &background_watch::PromptLedger,
    // Source of the broker reference used to cascade-cancel pending
    // delegations on parent prompt cancel / non-success TurnComplete.
    // `None` for test paths that don't wire delegation.
    delegation_injection: Option<&DelegationInjection>,
    // Connection-scoped (like `prompt_ledger`): the agent's stderr ring buffer,
    // read at turn end to explain a silent `EndTurn`.
    stderr_tail: &Arc<StderrTail>,
) -> Result<Option<ForkExitInfo>, agent_client_protocol::Error> {
    // Session-scoped cache for diffing cumulative `raw_output` snapshots
    // into incremental deltas. Shared across the idle loop and the active
    // turn loop so tool calls that span turns stay consistent.
    let mut raw_output_cache = ToolCallOutputCache::default();
    // Session-scoped CodeBuddy live state: authoritative title rewrites
    // (tool_call_id → "agent" / inner `mcp__…` name) so a later status-only
    // update can't downgrade an Agent / delegation card mid-stream, plus the
    // open-sub-agent window used to suppress a sub-agent's interleaved
    // thought/message chunks. See `emit_conversation_update`. Shared across the
    // idle and turn loops.
    let mut cb_state = CodeBuddyLiveState::default();
    // 1-based per-connection turn counter for the timing journal's ordinal
    // (see `turn_timings::TurnTiming::ord`) — incremented for EVERY Cursor
    // prompt turn, journaled or not, so consecutive ordinals prove adjacent
    // turns to the reader.
    let mut cursor_turn_ord: u64 = 0;
    // Session-scoped throttle for the two "we dropped an update we couldn't
    // read" lines below (the idle loop's and the turn loop's).
    //
    // Each fires ONCE PER NOTIFICATION, i.e. per streaming chunk, at WARN — so
    // they are live under the DEFAULT level, and one schema-drifted agent turns
    // them into a firehose. That is exactly the shape that wrote 34GB in 8.8h in
    // the 0.23.3 field report (issue #427). The information they carry is
    // "codeg is dropping this agent's output", which is worth one line per
    // window, not one per token: `TurnOutputProbe` keeps the exact counts and the
    // first redacted error, and surfaces them in the empty-turn diagnosis.
    //
    // One throttle across both sites on purpose — they are the same failure
    // (the agent is speaking a protocol we can't read) seen with and without a
    // turn open, so the operator wants one signal, not two interleaved ones.
    let mut drop_log_throttle = LeadingEdgeThrottle::new(DROPPED_UPDATE_LOG_WINDOW);
    // Options an agent push reverted after codeg asserted them at establishment.
    // Queued rather than re-asserted in place because the select below borrows
    // `session` for `read_update`, and the re-assert needs its connection;
    // drained immediately after the select, still inside the idle loop (the
    // OUTER loop only advances on a command, which may never come).
    let mut config_drift_to_reassert: Vec<(String, String)> = Vec::new();
    // For the `TurnComplete` of a turn Grok starts on its own, which the idle
    // loop ends while `read_update` holds `session`. A fork restarts this loop
    // with the new session, so it cannot go stale in here.
    let agent_turn_session_id = session.session_id().0.to_string();
    // Whether a prompt has run on this session in this loop — from then on the
    // idle loop records a custom agent's frames as the turn loop does (see the
    // `record_transcript_update` below). Before it, nothing the idle loop reads
    // is conversation: a banner the agent greets a new session with, which no
    // client renders and which would list a session the user never spoke in,
    // or a straggler of a `session/load` replay whose history the hydration
    // drain has already decided to keep or skip. Work the agent does on its
    // own needs a turn to have started it, and background work cannot outlive
    // the agent process this loop's connection owns.
    let mut prompted = false;
    loop {
        // Wait for either a user command or a session update (e.g. available_commands_update)
        let cmd = loop {
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => break cmd,
                update = session.read_update() => {
                    // An `Err` is the session's router going away, not one bad
                    // message (see `AgentSession::read_update`): the connection
                    // is gone, so the loop goes with it.
                    let dispatch = match update {
                        Ok(dispatch) => fix_usage_update_nulls(dispatch),
                        Err(e) => return Err(defer_to_connection_report(e).await),
                    };
                    let h = emitter.clone();
                    let st = Arc::clone(state);
                    let cwd_opt = Some(cwd);
                    // Background work outlives the turn that started it, so
                    // these frames arrive on the IDLE loop as often as inside
                    // one.
                    if let Some(delta) = air_async_task_delta(&dispatch)
                        .or_else(|| grok_workflow_task_delta(&dispatch, agent_type))
                    {
                        emit_with_state(&st, &h, AcpEvent::AsyncTask { delta }).await;
                    } else if let Some(notice) = session_notice(&dispatch) {
                        // Advisories land outside a turn as readily as inside
                        // one (a config warning at startup, a model reroute
                        // between prompts).
                        emit_with_state(&st, &h, AcpEvent::SessionNotice { notice }).await;
                    } else if let Some(event) = session_compaction_event(&dispatch) {
                        emit_with_state(&st, &h, event).await;
                    } else if let Some(completed) = grok_turn_completed(&dispatch, agent_type) {
                        // Grok's own end of a turn. Only the turn opened below
                        // ends here; one codeg started ended on its response.
                        if grok_turn_completed_ends(cb_state.grok_agent_turn.as_deref(), &completed) {
                            close_grok_agent_turn(
                                &st,
                                &h,
                                perms,
                                agent_type,
                                &agent_turn_session_id,
                                completed.stop_reason,
                                &mut cb_state,
                            )
                            .await;
                        }
                    } else {
                        // A turn Grok runs on its own (a background workflow's
                        // follow-up) gets a turn here, opened before its first
                        // chunk is emitted.
                        if let Some(prompt_id) = grok_agent_turn_opener(&dispatch, agent_type, &cb_state) {
                            open_grok_agent_turn(&st, &h, prompt_id, &mut cb_state).await;
                        }
                        let drift = &mut config_drift_to_reassert;
                        if let Err(e) = MatchDispatch::new(dispatch)
                            .if_notification(
                                async |notif: SessionNotification| {
                                    // BEFORE the emit: it overwrites the very
                                    // state the comparison reads.
                                    if let SessionUpdate::ConfigOptionUpdate(update) = &notif.update {
                                        drift.extend(
                                            take_asserted_config_drift(&st, &update.config_options).await,
                                        );
                                    }
                                    // A custom agent's history is this
                                    // transcript and nothing else, and what
                                    // arrives here is part of it: a turn the
                                    // agent runs on its own (a background job's
                                    // follow-up, with no `session/prompt` around
                                    // it), the late completion of a call an
                                    // earlier turn started, or the last frames
                                    // of a turn whose response won the turn
                                    // loop's select. No-op for every other agent.
                                    if prompted {
                                        record_transcript_update(agent_type, &notif.session_id.0, &notif.update);
                                    }
                                    emit_conversation_update(&st, &h, agent_type, notif.update, cwd_opt, &mut raw_output_cache, &mut cb_state).await;
                                    Ok(())
                                },
                            )
                            .await
                            .otherwise(async |dispatch| {
                                maybe_emit_ext_notification(&st, &h, agent_type, dispatch, &mut cb_state).await;
                                Ok(())
                            })
                            .await
                        {
                            // Both closures are infallible, so this is an update
                            // whose params did not deserialize — the same drop the
                            // in-turn loop counts, logged under the same throttle.
                            log_dropped_update(&mut drop_log_throttle, "idle", &e);
                        }
                    }
                }
            }
            // Reached only on the update arm (the command arm breaks out), so
            // `session` is free again here.
            for (config_id, value_id) in std::mem::take(&mut config_drift_to_reassert) {
                tracing::info!(
                    "[ACP] re-asserting '{config_id}'='{value_id}' — the agent reverted it \
                     after codeg applied it at session establishment"
                );
                let cx = session.connection();
                let sid = session.session_id().clone();
                if let Err(e) =
                    set_session_config_option(&cx, &sid, state, emitter, config_id.clone(), value_id)
                        .await
                {
                    // Advisory: the agent is running what it pushed and has already
                    // told the frontend so. Failing the connection over a selector
                    // would be far worse than the value not being restored.
                    tracing::warn!("[ACP] failed to re-assert '{config_id}' after a revert: {e}");
                }
            }
        };
        match cmd {
            Some(ConnectionCommand::Prompt {
                blocks,
                user_message,
            }) => {
                // Fingerprint the outgoing prompt for the background watcher's
                // foreground/out-of-turn classifier BEFORE the blocks are
                // consumed: the transcript record this prompt becomes must
                // classify as wire-rendered foreground, not overlay.
                prompt_ledger.record_prompt_blocks(&blocks);
                // Establishment is over the moment the user speaks: from here a
                // config push is attributable to the prompt (`/model` typed in
                // chat is one), so codeg stops arbitrating and the agent owns
                // the selectors. See `SessionState::asserted_config_values`.
                state.write().await.asserted_config_values.clear();
                // Cursor's ACP store carries no per-turn timestamps at all
                // (see `crate::turn_timings`), so codeg journals its own
                // observation of the turn span: hash + ordinal here (before
                // the blocks are consumed), the send stamp after the
                // `UserMessage` broadcast below, the append at TurnComplete.
                // The hash of the outgoing text blocks is what the history
                // parser correlates its user turns against; the ordinal is
                // its contiguity anchor (every turn consumes one, journaled
                // or not).
                let turn_timing_prep = matches!(agent_type, AgentType::Cursor).then(|| {
                    cursor_turn_ord += 1;
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            PromptInputBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    (crate::turn_timings::prompt_hash(&text), cursor_turn_ord)
                });
                // Keep the user's blocks pristine through ledgering, previews,
                // and cross-client broadcast. Only the final agent-bound prompt
                // receives the machine routing block derived from agent badges.
                let delegation_enabled = state.read().await.delegation_enabled;
                // [crewchat] Role + team frames (Phases 2.2 / 1.3). The role
                // frame is NOT delegation-gated: the persona describes how the
                // session works, codeg-mcp or not. The member list only makes
                // sense with the delegation tool group, so it stays gated.
                // Both resolve here — not at spawn — because the conversation
                // is linked on the first prompt; a plain session resolves to
                // nothing and appends nothing.
                let conversation_id = state.read().await.conversation_id;
                let role = match (delegation_injection, conversation_id) {
                    (Some(injection), Some(id)) => injection.broker.conversation_role(id).await,
                    _ => None,
                };
                let team_members = match (delegation_injection, conversation_id) {
                    (Some(injection), Some(id)) if delegation_enabled => {
                        injection.broker.team_members(id).await
                    }
                    _ => Vec::new(),
                };
                let prompt_blocks = prepare_agent_bound_prompt(
                    agent_type,
                    blocks,
                    delegation_enabled,
                    role.as_ref(),
                    &team_members,
                );
                if prompt_blocks.is_empty() {
                    // Defensive: the manager rejects empty prompts before the
                    // concurrency gate is set / the command is enqueued (see
                    // `send_prompt_inner`), and `map_prompt_blocks` is 1:1, so an
                    // empty prompt should never reach here. If one ever did, it
                    // would carry no turn-in-flight gate, so just surface the
                    // error and keep the idle loop alive.
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::Error {
                            message: "Prompt must contain at least one content block".into(),
                            agent_type: agent_type.to_string(),
                            code: None,
                            details: None,
                            // Recoverable: idle loop continues, awaiting the
                            // next user command. Connection stays alive.
                            terminal: false,
                        },
                    )
                    .await;
                    continue;
                }

                // A prompt the gate admitted while Grok ran a turn of its own
                // (the frontend queues behind that turn; a chat channel or an
                // automation does not). Grok queues this prompt behind its
                // turn, so end the one on screen first — kept as a turn of its
                // own rather than folded into this one's opening. Its stop
                // reason is `cancelled` because it is the one reason no
                // consumer acts on: `end_turn` would have the lifecycle mark
                // the conversation for review while this prompt runs. Whatever
                // that turn is still asking the user goes with it — otherwise
                // Grok stays parked on a card no one can see, and this prompt
                // waits behind it for good.
                if close_grok_agent_turn(
                    state,
                    emitter,
                    perms,
                    agent_type,
                    &agent_turn_session_id,
                    "cancelled",
                    &mut cb_state,
                )
                .await
                {
                    cancel_grok_agent_turn_asks(delegation_injection, conn_id).await;
                }

                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Prompting,
                    },
                )
                .await;

                // Broadcast the user's prompt to cross-client viewers BEFORE
                // issuing the agent request. Emitting here (rather than at the
                // manager enqueue site) guarantees its seq strictly precedes the
                // turn's assistant/status events — viewers apply events in seq
                // order, so otherwise the reply could render above the message.
                // It also means a prompt that is never processed (rejected /
                // dropped) broadcasts nothing. `apply_event` records it as
                // `pending_user_message` so a client attaching mid-turn still
                // renders the user turn from the snapshot.
                if let Some((message_id, blocks)) = user_message {
                    emit_with_state(state, emitter, AcpEvent::UserMessage { message_id, blocks })
                        .await;
                }

                // Stamp the journal's turn start AFTER the `UserMessage`
                // broadcast: `apply_in_flight_message_id`'s recency gate
                // compares parsed user-turn timestamps — which the journal
                // upgrade rewrites to this stamp — against the broadcast's
                // application instant (`pending_user_message_started_at`,
                // stored at millisecond precision for exactly this
                // comparison). `emit_with_state` applies the event before
                // returning, so this stamp is never earlier than the gate's
                // threshold and the in-flight user turn stays stampable in
                // the journal-written-but-turn-still-pending window.
                let mut turn_timing_probe = turn_timing_prep.map(|(prompt_sha, ord)| {
                    (crate::turn_timings::now_epoch_ms(), prompt_sha, ord)
                });

                // Clone connection and session ID before entering the
                // select loop so we can send CancelNotification without
                // conflicting with session.read_update()'s mutable borrow.
                let cx = session.connection();
                let sid = session.session_id().clone();
                // Record the prompt BEFORE sending, so the transcript's line
                // order matches the wire order even if the agent replies
                // instantly — and awaited, so the replay gate can never see
                // this conversation as transcript-less (see `record_prompt`).
                record_prompt(agent_type, &sid.0, &prompt_blocks).await;
                prompted = true;
                let turn_started_at_ms = crate::acp_transcript::now_epoch_ms();
                let prompt_request = PromptRequest::new(sid.clone(), prompt_blocks);
                // Snapshot the stderr write position BEFORE the request is
                // dispatched. An agent that fails the moment the prompt lands
                // (bad credentials, unknown model) prints its error almost
                // immediately; marking after dispatch would race those lines
                // out of "this turn" and demote the most relevant evidence to a
                // stale-looking `recent` fallback.
                let stderr_mark = stderr_tail.mark();
                // Use Box::pin (heap) instead of tokio::pin! (stack) so the
                // future can be moved into a background task on cancel.
                let mut prompt_response = Box::pin(
                    cx.clone()
                        .send_request_to(Agent, prompt_request)
                        .block_task(),
                );
                let mut tracked_terminal_tool_calls: HashMap<String, TrackedTerminalToolCall> =
                    HashMap::new();
                let mut terminal_poll_interval = tokio::time::interval(
                    std::time::Duration::from_millis(TERMINAL_POLL_INTERVAL_MS),
                );
                terminal_poll_interval
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut disconnect_requested = false;
                // What this turn was observed to produce. When an agent reports
                // `EndTurn` having produced no real output, we synthesize an
                // `"empty"` stop reason so the user gets an error toast instead
                // of a confusing `PendingReview` on a blank conversation — and
                // the probe's other fields say *why* it looked empty.
                let mut probe = TurnOutputProbe::new(stderr_mark);
                // A CodeBuddy native sub-agent's full lifecycle (Agent tool call
                // open → completed) happens within one turn, so reset the
                // suppression window at each turn start. This bounds the tracking
                // sets and guarantees a sub-agent that ended without a terminal
                // frame (cancel/abort) can never suppress the NEXT turn's
                // main-agent thinking. `title_overrides` intentionally persists
                // (a card's identity is session-stable).
                cb_state.open_subagents.clear();
                cb_state.closed_subagents.clear();
                // Grok spawn bookkeeping scoped to one turn: progress
                // eligibility (a prior turn's background child must never tick
                // into THIS turn's live message) and the un-paired pending
                // queue (a spawn whose `subagent_spawned` never arrived —
                // aborted turn — must not mispair with the next turn's first
                // spawn). The subagent→call map, seen and settled sets persist:
                // background children legitimately span turns.
                cb_state.grok_progress_eligible.clear();
                cb_state.grok_pending_spawn_ids.clear();
                // Same one-turn argument as the sub-agent sets above: a pi bash
                // call's whole lifecycle (`tool_execution_start` → `_update`* →
                // `_end`) happens inside one turn, so nothing here can still be
                // owed output when the next turn opens. Without this, a turn
                // canceled mid-command leaves an entry that never sees a final
                // status and so lives until the connection tears down. The
                // `session/load` replay path is unaffected: it runs on the
                // out-of-turn pump and its calls settle on the update that
                // immediately follows, before any turn starts.
                cb_state.hosted_terminal_calls.clear();
                // A stashed claude result is consumed by the completion that
                // follows it on the wire; one still waiting here belongs to a
                // call the canceled turn never completed.
                cb_state.claude_viewed_results.clear();
                // A codex web search whose request never reported (canceled
                // mid-request) must not set aside this turn's first report.
                // `/goal` continuation turns run on the idle loop and announce
                // no boundary (codex-acp turns `turn/started`/`turn/completed`
                // into nothing), so there such a search holds the next report
                // once — an under-read, where resetting on Cancel instead would
                // admit a searched report landing after the cancel.
                cb_state.codex_context_readings.turn_started();
                // Grok's context ring needs the active model's window paired
                // with the cumulative token count riding each update. Resolve it
                // once here (the model can't change mid-turn) so the per-update
                // peek below never touches the state lock. A switch since the
                // last turn changes the denominator, so re-key the live pair to
                // the new window right away rather than leaving the previous
                // model's ring on screen until this turn's first token count.
                if agent_type == AgentType::Grok {
                    let window = grok_current_model_context_window(state).await;
                    cb_state.grok_turn_context_window = window;
                    if let Some((used, size)) =
                        grok_window_change_usage(window, cb_state.grok_last_usage)
                    {
                        cb_state.grok_last_usage = Some((used, size));
                        emit_with_state(state, emitter, AcpEvent::UsageUpdate { used, size }).await;
                    }
                }

                // Read updates until turn completes.
                // We must also listen for commands (e.g. RespondPermission)
                // to avoid deadlocking when the agent awaits a permission response.
                loop {
                    tokio::select! {
                        update = session.read_update() => {
                            // An `Err` is the session's router going away, not one bad
                            // message (see `AgentSession::read_update`): the connection
                            // is gone, and so is any answer to this prompt.
                            let dispatch = match update {
                                Ok(dispatch) => fix_usage_update_nulls(dispatch),
                                Err(e) => return Err(defer_to_connection_report(e).await),
                            };
                            let h = emitter.clone();
                            let st = Arc::clone(state);
                            let runtime = terminal_runtime.clone();
                            let session_id = sid.clone();
                            let cwd_opt = Some(cwd);
                            // grok reports `/compact` results on ext methods
                            // that bypass the typed pipeline below and emit a
                            // compaction card/error from `.otherwise`. Count
                            // that as turn output up front (the dispatch is
                            // about to be consumed) so a compaction-only turn
                            // isn't misclassified as `"empty"` at turn end.
                            if grok_ext_notification_is_turn_output(&dispatch, agent_type) {
                                probe.saw_agent_output = true;
                            }
                            // Grok has no `usage_update` channel; its
                            // cumulative token count rides the outer
                            // `_meta` of ordinary updates. Peek it before
                            // the typed pipeline consumes the dispatch
                            // (which drops that `_meta`) so the composer
                            // ring tracks the turn as it streams.
                            if let Some((used, size)) = grok_live_usage_step(
                                &dispatch,
                                agent_type,
                                cb_state.grok_turn_context_window,
                                cb_state.grok_last_usage,
                            ) {
                                cb_state.grok_last_usage = Some((used, size));
                                emit_with_state(
                                    state,
                                    emitter,
                                    AcpEvent::UsageUpdate { used, size },
                                )
                                .await;
                            }
                            // Which prompt this turn is, so the frames it
                            // leaves for the idle loop can't open a turn
                            // there (see `grok_agent_turn_opener`).
                            if agent_type == AgentType::Grok {
                                note_grok_turn_prompt_id(
                                    &dispatch,
                                    &mut cb_state.grok_ended_prompt_ids,
                                );
                            }
                            // Consumed before the typed pipeline (see
                            // `air_async_task_delta`). Only a SPAWN
                            // counts as this turn's output: a turn whose
                            // only visible result is "I launched a
                            // background job" is not an empty turn, but
                            // a progress/state tick from a task an
                            // EARLIER turn started is not this turn's
                            // work — background frames arrive inside
                            // later turns as a matter of course, and
                            // letting them set the flag would silence
                            // the empty-turn diagnosis for a prompt the
                            // agent really did answer with nothing.
                            if let Some(delta) = air_async_task_delta(&dispatch) {
                                probe.saw_agent_output |= delta.spawned;
                                emit_with_state(
                                    &st,
                                    &h,
                                    AcpEvent::AsyncTask { delta },
                                )
                                .await;
                            } else if let Some(delta) =
                                grok_workflow_task_delta(&dispatch, agent_type)
                            {
                                // Never this turn's output: every Grok frame
                                // restates the run, so a spawn proves
                                // nothing here — the launch's own chunk or
                                // tool call is what counts.
                                emit_with_state(
                                    &st,
                                    &h,
                                    AcpEvent::AsyncTask { delta },
                                )
                                .await;
                            } else if let Some(notice) = session_notice(&dispatch) {
                                // Deliberately does NOT set
                                // `saw_agent_output`. A notice is an
                                // adapter-composed advisory, not
                                // something the model said — upstream
                                // draws the same line, which is why
                                // claude records `Turn.noticeTexts` so
                                // its own result-text fallback can skip
                                // a result that merely repeats one. A
                                // turn whose only output was "a hook
                                // blocked this" IS an empty turn.
                                emit_with_state(
                                    &st,
                                    &h,
                                    AcpEvent::SessionNotice { notice },
                                )
                                .await;
                            } else if let Some(event) =
                                session_compaction_event(&dispatch)
                            {
                                // Compaction, unlike a notice, IS this
                                // turn's work: an auto-compaction is
                                // the whole visible result of the turn
                                // that tripped the context limit, and a
                                // `/compact` turn produces nothing else
                                // at all.
                                probe.saw_agent_output = true;
                                emit_with_state(&st, &h, event).await;
                            } else if let Err(e) = MatchDispatch::new(dispatch)
                                .if_notification(
                                    async |notif: SessionNotification| {
                                        // Body lives in a named `-> ()`
                                        // function so this closure is
                                        // infallible by construction —
                                        // see `handle_turn_notification`.
                                        handle_turn_notification(
                                            notif,
                                            agent_type,
                                            &st,
                                            &h,
                                            runtime.as_ref(),
                                            &session_id,
                                            cwd_opt,
                                            &mut tracked_terminal_tool_calls,
                                            &mut raw_output_cache,
                                            &mut cb_state,
                                            &mut probe,
                                        )
                                        .await;
                                        Ok(())
                                    },
                                )
                                .await
                                .otherwise(async |dispatch| {
                                    maybe_emit_ext_notification(&st, &h, agent_type, dispatch, &mut cb_state).await;
                                    Ok(())
                                })
                                .await
                            {
                                // Both handler closures are infallible by
                                // construction, so this `Err` can only be a
                                // typed-deserialization failure — i.e. ACP
                                // schema drift. Recorded: an agent whose
                                // output we couldn't read looks identical to
                                // one that said nothing, and the two need
                                // completely different fixes.
                                probe.note_dropped(&e);
                                log_dropped_update(&mut drop_log_throttle, "turn", &e);
                            }
                        }
                        prompt_result = &mut prompt_response => {
                            // A rejected prompt is a TURN failure, not a dead
                            // connection: the agent answered, so it is still
                            // there, and the session it answered about is still
                            // addressable. Propagating the error instead (`?`)
                            // unwinds `run_connection` into a terminal `Error` →
                            // `Disconnected`, which kills the agent process,
                            // greys out the composer until a full respawn
                            // finishes, and has the lifecycle worker flip the
                            // conversation row to Cancelled — for a turn that
                            // merely failed. Only the three families
                            // `prompt_rejection_is_terminal` names (see there)
                            // still take that exit.
                            //
                            // ACP's `authRequired` is the rejection with a
                            // client-side answer, so it keeps its own stop
                            // reason and its own localized message: the client
                            // is expected to run its auth flow and come back on
                            // the SAME session. claude-agent-acp 0.74.0 made it
                            // reachable in the ordinary case (through 0.73.0 a
                            // mid-session sign-out settled an AIR client's turn
                            // with a disguised `end_turn` carrying the failure
                            // record; 0.74.0 publishes that record on the update
                            // channel and rejects the prompt as well).
                            // Agent-agnostic on purpose: every agent that answers
                            // -32000 here is asking for credentials, not
                            // reporting a dead process — which is why
                            // `session/load` already treats "Authentication
                            // required" as an expected outcome rather than an
                            // error to surface.
                            //
                            // Every other turn-scoped rejection reports the
                            // agent's OWN words, because they are the actionable
                            // part ("The command \"/mcp\" is not supported in
                            // this mode.") and codeg has no vocabulary for them.
                            let response = match prompt_result {
                                Ok(response) => response,
                                Err(e) if !prompt_rejection_is_terminal(&e) => {
                                    let auth_required = matches!(
                                        e.code,
                                        agent_client_protocol::schema::v1::ErrorCode::AuthRequired
                                    );
                                    // Synthesized like `empty`: no `StopReason`
                                    // ever arrives for a rejected prompt, so the
                                    // turn needs a reason of its own.
                                    let reason_str = if auth_required {
                                        "auth_required"
                                    } else {
                                        "rejected"
                                    };
                                    tracing::warn!(
                                        "[ACP] session/prompt rejected ({e}); ending the turn \
                                         as {reason_str} and keeping the session"
                                    );
                                    if !tracked_terminal_tool_calls.is_empty() {
                                        poll_tracked_terminal_tool_calls(
                                            terminal_runtime.as_ref(),
                                            &sid,
                                            state,
                                            emitter,
                                            &mut tracked_terminal_tool_calls,
                                        )
                                        .await;
                                    }
                                    // AIR-capable agents ALSO publish an `access`
                                    // failure record with a `login` action. The
                                    // frontend tells the two as ONE notification,
                                    // the record's, which carries the Sign in
                                    // button; this Error is the only report for
                                    // agents with no AIR.
                                    let err_event = if auth_required {
                                        turn_failure_error_event(
                                            reason_str,
                                            agent_type,
                                            None,
                                        )
                                    } else {
                                        Some(AcpEvent::Error {
                                            // Through `AcpError::protocol` for
                                            // its sanitizer: an agent's rejection
                                            // can quote a local path, and this
                                            // string is rendered in the UI and
                                            // pushed over the WebSocket.
                                            message: AcpError::protocol(e.to_string())
                                                .to_string(),
                                            agent_type: agent_type.to_string(),
                                            // No stable code: the payload IS the
                                            // agent's message, so the frontend's
                                            // fallback arm (show it verbatim) is
                                            // the right renderer.
                                            code: None,
                                            details: None,
                                            // The whole point: the connection
                                            // outlives this turn.
                                            terminal: false,
                                        })
                                    };
                                    if let Some(err_event) = err_event {
                                        emit_with_state(state, emitter, err_event).await;
                                    }
                                    // Not journaled (that is `end_turn` only),
                                    // but still recorded: the transcript's turn
                                    // ended here, and omitting it would leave the
                                    // history parser reading the next prompt as a
                                    // continuation of this one.
                                    record_turn_end(
                                        agent_type,
                                        &sid.0,
                                        reason_str,
                                        turn_started_at_ms,
                                        current_session_model_id(state).await,
                                    )
                                    .await;
                                    // Same wedge guard as the normal turn exit
                                    // below — see there for why the drain and the
                                    // event must share one critical section.
                                    drain_permissions_then_emit(
                                        perms,
                                        state,
                                        emitter,
                                        AcpEvent::TurnComplete {
                                            session_id: sid.0.to_string(),
                                            stop_reason: reason_str.into(),
                                            agent_type: agent_type.to_string(),
                                        },
                                    )
                                    .await;
                                    // Non-`end_turn`, so cascade-cancel like
                                    // every other turn-failure exit: the parent
                                    // will never consume an in-flight delegation
                                    // result. Turn-scoped — the connection is
                                    // still alive.
                                    if let Some(inj) = delegation_injection {
                                        inj.broker.cancel_by_parent_turn(conn_id).await;
                                    }
                                    break;
                                }
                                Err(e) if lost_the_connection(&e) => {
                                    return Err(defer_to_connection_report(e).await)
                                }
                                Err(e) => return Err(e),
                            };
                            // Grok names the prompt on its response as well:
                            // the id this turn's late frames will carry.
                            if agent_type == AgentType::Grok {
                                if let Some(prompt_id) = response
                                    .meta
                                    .as_ref()
                                    .and_then(|meta| air_task_str(meta.get("promptId")))
                                {
                                    remember_grok_ended_prompt(
                                        &mut cb_state.grok_ended_prompt_ids,
                                        &prompt_id,
                                    );
                                }
                            }
                            // A turn's terminal AIR failure rides on the
                            // response `_meta` (see `response_session_failure`
                            // — the update channel only carries the retry
                            // warnings). Emit it BEFORE `TurnComplete`: the
                            // same-id higher-revision severity-"error" upsert
                            // must land before `apply_event`'s turn-boundary
                            // settle, or the retry warnings it escalates would
                            // be marked recovered while the failure is live.
                            let terminal_failure =
                                response_session_failure(response.meta.as_ref());
                            if let Some(record) = &terminal_failure {
                                emit_with_state(
                                    state,
                                    emitter,
                                    AcpEvent::SessionFailure { record: record.clone() },
                                )
                                .await;
                            }
                            let reason = response.stop_reason;
                            if !tracked_terminal_tool_calls.is_empty() {
                                poll_tracked_terminal_tool_calls(
                                    terminal_runtime.as_ref(),
                                    &sid,
                                    state,
                                    emitter,
                                    &mut tracked_terminal_tool_calls,
                                )
                                .await;
                            }
                            let raw_reason_str = stop_reason_to_str(reason);
                            // Pure: resolves the reason and (for an empty
                            // turn) its diagnosis; every side effect stays
                            // below, in the order this exit needs it.
                            //
                            // Exception: a response carrying a typed terminal
                            // ERROR is a failed turn wearing the adapters'
                            // disguised `end_turn` — its blank output is
                            // explained by the AIR banner, so synthesizing an
                            // "empty" toast on top would misdiagnose a dead
                            // connection as "the agent produced nothing".
                            let (reason_str, empty_report) = if terminal_failure
                                .as_ref()
                                .is_some_and(|record| record.severity == "error")
                            {
                                (raw_reason_str, None)
                            } else {
                                finish_turn_reason(&probe, raw_reason_str, stderr_tail)
                            };
                            if let Some(err_event) =
                                turn_failure_error_event(reason_str, agent_type, empty_report.as_ref())
                            {
                                emit_with_state(state, emitter, err_event).await;
                            }
                            // Clean completions only — a canceled/empty turn
                            // may be unpersisted (see journal_turn_span).
                            if reason_str == "end_turn" {
                                journal_turn_span(&mut turn_timing_probe, conn_id, &sid.0).await;
                            }
                            // ACP has no turn-end notification — the stop
                            // reason arrives here, in the prompt RESPONSE — so
                            // codeg records it for the history parser. Unlike
                            // streamed chunks this one is bound-awaited: a
                            // conversation reopened right after a turn must not
                            // miss its tail.
                            record_turn_end(
                                agent_type,
                                &sid.0,
                                reason_str,
                                turn_started_at_ms,
                                current_session_model_id(state).await,
                            )
                            .await;
                            // The turn is over, so any card still parked here
                            // is moot — `TurnComplete` clears
                            // `pending_permission` from the snapshot
                            // unconditionally. Drain and emit as ONE critical
                            // section so the queue can't keep a `showing` id
                            // that no `RespondPermission` will ever match,
                            // which would wedge the queue and stop every LATER
                            // permission on this connection from displaying. A
                            // no-op on the normal path (an agent blocked on
                            // approval does not end its turn).
                            drain_permissions_then_emit(
                                perms,
                                state,
                                emitter,
                                AcpEvent::TurnComplete {
                                    session_id: sid.0.to_string(),
                                    stop_reason: reason_str.into(),
                                    agent_type: agent_type.to_string(),
                                },
                            )
                            .await;
                            // `TurnComplete` has just taken any
                            // `ask_user_question` card off every client, so
                            // decline a question still parked on this
                            // connection: nothing can answer it now, and while
                            // it holds the one-per-connection slot every later
                            // ask is refused. Usually there is none — an agent
                            // blocked in an ask cannot end its turn. Claude Code
                            // 2.1.286 (claude-agent-acp 0.85.0) is what makes it
                            // reachable: a foreground MCP call can move to the
                            // background and keep running past the end of the
                            // turn — after a steer (the Steer arm below declines
                            // first), or after a timeout when the user's
                            // environment sets `CLAUDE_AUTO_BACKGROUND_TASKS`.
                            // Awaited inline, like the Cancel reclaim, so it is
                            // scoped to the turn that just ended. Plan approvals
                            // are not swept: each is a request the agent itself
                            // blocks on, which no adapter moves to the
                            // background.
                            if let Some(inj) = delegation_injection {
                                inj.questions.cancel_questions_by_parent(conn_id).await;
                            }
                            // Cascade-cancel any pending delegations whenever
                            // the parent's turn ended for a reason other than
                            // clean `end_turn`. The `end_turn` path lets the
                            // legitimate delegation completion drain
                            // naturally; every other reason (cancelled /
                            // refusal / max_tokens / max_turn_requests / empty
                            // / unknown) means the parent will never consume
                            // the in-flight result, so the child must be torn
                            // down. The connection stays alive (only the turn
                            // ended), so use the turn-scoped cancel that keeps
                            // the parent's `consumed` tool_call memory — a
                            // late re-emit must not re-register and mis-bind
                            // the next same-key delegation.
                            //
                            // Await inline: the fast tracker + parked-call
                            // drain MUST finish before the loop accepts the
                            // next prompt so it stays scoped to the just-ended
                            // turn. The broker backgrounds the slow child
                            // teardown (spawner.cancel/disconnect) internally,
                            // so this won't block on slow agents; its
                            // idempotent drain also lets the cleanup-guard
                            // cascade at run_connection exit run without
                            // race-double-drain.
                            if reason_str != "end_turn" {
                                if let Some(inj) = delegation_injection {
                                    inj.broker.cancel_by_parent_turn(conn_id).await;
                                }
                            }
                            break;
                        }
                        _ = terminal_poll_interval.tick(), if !tracked_terminal_tool_calls.is_empty() => {
                            poll_tracked_terminal_tool_calls(
                                terminal_runtime.as_ref(),
                                &sid,
                                state,
                                emitter,
                                &mut tracked_terminal_tool_calls,
                            )
                            .await;
                        }
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                Some(ConnectionCommand::RespondPermission {
                                    request_id,
                                    option_id,
                                }) => {
                                    resolve_permission(
                                        perms, state, emitter, request_id, option_id,
                                    )
                                    .await;
                                }
                                Some(ConnectionCommand::SetMode { mode_id }) => {
                                    let req = SetSessionModeRequest::new(sid.clone(), mode_id.clone());
                                    match cx.send_request_to(Agent, req).block_task().await {
                                        Ok(_) => {
                                            emit_with_state(
                                                state,
                                                emitter,
                                                AcpEvent::ModeChanged { mode_id },
                                            )
                                            .await;
                                        }
                                        Err(e) => {
                                            emit_with_state(
                                                state,
                                                emitter,
                                                AcpEvent::Error {
                                                    message: format!("Failed to set mode: {e}"),
                                                    agent_type: agent_type.to_string(),
                                                    code: Some(
                                                        SET_MODE_FAILED_ERROR_CODE.to_string(),
                                                    ),
                                                    details: None,
                                                    // Recoverable: just a failed mode toggle.
                                                    terminal: false,
                                                },
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Some(ConnectionCommand::SetConfigOption {
                                    config_id,
                                    value_id,
                                }) => {
                                    let set_result = if agent_type == AgentType::Grok {
                                        set_grok_config_option(
                                            &cx, &sid, state, emitter, config_id, value_id,
                                        )
                                        .await
                                    } else {
                                        set_session_config_option(
                                            &cx, &sid, state, emitter, config_id, value_id,
                                        )
                                        .await
                                    };
                                    if let Err(e) = set_result {
                                        emit_with_state(
                                            state,
                                            emitter,
                                            AcpEvent::Error {
                                                message: format!("Failed to set config option: {e}"),
                                                agent_type: agent_type.to_string(),
                                                code: Some(
                                                    SET_CONFIG_OPTION_FAILED_ERROR_CODE.to_string(),
                                                ),
                                                details: None,
                                                // Recoverable: just a failed config-option toggle.
                                                terminal: false,
                                            },
                                        )
                                        .await;
                                    }
                                }
                                Some(ConnectionCommand::GoalControl { action, reply }) => {
                                    let method = state.read().await.goal_control_method.clone();
                                    let landed =
                                        match send_goal_control(&cx, &sid, action, &method).await {
                                            Ok(()) => true,
                                            Err(e) => {
                                                emit_with_state(
                                                    state,
                                                    emitter,
                                                    AcpEvent::Error {
                                                        message: format!(
                                                            "Failed to control goal: {e}"
                                                        ),
                                                        agent_type: agent_type.to_string(),
                                                        code: Some(
                                                            GOAL_CONTROL_FAILED_ERROR_CODE
                                                                .to_string(),
                                                        ),
                                                        details: None,
                                                        // Recoverable: the goal
                                                        // is unchanged and the
                                                        // session is untouched.
                                                        terminal: false,
                                                    },
                                                )
                                                .await;
                                                false
                                            }
                                        };
                                    if let Some(reply) = reply {
                                        // A dead receiver is fine — the caller
                                        // that wanted to follow up with an
                                        // interrupt went away, and the control
                                        // itself already happened.
                                        let _ = reply.send(landed);
                                    }
                                }
                                Some(ConnectionCommand::Steer { blocks, reply }) => {
                                    // Protocol round-trip only — the manager's
                                    // cancellation-shielded task records the
                                    // note + broadcasts `FeedbackSubmitted`
                                    // once this outcome arrives (Fork's
                                    // protocol/persistence split). Awaiting
                                    // inline matches SetMode/SetConfigOption:
                                    // the runtime pumps I/O on its own task, so the
                                    // round-trip only defers other queued
                                    // commands, not session updates. A dead
                                    // receiver is fine — the reply is then
                                    // moot (teardown), nothing to unwind.
                                    //
                                    // A steer while an `ask_user_question` card
                                    // is up answers it declined first. Through
                                    // Claude Code 2.1.284 the steer interrupted
                                    // the blocked MCP call; the CLI's
                                    // `notifications/cancelled` made codeg-mcp
                                    // abandon the ask and the card closed. From
                                    // 2.1.286 (claude-agent-acp 0.85.0) the CLI
                                    // moves the call to the background instead
                                    // and nothing cancels it, so the card would
                                    // stay up only until this turn ends (see
                                    // the turn exit's reclaim) and every new ask
                                    // until then would be refused, the slot
                                    // still taken. Declining it here keeps the
                                    // old meaning — redirecting the agent drops
                                    // its question — and the agent reads the
                                    // decline beside the steer.
                                    if let Some(inj) = delegation_injection {
                                        inj.questions.cancel_questions_by_parent(conn_id).await;
                                    }
                                    let outcome = send_steer_request(&cx, &sid, &blocks).await;
                                    // A steered message still lands in the
                                    // agent's OWN transcript: as a user record
                                    // when it runs as a turn of its own, and as
                                    // a `queued_command` attachment when the
                                    // CLI folds it into the running turn
                                    // (`parsers::claude::queued_human_prompt`).
                                    // Fingerprint it so the background watcher
                                    // classifies that turn, or the rest of the
                                    // turn it was folded into, as wire-rendered
                                    // foreground: the owning prompt stays in
                                    // flight across the steered work
                                    // (claude-agent-acp #958), so all of it
                                    // already streams into the live turn —
                                    // surfacing it as overlay activity too
                                    // renders it twice and reorders the
                                    // transcript as the upserts land.
                                    //
                                    // `Injected` ONLY: `PromptRequired` leaves
                                    // the content unconsumed (the caller
                                    // resends it as a real prompt, which
                                    // fingerprints itself, and a stale entry
                                    // would swallow a same-text out-of-turn
                                    // refire for the whole ledger TTL), and a
                                    // `StartedNewTurn` genuinely runs detached
                                    // — the overlay is the only place its work
                                    // can surface at all.
                                    if matches!(outcome, Ok(SteerOutcome::Injected)) {
                                        prompt_ledger.record_prompt_blocks(&blocks);
                                        // A custom agent's history is codeg's
                                        // own transcript, and only codeg knows
                                        // the steer happened (see
                                        // `record_steer`). Recorded in the
                                        // encoding it went out in, and before
                                        // the reply, so every frame this loop
                                        // takes in from here on lands after it.
                                        record_steer(
                                            agent_type,
                                            &sid.0,
                                            &map_prompt_blocks(blocks.clone()),
                                        )
                                        .await;
                                    }
                                    let _ = reply.send(outcome);
                                }
                                Some(ConnectionCommand::StopAsyncTask {
                                    task_id,
                                    reply,
                                }) => {
                                    // Mid-turn is the COMMON case: a background
                                    // task is usually launched by the turn that
                                    // is still running. Awaited inline like
                                    // Steer above — the runtime pumps I/O on its own
                                    // task, so this defers other queued
                                    // commands, not the turn's updates.
                                    let _ = reply.send(
                                        send_stop_async_task_request(&cx, &sid, &task_id).await,
                                    );
                                }
                                Some(ConnectionCommand::Cancel) => {
                                    // Send CancelNotification to agent to stop the current turn
                                    let _ = cx.send_notification_to(
                                        Agent,
                                        CancelNotification::new(sid.clone()),
                                    );
                                    // Also terminate any command runtimes created for this
                                    // session so cancellation does not hang on long-running
                                    // terminal tools.
                                    terminal_runtime
                                        .release_all_for_session(sid.0.as_ref())
                                        .await;
                                    tracked_terminal_tool_calls.clear();
                                    // Also cancel any pending permission requests
                                    // (queued ones included), clearing the card
                                    // that was on screen, and immediately emit
                                    // TurnComplete so the frontend transitions out
                                    // of "prompting" and the user can send new
                                    // messages. Don't wait for the agent -- it may
                                    // be slow to respond or not respond at all.
                                    // One critical section for the same reason as
                                    // the turn-end exits: a request admitted
                                    // between the two would be published and then
                                    // silently un-displayed by TurnComplete,
                                    // wedging the queue.
                                    drain_permissions_then_emit(
                                        perms,
                                        state,
                                        emitter,
                                        AcpEvent::TurnComplete {
                                            session_id: sid.0.to_string(),
                                            stop_reason: "cancelled".into(),
                                            agent_type: agent_type.to_string(),
                                        },
                                    )
                                    .await;
                                    // Cascade-cancel any in-flight delegations owned by
                                    // this parent connection. Idempotent with the
                                    // cleanup-guard cancel_by_parent at the end of
                                    // run_connection (#1: empty pending → no-op).
                                    // Without this, a user-initiated cancel of a parent
                                    // prompt mid-delegation would leave the child agent
                                    // running indefinitely (broker no longer applies a
                                    // timeout; only an MCP `notifications/cancelled` or
                                    // a parent/child disconnect would otherwise tear
                                    // the delegation down). Turn-scoped: the
                                    // connection stays alive after a prompt cancel,
                                    // so keep the parent's `consumed` tool_call
                                    // memory (a re-emit must not mis-bind the next
                                    // same-key delegation); the cleanup-guard
                                    // teardown still clears everything when the
                                    // connection finally goes away.
                                    //
                                    // Await inline so the fast tracker +
                                    // parked-call drain is ordered before the
                                    // next prompt (keeping it scoped to the
                                    // just-ended turn); the broker backgrounds
                                    // the slow child teardown internally, so the
                                    // user-visible Cancel path doesn't wait on
                                    // (potentially slow) child agent teardown.
                                    // The user already saw the parent's
                                    // TurnComplete above, and the broker's
                                    // drain-first lock guarantees no double
                                    // DelegationCompleted emit.
                                    if let Some(inj) = delegation_injection {
                                        inj.broker.cancel_by_parent_turn(conn_id).await;
                                        // Reclaim any parked `ask_user_question` /
                                        // Grok `exit_plan_mode` approval owned by this
                                        // connection. Unlike `perms` (drained inline
                                        // above), these registries live on the manager
                                        // and are otherwise only drained on full
                                        // connection teardown -- but a turn-scoped
                                        // Cancel keeps the connection alive. Without
                                        // this the entry lingers, and the
                                        // one-pending-per-connection guard in
                                        // `register_{question,plan_approval}` then
                                        // silently rejects the NEXT one on this
                                        // connection (its card never shows). Idempotent
                                        // with the cleanup-guard drain (empty map ->
                                        // no-op); the dropped sender declines the tool /
                                        // replies disconnect, exactly like the
                                        // permission drain above.
                                        inj.questions.cancel_questions_by_parent(conn_id).await;
                                        inj.plan_approvals
                                            .cancel_plan_approvals_by_parent(conn_id)
                                            .await;
                                    }
                                    // Keep awaiting the prompt response, in the
                                    // background: the agent still owes its
                                    // `cancelled` reply, and dropping the pending
                                    // request instead would make the runtime send
                                    // `$/cancel_request` on top of the
                                    // `session/cancel` above — a second
                                    // cancellation of the same turn.
                                    tokio::spawn(async move {
                                        let _ = prompt_response.await;
                                    });
                                    break;
                                }
                                Some(ConnectionCommand::Disconnect) | None => {
                                    tracing::info!(
                                        "[ACP] disconnect requested during prompting; connection_id={conn_id}"
                                    );
                                    let _ = cx.send_notification_to(
                                        Agent,
                                        CancelNotification::new(sid.clone()),
                                    );
                                    terminal_runtime
                                        .release_all_for_session(sid.0.as_ref())
                                        .await;
                                    tracked_terminal_tool_calls.clear();
                                    drain_permissions(perms, state, emitter).await;
                                    disconnect_requested = true;
                                    break;
                                }
                                Some(ConnectionCommand::Prompt { .. }) => {
                                    // Tripwire, not a user-facing case. The
                                    // `turn_in_flight` gate in `send_prompt_inner`
                                    // rejects a second prompt BEFORE it is ever
                                    // enqueued, so a `Prompt` reaching this mid-turn
                                    // command handler means an ungated sender slipped
                                    // past the gate (a broken invariant) — surface it
                                    // at `warn` instead of letting the `_ => {}` below
                                    // swallow it silently.
                                    tracing::warn!(
                                        connection_id = %conn_id,
                                        "[ACP] in-turn Prompt DROPPED — the turn_in_flight gate should have rejected this"
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }

                if disconnect_requested {
                    tracing::info!(
                        "[ACP] closing connection loop after disconnect; connection_id={conn_id}"
                    );
                    break;
                }

                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Connected,
                    },
                )
                .await;
            }
            Some(ConnectionCommand::RespondPermission {
                request_id,
                option_id,
            }) => {
                resolve_permission(perms, state, emitter, request_id, option_id).await;
            }
            Some(ConnectionCommand::SetMode { mode_id }) => {
                if let Err(e) = set_session_mode(session, state, emitter, mode_id).await {
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::Error {
                            message: format!("Failed to set mode: {e}"),
                            agent_type: agent_type.to_string(),
                            code: Some(SET_MODE_FAILED_ERROR_CODE.to_string()),
                            details: None,
                            // Recoverable: idle SetMode failure leaves the
                            // connection alive — same rationale as the
                            // mid-prompt SetMode site above.
                            terminal: false,
                        },
                    )
                    .await;
                }
            }
            Some(ConnectionCommand::SetConfigOption {
                config_id,
                value_id,
            }) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                let set_result = if agent_type == AgentType::Grok {
                    set_grok_config_option(&cx, &sid, state, emitter, config_id, value_id).await
                } else {
                    set_session_config_option(&cx, &sid, state, emitter, config_id, value_id).await
                };
                if let Err(e) = set_result {
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::Error {
                            message: format!("Failed to set config option: {e}"),
                            agent_type: agent_type.to_string(),
                            code: Some(SET_CONFIG_OPTION_FAILED_ERROR_CODE.to_string()),
                            details: None,
                            // Recoverable: idle SetConfigOption failure leaves
                            // the connection alive.
                            terminal: false,
                        },
                    )
                    .await;
                }
            }
            Some(ConnectionCommand::GoalControl { action, reply }) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                let method = state.read().await.goal_control_method.clone();
                let landed = match send_goal_control(&cx, &sid, action, &method).await {
                    Ok(()) => true,
                    Err(e) => {
                        emit_with_state(
                            state,
                            emitter,
                            AcpEvent::Error {
                                message: format!("Failed to control goal: {e}"),
                                agent_type: agent_type.to_string(),
                                code: Some(GOAL_CONTROL_FAILED_ERROR_CODE.to_string()),
                                details: None,
                                // Recoverable: an idle pause/clear failure leaves the
                                // connection alive.
                                terminal: false,
                            },
                        )
                        .await;
                        false
                    }
                };
                if let Some(reply) = reply {
                    // Reply — never drop — for the same reason the Steer arm
                    // below does: the manager awaits this oneshot to decide
                    // whether to follow up, and a dropped sender would hang it.
                    // (Its follow-up is an interrupt, which this idle arm's
                    // caller won't get anyway: there is no turn to stop.)
                    let _ = reply.send(landed);
                }
            }
            Some(ConnectionCommand::Steer { blocks: _, reply }) => {
                // Steering only means something for a RUNNING turn. Reply —
                // never drop — so the manager's shielded task can't hang on
                // the oneshot; the caller falls back to a normal prompt (the
                // same reroute the frontend already has for a turn-end race).
                let _ = reply.send(Err(AcpError::NoActiveTurn));
            }
            Some(ConnectionCommand::StopAsyncTask { task_id, reply }) => {
                // Unlike Steer, this is NOT turn-scoped: background work
                // outlives the turn that launched it, and stopping it between
                // turns is exactly what the button is for.
                let cx = session.connection();
                let sid = session.session_id().clone();
                let _ = reply.send(send_stop_async_task_request(&cx, &sid, &task_id).await);
            }
            Some(ConnectionCommand::Cancel) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
                terminal_runtime
                    .release_all_for_session(sid.0.as_ref())
                    .await;
                // The Stop button on a turn Grok is running on its own ends it
                // now, like the mid-turn Cancel does — without waiting on the
                // agent's own `turn_completed`, which the closed turn then
                // ignores. That is the one idle turn with a `TurnComplete`.
                let closed_agent_turn = close_grok_agent_turn(
                    state,
                    emitter,
                    perms,
                    agent_type,
                    &agent_turn_session_id,
                    "cancelled",
                    &mut cb_state,
                )
                .await;
                // Otherwise, unlike the mid-turn Cancel branch, this one does NOT
                // emit `TurnComplete` (there is no turn), so nothing else would ever
                // clear the on-screen permission card — before the compensating
                // `PermissionResolved` inside `drain_permissions`, an idle Cancel
                // left a card up on every client with its responder already
                // cancelled (clicking it did nothing) and pinned a work task at
                // `awaiting_input` forever, because `PermissionResolved` was only
                // emitted from the `RespondPermission` path.
                drain_permissions(perms, state, emitter).await;
                // Cascade-cancel any pending delegations owned by this parent.
                // Reached when Cancel arrives between prompts (idle path); the
                // inner Cancel handler covers mid-prompt. Both must trigger
                // because the per-prompt cancel path doesn't tear down the
                // parent connection, so the cleanup-guard cancel_by_parent
                // at run_connection's exit wouldn't fire. Turn-scoped for that
                // same reason: the connection stays alive, so keep the parent's
                // `consumed` tool_call memory (a re-emit must not mis-bind the
                // next same-key delegation).
                //
                // Awaited inline (fast drain before the next prompt; broker
                // backgrounds the slow child teardown): see inner Cancel
                // handler above for rationale.
                if let Some(inj) = delegation_injection {
                    inj.broker.cancel_by_parent_turn(conn_id).await;
                }
                if closed_agent_turn {
                    cancel_grok_agent_turn_asks(delegation_injection, conn_id).await;
                }
            }
            Some(ConnectionCommand::Fork { fork_point, reply }) => {
                if !supports_fork {
                    let _ = reply.send(Err(AcpError::protocol(
                        "This agent does not support session/fork".to_string(),
                    )));
                    continue;
                }
                // The manager's between-turns check reads `turn_in_flight`,
                // which a turn Grok started on its own never sets. Forking now
                // would leave that turn open on the session we are leaving.
                if cb_state.grok_agent_turn.is_some() {
                    let _ = reply.send(Err(AcpError::TurnInProgress));
                    continue;
                }
                let cx = session.connection();
                let sid = session.session_id().clone();
                let inherited_mode_id =
                    live_mode_for_fork(&*state.read().await, session.modes().as_ref());
                tracing::info!(
                    "[ACP] Sending session/fork for session_id={} cwd={} fork_point={:?}",
                    sid.0,
                    cwd,
                    fork_point.as_ref().map(|p| &p.message_id)
                );
                // The fork's establishment starts here. A Grok catalog broadcast
                // that landed before it is already on this session's picker, and
                // the forked session's handshake is answered from the catalog it
                // brought; only one landing from now on can be newer than that
                // handshake (see `SessionState::grok_catalog_broadcast`).
                state.write().await.grok_catalog_broadcast = None;
                let result =
                    crate::acp::fork::fork_session(&cx, &sid, cwd, fork_point.as_ref()).await;
                match result {
                    Ok((fork_response, fork_models_raw)) => {
                        tracing::info!(
                            "[ACP] Fork succeeded: new_session_id={}",
                            fork_response.session_id.0
                        );
                        return Ok(Some(ForkExitInfo {
                            fork_response,
                            fork_models_raw,
                            inherited_mode_id,
                            original_session_id: sid.0.to_string(),
                            reply,
                            connection: cx,
                        }));
                    }
                    Err(e) => {
                        tracing::error!("[ACP] Fork failed: {e}");
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Some(ConnectionCommand::Disconnect) | None => {
                break;
            }
        }
    }
    Ok(None)
}

/// Serialize tool-call `content` blocks into a single human-readable string.
///
/// `include_diffs = false` skips `Diff` blocks. Used when the edit has been
/// hoisted into a synthesized canonical `raw_input` (see
/// `synthesize_edit_input_from_diffs`): without this the same edit ships twice
/// (doubling the event) and the hunkless full-file `--- /+++` blob stays in the
/// tool `output`, where `extractEditLineChangeStats` mis-counts it as full-file
/// +/- totals in the card header even though the body shows the compact diff.
pub(crate) fn serialize_tool_call_content(
    content: &[ToolCallContent],
    include_diffs: bool,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for item in content {
        match item {
            ToolCallContent::Content(c) => {
                if let ContentBlock::Text(text) = &c.content {
                    parts.push(text.text.clone());
                }
            }
            // A codex AIR patch block's text fields are placeholders, never
            // file text (see `diff_block_payload`); its change reaches the
            // card through the synthesized input or not at all.
            ToolCallContent::Diff(diff)
                if include_diffs
                    && crate::acp::air_contract::air_meta_value(diff.meta.as_ref(), "diffPatch")
                        .is_none() =>
            {
                let path = diff.path.display();
                let mut diff_text = format!("--- {path}\n+++ {path}\n");
                if let Some(old) = &diff.old_text {
                    for line in old.lines() {
                        diff_text.push_str(&format!("-{line}\n"));
                    }
                }
                for line in diff.new_text.lines() {
                    diff_text.push_str(&format!("+{line}\n"));
                }
                parts.push(diff_text);
            }
            ToolCallContent::Terminal(t) => {
                parts.push(format!("[Terminal: {}]", t.terminal_id));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Synthesize a canonical edit `raw_input` from `ToolCallContent::Diff` block(s).
///
/// codex-acp reports file edits as ACP `Diff` content blocks and leaves
/// `raw_input` empty — the edit lives only in `content`, and the ACP `title` is
/// the diff header `--- <path>`. With no `raw_input` the frontend classifier
/// (`inferLiveToolName`) falls back to `normalizeToolName(title)`, which returns
/// unrecognized strings verbatim, so the tool call renders as a generic tool
/// literally *named* `--- <path>` (wrench icon, raw header as the title) instead
/// of an edit card. The historical path is unaffected because the JSONL parser
/// stores codex's native `*** Begin Patch` text.
///
/// Reconstructing from the already-serialized `--- /+++` string would be lossy
/// (content lines beginning with `-`/`+`/`---`/`+++`, the old/new boundary,
/// CRLF). Here the structured `Diff` is still intact, so map it losslessly:
/// - one path, one block  -> `{"file_path","old_string","new_string"}`
/// - several paths        -> `{"changes":{"<path>":{"old_text","new_text"},…}}`
/// - a Git patch (codex's AIR `diffPatch`, see [`diff_block_payload`]) ->
///   `{"changes":{"<path>":{"diff":"<patch>"}}}`, plus `file_path` when it is
///   the only path so the card keeps its "Edit <file>" title.
///
/// All shapes classify as `"edit"` (`inferFromInput`) and render through the
/// existing `EditToolInput` / `EditChangesToolInput` pipeline — a ready-made
/// `diff` verbatim, old/new text through `generateUnifiedDiff` (a real hunk
/// diff, minimal even for full-file old/new). Returns `None` when `content`
/// carries no usable `Diff`, so callers only fall back to it when the agent
/// supplied no `raw_input` of its own.
///
/// **Several blocks for ONE path** are one edit, and are combined rather than
/// keyed over each other. Through codex-acp 1.13.x each file came as a single
/// block holding the whole old and new file; 2.0.0 sends one block per hunk
/// (`FileChangeReporter`: "`oldText` and `newText` hold the changed lines and
/// the context lines of the hunk, not the whole file") whenever it sends no
/// patch, and claude has always sent one per `structuredPatch` hunk after a
/// multi-site Edit. Keyed by path, every hunk but the last used to vanish. The
/// hunks' texts are joined with a [`HUNK_SEPARATOR`] line common to both sides,
/// so the frontend's line diff shows each hunk with the elision marked between
/// them. A hunk carries no line numbers, so none are invented.
pub(crate) fn synthesize_edit_input_from_diffs(content: &[ToolCallContent]) -> Option<String> {
    // Keep `old_text` as `Option`: ACP reports `None` for a newly created file
    // (`Diff.old_text` semantics). That distinction is the whole point of this
    // function's fix — collapsing `None` to `""` and emitting an edit shape
    // makes the frontend build a `--- a/<path>` diff, which `isAddedFileDiff`
    // does NOT match, so a freshly created file mis-renders as a modification
    // (the historical apply_patch `*** Add File:` path classifies it correctly).
    let mut order: Vec<String> = Vec::new();
    let mut per_path: HashMap<String, Vec<DiffBlockPayload>> = HashMap::new();
    for item in content {
        let ToolCallContent::Diff(diff) = item else {
            continue;
        };
        // A patch block that does not validate has no usable text either (its
        // `oldText`/`newText` are placeholders), so it is shown as nothing
        // rather than as an empty file.
        let Some(payload) = diff_block_payload(diff) else {
            continue;
        };
        let path = diff.path.display().to_string();
        if !per_path.contains_key(&path) {
            order.push(path.clone());
        }
        per_path.entry(path).or_default().push(payload);
    }

    match order.as_slice() {
        [] => None,
        [path] => {
            let blocks = per_path.remove(path)?;
            if let Some(patch) = joined_patches(&blocks) {
                return Some(
                    serde_json::json!({
                        "file_path": path,
                        "changes": { path: { "diff": patch } },
                    })
                    .to_string(),
                );
            }
            match combined_text_blocks(&blocks)? {
                // New file (old_text absent) → write shape. `inferFromInput`
                // classifies `{file_path, content}` as `write`, whose diff
                // builder emits the `--- /dev/null` header `isAddedFileDiff`
                // keys on → renders as a new file, matching the
                // reloaded-from-DB path.
                (None, new) => Some(
                    serde_json::json!({
                        "file_path": path,
                        "content": new,
                    })
                    .to_string(),
                ),
                // Edit → canonical `{old_string,new_string}` for the frontend's
                // `generateUnifiedDiff` (a real hunk diff, minimal even for
                // full-file old/new).
                (Some(old), new) => Some(
                    serde_json::json!({
                        "file_path": path,
                        "old_string": old,
                        "new_string": new,
                    })
                    .to_string(),
                ),
            }
        }
        many => {
            let mut changes = serde_json::Map::new();
            for path in many {
                let Some(blocks) = per_path.remove(path) else {
                    continue;
                };
                // Per entry, mirror the single-path split: a patch verbatim, a
                // new file as a ready-made creation diff
                // (`buildChunkFromEditChange` returns it verbatim →
                // `--- /dev/null` → new file), an edit as old/new text for the
                // frontend to diff.
                let entry = if let Some(patch) = joined_patches(&blocks) {
                    serde_json::json!({ "diff": patch })
                } else {
                    match combined_text_blocks(&blocks) {
                        Some((None, new)) => {
                            serde_json::json!({ "diff": build_new_file_diff(path, &new) })
                        }
                        Some((Some(old), new)) => {
                            serde_json::json!({ "old_text": old, "new_text": new })
                        }
                        None => continue,
                    }
                };
                changes.insert(path.clone(), entry);
            }
            (!changes.is_empty())
                .then(|| serde_json::json!({ "changes": changes }).to_string())
        }
    }
}

/// The line placed between two hunks of the same file when their texts are
/// combined (see [`synthesize_edit_input_from_diffs`]). It is on both sides, so
/// the line diff treats it as context — an elision mark, not a change.
const HUNK_SEPARATOR: &str = "⋯";

/// What one ACP `Diff` block says, once codex's AIR `diffPatch` is accounted
/// for.
enum DiffBlockPayload {
    /// The standard `oldText` / `newText` pair.
    Text { old: Option<String>, new: String },
    /// A validated Git patch for the block's file, headers rewritten to the
    /// absolute path (see [`normalize_git_patch`]).
    Patch(String),
}

/// Read one `Diff` block. `None` for a block that declares a patch codeg cannot
/// use: codex-acp 2.0.0's contract is that such a block's `oldText: null` /
/// `newText: ""` are "compatibility placeholders … not file snapshots", and a
/// receiver that rejects the patch "must show the change as unavailable" — not
/// render the placeholders as an emptied file.
///
/// codeg declares `diffPatch` to codex only (`build_client_capabilities`), and
/// validates it the way the contract asks: `{version: 1, format: "git_patch",
/// text}` with at least one `@@` hunk.
fn diff_block_payload(diff: &agent_client_protocol::schema::v1::Diff) -> Option<DiffBlockPayload> {
    let Some(patch) =
        crate::acp::air_contract::air_meta_value(diff.meta.as_ref(), "diffPatch")
    else {
        return Some(DiffBlockPayload::Text {
            old: diff.old_text.clone(),
            new: diff.new_text.clone(),
        });
    };
    let valid = patch.get("version").and_then(serde_json::Value::as_i64) == Some(1)
        && patch.get("format").and_then(serde_json::Value::as_str) == Some("git_patch");
    let text = patch
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|_| valid)?;
    normalize_git_patch(text, &diff.path.display().to_string()).map(DiffBlockPayload::Patch)
}

/// Every patch among `blocks`, joined, or `None` when there is none.
fn joined_patches(blocks: &[DiffBlockPayload]) -> Option<String> {
    let patches: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            DiffBlockPayload::Patch(patch) => Some(patch.as_str()),
            DiffBlockPayload::Text { .. } => None,
        })
        .collect();
    (!patches.is_empty()).then(|| patches.join("\n"))
}

/// The text blocks of one path combined into a single `(old, new)` pair —
/// `None` when there are none. One block passes through unchanged (a `None`
/// old text keeps meaning "new file"); several are hunks of one edit, joined
/// with [`HUNK_SEPARATOR`] (a missing old text reads as empty there).
fn combined_text_blocks(blocks: &[DiffBlockPayload]) -> Option<(Option<String>, String)> {
    let texts: Vec<(&Option<String>, &String)> = blocks
        .iter()
        .filter_map(|block| match block {
            DiffBlockPayload::Text { old, new } => Some((old, new)),
            DiffBlockPayload::Patch(_) => None,
        })
        .collect();
    match texts.as_slice() {
        [] => None,
        [(old, new)] => Some(((*old).clone(), (*new).clone())),
        many => {
            let join = |parts: Vec<&str>| {
                let mut out = String::new();
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(HUNK_SEPARATOR);
                        out.push('\n');
                    }
                    out.push_str(part);
                }
                out
            };
            let old = join(
                many.iter()
                    .map(|(old, _)| old.as_deref().unwrap_or(""))
                    .collect(),
            );
            let new = join(many.iter().map(|(_, new)| new.as_str()).collect());
            Some((Some(old), new))
        }
    }
}

/// Rewrite a codex Git patch's file headers onto the absolute path, or `None`
/// when the text has no hunk (the contract: "at least one `@@` hunk").
///
/// codex names both sides as the absolute path WITHOUT its leading slash under
/// `a/` / `b/` (`a/workspace/src/app.ts`, Windows `a/C:/work/App.ts`), which the
/// diff preview's `normalizePath` would turn into a relative path — and the
/// preview's file link would then point nowhere. codeg's own diffs name the
/// absolute path (`generateUnifiedDiff`: `--- a/${path}`), so the headers are
/// rewritten to that: `+++` from the block's own `path` (the new side, a
/// rename's target), `---` from the `rename from` line on a rename and from the
/// block path otherwise; `/dev/null` sides stay. The `diff --git` line is
/// dropped — its paths are ambiguous with spaces, and the `---`/`+++` lines
/// that follow carry everything the preview reads from it. Only lines before
/// the first `@@` are headers; after it, a line starting `--- ` is content.
fn normalize_git_patch(patch: &str, block_path: &str) -> Option<String> {
    // Split on `\n` only: the contract keeps the provider's bytes, a `\r`
    // included ("The patch keeps the provider bytes, including a carriage
    // return"), and `str::lines` would strip it off every CRLF hunk line.
    let lines: Vec<&str> = patch.split('\n').collect();
    let first_hunk = lines.iter().position(|line| line.starts_with("@@"))?;
    let (headers, body) = lines.split_at(first_hunk);
    let header = |line: &&str| line.trim_end_matches('\r').to_string();
    let headers: Vec<String> = headers.iter().map(header).collect();
    let renamed_from = headers
        .iter()
        .find_map(|line| line.strip_prefix("rename from "))
        .map(patch_header_path);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in &headers {
        if line.starts_with("diff --git ") || line.starts_with("index ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            if rest.trim_end() == "/dev/null" {
                out.push("--- /dev/null".to_string());
            } else {
                let old = renamed_from.clone().unwrap_or_else(|| block_path.to_string());
                out.push(format!("--- a/{old}"));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest.trim_end() == "/dev/null" {
                out.push("+++ /dev/null".to_string());
            } else {
                out.push(format!("+++ b/{block_path}"));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            out.push(format!("rename from {}", patch_header_path(rest)));
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            out.push(format!("rename to {}", patch_header_path(rest)));
            continue;
        }
        out.push(line.clone());
    }
    // The body verbatim — including the empty segment after a final `\n`, so
    // the join below restores it.
    out.extend(body.iter().map(|line| (*line).to_string()));
    Some(out.join("\n"))
}

/// A path as a codex patch header writes it (`a/`/`b/`-prefixed or bare,
/// possibly C-quoted, possibly tab-terminated), back as the absolute path it
/// was made from: the leading slash restored, a Windows drive path as is.
fn patch_header_path(raw: &str) -> String {
    let raw = raw.trim_end_matches(['\t', '\r']);
    let unquoted = match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => c_unquote(inner),
        None => raw.to_string(),
    };
    let bare = unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(&unquoted);
    let is_drive = bare.len() >= 3
        && bare.as_bytes()[0].is_ascii_alphabetic()
        && bare.as_bytes()[1] == b':'
        && bare.as_bytes()[2] == b'/';
    if is_drive || bare.starts_with('/') {
        bare.to_string()
    } else {
        format!("/{bare}")
    }
}

/// Undo Git's C-style path quoting (what codex applies to a path holding a
/// double quote, a backslash or a control character): `\"`, `\\`, the named
/// control escapes, and `\NNN` octal bytes. An escape it does not know is kept
/// as written rather than guessed at.
fn c_unquote(inner: &str) -> String {
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let next = bytes[i + 1];
        let named = match next {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'v' => Some(0x0b),
            _ => None,
        };
        if let Some(byte) = named {
            out.push(byte);
            i += 2;
            continue;
        }
        let octal = bytes.get(i + 1..i + 4).filter(|digits| {
            digits.iter().all(|d| (b'0'..=b'7').contains(d))
        });
        if let Some(digits) = octal {
            let value = digits
                .iter()
                .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A claude `Edit` / `Write` input with the file text put back from the diff
/// block that carries it — or `None` when the input needs nothing.
///
/// claude-agent-acp 0.82.0 puts each fact of an AIR client's tool call in one
/// field only, and the file text of an edit lives in the `diff` content block:
/// `rawInput` arrives as `{file_path}` alone (Edit loses `old_string` /
/// `new_string`, Write its `content`), on the same frame as the diff. Every
/// codeg edit card, the live line stats and the permission preview read the
/// INPUT, never the content blocks, so a live Edit would render as a bare path.
/// The diff block holds exactly the model's strings (0.81.x sent the same
/// `oldText`/`newText` beside the full input), so the input is rebuilt from it —
/// the same move the codex path has always made, merged here with the keys the
/// input still has (`file_path` as spelled, `replace_all`). An input that still
/// carries its text (any older adapter) is left alone: it is authoritative.
fn claude_complete_file_edit_input(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    own_raw_input: Option<&str>,
    content: &[ToolCallContent],
) -> Option<String> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }
    let tool_name = meta
        .and_then(|m| m.get("claudeCode"))
        .and_then(|c| c.get("toolName"))
        .and_then(serde_json::Value::as_str)?;
    if !matches!(tool_name, "Edit" | "Write") {
        return None;
    }
    let own = serde_json::from_str::<serde_json::Value>(own_raw_input?).ok()?;
    let own = own.as_object()?;
    if ["old_string", "new_string", "content"]
        .iter()
        .any(|key| own.contains_key(*key))
    {
        return None;
    }
    let synthesized = synthesize_edit_input_from_diffs(content)?;
    let mut merged = serde_json::from_str::<serde_json::Value>(&synthesized)
        .ok()?
        .as_object()?
        .clone();
    for (key, value) in own {
        merged.insert(key.clone(), value.clone());
    }
    Some(serde_json::Value::Object(merged).to_string())
}

/// Drop every `Terminal` block from a tool call's `content`, keeping the rest
/// in order.
///
/// Used on the self-hosted-terminal path only (see
/// `hosted_terminal_meta_marks_shell`). A
/// `ToolCallContent::Terminal` serializes to the bare `[Terminal: <id>]`
/// placeholder, which is meaningful ONLY while codeg's own `TerminalRuntime`
/// owns that terminal and `poll_tracked_terminal_tool_calls` streams the real
/// output over it (`raw_output_chunks` then wins over `content` in the
/// frontend store). These agents' terminals are agent-hosted, so nothing ever
/// supersedes the placeholder from the terminal channel — it would be the ONLY
/// thing on screen for the whole runtime of the command. The `_meta` bridge
/// supplies the output instead; strip the dead placeholder so a slow command
/// shows an empty running card rather than an opaque id.
fn strip_terminal_blocks(content: &[ToolCallContent]) -> Vec<ToolCallContent> {
    content
        .iter()
        .filter(|item| !matches!(item, ToolCallContent::Terminal(_)))
        .cloned()
        .collect()
}

/// Build a minimal unified diff for a newly created file: the `--- /dev/null`
/// header the frontend's `isAddedFileDiff` keys on, then every line of
/// `new_text` as an addition. Byte-for-byte identical to the frontend `write`
/// op's diff builder (`session-files.ts`), so a multi-file batch's new-file
/// entries render exactly like a single-file creation.
fn build_new_file_diff(path: &str, new_text: &str) -> String {
    // `split('\n')` (not `lines()`) mirrors the frontend `content.split("\n")`:
    // it keeps the trailing empty segment from a final newline, so the `+N`
    // count and the trailing `+` addition line match exactly.
    let lines: Vec<&str> = new_text.split('\n').collect();
    let mut out = format!(
        "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@",
        lines.len()
    );
    for line in lines {
        out.push('\n');
        out.push('+');
        out.push_str(line);
    }
    out
}

/// Extract `ContentBlock::Image` payloads from a `ToolCallContent` slice.
/// Returns `None` when no images are present so the upstream `images` field
/// on `AcpEvent::ToolCall(Update)` stays absent for non-image tool calls
/// (preserves replace-on-update semantics: an absent field means "keep
/// prior", a `Some(vec)` replaces).
pub(crate) fn extract_tool_call_images(content: &[ToolCallContent]) -> Option<Vec<ToolCallImageInfo>> {
    let mut imgs: Vec<ToolCallImageInfo> = Vec::new();
    for item in content {
        if let ToolCallContent::Content(c) = item {
            if let ContentBlock::Image(img) = &c.content {
                imgs.push(ToolCallImageInfo {
                    data: img.data.clone(),
                    mime_type: img.mime_type.clone(),
                    uri: img.uri.clone(),
                });
            }
        }
    }
    if imgs.is_empty() {
        None
    } else {
        Some(imgs)
    }
}

/// If the output looks like numbered lines (`   115→content`), strip them
/// and return `{"start_line":N,"content":"..."}` — same as the historical path.
fn structurize_live_output(text: &str) -> String {
    if let Some(json) = crate::parsers::strip_numbered_lines(text) {
        return json;
    }
    text.to_string()
}

/// Resolve line numbers for live tool call input.
///
/// Resolve line numbers for live tool call input (string form).
///
/// - For apply_patch with bare `@@`: resolve line numbers in place.
/// - For canonical edit JSON: inject `_start_line`.
fn resolve_live_tool_input(text: &str, cwd: Option<&str>) -> String {
    if text.contains("@@\n") || text.contains("@@\r\n") {
        if let Some(resolved) = crate::parsers::resolve_patch_text(text, cwd) {
            return resolved;
        }
    }
    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(text) {
        if inject_start_line(&mut parsed, cwd) {
            return parsed.to_string();
        }
    }
    text.to_string()
}

/// Try to inject `_start_line` into a JSON object with `file_path` + `old_string`.
/// Returns true if injected.
///
/// The camelCase spellings are OpenCode's: its ACP adapter forwards the tool's
/// own arguments verbatim (`{filePath, oldString, newString}`), and the rename
/// to the canonical keys happens in the FRONTEND (`aliasToolInputKeys`) — so
/// reading only the snake_case names meant no live OpenCode edit ever got a
/// start line, and its hunks restarted at 1 until the conversation was reloaded
/// and the history parser recovered the real number from `metadata.diff`.
/// Resolution happens on the `in_progress` frame, before the edit is applied, so
/// `old_string` is still findable on disk.
fn inject_start_line(value: &mut serde_json::Value, cwd: Option<&str>) -> bool {
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    let fp = obj
        .get("file_path")
        .or_else(|| obj.get("filePath"))
        .or_else(|| obj.get("path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let old_str = obj
        .get("old_string")
        .or_else(|| obj.get("oldString"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let (Some(fp), Some(old_str)) = (fp, old_str) {
        if let Some(sl) = find_string_start_line(&fp, &old_str, cwd) {
            obj.insert("_start_line".to_string(), serde_json::json!(sl));
            return true;
        }
    }
    false
}

/// Find the 1-based start line of `needle` in the file at `path`.
fn find_string_start_line(path: &str, needle: &str, cwd: Option<&str>) -> Option<u64> {
    if needle.is_empty() {
        return None;
    }
    let file_lines = crate::parsers::load_file_lines(path, cwd)?;
    let file_content = file_lines.join("\n");
    let byte_offset = file_content.find(needle)?;
    Some(file_content[..byte_offset].matches('\n').count() as u64 + 1)
}

pub(crate) fn json_value_to_text(val: &Option<serde_json::Value>) -> Option<String> {
    match val {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(v) if !v.is_null() => Some(v.to_string()),
        _ => None,
    }
}

/// Resolve the live `raw_output` string for a Grok tool call.
///
/// Grok reports terminal output in the standard `content[]` channel (clean,
/// human-readable text) AND in a structured `rawOutput` object whose readable
/// text lives only in the string `output_for_prompt` (its `output` field is a
/// raw byte array, and the remaining keys — `command`, `exit_code`, … — are
/// metadata). Feeding that object through `json_value_to_text` stringifies the
/// whole thing into a JSON blob that (a) shadows the clean `content` — the live
/// renderer's `raw_output_chunks` win over `content`
/// (`conversation-runtime-store.ts`) — and (b) is then dropped by the terminal
/// renderer as a metadata-only "command envelope"
/// (`commandOutputFromJsonString` returns `""`), so a finished command shows no
/// result during live streaming even though the history parser renders it fine.
///
/// Mirror the history parser (`parsers/grok.rs::update_tool_output`): prefer the
/// already-serialized `content`, and only fall back — when `content` is empty —
/// to the object's string `output_for_prompt` (Bash/terminal), a background-task
/// `TaskOutput` envelope (see `parsers::grok::grok_task_output_envelope`, the
/// one exception to "never emit the object blob": the frontend parses it into a
/// background-task card), or, for an MCP `rawOutput`, the text under `output`
/// (see grok_mcp_output_text). Returning `None` lets the frontend render
/// `content`. Non-object / absent / unrecognized `rawOutput` → `None`.
///
/// Note: `content` here is `serialize_tool_call_content`, which for a Grok
/// terminal call is the plain text block (verified against real `~/.grok`
/// data). It could in principle also serialize `Diff`/`Terminal` blocks, in
/// which case a Grok tool carrying ONLY such a block plus `output_for_prompt`
/// would render the serialized block instead of the prompt text — but Grok's
/// `run_terminal_command` emits `content:text`, so this stays parity with
/// history for the shapes Grok actually produces.
fn grok_live_tool_output(
    content: &Option<String>,
    raw_output: &Option<serde_json::Value>,
) -> Option<String> {
    if content.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return None;
    }
    let raw = raw_output.as_ref()?;
    // Bash / terminal calls: the readable text lives only in `output_for_prompt`.
    if let Some(text) = raw
        .get("output_for_prompt")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(text.to_string());
    }
    // Background-task polls (`get_command_or_subagent_output`): the command,
    // exit code and shell text all live under the `TaskOutput` envelope, which
    // matches none of the paths around it — without this the card streams empty.
    // Shared with the history parser so both hand the frontend the same string.
    if let Some(envelope) = crate::parsers::grok::grok_task_output_envelope(raw) {
        return Some(envelope);
    }
    // MCP calls (Grok's `use_tool` envelope): the result text lives under
    // `output.<*Output>` instead (see grok_mcp_output_text). Without this a
    // finished MCP call — e.g. the `delegate_to_agent` ack carrying
    // `task_id=…` — would surface no output at all.
    grok_mcp_output_text(raw)
}

/// Resolve the live `raw_output` string for a pi tool call.
///
/// pi-acp hands ACP the pi tool result VERBATIM as `rawOutput` — the MCP-shaped
/// envelope `{"content":[{"type":"text","text":…}]}` — while putting that very
/// same text, already flattened, on the `content[]` channel
/// (`toolResultToText`; both `tool_execution_update` and `tool_execution_end`
/// emit the pair). Stringifying the envelope shadows the clean text, because the
/// live renderer's `raw_output_chunks` win over `content`
/// (`conversation-runtime-store.ts`), and nothing downstream unwraps that shape
/// (`commandOutputFromJsonString` bails on a `content` array) — so a finished
/// `bash` painted its terminal body with the JSON source string.
///
/// Same parity rule as Grok (see `grok_live_tool_output`): whenever `content`
/// carries anything, it IS pi-acp's own flattening of this very result — emit
/// `None` and let it render (that also covers the `details.diff` / `stdout` /
/// `output` shapes `toolResultToText` flattens, which the envelope alone does
/// not reach). Only with no `content` is the envelope unwrapped, through the
/// SAME flattener the history parser uses so both surfaces show one string.
///
/// pi's empty opening announcement is excluded up front — emitting anything for
/// it would strand a placeholder chunk over the whole call (see
/// `pi_result_is_empty_announcement`). Every OTHER result that carries something
/// but whose block array holds no text still stringifies exactly as before:
/// `toolResultToText` reaches `details.diff`, `stdout`/`stderr` and `output` that
/// the block array alone does not, so dropping those would lose a real result.
///
/// (pi ≥0.0.33 routes `bash` through `_meta.terminal_*` and sends no `rawOutput`
/// for it at all — see `hosted_terminal_chunk`. This is the path every OTHER pi
/// tool takes, and the one `bash` itself takes on earlier pi-acp builds.)
fn pi_live_tool_output(
    content: &Option<String>,
    raw_output: &Option<serde_json::Value>,
) -> Option<String> {
    let raw = raw_output.as_ref()?;
    if pi_result_is_empty_announcement(raw) {
        return None;
    }
    if content.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return None;
    }
    raw.get("content")
        .and_then(crate::parsers::pi::tool_result_content_text)
        .or_else(|| json_value_to_text(raw_output))
        .map(|text| structurize_live_output(&text))
}

/// Is this `rawOutput` pi's "I have started, and have nothing yet" announcement —
/// literally `{"content": []}`?
///
/// pi's bash tool fires `onUpdate({content: [], details: undefined})` before the
/// process writes a byte (`bash.js`), so this is the opening frame of EVERY pi
/// command. Both channels then carry noise: `rawOutput` is the empty envelope,
/// and pi-acp — with no text, diff, stdout or stderr to flatten — falls back to
/// `JSON.stringify(result, null, 2)` for `content[]` (`toolResultToText`), which
/// puts a literal `{\n  "content": []\n}` in the terminal card until the first
/// line of real output arrives. Worse, stringifying the envelope into
/// `raw_output` seeds it as the call's chunk, and since every later frame prefers
/// `content` and emits `None` — which the reducer treats as "keep the previous
/// chunks" (`acp-connections-context.tsx`) — that placeholder would outrank the
/// real output for the rest of the call.
///
/// So both channels are suppressed for this frame, and ONLY for this frame: the
/// predicate is deliberately the narrowest thing that identifies it — an empty
/// `content` array and no other non-null member. Any richer result (a diff,
/// `stdout`/`stderr`, an exit code, a truncation record) is something
/// `toolResultToText` may have flattened into real text, so it is left alone even
/// when its block array is empty. Keyed off the STRUCTURED payload, never off the
/// rendered string, so a command whose own output looks like an empty envelope is
/// untouched.
fn pi_result_is_empty_announcement(raw_output: &serde_json::Value) -> bool {
    let Some(obj) = raw_output.as_object() else {
        return false;
    };
    obj.get("content")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| blocks.is_empty())
        && obj
            .iter()
            .all(|(key, value)| key == "content" || value.is_null())
}

/// pi-gated wrapper for the `content` channel: see `pi_result_is_empty_announcement`.
fn pi_result_content_is_stringify_noise(
    agent_type: AgentType,
    raw_output: &Option<serde_json::Value>,
) -> bool {
    matches!(agent_type, AgentType::Pi)
        && raw_output
            .as_ref()
            .is_some_and(pi_result_is_empty_announcement)
}

/// `_meta` key carrying OpenCode's authoritative tool name (see
/// [`stamp_opencode_tool_name`]). Namespaced like every other agent's marker
/// (`claudeCode`, `qoder`, `x.ai/tool`) so it cannot collide with a payload the
/// adapter itself publishes.
const OPENCODE_META_KEY: &str = "opencode";

/// Record the raw tool name from an OpenCode `tool_call`'s opening frame as
/// `_meta.opencode.toolName`, so the frontend classifier has the same identity
/// the history parser reads out of `part.tool`.
///
/// OpenCode's ACP adapter states the tool's name EXACTLY ONCE, and only on this
/// frame: `pendingToolCall` titles a not-yet-running call with the bare tool id
/// (`toolTitle` falls through to `toolName` because `ToolStatePending` carries
/// no title), the `in_progress` update repeats it, and then the COMPLETION frame
/// replaces `title` with a display label and drops `kind`, `locations` and
/// `rawInput` entirely — verified against opencode 1.18.30 driven over real ACP:
///   tool_call        title="glob"  kind="search" rawInput={}
///   tool_call_update title="glob"  kind="search" rawInput={"pattern":"*.txt"}
///   tool_call_update (no title, no kind, no rawInput) content=[…]
/// (`read`→"notes.txt", `todowrite`→"3 todos", `grep`→"third", `bash` keeps the
/// command.) So from the second frame on, the only signal left is the input
/// shape — and OpenCode has several tools that are indistinguishable that way:
/// `glob` (`{pattern, path}`) classified as **grep**, `lsp_*` (`{path}`) as
/// **read**, and an MCP tool taking `{query}` as **websearch**, each of which the
/// history parser names correctly. This closes that live/history split at the
/// source instead of adding more input-shape heuristics.
///
/// Gated on `pending` + an empty `rawInput` because `loadSession`/`forkSession`
/// REPLAY finished tool parts through the same `pendingToolCall` builder: a
/// replayed frame is also `status: "pending"`, but it is built from the
/// COMPLETED state, so its title is the display label and its input is fully
/// populated. Requiring the empty input keeps the marker off those. (codeg
/// prefers `session/resume`, which replays nothing, so this is belt-and-braces.)
/// A `pending` frame that did arrive with partial input simply goes unstamped —
/// today's behavior, never a wrong name.
///
/// Merges into whatever `_meta` the adapter sent and never overwrites an
/// existing `opencode` key.
fn stamp_opencode_tool_name(
    agent_type: AgentType,
    status: &str,
    raw_input: &Option<serde_json::Value>,
    title: &str,
    meta: Option<serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::OpenCode || status != "pending" {
        return meta;
    }
    let input_is_empty = match raw_input {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::Object(map)) => map.is_empty(),
        _ => false,
    };
    let name = title.trim();
    if !input_is_empty || name.is_empty() {
        return meta;
    }
    let mut meta = meta.unwrap_or_default();
    if meta.contains_key(OPENCODE_META_KEY) {
        return Some(meta);
    }
    meta.insert(
        OPENCODE_META_KEY.to_string(),
        serde_json::json!({ "toolName": name }),
    );
    Some(meta)
}

/// `_meta` key marking a tool call as one of codex's `search` command actions
/// (see [`stamp_codex_search_action`]). Frontend twin:
/// `CODEX_SEARCH_ACTION_META_KEY` in `src/lib/codex-command-action.ts`.
const CODEX_SEARCH_ACTION_META_KEY: &str = "codeg.codexSearchAction";

/// Stamp codex's `search` command actions on their opening frame, so the
/// frontend can tell them from every other agent's grep.
///
/// It needs to because codeg advertises `_meta.terminal_output_delta`, and with
/// that capability codex-acp completes a command that printed nothing as a
/// bare `failed` status — no `rawOutput` envelope, so no exit code (see
/// `build_client_capabilities`). For a search that is almost always rg's exit
/// 1, "no matches", and `isGrepNoMatchResult` presents it that way — but a
/// bare `failed` with no output is also what an interrupted grep from another
/// adapter can look like, so the rule must know the call is codex's. Only the
/// backend knows the agent at frame level, hence the marker.
///
/// Keyed on the opening frame's `kind: "search"` with no `terminal_info`, which
/// is exactly `createCommandActionEvent`'s search arm (listFiles and read go out
/// as `kind: "read"`). Later frames carry no `_meta` for such a call unless it
/// streamed output, and the reducer keeps the opening `_meta` until one does —
/// by which point the call has output and the rule no longer applies anyway.
fn stamp_codex_search_action(
    agent_type: AgentType,
    kind: &ToolKind,
    hosted_shell: bool,
    meta: Option<serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::Codex || hosted_shell || !matches!(kind, ToolKind::Search) {
        return meta;
    }
    let mut meta = meta.unwrap_or_default();
    meta.insert(
        CODEX_SEARCH_ACTION_META_KEY.to_string(),
        serde_json::Value::Bool(true),
    );
    Some(meta)
}

/// Resolve the live `raw_output` string for an OpenCode tool call.
///
/// OpenCode's ACP adapter reports a finished tool on BOTH channels: the clean
/// result text on `content[]`, and the envelope `{output, metadata?,
/// attachments?}` — wrapping that very same string — on `rawOutput` (a failure
/// sends `{error, metadata?}` beside the error text). Stringifying the envelope
/// shadows the clean text, because the live renderer's `raw_output_chunks` win
/// over `content` (`conversation-runtime-store.ts`), so every card that parses
/// the result ITSELF is handed JSON source instead of the result.
///
/// Verified against opencode 1.18.23 (driven over real ACP with a stub MCP
/// server): a codeg-mcp `ask_user_question` completes as
///   content:   [{"type":"content","content":{"type":"text","text":"The user
///               answered your question(s):\n1. [框架] …\n   → 选项 A\n"}}]
///   rawOutput: {"output":"<that same text>","metadata":{"truncated":false}}
/// OpenCode drops the MCP `structuredContent` entirely, so the human-readable
/// lines ARE the whole record — and `AskQuestionResultCard`, seeing only the
/// one-line JSON blob, matched neither the structured envelope nor the text
/// fallback and rendered an answered question as "no selection", while the
/// history parser (which reads the same `state.output` bare) rendered it fine.
///
/// Same parity rule as Grok and pi (see [`grok_live_tool_output`]): whenever
/// `content` carries anything, it IS OpenCode's own rendering of this result —
/// return `None` and let it render. Only with no `content` is the envelope
/// unwrapped, mirroring `parsers/opencode.rs`: `output`, else the failure
/// `error`, else `metadata.output` (a command writing only to stderr leaves
/// `state.output` empty while the combined stream stays in the metadata). An
/// unrecognized payload still stringifies as before, so no result is ever lost.
fn opencode_live_tool_output(
    content: &Option<String>,
    raw_output: &Option<serde_json::Value>,
) -> Option<String> {
    // `read` is the one tool whose `content` is LOSSY rather than merely
    // redundant: OpenCode hands the client the file body with the line numbers
    // stripped, while `metadata.display` still carries `lineStart`. The history
    // parser rebuilds `{start_line, content}` from it, so without this the same
    // finished `read` renders numbered after a reload and unnumbered while
    // live. `metadata.display` is unique to `read`, so every other tool keeps
    // the parity rule below.
    if let Some(structured) = raw_output
        .as_ref()
        .and_then(|raw| crate::parsers::opencode::structure_read_output(raw.get("metadata")))
    {
        return Some(structured);
    }
    if content.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return None;
    }
    let raw = raw_output.as_ref()?;
    fn text(value: Option<&serde_json::Value>) -> Option<&str> {
        value
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
    }
    text(raw.get("output"))
        .or_else(|| text(raw.get("error")))
        .or_else(|| text(raw.pointer("/metadata/output")))
        .map(structurize_live_output)
        .or_else(|| json_value_to_text(raw_output).map(|t| structurize_live_output(&t)))
}

/// What a pi `agent_message_chunk` actually IS (issue #525).
///
/// pi-acp puts the assistant's prose AND its own lifecycle announcements on the
/// same `agent_message_chunk` channel, so a caffeinate extension's notify lands
/// spliced into the reply: `你好。有什么需要我帮你处理?Released pi-caffeinate
/// (agent finished).` See [`pi_message_chunk_route`] for the full inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PiChunkRoute {
    /// The agent's own words — pi's `message_update` / `text_delta`. Renders as
    /// today's `ContentDelta`. This is what EVERY other agent's chunk resolves
    /// to, since the classifier is pi-gated.
    Prose,
    /// A pi-acp lifecycle announcement with nothing downstream needs it.
    Drop,
    /// pi is auto-retrying the model call. Routed to the shared retry banner
    /// (`AcpEvent::TurnRetrying`) instead of the prose stream. The numbers are
    /// pi's own, recovered from the sentence it formats them into; all three are
    /// `None` for pi-acp's shapeless `"Retrying..."` fallback.
    Retrying {
        attempt: Option<u32>,
        max: Option<u32>,
        delay_ms: Option<u64>,
    },
}

/// Tell a pi `agent_message_chunk` that is the ASSISTANT SPEAKING from one that
/// is pi-acp ANNOUNCING SOMETHING (issue #525).
///
/// Real prose reaches this channel from exactly one place: pi-acp's
/// `message_update` arm, for `assistantMessageEvent.type === "text_delta"`.
/// Everything below is pi-acp synthesizing a sentence out of a pi RPC lifecycle
/// event and emitting it on that SAME channel, where the live store appends it
/// into whatever message is open — which is the whole bug:
///
/// | pi-acp `dist/index.js` | text | marker |
/// |---|---|---|
/// | L1265 `extension_ui_request` / `notify`   | the extension's own message | `_meta.piAcp.notify` |
/// | L1169 `auto_retry_start`                  | `Retrying (attempt N/M, waiting Ss)...` | — |
/// | L1176 `auto_retry_end`                    | `Retry finished, resuming.` | — |
/// | L1183 `auto_compaction_start`             | `Context nearing limit, running automatic compaction...` | — |
/// | L1193 `auto_compaction_end`               | `Automatic compaction finished; …` | — |
/// | L834  `prompt()` queue                    | `Queued message (position N).` | — |
/// | L1222 `agent_settled` queue               | `Starting queued message. (N remaining)` | — |
/// | L860  `cancel()` queue                    | `Cleared queued prompts.` | — |
///
/// The notify marker is read FIRST and wins outright, because that text is
/// arbitrary extension content — a pi extension can notify anything, including
/// something that reads exactly like prose, so the structured marker is the only
/// trustworthy handle on it. The other seven carry no marker at all, and pi-acp
/// offers no alternative channel for them, so they are matched as LITERALS.
/// Three things keep that honest: the whole classifier is gated on
/// `AgentType::Pi`; every rule matches the WHOLE trimmed chunk, never a
/// substring; and the adapter version is pinned in `registry.rs`, so a bump is
/// the natural place to re-check these strings. A false positive would need the
/// model to emit one of these exact sentences as an entire standalone delta, and
/// would cost one dropped delta — not a corrupted message.
///
/// Two families deliberately stay `Prose`, and must:
///
/// - **Slash-command replies** (pi-acp L2080+: `/compact`, `/session`, `/name`,
///   `/export`, `/follow-up`, `/steering`, `/changelog`). The user ASKED for
///   those; they ride the same channel and match no rule here, which is exactly
///   the point of matching whole literals rather than sniffing for
///   "status-looking" text. (The startup prelude — `sendStartupInfoIfPending` —
///   is NOT in this family: it is dropped, but by
///   [`pi_take_startup_banner`] against the text pi-acp itself reported, never
///   by a rule here.)
/// - **`Pi <method> UI request is not supported in ACP yet; cancelling it.`**
///   (L1257). pi asked the user for input and pi-acp auto-cancelled it — a rare,
///   actionable failure with no better home today. Dropping it would hide the
///   reason a turn went sideways, which is worse than the noise this fixes.
///
/// Used by BOTH the renderer (`emit_conversation_update`) and the empty-turn
/// probe (`is_agent_output_update`), so the two can never disagree about whether
/// a chunk was output — a status-only turn must not render blank AND report
/// success.
fn pi_message_chunk_route(
    agent_type: AgentType,
    text: &str,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> PiChunkRoute {
    if agent_type != AgentType::Pi {
        return PiChunkRoute::Prose;
    }
    // A pi extension's `ui.notify()`. pi's own semantics for it are a transient
    // toast (`rpc-mode.js`: "Fire and forget - no response needed"), never
    // conversation — so it is dropped at every level (`info` / `warning` /
    // `error`). Keyed off the marker alone: the message is the extension's, and
    // matching it as text would be matching arbitrary user content.
    if meta.is_some_and(|meta| {
        meta.get("piAcp")
            .and_then(|pi| pi.get("notify"))
            .is_some_and(serde_json::Value::is_object)
    }) {
        return PiChunkRoute::Drop;
    }
    let text = text.trim();
    match text {
        // `auto_retry_end`. No event of its own: the banner clears at the next
        // tool call / plan update / turn boundary, exactly as codex's does.
        "Retry finished, resuming." => PiChunkRoute::Drop,
        // `formatAutoRetryMessage`'s fallback, when pi's event carried no usable
        // attempt / maxAttempts / delayMs.
        "Retrying..." => PiChunkRoute::Retrying {
            attempt: None,
            max: None,
            delay_ms: None,
        },
        // Compaction. Dropped rather than rendered as the shared
        // `_meta.contextCompaction` card: a tool call synthesized at the END of a
        // turn would land after the reply, and `SessionState`'s "final assistant
        // text" is the text FOLLOWING the last tool call — so the card would
        // silently blank the delegation result / work-task summary. Restoring the
        // card is a follow-up, gated on teaching that extraction to skip
        // compaction refs (which codex and grok need too).
        "Context nearing limit, running automatic compaction..."
        | "Automatic compaction finished; context was summarized to continue the session." => {
            PiChunkRoute::Drop
        }
        // pi-acp's own prompt queue. codeg's turn gate normally makes this
        // unreachable (`manager.rs` rejects a concurrent prompt), so all three
        // are handled defensively and together — one of them showing up alone
        // would be the odd one out.
        "Cleared queued prompts." => PiChunkRoute::Drop,
        _ => {
            if let Some((attempt, max, delay_ms)) = pi_parse_retry_announcement(text) {
                PiChunkRoute::Retrying {
                    attempt: Some(attempt),
                    max: Some(max),
                    delay_ms: Some(delay_ms),
                }
            } else if pi_is_queue_announcement(text) {
                PiChunkRoute::Drop
            } else {
                PiChunkRoute::Prose
            }
        }
    }
}

/// Recover `(attempt, max, delay_ms)` from `Retrying (attempt 1/3, waiting 2s)...`
/// — pi-acp's `formatAutoRetryMessage`, which is the only place codeg can reach
/// these numbers: pi sends them structured to pi-acp, which formats them into a
/// sentence and forwards nothing else.
///
/// Worth recovering rather than shipping the sentence as the banner's message,
/// because the banner already has localized slots for exactly this data
/// (`claudeApiRetry.retryingWithMax` / `nextRetryIn`) — so a zh-CN user reads
/// `正在重试 1/3，2.0 秒后重试` instead of an English sentence with a Chinese
/// suffix bolted on.
///
/// Parsed by hand rather than by regex: the shape is fixed, and every field is
/// re-validated (`{n}/{n}`, `{n}s`, nothing left over), so a sentence that merely
/// starts the same way falls through to `Prose` instead of half-matching.
fn pi_parse_retry_announcement(text: &str) -> Option<(u32, u32, u64)> {
    let body = text
        .strip_prefix("Retrying (attempt ")?
        .strip_suffix("s)...")?;
    let (attempts, delay_seconds) = body.split_once(", waiting ")?;
    let (attempt, max) = attempts.split_once('/')?;
    Some((
        attempt.parse().ok()?,
        max.parse().ok()?,
        delay_seconds.parse::<u64>().ok()?.checked_mul(1000)?,
    ))
}

/// `Queued message (position N).` / `Starting queued message. (N remaining)` —
/// the two pi-acp queue announcements that carry an interpolated count (the
/// third, `Cleared queued prompts.`, is a plain literal handled by the caller).
fn pi_is_queue_announcement(text: &str) -> bool {
    let counted = text
        .strip_prefix("Queued message (position ")
        .and_then(|rest| rest.strip_suffix(")."))
        .or_else(|| {
            text.strip_prefix("Starting queued message. (")
                .and_then(|rest| rest.strip_suffix(" remaining)"))
        });
    counted.is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
}

/// The session prelude pi-acp reports on `session/new`, if this response is
/// carrying one.
///
/// pi-acp builds a markdown banner (`buildStartupInfo`: pi's version, the
/// project `AGENTS.md`, every discovered skill file, prompts, extensions, an
/// update notice) and pushes it down the ORDINARY `agent_message_chunk` channel
/// with no marker of any kind — so codeg rendered it as the assistant's opening
/// words, before the user had said anything, complete with the absolute paths of
/// every skill and extension on the machine.
///
/// The trustworthy handle is on the `session/new` RESPONSE instead:
/// `_meta.piAcp.startupInfo` holds the very same string (verified byte-for-byte
/// against pi-acp 0.0.33 driven over real stdio ACP — `startupInfo === chunk`).
/// Capturing it there and matching the chunk against it is what lets the drop be
/// exact rather than a guess about what "looks like a banner" — the same
/// preference for a structured handle over text that makes the `notify` marker
/// win outright in [`pi_message_chunk_route`]. A pi with `quietStartup` set
/// sends neither, and every other agent has no `piAcp` meta at all.
///
/// Gated on `AgentType::Pi` for the same reason the terminal-meta bridge is:
/// `piAcp` is pi-acp's own namespace and must not be read off arbitrary agents.
fn pi_startup_banner(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    if agent_type != AgentType::Pi {
        return None;
    }
    meta?
        .get("piAcp")?
        .get("startupInfo")?
        .as_str()
        .map(str::trim)
        .filter(|banner| !banner.is_empty())
        .map(str::to_string)
}

/// Whether this `agent_message_chunk` IS the pending startup banner — and, if so,
/// consume it so only the first match is dropped.
///
/// Taking rather than peeking is what keeps this from being a permanent text
/// filter: pi-acp guards its own emit with `startupInfoSent`, so exactly one
/// chunk can be the prelude, and a later message that happens to repeat the text
/// (the user pasting it back, say) is prose and renders.
///
/// NOT consulted by [`is_agent_output_update`], which has no session state to
/// read — and does not need it: pi-acp queues the prelude in a `setTimeout(…, 0)`
/// fired as `session/new` returns, so it lands on the idle loop, before any
/// prompt. Were it ever to arrive mid-turn, the probe would count it as output —
/// the safe direction, since a turn wrongly called "empty" is the failure that
/// predicate exists to prevent.
///
/// Cheap for everyone else: non-pi agents return before touching the lock, and a
/// pi session whose banner was already taken pays one read lock — the same lock
/// the emit on this path acquires anyway.
async fn pi_take_startup_banner(
    agent_type: AgentType,
    state: &Arc<RwLock<SessionState>>,
    text: &str,
) -> bool {
    if agent_type != AgentType::Pi {
        return false;
    }
    if state.read().await.pi_startup_banner.is_none() {
        return false;
    }
    let mut guard = state.write().await;
    match guard.pi_startup_banner.as_deref() {
        Some(banner) if banner == text.trim() => {
            guard.pi_startup_banner = None;
            true
        }
        _ => false,
    }
}

/// The mode id carried by a Gemini `[MODE_UPDATE] <mode>` chunk, if this chunk
/// IS one.
///
/// gemini-cli announces approval-mode changes as PROSE rather than as the
/// `current_mode_update` the protocol has for it: `handleApprovalModeChanged`
/// pushes an `agent_message_chunk` whose entire content is
/// `[MODE_UPDATE] ${payload.mode}` (packages/cli/src/acp/acpSession.ts, 0.60.0).
/// Rendered as-is that is a literal assistant bubble, and codeg's own mode
/// selector never follows the switch the user just made in the agent.
///
/// Two guards keep this from eating real prose:
///
/// - the marker is matched against the WHOLE trimmed chunk, never as a
///   substring, so an assistant paragraph that happens to quote the marker
///   mid-sentence still renders;
/// - the id must be one the agent ADVERTISED for this session, not a mode name
///   we hardcoded. That is both tighter and self-maintaining: `plan` only
///   exists when `isPlanEnabled()` (`buildAvailableModes`), so at a session
///   without it a user typing `[MODE_UPDATE] plan` gets their own text back
///   instead of a silent swallow, and a mode gemini adds later needs no change
///   here. Before the modes handshake lands there is nothing to match against
///   and this returns `None` — prose, which is the safe direction: showing one
///   ugly literal beats eating a reply.
///
/// Residual risk, accepted knowingly: gemini emits one chunk per model stream
/// event with boundaries it chooses, so a reply that DISCUSSES the marker could
/// in principle split so that one whole chunk is exactly `[MODE_UPDATE] yolo`.
/// That would both swallow the line and desync codeg's mode selector (the event
/// is latched into `modes.current_mode_id`) until the next real switch. There
/// is no signal on the wire that separates that chunk from a genuine one, and
/// the alternative — rendering the literal on every real mode switch — is the
/// common case rather than the pathological one.
///
/// NOT consulted by [`is_agent_output_update`], which is sync and has no
/// session state to read the advertised modes from. Same call as the pi banner
/// makes, for the same reason: counting one of these as output is the safe
/// direction, and the turn it would have to mislabel — one carrying a mode
/// update and nothing else — does not occur, since the mid-turn switch gemini
/// makes (`exit_plan_mode`) always lands alongside its own tool call.
///
/// Cheap for everyone else: non-Gemini agents return before touching the lock,
/// and a Gemini chunk that is ordinary prose returns at the prefix test.
async fn gemini_mode_update_chunk(
    agent_type: AgentType,
    state: &Arc<RwLock<SessionState>>,
    text: &str,
) -> Option<String> {
    if agent_type != AgentType::Gemini {
        return None;
    }
    let mode_id = text.trim().strip_prefix("[MODE_UPDATE] ")?.trim();
    if mode_id.is_empty() {
        return None;
    }
    state
        .read()
        .await
        .modes
        .as_ref()?
        .available_modes
        .iter()
        .any(|mode| mode.id == mode_id)
        .then(|| mode_id.to_string())
}

/// Grok wraps every MCP tool invocation in a generic `use_tool` envelope whose
/// `raw_input` is `{"tool_name": "<server>__<tool>", "tool_input": {..real args..}}`.
/// Peel it so the call is correlated (delegation `lifecycle.rs`), classified, and
/// parsed as a direct MCP call — identical to how hosts like Claude Code surface
/// MCP tools. Without this, Grok's `delegate_to_agent` (and the other codeg-mcp
/// companion tools) never resolve to their dedicated cards, and the delegation
/// broker can't correlate the parent tool call to bind the sub-session.
///
/// Returns `(inner_tool_name, inner_tool_input)` only for the envelope shape —
/// a non-empty string `tool_name` plus a `tool_input` value — so Grok's native
/// tools (`run_terminal_command`, `search_tool`, `spawn_subagent`, …), which
/// carry their args directly, pass through untouched.
fn unwrap_grok_use_tool(
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, serde_json::Value)> {
    let obj = raw_input?.as_object()?;
    let tool_name = obj
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())?;
    let tool_input = obj.get("tool_input")?;
    Some((tool_name.to_string(), tool_input.clone()))
}

/// Extract the human-readable text from a Grok MCP `rawOutput`
/// (`{"type":"MCP","output":{"OkayOutput":"…"}}`, or an `*Output` error variant).
/// The MCP result is the first string value under `output` (`output` may itself
/// be a bare string on some tools). Returns `None` for a non-MCP `rawOutput` so
/// the caller can fall through to the Bash/`output_for_prompt` path.
fn grok_mcp_output_text(raw_output: &serde_json::Value) -> Option<String> {
    if raw_output.get("type").and_then(serde_json::Value::as_str) != Some("MCP") {
        return None;
    }
    let output = raw_output.get("output")?;
    if let Some(text) = output.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    // First NON-EMPTY string value (the singleton `*Output` variant). Filtering
    // inside `find_map` — not after — so an earlier empty-string sibling can't
    // shadow a later populated one.
    output
        .as_object()?
        .values()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// Recover a codeg-mcp companion tool's identity from its RESULT text, for
/// Cursor sessions only.
///
/// Cursor's ACP layer announces every MCP call from the first streaming
/// partial — before `McpArgs` exists — so the announcement is the literal
/// title "MCP: tool" with an empty `raw_input`, and `sendToolCallUpdate`
/// (bundle-verified) never forwards `title`/`raw_input` again. The ONLY
/// wire signal that ever identifies the call is the MCP result text arriving
/// on the completion update, and for the codeg-mcp companion tools that text
/// is a codeg-owned contract:
///
/// * a `delegate_to_agent` ack opens with
///   `"Delegation successful. task_id="` (`broker.rs::running_ack`);
/// * `get_delegation_status` renders the compact `{"tasks":[..]}` JSON
///   (`companion.rs::render_batch_report`), whose items carry `task_id` +
///   a `status` from the fixed report vocabulary.
///
/// (`cancel_delegation` results are free-form report messages with no stable
/// prefix, so a canceled call keeps the generic title — a rare op, accepted.)
///
/// Matching those shapes lets the completion update rewrite the title to the
/// canonical `<server>__<tool>` form (the exact name the history parser
/// derives from `McpArgs`), so the frontend resolves the dedicated delegation
/// cards instead of a generic "MCP: tool" call. Returns `None` for everything
/// else — an unrecognized result keeps the wire title untouched. Callers gate
/// the sniff to calls ANNOUNCED with the identity-less "MCP: tool" title
/// (`CodeBuddyLiveState::cursor_generic_mcp_ids`), so a native tool whose
/// output merely echoes these shapes is never re-titled.
fn cursor_companion_title_from_content(content: Option<&str>) -> Option<&'static str> {
    let text = content?.trim_start();
    if text.starts_with("Delegation successful. task_id=") {
        return Some(crate::acp::delegation::DELEGATE_TOOL_REWRITE_TITLE);
    }
    // Cheap guards before the full JSON parse: the status report is a JSON
    // object whose first key is `tasks`.
    if !text.starts_with('{') || !text.contains("\"tasks\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let tasks = v.get("tasks")?.as_array()?;
    let is_report_item = |t: &serde_json::Value| {
        t.get("task_id").and_then(|x| x.as_str()).is_some()
            && t.get("status").and_then(|x| x.as_str()).is_some_and(|s| {
                matches!(s, "running" | "completed" | "failed" | "canceled" | "unknown")
            })
    };
    if !tasks.is_empty() && tasks.iter().all(is_report_item) {
        return Some(crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE);
    }
    None
}

/// Mirrors `parsers/opencode.rs:425-429` (and `parsers/codebuddy.rs`'s
/// `subagent_type → "Agent"` rewrite) so streaming and reload-from-DB render the
/// same Agent card. The SQLite-side condition is
/// `tool == "task" && state.input.subagent_type IS NOT NULL`, where `tool` is the
/// agent's **internal** tool name. ACP only exposes a user-facing `title` (e.g.
/// "Explore project structure") rather than the internal tool name, so we cannot
/// replicate the `tool == "task"` half of the AND here. We instead anchor on a
/// known sub-agent-capable `agent_type` (OpenCode and CodeBuddy — both surface a
/// description-style title and the standard `{…, subagent_type}` input, and never
/// emit a bare top-level `subagent_type` for anything but a sub-agent) plus the
/// non-empty `subagent_type` string in `raw_input` — together these uniquely
/// identify a sub-agent invocation in practice. Other agents stay excluded to
/// avoid any cross-agent collision a generic `subagent_type` field could cause.
fn is_subagent_invocation(agent_type: AgentType, raw_input: &Option<String>) -> bool {
    if !matches!(agent_type, AgentType::OpenCode | AgentType::CodeBuddy) {
        return false;
    }
    let Some(text) = raw_input.as_deref() else {
        return false;
    };
    // Cheap substring guard avoids parsing large `raw_input` payloads
    // (e.g. prompts with many KB of context) when the field is absent.
    if !text.contains("subagent_type") {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// CodeBuddy routes MCP tools through its `DeferExecuteTool` virtualization
/// layer, which surfaces over ACP as a tool call whose `raw_input` wraps the real
/// call as `{ "toolName": "mcp__…", "params": { … } }`. Return that inner
/// `toolName` so the caller can rewrite the live `title` to it — making the
/// frontend resolve the dedicated card (delegation / question / …), mirroring the
/// historical unwrap in `parsers/codebuddy.rs`. `raw_input` is left untouched
/// (the cards peel `params` themselves, and that keeps `inferFromInput` from
/// misclassifying `cancel_delegation`'s `{task_id}` as a generic task).
fn codebuddy_deferred_tool_name(agent_type: AgentType, raw_input: &Option<String>) -> Option<String> {
    if agent_type != AgentType::CodeBuddy {
        return None;
    }
    let text = raw_input.as_deref()?;
    // Cheap substring guard before parsing a potentially large payload.
    if !text.contains("toolName") {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    crate::parsers::codebuddy::deferred_tool_name(&value).map(|s| s.to_string())
}

/// CodeBuddy ships a deferred MCP tool's RESULT as a single re-serialized
/// `{ "type": "text", "text": <inner> }` content part (the OpenAI-Agents content
/// shape), where `<inner>` is the MCP `CallToolResult` content text — for the
/// delegation companion, the compact report / `{ "tasks": [...] }` JSON. The
/// dedicated cards (`parseStatusReport` / `parseToolOutput`) expect that bare
/// inner payload (the content-only host shape they already handle for Claude
/// Code), NOT this wrapper, so a live `get_delegation_status` / `cancel_delegation`
/// poll otherwise renders as raw JSON text. Peel the wrapper to its inner `text`,
/// mirroring the historical `deferred_result_envelope` normalization in
/// `parsers/codebuddy.rs`.
///
/// Gated on CodeBuddy + the exact wrapper shape (`type == "text"` with a string
/// `text`): a non-deferred result (Bash/Read/ToolSearch/…) is never a lone
/// `{type,text}` object, and no delegation report carries a top-level `type`, so
/// those pass through untouched. Unlike the title rewrite, this needs no
/// `raw_input`, so it also normalizes a result-only `ToolCallUpdate` that omits it.
fn unwrap_codebuddy_deferred_output(agent_type: AgentType, text: &str) -> Option<String> {
    if agent_type != AgentType::CodeBuddy {
        return None;
    }
    // Cheap substring guard before parsing a potentially large payload.
    if !text.contains("\"type\"") {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let obj = value.as_object()?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("text") {
        return None;
    }
    obj.get("text").and_then(|t| t.as_str()).map(str::to_string)
}

/// True when a CodeBuddy tool call's ACP `_meta` identifies it as a native
/// sub-agent (`Agent`) invocation. CodeBuddy tags this in `_meta` from the FIRST
/// frame (`codebuddy.ai/toolName == "Agent"`) and later adds
/// `codebuddy.ai/isSubagent` / `subagentType` — whereas the `subagent_type`
/// field in `raw_input` (see `is_subagent_invocation`) only streams in dozens of
/// frames later. Reading the meta lets the title rewrite fire on frame 1, so the
/// Agent pill never spends an opening window classified as a generic tool (and
/// its child tool calls, which carry `codebuddy.ai/parentToolCallId` every frame,
/// nest from the start). Gated on CodeBuddy so the generic `codebuddy.ai/*` keys
/// can never affect another agent.
fn codebuddy_meta_marks_subagent(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    let Some(meta) = meta else {
        return false;
    };
    if meta.get("codebuddy.ai/toolName").and_then(|v| v.as_str()) == Some("Agent") {
        return true;
    }
    if meta.get("codebuddy.ai/isSubagent").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    meta.get("codebuddy.ai/subagentType")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// Two adapters report a shell tool call as an ACP `Terminal` content block
/// whose `terminalId` is its OWN tool-call id, then stream the command's output
/// over a bespoke `_meta` channel instead of the ACP terminal channel. Neither
/// ever calls `terminal/create`, so the id they name can never resolve against
/// `TerminalRuntime` — which only ever mints `term_<uuid>` ids. Codeg renders
/// the resulting placeholder and then polls a terminal that does not exist, so
/// the card stays at `[Terminal: <id>]` with no command output, ever (#519).
///
/// **pi-acp** says it outright in its README: "No ACP filesystem delegation
/// (`fs/*`) and no ACP terminal delegation (`terminal/*`). pi reads/writes and
/// executes locally." Its wire, per 0.0.33 (`emitBashToolCall` /
/// `emitBashOutputUpdate`):
/// - `tool_call`: `title` = the command, `kind: execute`, the `Terminal` block,
///   `_meta.terminal_info = {terminal_id, cwd}`, and NO `rawInput`.
/// - `tool_call_update` ×N: `_meta.terminal_output = {terminal_id, data}` where
///   `data` is an incremental delta, plus `_meta.terminal_exit =
///   {terminal_id, exit_code, signal}` on the final frame. No `content`, no
///   `rawOutput` — this `_meta` is the only channel carrying the output.
///
/// **codex-acp** has the SAME shape and codeg never noticed, because unlike pi
/// it also repeats the whole output as `rawOutput` at the end, so the card
/// filled in — just not until the command had finished. `createTerminalCommandEvent`
/// builds `content: [{type: "terminal", terminalId: item.id}]` +
/// `_meta.terminal_info` for every command `commandExecutionUsesTerminalOutput`
/// accepts (one with no single recognized `commandAction` — i.e. a real shell
/// command rather than a `search`/`listFiles` it renders as its own card), and
/// `createCommandOutputDeltaEvent` streams the output as
/// `_meta.terminal_output_delta = {terminal_id, data}` deltas throughout. That
/// key is what `resolveTerminalOutputMode` returns **by default**, with no
/// client capability involved, so this has been on the wire all along. The
/// completion frame then carries `rawOutput = {formatted_output: <the whole
/// aggregated output>, exit_code}` plus `_meta.terminal_exit`. Unlike pi,
/// codex DOES send `rawInput = {command, cwd}` on the opening frame, so the
/// card classifies on its own and only the output channel needs bridging.
///
/// These readers bridge that channel into the same `raw_output` stream the
/// host-terminal poller produces, so both agents' shell cards read exactly like
/// every other agent's — and stream while the command runs instead of appearing
/// all at once when it ends.
///
/// GATED ON AN AGENT SET ON PURPOSE: both adapters' keys are unnamespaced
/// (`terminal_output`, not `pi/terminalOutput`), so an ungated reader would be a
/// collision waiting to happen. Known limitation: the agent types resolve from
/// the built-in registry ids, so a user who registers either adapter under a
/// CUSTOM agent id gets `AgentType::Custom` and keeps the old behaviour. That is
/// the right trade — an unnamespaced-meta bridge must not apply to arbitrary
/// agents.
///
/// Deliberately keyed off the adapter's own `terminal_info` marker rather than
/// the agent type wholesale, so a future release that DOES delegate `terminal/*`
/// goes back to being polled normally.
fn hosted_terminal_meta_marks_shell(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if hosted_terminal_output_key(agent_type).is_none() {
        return false;
    }
    meta.is_some_and(|meta| meta.get("terminal_info").is_some_and(|v| v.is_object()))
}

/// The `_meta` key an adapter streams its self-hosted terminal output under,
/// for the agents whose unnamespaced keys codeg is willing to read.
///
/// The two spell it differently for a reason: pi predates the ACP delta
/// convention and reuses `terminal_output`, while codex-acp picks between
/// `terminal_output` and `terminal_output_delta` in `resolveTerminalOutputMode`
/// and lands on the delta key unless the client asks for the other one —
/// which codeg does not: it advertises the delta key itself, for the duplicate
/// `rawOutput` that drops (see `build_client_capabilities`). Both carry the
/// same `{terminal_id, data}` payload with incremental `data`, so only the key
/// differs.
fn hosted_terminal_output_key(agent_type: AgentType) -> Option<&'static str> {
    match agent_type {
        AgentType::Pi => Some("terminal_output"),
        AgentType::Codex => Some("terminal_output_delta"),
        _ => None,
    }
}

/// The incremental output chunk from the agent's output key, if any.
///
/// pi computes this delta itself as `next.startsWith(prev) ? next.slice(prev.len)
/// : next`, so in the degenerate case where its cumulative text stops being a
/// prefix extension (stdout still growing AFTER stderr was first folded in) it
/// re-sends the WHOLE text as a "delta" and we append it, duplicating. Codeg
/// cannot detect that without holding the full snapshot, which
/// `ToolCallOutputCache` deliberately does not do (8 KB tail only). Appending is
/// the correct reading of the wire contract; the duplication is an upstream
/// residual. codex has no such degenerate case: its deltas come straight from
/// the exec stream (`item/commandExecution/outputDelta`).
///
/// Text codex wrote to a running command's stdin rides the same stream. Through
/// 1.13.x codex-acp sent it as an output delta of its own, `\n<stdin>\n`;
/// 2.0.0 sends an AIR client a separate `_meta.terminal_input = {terminal_id,
/// data}` instead ("It is not output"). Rendered the way the older adapter —
/// and 2.0.0 for every other client — renders it, so a card that answered a
/// prompt still shows the answer where it was typed.
fn hosted_terminal_output_delta(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    let key = hosted_terminal_output_key(agent_type)?;
    let meta = meta?;
    let data = |key: &str| {
        meta.get(key)?
            .get("data")?
            .as_str()
            .filter(|data| !data.is_empty())
    };
    let output = data(key).map(str::to_string);
    let stdin = (agent_type == AgentType::Codex)
        .then(|| data("terminal_input"))
        .flatten()
        .map(|input| format!("\n{input}\n"));
    match (output, stdin) {
        (Some(output), Some(stdin)) => Some(output + &stdin),
        (output, stdin) => output.or(stdin),
    }
}

/// The `[terminal exited: …]` line for `_meta.terminal_exit`, if present.
///
/// The values are read key by key and fed to `format_terminal_exit_status`, so
/// the wording is byte-for-byte what a host-owned terminal produces and can
/// never drift. Do NOT be tempted to `serde_json::from_value::<TerminalExitStatus>`
/// the object instead: pi writes SNAKE_case (`exit_code`) while the schema type
/// is `rename_all = "camelCase"`, and unknown fields are ignored — so it
/// deserializes CLEANLY into an all-`None` status and silently prints
/// "[terminal exited: finished]", dropping the exit code the report explicitly
/// asks for. codex-acp writes the same snake_case shape (`{exit_code, signal,
/// terminal_id}`), so the identical reasoning applies to it.
fn hosted_terminal_exit_line(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    hosted_terminal_output_key(agent_type)?;
    let exit = meta?.get("terminal_exit")?.as_object()?;
    let code = exit.get("exit_code").and_then(serde_json::Value::as_i64);
    let signal = exit
        .get("signal")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    // `TerminalExitStatus::exit_code` is `u32`, but pi's is a JS number and some
    // runtimes report a signal death as a negative. Format those directly rather
    // than dropping the only failure evidence a killed command leaves behind.
    let formatted = match code.map(u32::try_from) {
        Some(Err(_)) => {
            let mut parts = vec![format!("exit code: {}", code.unwrap_or_default())];
            if let Some(signal) = &signal {
                parts.push(format!("signal: {signal}"));
            }
            parts.join(", ")
        }
        narrowed => format_terminal_exit_status(
            &TerminalExitStatus::new()
                .exit_code(narrowed.and_then(Result::ok))
                .signal(signal),
        ),
    };
    Some(format!("[terminal exited: {formatted}]"))
}

/// Bridge the agent's `_meta` terminal channel onto the `raw_output` stream,
/// returning the `(payload, append)` pair to emit — or `None` when this frame
/// carries no terminal data (which is every frame of every agent outside
/// `hosted_terminal_output_key`).
///
/// `append` is false for a call's FIRST chunk, so it REPLACES whatever the
/// opening frame left on the card, and true for every chunk after — the same
/// rule `poll_terminal_tool_call_output` applies via
/// `TrackedTerminalToolCall::has_emitted_output`. The payload goes through
/// `build_emit_payload` for the pipeline-wide ANSI-safe single-event cap.
///
/// The exit line is appended on EVERY exit, `exit code: 0` included: for a
/// command that printed nothing it is the only thing that supersedes the
/// placeholder, and it is what a host-owned terminal shows.
///
/// The entry is created here rather than required up front, so a client that
/// attached mid-turn (and so never saw the opening `terminal_info` frame) still
/// gets the output instead of silently dropping it.
fn hosted_terminal_chunk(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    tool_call_id: &str,
    tracked: &mut HashMap<String, bool>,
) -> Option<(String, bool)> {
    let mut chunk = hosted_terminal_output_delta(agent_type, meta).unwrap_or_default();
    if let Some(exit_line) = hosted_terminal_exit_line(agent_type, meta) {
        if !chunk.is_empty() && !chunk.ends_with('\n') {
            chunk.push('\n');
        }
        chunk.push_str(&exit_line);
    }
    if chunk.is_empty() {
        return None;
    }
    let has_emitted = tracked.entry(tool_call_id.to_string()).or_insert(false);
    let append = *has_emitted;
    *has_emitted = true;
    Some(build_emit_payload(&chunk, append))
}

/// pi-acp titles a bash tool call `bashCommand(args) ?? toolName`. On the first
/// `toolcall_start` frame the arguments are still partial JSON, so the title is
/// the bare tool name `"bash"` and the real command only lands on a later frame.
///
/// Synthesize a canonical `{"command": …}` `raw_input` from a title that IS the
/// command, so `inferLiveToolName` classifies the call as `bash` and it renders
/// through the Bash card (`$ <cmd>`) — the same trick
/// `synthesize_edit_input_from_diffs` plays for codex's input-less diffs. Without
/// it the input shape is silent and the title fallback makes the card a generic
/// tool literally NAMED after the command (`node --version`), which is also how
/// it diverged from pi's own history parser (`parsers/pi.rs`, which builds a real
/// `bash` call with `{"command": …}`).
///
/// Returns `None` for the bare `"bash"` title: `normalizeToolName("bash")`
/// already resolves that frame to the Bash card, and synthesizing there would
/// flash `$ bash` for one frame before the real command arrives.
fn pi_bash_input_from_title(title: Option<&str>) -> Option<String> {
    let command = title?.trim();
    if command.is_empty() || command.eq_ignore_ascii_case("bash") {
        return None;
    }
    Some(serde_json::json!({ "command": command }).to_string())
}

/// Rebuild a Gemini tool call's `raw_input` from its title and locations.
///
/// gemini-cli puts NO `rawInput` on the ACP wire at all: a call arrives as
/// title + kind + locations + content and nothing else (`acpSession.ts`,
/// 0.60.0). codeg classifies a live call by the SHAPE of its input, so with
/// none of it every gemini tool lands on the generic card named after its own
/// title — no Bash card, no file card, no diff header.
///
/// This is a whitelist of exact title formats and NOT a match on `kind`,
/// because `kind` cannot identify the tool: `search` covers glob, grep AND web
/// search, and every MCP tool is hardcoded to `other` (`DiscoveredMCPTool`).
/// Synthesizing off `kind` alone would actively mislabel calls — codeg reads a
/// bare `pattern` as grep and a bare `query` as web search, so a guessed
/// `{"pattern": title}` would turn every glob into a grep. Not synthesizing
/// costs a generic card; guessing wrong invents a card that states something
/// untrue, so anything unrecognized returns `None` on purpose. Two formats are
/// deliberately left out for exactly that reason:
///
/// - glob and grep can both title themselves a bare `'<pattern>'`. Note it is
///   `grep.ts` that collides with `glob.ts`, NOT the `ripGrep.ts` that normally
///   serves this kind: ripgrep appends ` within …` unconditionally (its
///   `dir_path` defaults to `"."`), while glob and the plain-grep fallback both
///   append it only when `dir_path` is set. So the collision only happens on
///   machines without ripgrep — which is exactly the kind of "usually fine"
///   that makes guessing here a bad trade.
/// - web-fetch's prompt variant carries a `prompt`, not a `url`, so there is no
///   honest shape to emit; above 100 chars it is truncated to 97 + `...` as
///   well.
///
/// Every format below is the tool's own `getDisplayTitle()`, which the ACP
/// layer prefers over `getDescription()`. That preference is what makes the
/// `execute` rule exact: shell overrides `getDisplayTitle()` to return
/// `params.command` verbatim, so the title IS the command. (`getDescription()`
/// is the one that swaps in the model's prose description for commands over
/// 150 chars — it feeds the CLI's own TUI and never reaches the wire. Reading
/// that function instead would have made this rule render a Bash card whose
/// `$ …` line was an English sentence.)
///
/// Only consulted when the agent sent no `raw_input` of its own, and after
/// `synthesize_edit_input_from_diffs`, which reconstructs a better input for
/// the edits that do carry a diff.
fn gemini_synthesize_tool_input(
    agent_type: AgentType,
    kind: &ToolKind,
    title: &str,
    locations: &[ToolCallLocation],
) -> Option<String> {
    if agent_type != AgentType::Gemini {
        return None;
    }
    let title = title.trim();
    let input = match kind {
        ToolKind::Execute if !title.is_empty() => serde_json::json!({ "command": title }),
        // NOTE: no `edit`/write-file rule on purpose. gemini ships the edit as a
        // `diff` content block on both the permission and completion frames
        // (`acpSession.ts`), so `synthesize_edit_input_from_diffs` — which runs
        // ahead of this in the `.or()` chain — already rebuilds a real
        // `{old_string, new_string}` for it. A `{file_path}` here could only
        // ever fire on a frame that carries NO diff, and there it would make
        // things worse, not better: `inferFromInput` sees a path plus
        // `kind: edit` and returns "edit", which renders through EditToolInput
        // and shows BLANK because the old/new strings it reads are absent. A
        // generic card beats an empty edit card.
        //
        // read-file reports exactly one location, carrying the resolved
        // absolute path and `params.start_line` (`read-file.ts
        // toolLocations()`). One location is a sound discriminator because
        // `toolLocations()` is overridden by only three tools in the whole
        // tree — read-file, write-file and edit — and everything else inherits
        // `tools.ts`'s `return []`. So every OTHER Read-kind tool
        // (read_many_files, read-mcp-resource, the shell-background and
        // tracker tools) reports ZERO, not several.
        //
        // The line becomes `offset`, which is the key codeg's file card reads
        // (`content-parts-renderer.tsx`, `FileToolInput`); `start_line` is not
        // in its vocabulary and would render nothing.
        ToolKind::Read if locations.len() == 1 => {
            let location = &locations[0];
            match location.line {
                Some(line) => serde_json::json!({ "file_path": location.path, "offset": line }),
                None => serde_json::json!({ "file_path": location.path }),
            }
        }
        ToolKind::Search => serde_json::json!({
            "query": title
                .strip_prefix("Searching the web for: \"")?
                .strip_suffix('"')?,
        }),
        ToolKind::Fetch => serde_json::json!({
            "url": title.strip_prefix("Fetching content from: ")?,
        }),
        _ => return None,
    };
    Some(input.to_string())
}

/// Name used when a codex sub-agent's `path` carries no usable segment. Matches
/// the fallback codex-acp itself uses when building the activity title.
const CODEX_SUBAGENT_FALLBACK_NAME: &str = "subagent";

/// How a Codex live `subAgentActivity` (codex-acp #304) should be handled.
///
/// codex 0.147's native team-of-agents runs sub-agents entirely inside the codex
/// process. Its orchestration calls (`spawn_agent` / `wait_agent`, the
/// `collaboration` namespace) never reach the ACP wire, and the inter-agent
/// message is an opaque encrypted envelope even in the on-disk rollout. The one
/// thing codex-acp forwards is `subAgentActivity`, as a `tool_call(kind:other)`
/// carrying `_meta.codex.subagent = {threadId, path, activity}`.
///
/// codeg used to DROP every one of these, on the premise that the
/// `collabAgentToolCall` capsule already showed the same thing. That premise
/// died with the team-of-agents rewrite: codex raises no `collabAgentToolCall`
/// for a spawn any more, so dropping this left a codex sub-agent completely
/// invisible while it ran — nothing appeared in the timeline until the session
/// was reopened and the rollout re-parsed.
#[derive(Debug)]
enum CodexSubagentActivity {
    /// Not a codex sub-agent activity — handle the call normally.
    None,
    /// A sub-agent was launched. Carries the Agent-card input to render it with.
    Started {
        thread_id: Option<String>,
        input: String,
    },
    /// The child reached a terminal state (`completed` / `interrupted`). Carries
    /// no card of its own — its `toolCallId` is a synthetic id codeg has never
    /// seen (`subagent-completed-<uuid>`), so rendering it would open a SECOND
    /// capsule with the same name and no way to tell it from the launch. It is
    /// forwarded onto the LAUNCH capsule instead, keyed by `threadId`.
    Terminal { thread_id: String, kind: String },
    /// A mid-life marker (`interacted`). Still dropped: it carries no content of
    /// its own and says nothing the launch capsule does not already say.
    Other,
}

/// The `_meta` to classify a codex tool call's sub-agent activity from, when the
/// frame's own does not say: codex-acp 2.0.0 sends `_meta.codex.subagent` to no
/// client, and an activity item is then recognised by its `rawInput`
/// (`air_contract::codex_activity_meta_from_raw_input`). `None` means "the
/// frame's own `_meta` is the one to read" — it already carries the legacy
/// marker, or this is not codex, or the input is not an activity.
fn codex_activity_classification_meta(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    raw_input: Option<&serde_json::Value>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::Codex {
        return None;
    }
    if meta
        .and_then(|m| m.get("codex"))
        .and_then(|codex| codex.get("subagent"))
        .is_some()
    {
        return None;
    }
    let derived = crate::acp::air_contract::codex_activity_meta_from_raw_input(raw_input)?;
    let mut merged = meta.cloned().unwrap_or_default();
    if let Some(derived) = derived.as_object() {
        for (key, value) in derived {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(merged)
}

/// The stashed viewed-file result a claude completion should carry, if any
/// (see `CodeBuddyLiveState::claude_viewed_results`).
///
/// The stash entry is spent at the call's terminal status either way; it is
/// only RETURNED for a completion that carried no result of its own — the
/// case claude-agent-acp 0.82.0 produces for an AIR client's read or search.
/// A failed call is not filled: the adapter sends a failure's text itself.
fn claude_viewed_result_for_completion(
    agent_type: AgentType,
    stash: &mut HashMap<String, String>,
    tool_call_id: &str,
    status: Option<&agent_client_protocol::schema::v1::ToolCallStatus>,
    carries_result: bool,
) -> Option<String> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }
    let status = status?;
    let completed = matches!(status, agent_client_protocol::schema::v1::ToolCallStatus::Completed);
    let failed = matches!(status, agent_client_protocol::schema::v1::ToolCallStatus::Failed);
    if !completed && !failed {
        return None;
    }
    let stashed = stash.remove(tool_call_id)?;
    (completed && !carries_result).then_some(stashed)
}

/// Classify a live tool call's `_meta`, building the Agent-card input for a
/// launch. The three fields are the ones `parsers/codex.rs` writes on reload, so
/// live and history render the same capsule: the sub-agent's name is the last
/// segment of its `path` (`/root/pnpm_build` → `pnpm_build`) and `agent_id` is
/// its codex thread id, which the card renders as a short badge. No `prompt` —
/// the task text is encrypted on this wire.
///
/// The capsule settles as soon as codex acknowledges the launch, NOT when the
/// child finishes: the activity item's own lifecycle is the spawn's. codex DOES
/// say so later, with a `completed` / `interrupted` activity of its own — see
/// [`CodexSubagentActivity::Terminal`], which is routed back onto the launch
/// capsule rather than rendered.
fn classify_codex_subagent_activity(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> CodexSubagentActivity {
    if agent_type != AgentType::Codex {
        return CodexSubagentActivity::None;
    }
    let Some(subagent) = meta
        .and_then(|m| m.get("codex"))
        .and_then(|codex| codex.get("subagent"))
    else {
        return CodexSubagentActivity::None;
    };
    let thread_id = subagent
        .get("threadId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    // A status-only follow-up carries the same meta with the same `activity`,
    // so this classification is stable across the call's whole lifetime.
    match subagent.get("activity").and_then(|v| v.as_str()) {
        Some("started") => {}
        // Without a thread id there is no capsule to attribute the outcome to,
        // so it degrades to the old drop rather than opening a stray card.
        Some(kind @ ("completed" | "interrupted")) => {
            return match thread_id {
                Some(thread_id) => CodexSubagentActivity::Terminal {
                    thread_id: thread_id.to_string(),
                    kind: kind.to_string(),
                },
                None => CodexSubagentActivity::Other,
            };
        }
        _ => return CodexSubagentActivity::Other,
    }
    let name = subagent
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(CODEX_SUBAGENT_FALLBACK_NAME);
    let mut input = serde_json::Map::new();
    input.insert(
        "subagent_type".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    if let Some(thread_id) = thread_id {
        input.insert(
            "agent_id".to_string(),
            serde_json::Value::String(thread_id.to_string()),
        );
    }
    // Say that this card stands for the LAUNCH, so its "completed" is not read
    // as "the sub-agent finished" (see the constant's doc).
    input.insert(
        crate::parsers::codex::CODEX_SUBAGENT_LAUNCH_KEY.to_string(),
        serde_json::Value::Bool(true),
    );
    CodexSubagentActivity::Started {
        thread_id: thread_id.map(str::to_string),
        input: serde_json::Value::Object(input).to_string(),
    }
}

/// Re-stamp a launch capsule's input with the child's terminal state.
///
/// The card's whole meaning lives in its `rawInput` — that is where both the
/// live path and the rollout parser put `agent_id` and the launch marker — so
/// the outcome is delivered the same way, by re-sending the input it already
/// has plus one key. Deliberately NOT `_meta`: that field is replace-on-update
/// (`SessionState::upsert_tool_call`), so a meta-only patch would drop whatever
/// codex-acp had put there, whereas `rawInput` is parsed and swapped in whole.
fn codex_subagent_terminal_input(launch_input: &str, kind: &str) -> String {
    let mut obj = serde_json::from_str::<serde_json::Value>(launch_input)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    obj.insert(
        crate::parsers::codex::CODEX_SUBAGENT_STATE_KEY.to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    serde_json::Value::Object(obj).to_string()
}

/// Deliver a codex sub-agent's terminal state onto the capsule that launched it.
///
/// GATED on that capsule still being live, and the gate must be a gate: a child
/// can outlive the turn that spawned it, and once `TurnComplete` has cleared
/// `active_tool_calls` an update naming the launch id would be materialized from
/// nothing by `upsert_tool_call`'s insert-on-miss — a card that exists only in
/// memory, disappears on refresh, and says nothing. Probing with a read lock
/// first would not help: the turn can complete in the window before the write
/// lock is taken, which is why `emit_with_state_gated` evaluates the predicate
/// under the same lock as the apply.
///
/// Nothing is lost when the gate refuses: the rollout carries the same
/// `SubAgentActivity`, and the parser stamps the same key on reload.
async fn settle_codex_subagent_launch(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    cb_state: &CodeBuddyLiveState,
    thread_id: &str,
    kind: &str,
) {
    let Some((launch_id, launch_input)) = cb_state.codex_subagent_launches.get(thread_id) else {
        return;
    };
    let launch_id = launch_id.clone();
    emit_with_state_gated(
        state,
        emitter,
        AcpEvent::ToolCallUpdate {
            tool_call_id: launch_id.clone(),
            title: None,
            status: None,
            content: None,
            raw_input: Some(codex_subagent_terminal_input(launch_input, kind)),
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        },
        |s| s.active_tool_calls.contains_key(&launch_id),
    )
    .await;
}

/// The `_meta` to seed codex's Plan-mode review card with, when a
/// `session/request_permission` is that gate — `None` for every other request.
///
/// codex-acp raises this once a plan item settles while `collaboration_mode` is
/// `plan`, asking whether to implement the plan (`implement_plan`) or stay in
/// plan mode (`revise_plan`). Its `toolCall` (`plan-review:<itemId>`) is NEVER
/// announced as a `tool_call` — the only follow-up on the wire is a
/// `tool_call_update` carrying just a status and `rawOutput`. Without seeding a
/// tool call from this request that update lands on an unknown id and renders as
/// an untitled generic tool card, so `handle_permission_request` emits one.
/// Gated on Codex, mirroring [`classify_codex_subagent_activity`].
///
/// Two ways it is recognised, one per adapter generation. 1.1.8–1.13.x mark the
/// REQUEST (sibling of `options`) `_meta.codex = {kind: "plan_review",
/// planItemId}`, and that meta is what the frontend names the card by
/// (`codexMarksPlanReview`). 2.0.0 sends that marker to no client; the gate is
/// then recognised by its id, which every version shares, and the legacy
/// marker is written onto the seeded card so the frontend's reading holds.
fn codex_plan_review_meta(
    agent_type: AgentType,
    tool_call_id: &str,
    switch_mode: bool,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::Codex {
        return None;
    }
    let marked = meta
        .and_then(|m| m.get("codex"))
        .and_then(|codex| codex.get("kind"))
        .and_then(serde_json::Value::as_str)
        == Some("plan_review");
    if marked {
        return meta.cloned();
    }
    // The id alone is a naming convention; the review request is also the one
    // codex sends as `kind: switch_mode` (`PlanReviewReporter`, every client).
    if !switch_mode {
        return None;
    }
    let plan_item_id = crate::acp::air_contract::codex_plan_review_item_id(tool_call_id)?;
    let mut seeded = meta.cloned().unwrap_or_default();
    seeded.insert(
        "codex".to_string(),
        serde_json::json!({ "kind": "plan_review", "planItemId": plan_item_id }),
    );
    Some(seeded)
}

/// Copy a permission request's REQUEST-level `_meta.permission` onto the card's
/// tool call, where the dialog can reach it.
///
/// `session/request_permission` carries presentation data at two levels: on each
/// option (`PermissionOptionInfo.meta`, already forwarded verbatim) and on the
/// request itself. Only the tool call and the options reach the frontend —
/// `AcpEvent::PermissionRequest` has no request-meta field — so without this the
/// request level is dropped on the floor.
///
/// That became load-bearing in codex-acp 1.7.0, which moved Codex's own reason
/// for asking out of `toolCall.title` (1.4.0 sent
/// `params.reason ?? "Permissions Request"`) into
/// `_meta.permission = {version: 1, title, description?}`. The title is now one
/// of four fixed strings and the reason lives only in `description`, so a card
/// built from the tool call alone would read "Edit files" where it used to
/// explain WHY the edit needs approval.
///
/// claude-agent-acp 0.73.0 made the same move, which is why this must stay
/// UNGATED. There it is not merely load-bearing but the ONLY source of a
/// heading: its rebuilt `permissions/` layer emits exactly one `_meta` in the
/// whole subtree — this request-level block, whose `description` is
/// `Reason: <decisionReason>` — and the permission tool call it pairs with is
/// built from `toolInfoFromToolUse`, which returns `{title, kind, content}` and
/// no `_meta.claudeCode` at all. So the dialog's preferred
/// `_meta.claudeCode.title` is absent on 0.73.0 cards and falls through to what
/// this hoists.
///
/// claude-agent-acp 0.82.0 and codex-acp 2.0.0 moved the record to
/// `_meta.jetbrains.air.permission` and send the old key to no client
/// ([`crate::acp::air_contract::permission_record`] reads both). The card keeps
/// reading `_meta.permission`: this is where the new spelling becomes it.
///
/// Hoisting rather than adding an event field is deliberate: the tool call is
/// already the card's payload end-to-end (`PendingPermissionState.tool_call`,
/// the snapshot, the WebSocket envelope, `parsePermissionToolCall`), so the
/// reason survives a reconnect and a snapshot restore for free. `_meta` is
/// namespaced by producer, and `permission` is unclaimed at tool-call level —
/// codex's permission tool calls carry no `_meta` at all, and claude's carries
/// only `claudeCode` (nothing at all since 0.73.0). An existing
/// `_meta.permission` is therefore never
/// overwritten: the insert is skipped if the key is already present.
fn hoist_request_permission_meta(
    tool_call: &mut serde_json::Value,
    request_meta: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    let Some(permission) = crate::acp::air_contract::permission_record(request_meta) else {
        return;
    };
    let Some(obj) = tool_call.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("_meta")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(meta) = meta.as_object_mut() else {
        // A non-object `_meta` is malformed; leave it exactly as the agent sent
        // it rather than replacing content the card may still be parsing.
        return;
    };
    meta.entry("permission").or_insert_with(|| permission.clone());
}

/// True when an `initialize` response advertises the ACP steering extension —
/// the TOP-LEVEL `_meta.steering.supported` flag, a sibling of
/// `agentCapabilities` (NOT `agentCapabilities._meta`, which belongs to other
/// conventions such as the ACP runtime's symposium capability ext). Both claude-agent-acp
/// (0.61+) and codex-acp (1.1.6+) advertise here; whether codeg actually
/// steers natively additionally requires the
/// `registry::steering_prompt_required_min_version` policy plus the runtime
/// version proof (see the synthesis in `run_connection`).
fn init_advertises_steering(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> bool {
    meta.and_then(|m| m.get("steering"))
        .and_then(|s| s.get("supported"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Whether the `initialize` response advertises the provider-neutral goal
/// extension: top-level `_meta.goal = {version: <integer >= 1>, controlMethod,
/// actions}` — or, from claude-agent-acp 0.82.0 / codex-acp 2.0.0, the same
/// object at `_meta.jetbrains.air.goal` (the AIR-only home those releases moved
/// it to; the top-level key then goes to no client). Advertised ⇒ goal state
/// arrives as `session_info_update._meta.goal` (or
/// `_meta.jetbrains.air.goal`) snapshots and the legacy
/// `_meta.codex.goal` key is ignored for the WHOLE connection — codex-acp
/// switched to the neutral key silently in 1.2.0 (legacy key gone), and
/// claude-agent-acp speaks only the neutral form (0.66.0+). Pinning the
/// channel here, at initialize, makes the selection independent of update
/// arrival order, so a transitional adapter double-publishing one goal
/// transition through both namespaces can never produce two goal cards.
/// Non-integer or sub-1 versions fail closed onto the legacy channel.
fn init_advertises_goal(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> bool {
    crate::acp::air_contract::legacy_or_air(meta, "goal")
        .and_then(|g| g.get("version"))
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|version| version >= 1)
}

/// The goal-control surface an `initialize` response advertises:
/// `(_meta.goal.controlMethod, _meta.goal.actions)` — claude 0.66+ offers
/// `("_session/goal", ["set","clear"])`, codex 1.2+ the same method with all
/// four actions. `None` when the neutral goal extension isn't advertised
/// (see [`init_advertises_goal`]) or carries no usable method string — the
/// session then keeps the legacy codex method + actions
/// (`codex_goal::LEGACY_GOAL_CONTROL_METHOD` / `LEGACY_GOAL_ACTIONS`). An
/// advertised-but-empty actions array is honored as "no controls": the goal
/// card gates its buttons on this list, so only affordances the adapter
/// actually implements are offered.
fn goal_advertised_control(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<(String, Vec<String>)> {
    if !init_advertises_goal(meta) {
        return None;
    }
    let goal = crate::acp::air_contract::legacy_or_air(meta, "goal")?;
    let method = goal
        .get("controlMethod")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())?
        .to_string();
    let actions = goal
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some((method, actions))
}

/// Resolve [`goal_advertised_control`]'s answer into what the session state
/// stores: an OPTIONAL method override (absent ⇒ keep the legacy codex method
/// the state was built with) and the CONCRETE action vocabulary.
///
/// The legacy pair belongs here, at initialize, and not in `SessionState::new`.
/// A client can read the snapshot during the handshake — `spawn_agent` returns
/// with `initialize` still in flight — and it latches whatever it finds, so a
/// construction-time legacy default hands a claude session a Pause its adapter
/// answers with `Invalid params: goal action must be "set" or "clear"`. Keeping
/// the field `None` until this runs is what makes "unknown" tellable from
/// "legacy" on the wire.
fn resolve_goal_control(
    advertised: Option<(String, Vec<String>)>,
) -> (Option<String>, Vec<String>) {
    match advertised {
        Some((method, actions)) => (Some(method), actions),
        None => (
            None,
            crate::acp::codex_goal::LEGACY_GOAL_ACTIONS
                .iter()
                .map(|a| (*a).to_string())
                .collect(),
        ),
    }
}

/// Pick the goal payload out of a `session_info_update`'s `_meta` according to
/// the channel pinned at initialize (see [`init_advertises_goal`]): the
/// neutral `_meta.goal` (or its AIR home `_meta.jetbrains.air.goal`) for
/// advertising connections, the legacy `_meta.codex.goal` otherwise — never
/// both. Pure so the either/or contract
/// is unit-tested without the connection machinery.
fn session_info_goal_value(
    neutral_goal_channel: bool,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<&serde_json::Value> {
    if neutral_goal_channel {
        crate::acp::air_contract::legacy_or_air(meta, "goal")
    } else {
        meta?.get("codex").and_then(|codex| codex.get("goal"))
    }
}

/// The raw `sessionFailure` value out of a `_meta.jetbrains.air` envelope —
/// present only when the envelope itself is well-formed (integer
/// `version >= 1`, mirroring the advertisement check the adapters run on
/// codeg's `clientCapabilities._meta.jetbrains.air`). A malformed or
/// future-incompatible envelope yields `None` and the carrier is treated as
/// holding no failure. Records ride TWO carriers with this same envelope: the
/// per-attempt upserts on `session_info_update._meta`, and a turn's terminal
/// failure on the prompt RESPONSE `_meta` (see [`response_session_failure`]).
fn air_session_failure(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<&serde_json::Value> {
    let air = meta?.get("jetbrains")?.get("air")?;
    let version = air.get("version").and_then(serde_json::Value::as_i64)?;
    if version < 1 {
        return None;
    }
    air.get("sessionFailure")
}

/// Validate one AIR failure upsert into a [`SessionFailureRecord`].
///
/// `id` (non-blank string) and `revision` (integer >= 1) are HARD
/// requirements — without identity there is nothing to merge
/// deterministically, so a record missing either is dropped (the caller logs
/// it at debug). Everything else is lenient: `category`/`severity` default to
/// `"unknown"`/`"error"` and pass through unrecognized values as plain
/// strings (the frontend falls back per field), `title` may be blank,
/// non-string `actions` entries are skipped. `resolved` starts `false`; the
/// stores flip it (see the type docs).
fn parse_session_failure_record(value: &serde_json::Value) -> Option<SessionFailureRecord> {
    let id = value.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let revision = value.get("revision")?.as_u64()?;
    if revision < 1 {
        return None;
    }
    let text = |key: &str, default: &str| -> String {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    };
    Some(SessionFailureRecord {
        id: id.to_string(),
        revision,
        category: text("category", "unknown"),
        severity: text("severity", "error"),
        title: text("title", ""),
        details: value
            .get("details")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        actions: value
            .get("actions")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        resolved: false,
    })
}

/// The terminal AIR failure riding on a prompt RESPONSE's `_meta`, if any.
///
/// With `jetbrains.air` negotiated, BOTH adapters deliver a turn's terminal
/// failure ON the prompt response rather than as another
/// `session_info_update`: claude-agent-acp's `failActiveWithSessionFailure`
/// settles the turn with a disguised `end_turn` stop reason and attaches the
/// record here (its own comment calls the response "the canonical AIR
/// carrier" — the update channel only ever carries the per-attempt retry
/// warnings), and codex-acp's `terminalFailurePromptResponse` mirrors the
/// same shape as the catch-up for a record whose update was missed (the
/// strict revision merge de-duplicates when both arrive). Field report
/// 2026-08-15: a mid-turn network drop on claude 0.68.0 published
/// "Reconnecting to Claude, attempt N of 5" warnings via updates, then the
/// `transport_lost` error escalation ONLY here — ignoring this carrier lost
/// the terminal record entirely, so the turn-boundary settle painted the
/// still-dead connection as a recovered warning.
fn response_session_failure(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<SessionFailureRecord> {
    let raw = air_session_failure(meta)?;
    let record = parse_session_failure_record(raw);
    if record.is_none() {
        tracing::debug!(
            "[ACP] dropped prompt-response AIR sessionFailure without usable id/revision: {raw:?}"
        );
    }
    record
}

/// Strict SemVer floor check: true when `version >= min` by SemVer
/// PRECEDENCE. Prerelease ordering matters here — `0.64.0-rc1` precedes
/// `0.64.0` and may predate the very commit that shipped the
/// `promptRequired` guarantee, so it must NOT satisfy a `0.64.0` floor
/// (`0.64.1-beta.2` still does: its numeric core is above the floor). Build
/// metadata (`+sha`) is precedence-ignored per spec. Fail closed: anything
/// `semver` can't parse (missing segments, `v` prefixes, garbage suffixes)
/// routes live feedback to the MCP pull path — today's behavior.
fn version_at_least(version: &str, min: &str) -> bool {
    let (Ok(actual), Ok(floor)) = (
        semver::Version::parse(version.trim()),
        semver::Version::parse(min),
    ) else {
        return false;
    };
    actual.cmp_precedence(&floor) != std::cmp::Ordering::Less
}

/// Runtime half of the native-steering gate: does the adapter binary that is
/// ACTUALLY running — which launch may have resolved from PATH rather than
/// the pinned npx package (see `commands::acp::acp_get_agent_status_core`) —
/// report an `agent_info.version` at or above the registry minimum? Fail
/// closed on a missing `agent_info` or an unparseable version.
fn steering_version_ok(agent_info: Option<&agent_client_protocol::schema::v1::Implementation>, min: &str) -> bool {
    agent_info.is_some_and(|info| version_at_least(&info.version, min))
}

/// Synthesize `SessionState.native_steering_available` from an `initialize`
/// response: extension advertised (top-level `_meta`) AND registry policy says
/// this agent honors `promptRequired` AND the running binary's
/// `agent_info.version` proves it. Pure so the full gate matrix is unit-tested;
/// `run_connection` calls it once and everything downstream reads the stored
/// bool.
fn synthesize_native_steering(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    agent_info: Option<&agent_client_protocol::schema::v1::Implementation>,
) -> bool {
    init_advertises_steering(meta)
        && registry::steering_prompt_required_min_version(steering_policy_agent(
            agent_type, agent_info,
        ))
        .is_some_and(|min| steering_version_ok(agent_info, min))
}

/// Whose steering policy a connection follows. A custom agent whose running
/// adapter reports itself as claude-agent-acp (a second Claude Code account
/// registered as a custom npx agent, say) speaks exactly the protocol the
/// built-in Claude Code policy was written for, so it follows that policy; the
/// advertisement and the version floor still gate it as they gate the built-in
/// agent. The name must match exactly — a fork or a look-alike has no policy
/// and keeps the pull channel — and a built-in type is never remapped.
///
/// Only the steering channel changes for such an agent: its history is still
/// the transcript codeg records, which is why an injected steer is recorded
/// there (see the `Steer` arm).
fn steering_policy_agent(
    agent_type: AgentType,
    agent_info: Option<&agent_client_protocol::schema::v1::Implementation>,
) -> AgentType {
    match agent_type {
        AgentType::Custom(_)
            if agent_info.is_some_and(|info| info.name == registry::CLAUDE_AGENT_ACP_PACKAGE) =>
        {
            AgentType::ClaudeCode
        }
        other => other,
    }
}

/// codex-acp 1.12.0 swapped the question and the short tab header between a
/// `request_user_input` property's `title` and `description`. Both generations
/// emit both strings, so the FORM cannot be dated — but the adapter that built
/// it can, from the `agentInfo.version` it reports at `initialize`.
///
/// Pinned once into `SessionState::codex_user_input_shape` because the running
/// adapter is not necessarily the pinned one: launch prefers a PATH-resolved
/// install, and a custom pinned version is a supported configuration
/// (`registry::supports_custom_version`). `None` for every other agent, and for
/// a codex adapter that reports no parseable version — the elicitation parser
/// then falls back to the form's own markers (see
/// [`crate::acp::question::CodexUserInputShape`]).
fn codex_user_input_shape(
    agent_type: AgentType,
    agent_info: Option<&agent_client_protocol::schema::v1::Implementation>,
) -> Option<crate::acp::question::CodexUserInputShape> {
    use crate::acp::question::CodexUserInputShape;
    if agent_type != AgentType::Codex {
        return None;
    }
    let version = agent_info.map(|info| info.version.trim())?;
    // Fail closed on an unparseable version rather than guessing a generation:
    // `None` lets the parser use the form's markers, which is strictly better
    // information than a coin flip.
    if semver::Version::parse(version).is_err() {
        return None;
    }
    Some(if version_at_least(version, "1.12.0") {
        CodexUserInputShape::QuestionInTitle
    } else {
        CodexUserInputShape::QuestionInDescription
    })
}

/// Extract a retryable-turn-error indicator from a Codex `session_info_update`'s
/// `_meta` (codex-acp #289, v1.1.3+). codex ships a transient, auto-retried
/// error as `_meta.codex.error = {message, codexErrorInfo, additionalDetails,
/// turnId, willRetry}` and keeps the prompt alive; it emits this only when
/// `willRetry == true`. Returns `(message, http_status)` when a non-empty
/// message is present. `codexErrorInfo` may be a bare string enum, an object
/// variant carrying an inner `httpStatusCode`, or absent — only the object form
/// yields a status. Defensively refuses a `willRetry == false` payload so a
/// terminal error can never render as "retrying".
fn codex_retry_indicator(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<(String, Option<i64>)> {
    let err = meta?.get("codex")?.get("error")?;
    if err.get("willRetry").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    let message = err.get("message").and_then(|v| v.as_str())?.trim();
    if message.is_empty() {
        return None;
    }
    let http_status = err
        .get("codexErrorInfo")
        .and_then(|info| info.as_object())
        .and_then(|obj| obj.values().next())
        .and_then(|inner| inner.get("httpStatusCode"))
        .and_then(|v| v.as_i64());
    Some((message.to_string(), http_status))
}

/// True when an available command is really a config-option state toggle rather
/// than an invokable slash command (codex-acp #293, v1.1.4). codex advertises
/// e.g. `/plan` as an `AvailableCommand` tagged
/// `_meta.commandAction = {kind:"setConfigOption", configId:"collaboration_mode",
/// value:"plan", resetValue:"default", presentation:"state"}` — codex's signal
/// that the client should represent it as STATE. codeg already surfaces that
/// state as the `collaboration_mode` config-option selector (the generic
/// `SessionConfigOption` path), so also listing `/plan` as a slash command is
/// redundant and its static "Turn plan mode on" description is wrong once plan
/// mode is already on. Suppress these from the command list. Commands with any
/// other action kind (e.g. `/goal`'s `prefixPrompt`, which takes an objective
/// argument) are real commands and kept. Gated on Codex — `commandAction` is a
/// codex-private `_meta` extension (the ACP schema has no such type).
fn is_config_option_state_command(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::Codex {
        return false;
    }
    // codex-acp 2.0.0 moved the action to `_meta.jetbrains.air.commandAction`
    // (an AIR-only key; the top-level one then goes to no client).
    crate::acp::air_contract::legacy_or_air(meta, "commandAction")
        .and_then(|action| action.get("kind"))
        .and_then(|kind| kind.as_str())
        == Some("setConfigOption")
}

/// True when a CodeBuddy sub-agent tool call's `_meta` marks it as a BACKGROUND
/// sub-agent (`codebuddy.ai/isBackground == true`). A background sub-agent runs
/// concurrently with the main agent, so the suppression-window invariant (parent
/// blocked → only sub-agent chunks in the window) does NOT hold for it — see
/// `track_subagent_window`, which excludes it from the window. Gated on CodeBuddy.
fn codebuddy_meta_marks_background(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    meta.and_then(|m| m.get("codebuddy.ai/isBackground"))
        .and_then(|v| v.as_bool())
        == Some(true)
}

/// True when a CodeBuddy thought/message `ContentChunk`'s own `_meta` marks the
/// chunk as sub-agent output (`codebuddy.ai/isSubagent`, or a
/// `codebuddy.ai/parentToolCallId` link to the Agent call). This is a precision
/// supplement to the open-sub-agent window — CodeBuddy is not confirmed to
/// populate chunk `_meta`, so suppression never relies on it alone. Gated on
/// CodeBuddy.
fn codebuddy_chunk_marks_subagent(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    let Some(meta) = meta else {
        return false;
    };
    if meta.get("codebuddy.ai/isSubagent").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    meta.get("codebuddy.ai/parentToolCallId")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// Whether a live thought/message chunk should be dropped from the top-level
/// stream because it belongs to a CodeBuddy sub-agent (whose work is already
/// represented by the Agent pill + its nested tool calls).
///
/// Claude Code is NOT handled here: its sub-agent chunks (claude-agent-acp
/// ≥0.63 with the `subagent-transcript` capability) arrive with a precise
/// per-chunk `_meta.claudeCode.parentToolUseId` and are forwarded WITH that
/// attribution instead of suppressed — see `claude_chunk_parent_tool_use_id`.
///
/// Suppress while we're inside an open sub-agent window OR when the chunk's own
/// meta marks it. The window safety rests on a structural invariant: the window
/// only ever holds FOREGROUND (blocking) sub-agents — a synchronous `Agent` tool
/// call suspends the parent model until the tool returns its result, so between
/// that call's open frame and its terminal frame the main session carries ONLY
/// the sub-agent's chunks, never main-agent output. BACKGROUND sub-agents (which
/// run concurrently and could interleave main-agent output) are deliberately
/// excluded from the window by `track_subagent_window`, so `window_open` can
/// never cause a main-agent chunk to be dropped. Gated on CodeBuddy; every other
/// agent always emits.
fn should_suppress_subagent_chunk(
    agent_type: AgentType,
    window_open: bool,
    chunk_meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    window_open || codebuddy_chunk_marks_subagent(agent_type, chunk_meta)
}

/// Extract the update-level `_meta.claudeCode.parentToolUseId` of a live
/// text/thought chunk — set by claude-agent-acp ≥0.63 on a subagent's
/// forwarded chunks once the client advertises the `subagent-transcript`
/// capability (see `build_client_capabilities`). The chunk is emitted WITH
/// this attribution (never suppressed): the frontend routes parented chunks
/// into the live Agent capsule instead of the main thread. Gated on
/// ClaudeCode so no other agent's namespaced meta can alias into parented
/// routing; empty strings are treated as absent.
fn claude_chunk_parent_tool_use_id(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }
    meta?
        .get("claudeCode")?
        .get("parentToolUseId")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `json_value_to_text` for a tool call's `rawInput`, with Claude Code's
/// file-tool argument aliases settled first, and Antigravity's MCP input
/// folded back into its history shape ([`fold_antigravity_mcp_raw_input`]).
///
/// claude-agent-acp forwards the model's `tool_use.input` verbatim as
/// `rawInput`, so a Write the model spelled `{path, file_text}` — which CLI
/// 2.1.280 accepts and renames for itself — reaches the card, the live line
/// stats and the file tally without the `file_path` / `content` every one of
/// them reads. 0.81.1 (#1161) fixed only the adapter's own title and diff for
/// it. This settles the input itself, with the renames the history parser
/// applies to the same calls (`parsers::claude::canonical_file_tool_input`),
/// so a live card and its history twin read the same arguments.
///
/// The tool is named by `_meta.claudeCode.toolName`, which rode every claude
/// tool frame that carries input through 0.81.x — the opening `tool_call`, the
/// refining `tool_call_update` and the streamed-input refinements alike (the
/// top-level ACP `name` is only on the first). From 0.82.0 an AIR client gets it
/// on the opening frame only, and the `_meta` ledger
/// (`crate::acp::air_contract::ToolCallMetaLedger`) hands it to every later one.
/// A permission request needs nothing: the CLI coerces the input before it
/// asks, so it already carries the canonical names.
fn tool_call_raw_input_text(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    raw_input: &Option<serde_json::Value>,
) -> Option<String> {
    if agent_type == AgentType::ClaudeCode {
        let tool_name = meta
            .and_then(|m| m.get("claudeCode"))
            .and_then(|c| c.get("toolName"))
            .and_then(serde_json::Value::as_str);
        let canonical = tool_name.zip(raw_input.as_ref()).and_then(|(name, input)| {
            crate::parsers::claude::canonical_file_tool_input(name, input)
        });
        if canonical.is_some() {
            return json_value_to_text(&canonical);
        }
    }
    if agent_type == AgentType::Antigravity {
        let folded = fold_antigravity_mcp_raw_input(meta, raw_input.as_ref());
        if folded.is_some() {
            return json_value_to_text(&folded);
        }
    }
    json_value_to_text(raw_input)
}

/// Antigravity 1.2's live MCP `rawInput`, folded back into the shape its
/// history takes.
///
/// The server rewrites every MCP dispatch before a client sees it
/// (`tools.py::unwrap_mcp_tool_call`). Through 1.1 that produced
/// `{"arguments": {…}, "prompt": "<sentence>"}`, and `parsers::antigravity`
/// emits exactly that for history. 1.2 ALSO copies each argument onto the top
/// level — for clients that read `rawInput` without knowing about MCP — and
/// moves the sentence into `_meta.prompt`. The dedicated cards peel
/// `arguments` either way, but the generic card lists every top-level key, so
/// each argument would render twice, once flat and once in the `arguments`
/// tree, and the live card would stop matching its history twin. History
/// cannot take the new shape instead: the copy doubles every input against the
/// parser's `TOOL_INPUT_CAP`, and an input cut there is one no card can parse.
///
/// Folds only an EXACT copy — every key beside `arguments` equal to the same
/// key inside it, none missing — on a call whose `_meta` marks it as MCP.
/// Anything else passes through: a 1.1 input is already in this shape, and a
/// tool whose own argument is named `arguments` overwrites the wrapper
/// upstream, leaving no nested copy to fold into.
fn fold_antigravity_mcp_raw_input(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    raw_input: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let meta = meta?;
    if meta.get("is_mcp_tool_call") != Some(&serde_json::Value::Bool(true)) {
        return None;
    }
    let input = raw_input?.as_object()?;
    let arguments = input.get("arguments")?.as_object()?;
    let flattened_copy = input.len() - 1 == arguments.len()
        && arguments
            .iter()
            .all(|(key, value)| input.get(key) == Some(value));
    if !flattened_copy {
        return None;
    }
    let mut folded = serde_json::Map::new();
    folded.insert(
        "arguments".to_string(),
        serde_json::Value::Object(arguments.clone()),
    );
    let prompt = meta
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty());
    if let Some(prompt) = prompt {
        folded.insert(
            "prompt".to_string(),
            serde_json::Value::String(prompt.to_string()),
        );
    }
    Some(serde_json::Value::Object(folded))
}

/// Maintain the set of OPEN CodeBuddy sub-agent tool calls (`open`). `is_agent`
/// is true once `resolve_rewritten_title` classified this `tool_call_id` as a
/// native sub-agent (`"agent"`). A non-final status opens the window; a final
/// status (`completed` / `failed`) closes it and records the id in `closed`, so a
/// stray late non-final frame can't re-open an already-finished sub-agent.
///
/// `is_background` (from `codebuddy_meta_marks_background`) EXCLUDES a sub-agent
/// from the window: a background sub-agent runs concurrently with the main agent,
/// so the "window holds only sub-agent chunks" invariant that makes
/// `should_suppress_subagent_chunk` safe would not hold. We treat a background
/// marker exactly like a terminal frame (remove + record closed) so it can never
/// suppress interleaved main-agent output. (`isBackground` can stream in a frame
/// or two after the call opens, so a background sub-agent's earliest chunks may be
/// briefly suppressed before the marker arrives — an accepted, rare imperfection;
/// the user-reported case is foreground, where the marker is `false`.)
///
/// Gated on CodeBuddy so a single-agent-type connection of any other agent stays
/// inert.
fn track_subagent_window(
    agent_type: AgentType,
    is_agent: bool,
    is_background: bool,
    status: Option<&str>,
    tool_call_id: &str,
    open: &mut HashSet<String>,
    closed: &mut HashSet<String>,
) {
    if agent_type != AgentType::CodeBuddy || !is_agent {
        return;
    }
    let is_final = matches!(status, Some("completed") | Some("failed"));
    if is_final || is_background {
        open.remove(tool_call_id);
        closed.insert(tool_call_id.to_string());
    } else if !closed.contains(tool_call_id) {
        open.insert(tool_call_id.to_string());
    }
}

/// Per-session CodeBuddy live-stream state threaded through
/// `emit_conversation_update`. Consolidates the authoritative title rewrites and
/// the open-sub-agent suppression window so CodeBuddy's sparse, multi-frame
/// sub-agent stream resolves to a stable Agent pill (whose children nest) with
/// its interleaved thought/message chunks suppressed. Created per connection,
/// shared across the idle and active-turn loops; the historical-replay path uses
/// a throwaway instance. Mirrors `ToolCallOutputCache`'s lifetime.
#[derive(Default)]
struct CodeBuddyLiveState {
    /// tool_call_id → authoritative title: `"agent"` for a native sub-agent, or
    /// the inner `mcp__…` name for a `DeferExecuteTool` MCP call. Re-asserted on
    /// every later frame so a status-only update can't downgrade the card.
    title_overrides: HashMap<String, String>,
    /// Sub-agent tool calls currently OPEN (classified, not yet completed/failed).
    /// While non-empty, interleaved thought/message chunks belong to a sub-agent
    /// and are suppressed from the top-level stream (matching Claude Code).
    open_subagents: HashSet<String>,
    /// Sub-agent tool calls that already reached a final status — guards against a
    /// stray late non-final frame re-opening a finished sub-agent.
    closed_subagents: HashSet<String>,
    /// Objective of the Codex `/goal` run currently open on this connection (set
    /// by the latest `active` `session_info_update` goal, cleared on any terminal
    /// status). Lets a later `goal:null` close the run by objective — and be a
    /// no-op when no run is open. See `crate::acp::codex_goal::next_goal_marker`.
    ///
    /// This lives here (not in `SessionState`) because `CodeBuddyLiveState` and
    /// `SessionState` share one lifetime: a browser refresh / reconnect re-attaches
    /// to the *running* connection (`find_connection_for_reuse`), keeping both; a
    /// brand-new connection resets both together (empty live blocks + fresh state).
    /// So this state never resets while goal blocks it would close still exist.
    codex_open_goal: Option<String>,
    /// Monotonic per-connection counter for synthetic goal tool-call ids. Occurrence
    /// (not content) addressing keeps two runs that share an objective from
    /// colliding in the reducer's id-keyed live block list.
    codex_goal_seq: u64,
    /// Cursor tool calls announced with the identity-less "MCP: tool" title.
    /// Only these are eligible for the completion-time result sniff
    /// (`cursor_companion_title_from_content`) — a `shell`/`read` call whose
    /// OUTPUT merely echoes a delegation ack must never be re-titled. Entries
    /// are dropped once the call reaches a terminal status (the sniff, if any,
    /// has recorded its override by then), so the set tracks only in-flight
    /// calls.
    cursor_generic_mcp_ids: HashSet<String>,
    /// Grok tool_call ids whose interactive question already renders via the
    /// `_x.ai/ask_user_question` ext bridge (`handle_grok_ask_user_question`). The
    /// redundant native `tool_call` / `tool_call_update` stream for these is
    /// dropped so the card doesn't double-render; tracked by id because a later
    /// status-only update may drop the `x.ai/tool` meta that first identified it.
    grok_ask_tool_ids: HashSet<String>,
    /// Codex sub-agent thread id → the launch capsule that spawned it, paired
    /// with the `rawInput` that capsule was announced with.
    ///
    /// codex reports a child's completion as a `subAgentActivity` of its own,
    /// under a synthetic tool-call id (`subagent-completed-<uuid>`) that shares
    /// nothing with the launch except the thread id — so this is the only way
    /// back to the card that should carry the outcome. Keeping the input too
    /// avoids reconstructing it: the outcome is delivered by re-sending it with
    /// one key added (`codex_subagent_terminal_input`).
    ///
    /// NOT cleared per turn, for the same reason the grok sub-agent map is not:
    /// a child legitimately outlives the turn that launched it.
    codex_subagent_launches: HashMap<String, (String, String)>,
    /// Grok `spawn_subagent` tool_call ids ever announced on this connection
    /// (dedupe for the pending queue + status tracking on meta-less updates).
    grok_spawn_seen: HashSet<String>,
    /// Announced spawn calls not yet paired with a `subagent_spawned`
    /// notification. Grok's ext notifications carry a `subagent_id` but no
    /// `tool_call_id`, so pairing is by matching `(description, subagent_type)`
    /// captured from the launch `rawInput` against the notification's own
    /// fields, first match in stream order — FIFO in the common in-order case
    /// (mirroring the history parser's pairing), while a DELAYED prior-turn
    /// `subagent_spawned` fails the match against a new turn's differently-
    /// described entry instead of stealing its slot. A spawn that FAILS before
    /// pairing is removed so later pairs can't shift; the queue is cleared at
    /// every turn start (staleness bound).
    grok_pending_spawn_ids: VecDeque<GrokPendingSpawn>,
    /// subagent_id → its launching spawn tool_call id, from `subagent_spawned`.
    /// Routes `subagent_progress` ticks (live meta on the Agent card) and the
    /// `subagent_finished` settle; the entry is dropped at finish.
    grok_subagent_to_call: HashMap<String, String>,
    /// spawn tool_call id → the child's OWN session id (`child_session_id`, or
    /// the subagent id it always equals in practice). Grok runs each sub-agent
    /// as a standalone session that writes its transcript to disk while it
    /// works, so this is what lets the live Agent card offer "open the child's
    /// session" — the only way to watch a blocking child, whose launch call
    /// carries no output until it finishes. Kept in state (not just emitted
    /// once) because `upsert_tool_call` REPLACES a block's meta: every later
    /// progress tick must re-send it or the id would be dropped. Entry dropped
    /// at finish, alongside `grok_subagent_to_call`.
    grok_call_child_session: HashMap<String, String>,
    /// Spawn calls that reached a terminal wire status. A `subagent_finished`
    /// for one of these is a BACKGROUND child settling after its launch ack —
    /// the case whose result would otherwise never reach the card live; a
    /// blocking spawn's own completion frame carries the output instead.
    grok_settled_spawn_ids: HashSet<String>,
    /// Spawn calls announced in the CURRENT turn — the only ones whose
    /// `subagent_progress` ticks may emit a live `ToolCallUpdate`. Cleared at
    /// every turn start (with the pending queue): a tick for a PRIOR turn's
    /// background child would otherwise pass the Prompting gate during a later
    /// turn and get appended into that turn's live message as a ghost card
    /// (its real card lives in the promoted history). The settle path
    /// (`subagent_finished` → BackgroundActivity) is unaffected — it targets
    /// promoted turns by design.
    grok_progress_eligible: HashSet<String>,
    /// Context window of the model this Grok turn runs on, resolved ONCE at turn
    /// start (`grok_current_model_context_window`) so the per-update usage peek
    /// never takes the state lock on the streaming hot path. `None` for non-Grok
    /// agents, and for a Grok model whose spec carried no `totalContextTokens`
    /// — the ring then keeps falling back to the parsed per-turn stats.
    grok_turn_context_window: Option<u64>,
    /// Last `(used, size)` emitted as a live `UsageUpdate` on this connection.
    /// Grok repeats its cumulative `totalTokens` on nearly every update (169 of
    /// 189 in a real capture), so re-emitting each one would put a broadcast on
    /// a per-chunk hot path for a number that changes a handful of times a turn.
    /// The WINDOW is part of the key so a between-turn model switch re-emits
    /// even when the token count hasn't moved yet — otherwise the ring would
    /// keep dividing by the previous model's window.
    grok_last_usage: Option<(u64, u64)>,
    /// The prompt id of the turn Grok is running on its own that the idle loop
    /// is rendering (see [`grok_agent_turn_opener`]); `None` when none is open.
    /// Paired with `SessionState::agent_initiated_turn`, which keeps that
    /// turn's end off the prompt gate.
    grok_agent_turn: Option<String>,
    /// The prompt id of every Grok turn this session has seen END — ones codeg
    /// started (read off their frames and responses), and agent-initiated ones
    /// it closed. Frames Grok still delivers for them afterwards must neither
    /// open a turn nor end one. Every one, not the latest: a prompt that
    /// answers at once (a slash command) can end while an earlier turn's
    /// frames are still queued, and nothing marks when those have drained. One
    /// short id per turn is nothing to keep for a session's life.
    grok_ended_prompt_ids: HashSet<String>,
    /// Which of codex's `usage_update`s read the context window: the one for a
    /// request that ran a hosted web search covers the provider's whole search
    /// loop, not the context (see `crate::acp::codex_context`).
    codex_context_readings: crate::acp::codex_context::ContextReadings,
    /// Shell tool calls whose terminal the AGENT hosts itself (pi bash,
    /// codex command execution) → whether any output has already been emitted
    /// for that call.
    ///
    /// Registered from the opening frame's `_meta.terminal_info`
    /// (see `hosted_terminal_meta_marks_shell`), because the frames that actually
    /// CARRY the output name only the tool-call id — `terminal_info` never
    /// repeats. The flag is what makes the first bridged chunk a replacement and
    /// every later one an append, the same rule
    /// `TrackedTerminalToolCall::has_emitted_output` applies on the host-terminal
    /// path. Entries are dropped at a final status, alongside
    /// `ToolCallOutputCache::remove_if_final`, and the whole map is cleared at
    /// turn start — a shell call whose turn was canceled never sees a final
    /// status, and its lifecycle cannot span turns anyway.
    hosted_terminal_calls: HashMap<String, bool>,
    /// The merged `_meta` of every claude / codex tool call on this connection
    /// — the AIR contract (claude-agent-acp 0.82.0, codex-acp 2.0.0) sends each
    /// `_meta` key once and expects the client to merge. See
    /// [`crate::acp::air_contract`].
    air_tool_call_meta: crate::acp::air_contract::ToolCallMetaLedger,
    /// codex `subAgentActivity` tool calls classified on their opening frame:
    /// `Some(input)` for a launch capsule (the Agent-card input it was announced
    /// with), `None` for an activity that is dropped. codex-acp 2.0.0 sends the
    /// follow-up of such a call as a bare status — no `rawInput`, no marker — so
    /// the id is the only thing left to classify it by (1.4.0–1.13.x repeated
    /// `_meta.codex.subagent` on every frame). Kept for the connection, like
    /// `codex_subagent_launches`: a child outlives its turn.
    codex_activity_calls: HashMap<String, Option<String>>,
    /// claude `Read` / `Grep` / `Glob` results taken off the raw SDK stream
    /// (`_claude/sdkMessage`, a `user` message's `tool_result`), keyed by the
    /// tool-use id, waiting for the call's completion frame.
    ///
    /// claude-agent-acp 0.82.0 withholds the text of those results from an AIR
    /// client ("AIR shows a read or a search that names a path as the list of
    /// viewed files … the text of the result stays out"), so the completion
    /// arrives as a bare status and the card would show the path and nothing
    /// else — while the same call reloaded from the transcript shows the text.
    /// codeg already subscribes to the raw SDK stream (`emitRawSDKMessages`),
    /// and the adapter forwards each raw message BEFORE it reports it, so the
    /// result is here first. It only ever fills a completion that carries no
    /// result of its own (see the `ToolCallUpdate` arm), and the map is cleared
    /// at turn start like `hosted_terminal_calls`.
    claude_viewed_results: HashMap<String, String>,
    /// The plugin-failure list last forwarded as
    /// [`AcpEvent::PluginLoadFailures`] (see `take_new_claude_plugin_failures`).
    /// Claude Code repeats `plugin_errors` on every turn's `system/init`, so
    /// without this each turn would re-raise the same warning. Kept across
    /// turns — only a CHANGED list is news — and cleared by an init that lists
    /// none, so a failure that went away and came back is reported again.
    claude_reported_plugin_failures: Option<Vec<PluginLoadFailure>>,
}

/// One announced-but-unpaired Grok `spawn_subagent` call. `description` /
/// `subagent_type` come from the launch `rawInput` and validate the pairing
/// against the `subagent_spawned` notification's own fields (`None` = wildcard,
/// tolerant of older wire shapes).
#[derive(Debug)]
struct GrokPendingSpawn {
    call_id: String,
    description: Option<String>,
    subagent_type: Option<String>,
}

/// True when a Grok tool call's ACP `_meta` identifies it as the native
/// `spawn_subagent` launcher (`_meta["x.ai/tool"].name`). Present on the very
/// first frame (verified against real captures), unlike `subagent_type` which
/// rides `rawInput`. Gated on Grok so the namespaced key can't affect others.
fn grok_meta_marks_spawn_subagent(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    matches!(agent_type, AgentType::Grok)
        && meta
            .and_then(|m| m.get("x.ai/tool"))
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            == Some("spawn_subagent")
}

/// Track a Grok `spawn_subagent` call's lifecycle for the subagent-notification
/// pairing (see the `grok_*` fields on [`CodeBuddyLiveState`]). `is_spawn` is
/// the precomputed meta marker; a status-only update that lost the meta still
/// tracks via the seen-set. `raw_input` (the launch input, when this frame
/// carries it) supplies the `(description, subagent_type)` the pairing
/// validates against. No-op for other agents/tools by construction
/// (`is_spawn` false + id never seen).
fn track_grok_spawn_call(
    cb_state: &mut CodeBuddyLiveState,
    is_spawn: bool,
    status: Option<&str>,
    tool_call_id: &str,
    raw_input: &Option<String>,
) {
    if is_spawn && cb_state.grok_spawn_seen.insert(tool_call_id.to_string()) {
        let (description, subagent_type) = raw_input
            .as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .map(|input| {
                let field = |key: &str| {
                    input
                        .get(key)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                (field("description"), field("subagent_type"))
            })
            .unwrap_or((None, None));
        cb_state.grok_pending_spawn_ids.push_back(GrokPendingSpawn {
            call_id: tool_call_id.to_string(),
            description,
            subagent_type,
        });
        // Announced in the current turn → its progress ticks may render live.
        cb_state
            .grok_progress_eligible
            .insert(tool_call_id.to_string());
    }
    if !cb_state.grok_spawn_seen.contains(tool_call_id) {
        return;
    }
    match status {
        Some("completed") => {
            cb_state
                .grok_settled_spawn_ids
                .insert(tool_call_id.to_string());
        }
        Some("failed") => {
            cb_state
                .grok_settled_spawn_ids
                .insert(tool_call_id.to_string());
            // Never spawned a child (depth-limit / config error): drop it from
            // the pairing queue so the next spawn doesn't inherit its slot.
            cb_state
                .grok_pending_spawn_ids
                .retain(|pending| pending.call_id != tool_call_id);
        }
        _ => {}
    }
}

/// True when a tool call's ACP `_meta` marks it as grok's native
/// `ask_user_question` (`x.ai/tool.kind == "ask_user"`). Codeg answers grok's
/// blocking `_x.ai/ask_user_question` ext request by rendering the interactive
/// `AskQuestionCard` (see `handle_grok_ask_user_question`), so the parallel
/// `tool_call` stream grok emits for the same call is redundant — it is dropped
/// live so the question doesn't render twice (once answerable, once inert).
fn grok_meta_marks_ask_user(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    matches!(agent_type, AgentType::Grok)
        && meta
            .and_then(|m| m.get("x.ai/tool"))
            .and_then(|t| t.get("kind"))
            .and_then(|k| k.as_str())
            == Some("ask_user")
}

/// Resolve a tool call's title, honoring an authoritative rewrite recorded for
/// the session in `overrides` (tool_call_id → resolved title).
///
/// Returns `Some(name)` when this event identifies a CodeBuddy `DeferExecuteTool`
/// (the inner `mcp__…` name, from `raw_input`) or a sub-agent invocation
/// (`"agent"`) — recording it — OR when a PRIOR event already classified this
/// `tool_call_id` and this event lost the marker (the override is re-asserted).
/// Returns `None` only when the call was never classified, so the caller falls
/// back to the event's own title.
///
/// Sub-agent detection fires on EITHER `raw_input.subagent_type`
/// (`is_subagent_invocation`) OR `meta_marks_subagent` — the precomputed
/// `codebuddy_meta_marks_subagent` result. The meta signal is what makes the pill
/// stable: CodeBuddy carries `codebuddy.ai/toolName == "Agent"` from the very
/// first frame, whereas `subagent_type` only reaches `raw_input` dozens of frames
/// later, so meta-first detection records the override immediately and every
/// later (sparse) frame re-asserts it.
///
/// The re-assertion is the fix for CodeBuddy's status-only `ToolCallUpdate`s:
/// they arrive without the original `subagent_type`/`toolName` payload but WITH
/// the agent's raw (non-agent) title. Without it the frontend
/// (`inferLiveToolName` → `getToolName`) downgrades the Agent / delegation card
/// back to a generic tool call mid-stream — which also un-nests its children.
/// `on_update` only tunes the (PII-safe, id-only) trace wording.
fn resolve_rewritten_title(
    agent_type: AgentType,
    raw_input: &Option<String>,
    tool_call_id: &str,
    on_update: bool,
    meta_marks_subagent: bool,
    overrides: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(inner) = codebuddy_deferred_tool_name(agent_type, raw_input) {
        tracing::info!(
            "[ACP][{agent_type}] unwrapped DeferExecuteTool to its real MCP tool (tool_call_id={tool_call_id}, on_update={on_update})"
        );
        overrides.insert(tool_call_id.to_string(), inner.clone());
        return Some(inner);
    }
    if is_subagent_invocation(agent_type, raw_input) || meta_marks_subagent {
        tracing::info!(
            "[ACP][{agent_type}] subagent detected, rewrote tool title to 'agent' (tool_call_id={tool_call_id}, on_update={on_update})"
        );
        overrides.insert(tool_call_id.to_string(), "agent".to_string());
        return Some("agent".to_string());
    }
    overrides.get(tool_call_id).cloned()
}

fn map_plan_priority(priority: &PlanEntryPriority) -> String {
    match priority {
        PlanEntryPriority::High => "high",
        PlanEntryPriority::Medium => "medium",
        PlanEntryPriority::Low => "low",
        _ => "unknown",
    }
    .to_string()
}

fn map_plan_status(status: &PlanEntryStatus) -> String {
    match status {
        PlanEntryStatus::Pending => "pending",
        PlanEntryStatus::InProgress => "in_progress",
        PlanEntryStatus::Completed => "completed",
        _ => "unknown",
    }
    .to_string()
}

fn map_plan_entries(plan: &Plan) -> Vec<PlanEntryInfo> {
    plan.entries
        .iter()
        .map(|entry| PlanEntryInfo {
            content: entry.content.clone(),
            priority: map_plan_priority(&entry.priority),
            status: map_plan_status(&entry.status),
        })
        .collect()
}

fn parse_claude_sdk_message_params(
    params: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let obj = params.as_object()?;
    let session_id = obj.get("sessionId")?.as_str()?.to_string();
    let message = obj.get("message")?.clone();
    Some((session_id, message))
}

fn is_claude_api_retry_message(message: &serde_json::Value) -> bool {
    let obj = match message.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    let message_type = obj.get("type").and_then(|v| v.as_str());
    let message_subtype = obj.get("subtype").and_then(|v| v.as_str());
    matches!(message_type, Some("system")) && matches!(message_subtype, Some("api_retry"))
}

/// The JSON-RPC method claude-agent-acp uses to mirror raw SDK messages. Named
/// so `is_known_ext_method` and the mapper can't drift apart.
const CLAUDE_SDK_EXT_METHOD: &str = "_claude/sdkMessage";

fn map_claude_sdk_ext_notification(notification: &UntypedMessage) -> Option<AcpEvent> {
    if notification.method() != CLAUDE_SDK_EXT_METHOD {
        return None;
    }

    let (session_id, message) = parse_claude_sdk_message_params(notification.params())?;
    if !is_claude_api_retry_message(&message) {
        return None;
    }
    Some(AcpEvent::ClaudeSdkMessage {
        session_id,
        message,
    })
}

/// The JSON-RPC methods grok uses for its private, namespaced session updates.
/// Both share the standard `session/update` envelope (`params.update.
/// sessionUpdate` + fields, verified live against grok 0.2.111) but carry
/// variants the typed ACP pipeline can't deserialize, so codeg drops them.
const GROK_EXT_UPDATE_METHODS: [&str; 2] =
    ["_x.ai/session_notification", "_x.ai/session/update"];

/// A stable id for a synthetic event derived from a grok ext notification —
/// grok stamps `params._meta.eventId`; fall back to a fresh uuid.
fn grok_ext_event_id(params: &serde_json::Value) -> String {
    params
        .get("_meta")
        .and_then(|m| m.get("eventId"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("grok-ext-{}", uuid::Uuid::new_v4().simple()))
}

/// Map grok's private ext notifications — context compaction and dropped
/// prompt images — into `AcpEvent`s.
///
/// grok reports `/compact` (and auto-compaction) results, and the fate of an
/// image it refused to send, on
/// `_x.ai/session_notification` / `_x.ai/session/update` rather than as normal
/// `agent_message_chunk`s. Those methods never match the typed `session/update`
/// pipeline, so without this the whole turn is blank and `/compact` looks like
/// it failed. Only grok emits these, so gate on the agent. Turn-level failures
/// are intentionally NOT handled here — the `session/prompt` response path
/// (`turn_failure_error_event`) already surfaces those, and duplicating them
/// would double-report. (The one turn with no response, a turn Grok starts on
/// its own, is ended by the idle loop — see [`close_grok_agent_turn`].)
fn map_grok_ext_notification(
    notification: &UntypedMessage,
    agent_type: AgentType,
) -> Option<AcpEvent> {
    if !matches!(agent_type, AgentType::Grok) {
        return None;
    }
    if !GROK_EXT_UPDATE_METHODS.contains(&notification.method()) {
        return None;
    }
    let params = notification.params();
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(|v| v.as_str())? {
        // grok always emits `completed` (even a no-op where before == after).
        // Render the shared context-compaction card (recognized frontend-side by
        // `_meta.contextCompaction`, same as codex) carrying the token delta.
        "auto_compact_completed" => {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "contextCompaction".to_string(),
                serde_json::Value::Bool(true),
            );
            if let Some(before) = update.get("tokens_before").and_then(|v| v.as_u64()) {
                meta.insert("tokensBefore".to_string(), before.into());
            }
            if let Some(after) = update.get("tokens_after").and_then(|v| v.as_u64()) {
                meta.insert("tokensAfter".to_string(), after.into());
            }
            Some(AcpEvent::ToolCall {
                tool_call_id: grok_ext_event_id(params),
                title: "Context compaction".to_string(),
                kind: "other".to_string(),
                status: "completed".to_string(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: Some(serde_json::Value::Object(meta)),
                images: None,
            })
        }
        // A prompt image was accepted on the wire but dropped before the
        // describe sidecar (too small, oversize, decode failure). Surface it
        // so the user isn't left wondering why Grok "can't see" the shot.
        "image_dropped" => {
            // `notes` is grok's own user-facing sentence, one per dropped image
            // ("Image 1 was dropped before send: too small (1×1); images must be
            // at least 8×8 pixels."). It already names the subject, so it is
            // shown verbatim — prefixing it would read "Image dropped: Image 1
            // was dropped before send: …". Only the shapeless fallbacks get a
            // prefix, because on their own they say nothing about images.
            let message = update
                .get("notes")
                .and_then(|v| v.as_array())
                .map(|notes| {
                    notes
                        .iter()
                        .filter_map(|n| n.as_str())
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    update
                        .get("reason")
                        .or_else(|| update.get("message"))
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        // A blank reason would render as a bare "Image dropped: "
                        // — worse than the generic sentence below.
                        .filter(|d| !d.is_empty())
                        .map(|d| format!("Image dropped: {d}"))
                })
                .unwrap_or_else(|| "An image was dropped before send.".to_string());
            Some(AcpEvent::Error {
                message,
                agent_type: agent_type.to_string(),
                code: Some(IMAGE_DROPPED_ERROR_CODE.to_string()),
                details: None,
                terminal: false,
            })
        }
        // Compaction itself blew up (e.g. the summarizer model call failed) while
        // the turn still ended cleanly. It lands where every other compaction
        // outcome does — the shared context-compaction card, here in its FAILED
        // state with the reason as the card's hover text — rather than as a
        // session error under the composer. That is also how a failed
        // compaction reads on codex and claude (`session_compaction_event`), and
        // the history parser synthesizes the same card from the log. The coded
        // `Error` readers other than the app still need is emitted beside it by
        // the caller (`compaction_failure_error`), keeping this mapper 1:1.
        "auto_compact_failed" => Some(AcpEvent::ToolCall {
            tool_call_id: grok_ext_event_id(params),
            title: "Context compaction".to_string(),
            kind: "other".to_string(),
            status: "failed".to_string(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: Some(grok_failed_compaction_meta(update)),
            images: None,
        }),
        _ => None,
    }
}

/// `_meta` for a failed grok auto-compaction: the versioned
/// `contextCompaction` payload (`{version: 1, error?}`), whose `error` the
/// compaction card shows as the reason. Shared by the live mapper and the
/// history parser so a reopened conversation renders the same card.
pub(crate) fn grok_failed_compaction_meta(update: &serde_json::Value) -> serde_json::Value {
    let mut compaction = serde_json::Map::new();
    compaction.insert("version".to_string(), 1.into());
    if let Some(reason) = update
        .get("reason")
        .or_else(|| update.get("message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        compaction.insert("error".to_string(), reason.into());
    }
    serde_json::json!({ "contextCompaction": compaction })
}

/// Map grok's sub-agent lifecycle notifications (`_x.ai/session/update` with
/// `sessionUpdate: subagent_spawned | subagent_progress | subagent_finished`)
/// onto live events for the launching `spawn_subagent` Agent card. STATEFUL —
/// kept out of the pure [`map_grok_ext_notification`] (which doubles as the
/// turn-output predicate and must stay re-invocable).
///
/// Grok never forwards a child's chunks or tool calls over ACP (unlike
/// claude-agent-acp ≥0.63) — these three notifications are the ONLY live
/// signal a subagent emits, so:
///
/// * `subagent_spawned` pairs `subagent_id` → the oldest unpaired spawn call
///   (FIFO; the notifications carry no `tool_call_id` — same pairing as the
///   history parser).
/// * `subagent_progress` becomes a meta-only `ToolCallUpdate`
///   (`meta.grokSubagentProgress`) so the running Agent card can show a live
///   "N tools · M turns · ctx%" line. Replacing the block's meta is safe for
///   this call: its classification rides `rawInput.subagent_type`, not meta.
///   GATED on `turn_active`: after `TurnComplete` the frontend diverts wire
///   tool updates out of the transcript anyway, while the backend
///   `SessionState` apply arm would lazily RECREATE `live_message` for the
///   update — resurrecting a ghost pending card into every later
///   snapshot/attach. An out-of-turn tick is therefore dropped whole (the
///   `subagent_finished` settle below is the out-of-turn-safe channel).
/// * `subagent_finished` for a spawn whose CALL already settled (= a
///   BACKGROUND child; the launch ack was its final wire output) settles via
///   the same `BackgroundActivity` channel Claude's async sub-agents use: the
///   frontend flips the launch card's `[[codeg-background-task]]` marker
///   in-memory (works out-of-turn too), raises the OS notification, and
///   mirrors `outstanding` for the idle-sweep exemption. A BLOCKING spawn is
///   skipped — its own completion frame delivers the output.
fn map_grok_subagent_notification(
    notification: &UntypedMessage,
    agent_type: AgentType,
    turn_active: bool,
    cb_state: &mut CodeBuddyLiveState,
) -> Vec<AcpEvent> {
    map_grok_subagent_notification_inner(notification, agent_type, turn_active, cb_state)
        .unwrap_or_default()
}

/// The body of [`map_grok_subagent_notification`], written with `?` on the many
/// "this notification isn't one of ours / is malformed" checks. A spawn can
/// produce TWO events (the card's session stamp plus the background-activity
/// report), hence the list.
fn map_grok_subagent_notification_inner(
    notification: &UntypedMessage,
    agent_type: AgentType,
    turn_active: bool,
    cb_state: &mut CodeBuddyLiveState,
) -> Option<Vec<AcpEvent>> {
    if !matches!(agent_type, AgentType::Grok) {
        return None;
    }
    if !GROK_EXT_UPDATE_METHODS.contains(&notification.method()) {
        return None;
    }
    let params = notification.params();
    let update = params.get("update")?;
    let subagent_id = update.get("subagent_id").and_then(|v| v.as_str())?;
    // `outstanding` = paired subagents still running whose launch call already
    // settled — i.e. background children codeg would otherwise sweep as idle.
    let outstanding = |cb_state: &CodeBuddyLiveState| {
        cb_state
            .grok_subagent_to_call
            .values()
            .filter(|call_id| cb_state.grok_settled_spawn_ids.contains(*call_id))
            .count() as u32
    };
    match update.get("sessionUpdate").and_then(|v| v.as_str())? {
        "subagent_spawned" => {
            // First pending entry whose captured launch `(description,
            // subagent_type)` is consistent with the notification's own
            // fields. ASYMMETRIC rule: a value captured from the launch input
            // must be matched by an EQUAL value on the notification — a
            // notification that omits the field does NOT wildcard past it
            // (that shape would let a delayed, description-less prior-turn
            // notification steal a described new-turn entry). Only a pending
            // side that captured nothing (unparseable/absent input field —
            // then there is nothing to validate against) is a wildcard. Real
            // captures always carry both fields on both sides, so in-order
            // streams match at the head (plain FIFO); the fail direction for
            // an unmatched notification is pairing nothing (live progress/
            // settle degrade gracefully; history re-parse is unaffected).
            let event_field = |key: &str| {
                update
                    .get(key)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            };
            let matches = |pending: &Option<String>, event: Option<&str>| match (pending, event) {
                (Some(p), Some(e)) => p == e,
                (Some(_), None) => false,
                (None, _) => true,
            };
            let idx = cb_state.grok_pending_spawn_ids.iter().position(|p| {
                matches(&p.description, event_field("description"))
                    && matches(&p.subagent_type, event_field("subagent_type"))
            })?;
            let call_id = cb_state.grok_pending_spawn_ids.remove(idx)?.call_id;
            cb_state
                .grok_subagent_to_call
                .insert(subagent_id.to_string(), call_id.clone());
            // The child's own session id — `child_session_id` when the wire
            // carries it, else the subagent id (they are the same value in
            // every capture). Remembered so later progress ticks can re-send it
            // (`upsert_tool_call` replaces meta wholesale, so every tick must
            // carry it again).
            //
            // Gated by the same `is_safe_subagent_id` the history parser applies
            // to this field: the value is handed to the frontend, which asks
            // `get_conversation` to resolve a session directory by it, so both
            // paths have to reject a traversal-shaped id — not just the one that
            // reads from disk itself. Failing the gate simply leaves the card
            // without a session to open.
            let child_session_id = update
                .get("child_session_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(subagent_id);
            let child_session_id = crate::parsers::is_safe_subagent_id(child_session_id)
                .then(|| child_session_id.to_string());
            if let Some(child) = &child_session_id {
                cb_state
                    .grok_call_child_session
                    .insert(call_id.clone(), child.clone());
            }

            let mut events = Vec::new();
            // Stamp the child's session onto the launching Agent card so it can
            // offer to open the child's transcript WHILE it runs — for a
            // blocking spawn that is the only live window into the child, since
            // its call produces no output until the very end. Gated exactly like
            // a progress tick: out of turn, the `SessionState` apply arm would
            // lazily recreate `live_message` and strand a ghost card in every
            // later snapshot.
            if turn_active && cb_state.grok_progress_eligible.contains(&call_id) {
                events.push(AcpEvent::ToolCallUpdate {
                    tool_call_id: call_id.clone(),
                    title: None,
                    status: None,
                    content: None,
                    raw_input: None,
                    raw_output: None,
                    raw_output_append: None,
                    locations: None,
                    meta: Some(grok_subagent_meta(
                        subagent_id,
                        child_session_id.as_deref(),
                        None,
                    )),
                    images: None,
                });
            }
            // A background launch's call settles before/around the pairing;
            // surface the outstanding count so the connection is exempt from
            // idle sweeps while the child works. Nothing to add for a
            // blocking spawn (its turn is open — nothing can sweep it).
            if cb_state.grok_settled_spawn_ids.contains(&call_id) {
                let session_id = params.get("sessionId").and_then(|v| v.as_str())?;
                events.push(AcpEvent::BackgroundActivity {
                    session_id: session_id.to_string(),
                    turns: Vec::new(),
                    outstanding: outstanding(cb_state),
                    settled: Vec::new(),
                    watermark: 0,
                });
            }
            Some(events)
        }
        "subagent_progress" => {
            if !turn_active {
                return None;
            }
            let call_id = cb_state.grok_subagent_to_call.get(subagent_id)?.clone();
            // Launch-turn-specific gate on top of Prompting: a tick for a
            // PRIOR turn's background child must not be appended into the
            // CURRENT turn's live message (see `grok_progress_eligible`).
            if !cb_state.grok_progress_eligible.contains(&call_id) {
                return None;
            }
            let mut progress = serde_json::Map::new();
            for (wire, out) in [
                ("duration_ms", "durationMs"),
                ("turn_count", "turnCount"),
                ("tool_call_count", "toolCallCount"),
                ("context_usage_pct", "contextUsagePct"),
                ("error_count", "errorCount"),
            ] {
                if let Some(v) = update.get(wire).filter(|v| v.is_number()) {
                    progress.insert(out.to_string(), v.clone());
                }
            }
            if progress.is_empty() {
                return None;
            }
            // Carry the session stamp along: `upsert_tool_call` replaces the
            // block's meta wholesale, so a progress-only payload would drop the
            // "open the child's session" affordance mid-run.
            let child_session_id = cb_state.grok_call_child_session.get(&call_id).cloned();
            Some(vec![AcpEvent::ToolCallUpdate {
                tool_call_id: call_id,
                title: None,
                status: None,
                content: None,
                raw_input: None,
                raw_output: None,
                raw_output_append: None,
                locations: None,
                meta: Some(grok_subagent_meta(
                    subagent_id,
                    child_session_id.as_deref(),
                    Some(serde_json::Value::Object(progress)),
                )),
                images: None,
            }])
        }
        "subagent_finished" => {
            let call_id = cb_state.grok_subagent_to_call.remove(subagent_id)?;
            cb_state.grok_call_child_session.remove(&call_id);
            if !cb_state.grok_settled_spawn_ids.contains(&call_id) {
                // Blocking spawn: the child's output arrives on the call's own
                // completion frame; the settle channel would double-render it.
                return None;
            }
            let session_id = params.get("sessionId").and_then(|v| v.as_str())?;
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            let result = update
                .get("output")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| crate::parsers::truncate_str(s, crate::parsers::claude::BACKGROUND_RESULT_MAX_CHARS));
            Some(vec![AcpEvent::BackgroundActivity {
                session_id: session_id.to_string(),
                turns: Vec::new(),
                outstanding: outstanding(cb_state),
                settled: vec![crate::acp::types::BackgroundSettledInfo {
                    task_id: subagent_id.to_string(),
                    status: status.to_string(),
                    summary: None,
                    tool_use_id: Some(call_id),
                    result,
                }],
                watermark: 0,
            }])
        }
        _ => None,
    }
}

/// The `meta` payload a Grok sub-agent's launching Agent card carries:
/// `grokSubagentSession` (ids — lets the card open the child's transcript) and,
/// while it runs, `grokSubagentProgress` (the live "N tools · M turns · ctx%"
/// line). Built in one place because `upsert_tool_call` REPLACES a block's
/// meta: every emission has to carry the whole picture, not just its own half.
fn grok_subagent_meta(
    subagent_id: &str,
    child_session_id: Option<&str>,
    progress: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut session = serde_json::Map::new();
    session.insert("subagentId".to_string(), subagent_id.into());
    if let Some(child) = child_session_id {
        session.insert("childSessionId".to_string(), child.into());
    }
    let mut meta = serde_json::Map::new();
    meta.insert(
        "grokSubagentSession".to_string(),
        serde_json::Value::Object(session),
    );
    if let Some(progress) = progress {
        meta.insert("grokSubagentProgress".to_string(), progress);
    }
    serde_json::Value::Object(meta)
}

/// Whether a dispatch is a grok ext notification that
/// `map_grok_ext_notification` renders as visible turn output (a compaction
/// card, a compaction error, or a dropped-image error). The active-turn loop
/// consults this BEFORE the typed
/// pipeline to mark the turn as non-empty: a `/compact` turn emits only these
/// ext notifications and no standard `agent_message_chunk`, so without this its
/// `end_turn` is reclassified as `"empty"` and re-surfaced as a spurious error —
/// the exact symptom this change removes. Reuses `map_grok_ext_notification` so
/// the handled-variant set can never drift from what actually emits. A turn
/// whose only output was a dropped image therefore reports THAT, rather than
/// the generic empty-turn failure it used to.
fn grok_ext_notification_is_turn_output(dispatch: &Dispatch, agent_type: AgentType) -> bool {
    match dispatch {
        Dispatch::Notification(notification) => {
            map_grok_ext_notification(notification, agent_type).is_some()
        }
        _ => false,
    }
}

/// Whether the historical `session/load` replay drops a grok ext notification:
/// every variant [`map_grok_ext_notification`] handles, so none of them reach
/// the live `SessionState` or the clients.
///
/// Those notifications describe a PAST session. An alert (a dropped image)
/// would report something as happening now, for a session the user is merely
/// opening. A compaction card, completed or failed, would anchor into
/// `live_message` — the replay runs outside any turn, and no `TurnComplete`
/// follows to clear it — so every later snapshot would carry a historical card
/// as in-flight content. Nothing is lost by dropping them: grok's transcript,
/// compaction cards included, comes from its own session log
/// (`parsers::grok`), not from this replay. Reuses the mapper for the same
/// reason [`grok_ext_notification_is_turn_output`] does — the set cannot drift
/// away from what actually emits.
fn grok_ext_notification_skipped_on_replay(dispatch: &Dispatch, agent_type: AgentType) -> bool {
    match dispatch {
        Dispatch::Notification(notification) => {
            map_grok_ext_notification(notification, agent_type).is_some()
        }
        _ => false,
    }
}

/// The `Error` that accompanies a FAILED compaction card from
/// [`map_grok_ext_notification`] (see `auto_compact_failed` there), or `None`.
///
/// The card is where the failure is shown in the app. This coded, non-terminal
/// `Error` is for everything that reads errors off the event stream rather
/// than cards — the chat-channel bridges (a remote user sees no transcript),
/// `SessionState::last_error`, the pet's error state — exactly as before the
/// card existed. The frontend knows the code and draws nothing for it (the
/// card already says it), so the app still shows the failure once.
fn compaction_failure_error(event: &AcpEvent, agent_type: AgentType) -> Option<AcpEvent> {
    let AcpEvent::ToolCall { status, meta, .. } = event else {
        return None;
    };
    if status != "failed" {
        return None;
    }
    let compaction = meta.as_ref()?.get("contextCompaction")?;
    let reason = compaction.get("error").and_then(|v| v.as_str());
    Some(AcpEvent::Error {
        message: match reason {
            Some(reason) => format!("Context compaction failed: {reason}"),
            None => "Context compaction failed".to_string(),
        },
        agent_type: agent_type.to_string(),
        code: Some(COMPACTION_FAILED_ERROR_CODE.to_string()),
        details: None,
        // Recoverable: the turn goes on uncompacted.
        terminal: false,
    })
}

/// Claims an agent message whose `sessionId` is present but `null`, before the
/// ACP runtime's session routing can park it: a notification is dropped, a
/// request is answered with `invalid_params`.
///
/// The runtime decides a message is session-bound by FIELD PRESENCE alone
/// (`Dispatch::has_session_id` → `params.get("sessionId").is_some()`), and parks
/// every session-bound message nobody claims in a retry queue that is replayed
/// into each newly added dynamic handler. A `"sessionId": null` therefore looks
/// session-bound and gets parked. Under `sacp` 11 the replay went into the
/// runtime's own session handler, whose `get_session_id` fails on the null with
/// `invalid type: null, expected a string` — and a handler `Err` brought the
/// whole connection down, so the user saw `ACP protocol error: Invalid params:
/// "invalid type: null, expected a string"` instead of a session (issue #794).
/// The 2.x runtime logs a handler error instead of dying on it, and codeg's own
/// `AgentSession` router simply matches no session on a null id — but nothing
/// would ever claim the frame either, so without this guard it would sit in the
/// retry queue for the connection's lifetime and be replayed into every handler
/// registered after it.
///
/// grok 1.0.40 is what made this reachable: it narrates `session/new` progress
/// on `_x.ai/session/setup`, and its first five phases (`auth`,
/// `resolve_workspace`, `folder_trust`, `plugin_registry`, `mcp_merge`) run
/// BEFORE the session id exists, so they carry `null`. 1.0.34 (the previous
/// pin) sent no such notification at all. The guard is deliberately NOT gated
/// on grok: no ACP notification legitimately carries a null session id, and
/// any agent that sent one would hit the same path.
///
/// This has to sit in the BUILDER chain, not among the session handlers: the
/// static chain is the only thing that runs before a message can be parked for
/// retry, and a parked message is replayed straight into the newly added
/// dynamic handler without passing through this chain again.
///
/// A null session id means "not about a session yet", and nothing codeg renders
/// rides on such a notification, so it is dropped. A REQUEST shaped this way is
/// the same protocol violation, and it would be parked just the same — the
/// runtime's `method_not_found` fallback only answers requests with no
/// `sessionId` field at all — leaving the agent blocked on a reply that never
/// comes. It is answered `invalid_params` instead, which is also the truth.
/// Being first in the chain, the guard answers it before codeg's own request
/// handlers see it: the typed ones would reject the null anyway, and no agent
/// codeg drives sends a null to the raw-params bridges (grok's ask and plan
/// exit, the elicitation bridge codex and DeepSeek use, cursor's extension
/// methods — which carry no `sessionId` at all).
struct ClaimNullSessionIds;

impl<Counterpart: Role> HandleDispatchFrom<Counterpart> for ClaimNullSessionIds {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        _connection: ConnectionTo<Counterpart>,
    ) -> Result<Handled<Dispatch>, agent_client_protocol::Error> {
        match message {
            Dispatch::Notification(notification) if has_null_session_id(&notification) => {
                tracing::debug!(
                    method = %notification.method(),
                    "[ACP] dropping notification with a null sessionId"
                );
                Ok(Handled::Yes)
            }
            Dispatch::Request(request, responder) if has_null_session_id(&request) => {
                tracing::warn!(
                    method = %request.method(),
                    "[ACP] rejecting request with a null sessionId"
                );
                if let Err(e) = responder.respond_with_error(
                    agent_client_protocol::Error::invalid_params()
                        .data("sessionId must be a session id, not null"),
                ) {
                    tracing::debug!("[ACP] could not answer a null-sessionId request: {e}");
                }
                Ok(Handled::Yes)
            }
            message => Ok(Handled::No {
                message,
                retry: false,
            }),
        }
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "ClaimNullSessionIds"
    }
}

/// True when `params.sessionId` exists and is JSON `null` — the one shape
/// [`ClaimNullSessionIds`] exists for. A MISSING `sessionId` is a
/// perfectly ordinary connection-level notification (`_auth/status_update`,
/// grok's `_x.ai/settings/update`) and must keep flowing.
fn has_null_session_id(message: &UntypedMessage) -> bool {
    message
        .params()
        .get("sessionId")
        .is_some_and(serde_json::Value::is_null)
}

/// Claims Grok's model catalog broadcast, `_x.ai/models/update`, and folds it
/// into the connection's model picker (issue #876).
///
/// Grok fetches its catalog after startup, and again when its login changes —
/// after refreshing an expired token, for one — and announces every fetch on
/// this method, typically several identical frames within milliseconds. A
/// session established before the fetch landed was handed whatever catalog grok
/// had then, which can be only its bundled models: loaded on an expired token,
/// a session showed Grok 4.6 and 4.5 while the catalog that arrived 0.7s later
/// also had both 4.7 models.
///
/// The broadcast names no session. The per-session router matches on
/// `params.sessionId`, so it never claimed this frame, nothing else did either,
/// and the runtime dropped it unhandled: the picker kept the handshake's list
/// for the life of the connection. It has to be claimed here in the builder
/// chain — a message without a `sessionId` is never parked for a router added
/// later. A connection drives one grok process and the catalog is that
/// process's, so the broadcast belongs to whatever session the connection holds
/// or is establishing; for the latter it waits in
/// `SessionState::grok_catalog_broadcast`.
///
/// Handled inline on the dispatch loop rather than spawned: in arrival order,
/// each frame folds against the state the one before it left, which is what
/// makes a repeat a no-op. It takes nothing but the session state's lock, which
/// the permission handler on this same loop already takes inline
/// (`handle_permission_request`).
struct GrokModelCatalogBroadcasts {
    agent_type: AgentType,
    state: Arc<RwLock<SessionState>>,
    emitter: EventEmitter,
}

impl<Counterpart: Role> HandleDispatchFrom<Counterpart> for GrokModelCatalogBroadcasts {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        _connection: ConnectionTo<Counterpart>,
    ) -> Result<Handled<Dispatch>, agent_client_protocol::Error> {
        match message {
            Dispatch::Notification(notification)
                if self.agent_type == AgentType::Grok
                    && notification.method() == GROK_MODELS_UPDATE_METHOD =>
            {
                apply_grok_model_catalog(&self.state, &self.emitter, notification.params()).await;
                Ok(Handled::Yes)
            }
            message => Ok(Handled::No {
                message,
                retry: false,
            }),
        }
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "GrokModelCatalogBroadcasts"
    }
}

/// Fold one `_x.ai/models/update` into the connection's state: always into the
/// session's model specs and the establishment slot, and into the picker on
/// screen if there is one — emitted only when that changed it, so a repeated
/// broadcast is a no-op.
async fn apply_grok_model_catalog(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    params: &serde_json::Value,
) {
    let Some(catalog) = parse_grok_model_catalog(params) else {
        tracing::debug!("[ACP][grok] model catalog broadcast lists no usable model; ignored");
        return;
    };
    let models = catalog.models.len();
    let refreshed = emit_with_state_built(state, emitter, move |s| {
        let specs = s.grok_model_specs.get_or_insert_with(HashMap::new);
        fold_grok_catalog_specs(specs, &catalog.specs);
        let picker = s.config_options.as_ref().and_then(|shown| {
            let mut next = shown.clone();
            fold_grok_catalog_into_picker(&mut next, &catalog, specs);
            (next != *shown).then_some(next)
        });
        s.grok_catalog_broadcast = Some(catalog);
        picker.map(|config_options| AcpEvent::SessionConfigOptions { config_options })
    })
    .await;
    if refreshed {
        tracing::info!(
            models,
            "[ACP][grok] model catalog broadcast refreshed the model picker"
        );
    } else {
        tracing::debug!(
            models,
            "[ACP][grok] model catalog broadcast left the model picker as it was"
        );
    }
}

/// `_auth/status_update` — the agent reporting which identity IT is logged in
/// with. Introduced by codex-acp 1.9.0; claude-agent-acp 0.75.0 adopted the
/// same method with its own vocabulary.
///
/// Connection-level: unlike every other agent push codeg reads, the params carry
/// NO `sessionId`, which is exactly why it needs a handler of its own (see
/// [`handle_auth_status_update`]). Only `authStatus` is modelled; the payload is
/// kept as a raw value so a new `kind` or an added field can never turn a
/// well-formed push into a deserialization failure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, agent_client_protocol::JsonRpcNotification)]
#[notification(method = "_auth/status_update")]
#[serde(rename_all = "camelCase")]
struct AuthStatusUpdateNotification {
    auth_status: serde_json::Value,
}

/// Claim `_auth/status_update` and drop it, loudly enough to be greppable.
///
/// Two agents push it, both unconditionally. codex-acp 1.9.0: once right after
/// the `initialize` response, then on every authenticate / logout / session
/// create, and whenever the app-server reports `account/updated`.
/// claude-agent-acp 0.75.0: the same points PLUS one codex does not have — the
/// start of every user prompt, which fires an async `claude auth status --json`
/// probe (5s timeout), so a push can land MID-TURN and a consumer must not
/// assume the channel is quiet while a turn is open. Neither is gated on
/// anything codeg advertises; the agent merely ANNOUNCES the channel with
/// `agentCapabilities._meta.authStatus = {}`, and codeg registers this handler
/// for every agent rather than per type, so a third adopter is already claimed.
///
/// Both push only when the payload DIFFERS from the last one sent, so the
/// absence of a push means "unchanged", never "not signed in". Claude draws one
/// more distinction worth keeping: it stays SILENT when it cannot determine the
/// identity at all (probe failed, timed out, unparseable), and reserves
/// `kind: "none"` for a known signed-OUT state.
///
/// A handler is registered rather than letting the frame fall through. Under
/// `sacp` 11 falling through was not free: with no claimant (the per-session
/// router only matches frames carrying its own `sessionId`, and this one carries
/// none) it logged `Rejecting message with error, no handler` at INFO and wrote
/// a bare JSON-RPC error object back to an agent that never asked a question.
/// The 2.x runtime just drops an unclaimed notification at debug level, so the
/// claim is now what keeps the payload's shape recorded against a real reader.
///
/// Nothing consumes the payload yet, and that is a deliberate stop: the status
/// describes the AGENT-owned login only (on codex, routing codeg itself
/// configured through `providers/set` is explicitly excluded upstream), and on
/// both agents every failure it could warn about already arrives as an AIR
/// `sessionFailure` carrying an actionable `login` — claude 0.74.0 additionally
/// rejects the prompt with `authRequired`. The shape is recorded here so a
/// future consumer does not have to re-derive it:
///
///   {"authStatus": {"kind": "account" | "api_key" | "external" | "gateway"
///                           | "none",
///                   "label": "ChatGPT Pro" | "OpenAI API key"
///                            | "Custom model gateway" | "Not logged in" | …,
///                   "detail"?: "<gateway provider id>",
///                   "account"?: {"email"?, "plan"?, "organization"?},
///                   "vendor"?: {…}}}
///
/// Observed against a live 1.10.0 whose `~/.codex/config.toml` selects a custom
/// provider: `{"kind":"gateway","label":"Custom model gateway","detail":"codeg"}`.
/// Against a live claude-agent-acp 0.75.1 with no credential:
/// `{"kind":"none","label":"Not logged in"}`.
/// The payload is NOT logged whole: `account.email` and `account.organization`
/// are the signed-in person's identity, and codeg's log file is user-visible
/// (and shipped in diagnostics). `kind` and `label` are the two fields that
/// answer "which identity is this connection using", and neither identifies a
/// person — `label` is one of a fixed per-agent vocabulary ("ChatGPT Pro",
/// "OpenAI API key", "Custom model gateway", "Not logged in" on codex; "Claude
/// Max", "Anthropic API key", "AWS Bedrock", "Not logged in" on claude) plus,
/// for a gateway, the provider id the user configured locally.
fn handle_auth_status_update(agent_type: AgentType, notif: AuthStatusUpdateNotification) {
    let field = |key: &str| {
        notif
            .auth_status
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    tracing::debug!(
        agent = %agent_type,
        kind = %field("kind"),
        label = %field("label"),
        "[ACP] agent reported its auth status (no consumer)"
    );
}

/// Whether codeg has a mapper for this ext-notification method.
///
/// Used ONLY to keep the unrecognized-method log quiet about methods we do know
/// and merely declined to map this time. That distinction is the whole point:
/// `_claude/sdkMessage` arrives for every SDK message and only maps when the
/// payload is an API retry, so logging every unmapped one would put a line on a
/// per-message hot path — the shape that once grew a server's log file to 217GB.
///
/// Forgetting to list a newly-mapped method here fails in the SAFE direction —
/// its unmapped payloads just get logged (noise, still visible). The dangerous
/// direction is the reverse: listing a method no mapper claims would silence
/// exactly the gap this log exists to expose.
fn is_known_ext_method(method: &str) -> bool {
    method == CLAUDE_SDK_EXT_METHOD || GROK_EXT_UPDATE_METHODS.contains(&method)
}

/// Keep the text of a claude `Read` / `Grep` / `Glob` result off the raw SDK
/// stream, for the completion frame that follows it (see
/// `CodeBuddyLiveState::claude_viewed_results`).
///
/// The tool is named by the call's own opening frame (`claudeCode.toolName`,
/// kept in the `_meta` ledger), never guessed from the result: a result for a
/// call codeg has not seen announced is not stashed. The text is taken the way
/// the transcript parser takes it (`parsers::claude::extract_tool_result_text`),
/// which is also the string claude-agent-acp 0.81.x put in `rawOutput`, so the
/// live card and its reloaded twin read the same thing.
fn stash_claude_viewed_results(params: &serde_json::Value, cb_state: &mut CodeBuddyLiveState) {
    let Some(message) = params.get("message") else {
        return;
    };
    for result in crate::acp::air_contract::claude_sdk_tool_results(message) {
        if result.is_error {
            continue;
        }
        let viewed = cb_state
            .air_tool_call_meta
            .claude_tool_name(result.tool_use_id)
            .is_some_and(crate::acp::air_contract::claude_viewed_file_tool);
        if !viewed {
            continue;
        }
        if let Some(text) = crate::parsers::claude::extract_tool_result_text(result.block) {
            cb_state
                .claude_viewed_results
                .insert(result.tool_use_id.to_string(), text);
        }
    }
}

/// The plugins a claude `system/init` frame reports it could not load
/// (`plugin_errors`, CLI 2.1.283+), read off the raw SDK stream.
///
/// claude-agent-acp only logs these ("Plugin load failures … have no ACP
/// surface"), and its stderr reaches nobody. `None` for any other frame, and
/// for an init that lists none: the CLI omits the key when nothing failed, and
/// every CLI before 2.1.283 omits it always. An entry with no `plugin` name is
/// dropped — nothing would say which plugin it is about. `type` is an open
/// category ("treat a value you do not recognize as a generic failure"), so a
/// missing one reads as `generic-error`.
fn claude_plugin_load_failures(params: &serde_json::Value) -> Option<Vec<PluginLoadFailure>> {
    let message = claude_init_frame(params)?;
    let text = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let failures: Vec<PluginLoadFailure> = message
        .get("plugin_errors")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            Some(PluginLoadFailure {
                plugin: text(entry, "plugin")?,
                kind: text(entry, "type").unwrap_or_else(|| "generic-error".to_string()),
                message: text(entry, "message").unwrap_or_default(),
                path: text(entry, "path"),
            })
        })
        .collect();
    (!failures.is_empty()).then_some(failures)
}

/// The `message` of `_claude/sdkMessage` params when it is the CLI's
/// `system/init` frame, which the CLI sends again at the start of every turn.
fn claude_init_frame(params: &serde_json::Value) -> Option<&serde_json::Value> {
    let message = params.get("message")?;
    (message.get("type").and_then(|v| v.as_str()) == Some("system")
        && message.get("subtype").and_then(|v| v.as_str()) == Some("init"))
    .then_some(message)
}

/// [`claude_plugin_load_failures`] when the list is news. The CLI repeats
/// `plugin_errors` on every turn's `system/init`, so a list is returned only
/// when it differs from the one last forwarded on this connection. An init
/// that lists none means the failures are gone: it forwards nothing, and
/// forgets the last list, so the same failure coming back later is news again.
fn take_new_claude_plugin_failures(
    params: &serde_json::Value,
    cb_state: &mut CodeBuddyLiveState,
) -> Option<Vec<PluginLoadFailure>> {
    claude_init_frame(params)?;
    let Some(failures) = claude_plugin_load_failures(params) else {
        cb_state.claude_reported_plugin_failures = None;
        return None;
    };
    if cb_state.claude_reported_plugin_failures.as_ref() == Some(&failures) {
        return None;
    }
    cb_state.claude_reported_plugin_failures = Some(failures.clone());
    Some(failures)
}

/// Whether the historical `session/load` replay drops a claude raw-SDK frame
/// that reports plugin load failures.
///
/// claude-agent-acp sends `_claude/sdkMessage` only from its live consumer,
/// which the first prompt starts, and replays history as `session/update`s —
/// so no such frame reaches the drain today. The replay seam still drops one,
/// for two reasons that hold whatever the adapter does: a plugin failure is
/// only news about the process running now, which the next turn's init reports
/// to the live loop anyway; and the drain hands every frame a throwaway
/// `CodeBuddyLiveState`, so nothing there could tell a repeat from news.
fn claude_plugin_failures_skipped_on_replay(dispatch: &Dispatch) -> bool {
    match dispatch {
        Dispatch::Notification(notification) => {
            notification.method() == CLAUDE_SDK_EXT_METHOD
                && claude_plugin_load_failures(notification.params()).is_some()
        }
        _ => false,
    }
}

/// Last stop for a dispatch the typed `session/update` pipeline didn't claim.
///
/// A notification no mapper claims is dropped, but not invisibly; a request is
/// answered `method_not_found` rather than left hanging (see its arm). All
/// lines are `debug!`: an agent is free to speak methods codeg doesn't
/// implement, and a per-message `warn!` on a chatty agent is how log storms
/// start.
async fn maybe_emit_ext_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    dispatch: Dispatch,
    cb_state: &mut CodeBuddyLiveState,
) {
    let notification = match dispatch {
        Dispatch::Notification(notification) => notification,
        // An agent calling a client method codeg doesn't implement. It only
        // lands here because it names the session — the session router claims
        // every such message before the runtime's own fallback, which answers
        // `method_not_found` to an unhandled request that names none. Answer
        // this one the same way: dropping the responder would leave the agent
        // blocked on a reply that never comes.
        Dispatch::Request(request, responder) => {
            let method = request.method().to_string();
            tracing::debug!(%method, "[ACP] answering unhandled agent request with method_not_found");
            if let Err(e) = responder
                .respond_with_error(agent_client_protocol::Error::method_not_found().data(method))
            {
                tracing::debug!("[ACP] could not answer an unhandled agent request: {e}");
            }
            return;
        }
        // A response is normally consumed by the caller waiting on it, so one
        // surfacing here is unexpected rather than routine. (It still carries a
        // `ResponseRouter`, so "unroutable" would overstate it — the point is
        // only that nothing on this path wants it.)
        Dispatch::Response(..) => {
            tracing::debug!("[ACP] dropping unexpected response dispatch");
            return;
        }
    };

    // The CURRENT connection status decides whether a grok `subagent_progress`
    // tick may touch the live tool call — the same Prompting predicate the
    // out-of-turn chunk defenses use (#870). Read here (not at the call sites)
    // so a stray notification the active-turn loop drains AFTER the status
    // already flipped back is still classified as out-of-turn.
    let turn_active = state.read().await.status == ConnectionStatus::Prompting;
    if agent_type == AgentType::ClaudeCode && notification.method() == CLAUDE_SDK_EXT_METHOD {
        stash_claude_viewed_results(notification.params(), cb_state);
        if let Some(failures) = take_new_claude_plugin_failures(notification.params(), cb_state) {
            emit_with_state(state, emitter, AcpEvent::PluginLoadFailures { failures }).await;
        }
    }
    // A grok `subagent_spawned` can yield TWO events (the card's session stamp
    // and the background-activity report), so this mapper hands back a list; an
    // empty one means "not mine", and the chain continues as before.
    let grok_subagent_events =
        map_grok_subagent_notification(&notification, agent_type, turn_active, cb_state);
    if !grok_subagent_events.is_empty() {
        for event in grok_subagent_events {
            emit_with_state(state, emitter, event).await;
        }
    } else if let Some(event) = map_claude_sdk_ext_notification(&notification)
        .or_else(|| map_grok_ext_notification(&notification, agent_type))
    {
        let companion = compaction_failure_error(&event, agent_type);
        emit_with_state(state, emitter, event).await;
        if let Some(error) = companion {
            emit_with_state(state, emitter, error).await;
        }
    } else if !is_known_ext_method(notification.method()) {
        // The gap #409's second point was reaching for: an agent emitting an ext
        // method codeg has never heard of was previously indistinguishable from
        // an agent saying nothing at all.
        tracing::debug!(
            method = %notification.method(),
            agent = %agent_type,
            "[ACP] ignoring unrecognized ext notification"
        );
    }
}

/// Fix null fields in `usage_update` notifications that would otherwise fail deserialization.
///
/// Some ACP agents send `"used": null` in usage_update notifications, but the
/// upstream schema expects `u64`. This function patches the raw JSON params
/// so that `null` numeric fields default to `0`.
fn fix_usage_update_nulls(mut dispatch: Dispatch) -> Dispatch {
    if let Dispatch::Notification(ref mut msg) = dispatch {
        if let Some(update) = msg.params.get_mut("update") {
            if update.get("sessionUpdate").and_then(|v| v.as_str()) == Some("usage_update") {
                if update.get("used").map(|v| v.is_null()).unwrap_or(false) {
                    update["used"] = serde_json::Value::from(0u64);
                }
                if update.get("size").map(|v| v.is_null()).unwrap_or(false) {
                    update["size"] = serde_json::Value::from(0u64);
                }
            }
        }
    }
    dispatch
}

/// Read one AIR async-task frame out of a raw `session/update` dispatch.
///
/// This runs BEFORE `MatchDispatch` on purpose, and consuming the frame here is
/// not an optimization — it is the only way to see it at all.
/// `MatchDispatch::if_notification` matches on the METHOD first and then hard-
/// errors when the params don't parse, so it never reaches `.otherwise()`; and
/// the v1 `SessionUpdate` (schema 1.9) is still an internally-tagged enum with
/// no catch-all arm — only the draft v2 enum gained `Other` — so these three
/// variants cannot deserialize. Left alone they would land on the dropped-update path, which
/// also feeds the empty-turn diagnosis — a turn that only ran background work
/// would be reported as an agent that said nothing. Same raw-rewrite seam as
/// [`fix_usage_update_nulls`].
///
/// Deliberately NOT gated on `agent_type`. claude-agent-acp and codex-acp are
/// the two adapters offered the `asyncTasks` capability (see
/// `build_client_capabilities`), so nothing else should send these — but if
/// something does, reading the frame is strictly better than dropping it, since
/// every AIR speaker uses the same vocabulary. The two shapes differ only in how
/// much they fill in: claude names a `description`, `usage` and
/// `outputFilePath`; codex sends the command as `name` and stops there.
fn air_async_task_delta(dispatch: &Dispatch) -> Option<AsyncTaskDelta> {
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    // Method-checked as well as shape-checked. These frames only ever ride on
    // `session/update`, and claiming a dispatch before the typed pipeline is
    // destructive — nothing downstream gets a second look at it — so a future
    // extension method that happens to nest an `update.sessionUpdate` must not
    // be swallowed here. (Session scoping is already settled upstream: this
    // dispatch came off an `AgentSession` read, which only yields frames whose
    // session id matches.)
    if msg.method() != "session/update" {
        return None;
    }
    let update = msg.params.get("update")?;
    let spawned = match update.get("sessionUpdate").and_then(|v| v.as_str())? {
        "async_task_spawned" => true,
        "async_task_progress" | "async_task_state_update" => false,
        _ => return None,
    };
    let field = |key: &str| air_task_str(update.get(key));
    let task_id = field("asyncTaskId")?;
    Some(AsyncTaskDelta {
        task_id,
        spawned,
        name: field("name"),
        task_type: field("taskType"),
        description: field("description"),
        show_in_transcript: update.get("showInTranscript").and_then(|v| v.as_bool()),
        can_stop: update.get("canStop").and_then(|v| v.as_bool()),
        state: field("state"),
        summary: field("summary"),
        last_tool_name: field("lastToolName"),
        usage: air_task_usage(update.get("usage")),
        output_file_path: field("outputFilePath"),
        tool_call_id: field("toolCallId"),
        phase: None,
        current_agent: None,
    })
}

/// A trimmed non-blank string field, or `None`. Blank is treated as absent so a
/// whitespace-only summary can't blank out a good one during the merge.
fn air_task_str(value: Option<&serde_json::Value>) -> Option<String> {
    let s = value?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// All-or-nothing, matching the adapter: it drops a `usage` object that is
/// missing any of the three counters rather than publishing a partial one.
fn air_task_usage(value: Option<&serde_json::Value>) -> Option<AsyncTaskUsage> {
    let usage = value?;
    let n = |key: &str| usage.get(key).and_then(serde_json::Value::as_u64);
    Some(AsyncTaskUsage {
        total_tokens: n("totalTokens")?,
        tool_uses: n("toolUses")?,
        duration_ms: n("durationMs")?,
    })
}

/// Read one Grok background-workflow frame as an async-task delta.
///
/// Grok speaks no AIR, and is not advertised `asyncTasks`: a run launched by
/// `/workflow`, `/deep-research` or its `workflow` tool (whose call returns at
/// once) reports on its own private channel instead — `workflow_updated` on
/// `_x.ai/session_notification`, persisted as `_x.ai/session/update`. Captured
/// from grok 1.0.41:
///
///   {"sessionUpdate":"workflow_updated","run_id":"wf_01a0e8a2…","revision":5,
///    "name":"codeg-probe","objective":"…","status":"active",
///    "phases":[{"title":"Alpha","state":"active"},…],"current_phase":"Alpha",
///    "active_agents":1,"current_agent_label":"probe-agent","agents":[…],…}
///
/// Every frame restates the whole run, so each one is a `spawned` delta (a
/// client that joins mid-run still gets its row), and the fields a frame can
/// DROP — the agent once it finishes, the reason once a paused run resumes —
/// are restated as an empty string rather than left absent, which the shared
/// merge would read as "unchanged". Consumed before the typed pipeline like
/// [`air_async_task_delta`], at the same three seams.
fn grok_workflow_task_delta(dispatch: &Dispatch, agent_type: AgentType) -> Option<AsyncTaskDelta> {
    if agent_type != AgentType::Grok {
        return None;
    }
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    if !GROK_EXT_UPDATE_METHODS.contains(&msg.method()) {
        return None;
    }
    let update = msg.params.get("update")?;
    if update.get("sessionUpdate").and_then(|v| v.as_str())? != "workflow_updated" {
        return None;
    }
    let field = |key: &str| air_task_str(update.get(key));
    let task_id = field("run_id")?;
    // Without a status the frame says nothing about the run, and since every
    // frame may create its row, reading it would leave one with no state.
    let status = field("status")?;
    Some(AsyncTaskDelta {
        task_id,
        spawned: true,
        name: field("name"),
        task_type: Some("workflow".to_string()),
        description: field("objective"),
        // The run gets no transcript card of its own; a model-launched run's
        // `workflow` tool call is the launch, not the run.
        show_in_transcript: Some(false),
        // Grok has no `_session/async_task/stop`.
        can_stop: Some(false),
        state: Some(grok_workflow_state(&status)),
        // The result on completion; on a pause or failure, the reason (Grok
        // carries a failure's error in `pause_message` too).
        summary: Some(
            field("result_summary")
                .or_else(|| field("pause_message"))
                .unwrap_or_default(),
        ),
        last_tool_name: None,
        usage: None,
        output_file_path: None,
        tool_call_id: None,
        phase: Some(field("current_phase").unwrap_or_default()),
        current_agent: Some(field("current_agent_label").unwrap_or_default()),
    })
}

/// A Grok workflow `status` in the async-task vocabulary. Grok 1.0.41 has
/// `active`, five pauses (`user_paused`, `back_off_paused`,
/// `no_progress_paused`, `infra_paused`, and `blocked` — the verification
/// gate), `budget_limited`, and the terminal `complete`, `failed`,
/// `cancelled` and `interrupted` (the session ended mid-run; not resumable).
/// An unrecognised status passes through as itself and so reads as live,
/// matching how the record treats any state outside the terminal three.
fn grok_workflow_state(status: &str) -> String {
    match status {
        "active" => "running",
        "paused" | "user_paused" | "back_off_paused" | "no_progress_paused" | "infra_paused"
        | "blocked" | "budget_limited" => "paused",
        "complete" | "completed" => "completed",
        "failed" | "interrupted" => "failed",
        "cancelled" | "canceled" => "stopped",
        other => other,
    }
    .to_string()
}

/// A Grok `turn_completed` — the end of a turn as Grok itself reports it, on
/// its private channel: `{prompt_id, stop_reason, usage?, elapsed_ms}`.
#[derive(Debug, PartialEq, Eq)]
struct GrokTurnCompleted {
    prompt_id: Option<String>,
    /// Already in codeg's stop-reason vocabulary (see [`grok_stop_reason`]).
    stop_reason: &'static str,
}

fn grok_turn_completed(dispatch: &Dispatch, agent_type: AgentType) -> Option<GrokTurnCompleted> {
    if agent_type != AgentType::Grok {
        return None;
    }
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    if !GROK_EXT_UPDATE_METHODS.contains(&msg.method()) {
        return None;
    }
    let update = msg.params.get("update")?;
    if update.get("sessionUpdate").and_then(|v| v.as_str())? != "turn_completed" {
        return None;
    }
    Some(GrokTurnCompleted {
        prompt_id: air_task_str(update.get("prompt_id")),
        stop_reason: grok_stop_reason(
            update.get("stop_reason").and_then(|v| v.as_str()).unwrap_or(""),
        ),
    })
}

/// Grok's `stop_reason` mapped the way [`stop_reason_to_str`] maps a typed
/// one: ACP's five as themselves, anything else — Grok's own `error`
/// included — as `unknown`.
fn grok_stop_reason(raw: &str) -> &'static str {
    match raw {
        "end_turn" => "end_turn",
        "cancelled" => "cancelled",
        "refusal" => "refusal",
        "max_tokens" => "max_tokens",
        "max_turn_requests" => "max_turn_requests",
        _ => "unknown",
    }
}

/// The prompt id Grok stamps on a `session/update`'s OUTER `_meta` — the
/// prompt whose turn produced the frame, whether a client sent it or Grok
/// queued it itself (`workflow-completed-<run>-<revision>`).
fn grok_frame_prompt_id(dispatch: &Dispatch) -> Option<&str> {
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    if msg.method() != "session/update" {
        return None;
    }
    msg.params
        .get("_meta")?
        .get("promptId")?
        .as_str()
        .filter(|id| !id.is_empty())
}

/// If this idle-loop frame starts a turn Grok is running on its own, the
/// prompt id to track it by.
///
/// When a background workflow stops, Grok runs a follow-up turn for it in the
/// same process — a prompt it queues itself, never a client `session/prompt`,
/// so no response will ever end it. Only a main-thread chunk opens one
/// (message or thought — that is the first thing a follow-up streams), and
/// only one stamped with a prompt id codeg has not already seen END:
///
/// * a turn codeg started leaves stragglers on the idle loop whenever the loop
///   picks its response before its last queued frames — those carry the id
///   of that turn (see [`note_grok_turn_prompt_id`]);
/// * a turn Grok cancels flushes a chunk AFTER its own `turn_completed`, with
///   no prompt id at all (captured from 1.0.41). Opening on that would start a
///   turn nothing ever ends.
fn grok_agent_turn_opener(
    dispatch: &Dispatch,
    agent_type: AgentType,
    cb_state: &CodeBuddyLiveState,
) -> Option<String> {
    if agent_type != AgentType::Grok || cb_state.grok_agent_turn.is_some() {
        return None;
    }
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    let kind = msg.params.get("update")?.get("sessionUpdate")?.as_str()?;
    if !matches!(kind, "agent_message_chunk" | "agent_thought_chunk") {
        return None;
    }
    let prompt_id = grok_frame_prompt_id(dispatch)?;
    (!cb_state.grok_ended_prompt_ids.contains(prompt_id)).then(|| prompt_id.to_string())
}

/// Whether a Grok `turn_completed` ends the agent-initiated turn the idle loop
/// has open. A turn codeg started already ended on its prompt response, so its
/// own `turn_completed` — which can reach the idle loop after that response —
/// must end nothing a second time.
fn grok_turn_completed_ends(open: Option<&str>, completed: &GrokTurnCompleted) -> bool {
    match (open, completed.prompt_id.as_deref()) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(open), Some(done)) => open == done,
    }
}

/// Remember the prompt id of the turn codeg is running, off any of its frames
/// that carries one, so the stragglers it leaves on the idle loop can't open a
/// turn of their own (see [`grok_agent_turn_opener`]).
fn note_grok_turn_prompt_id(dispatch: &Dispatch, ended: &mut HashSet<String>) {
    if let Some(prompt_id) = grok_frame_prompt_id(dispatch) {
        remember_grok_ended_prompt(ended, prompt_id);
    }
}

/// Record that the Grok turn `prompt_id` has ended.
fn remember_grok_ended_prompt(ended: &mut HashSet<String>, prompt_id: &str) {
    if !ended.contains(prompt_id) {
        ended.insert(prompt_id.to_string());
    }
}

/// Open a turn for one Grok started on its own, BEFORE its first chunk is
/// emitted: out of `Prompting` the frontend drops streamed content, and the
/// transcript promotion that keeps a turn only runs on the way out of it.
///
/// Skipped when a prompt codeg admitted is about to start — Grok queues that
/// prompt behind its own turn, so what it streams now lands in the admitted
/// one instead (see `SessionState::begin_agent_initiated_turn`).
async fn open_grok_agent_turn(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    prompt_id: String,
    cb_state: &mut CodeBuddyLiveState,
) {
    if !state.write().await.begin_agent_initiated_turn() {
        return;
    }
    // The same one-turn scope a prompt's turn start resets (see there): a
    // prior turn's background sub-agent must not tick into this one.
    cb_state.grok_progress_eligible.clear();
    cb_state.grok_pending_spawn_ids.clear();
    cb_state.grok_agent_turn = Some(prompt_id);
    emit_with_state(
        state,
        emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        },
    )
    .await;
}

/// End the agent-initiated turn the idle loop has open, if any, the way a
/// prompt's turn ends: the failure `Error` first when the stop reason is one,
/// then `TurnComplete` (drained together with any permission card still up —
/// see the turn loop's exit), then `Connected`. Returns whether a turn was
/// open.
///
/// Its prompt id is remembered as ended, so a chunk Grok flushes after the end
/// can't reopen it.
#[allow(clippy::too_many_arguments)]
async fn close_grok_agent_turn(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    agent_type: AgentType,
    session_id: &str,
    stop_reason: &str,
    cb_state: &mut CodeBuddyLiveState,
) -> bool {
    let Some(prompt_id) = cb_state.grok_agent_turn.take() else {
        return false;
    };
    remember_grok_ended_prompt(&mut cb_state.grok_ended_prompt_ids, &prompt_id);
    if let Some(error) = turn_failure_error_event(stop_reason, agent_type, None) {
        emit_with_state(state, emitter, error).await;
    }
    drain_permissions_then_emit(
        perms,
        state,
        emitter,
        AcpEvent::TurnComplete {
            session_id: session_id.to_string(),
            stop_reason: stop_reason.to_string(),
            agent_type: agent_type.to_string(),
        },
    )
    .await;
    emit_with_state(
        state,
        emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Connected,
        },
    )
    .await;
    true
}

/// Answer, declined, a question or plan approval an agent-initiated turn is
/// parked on, once codeg has ended that turn early (Stop, or a prompt that has
/// to run next). Both cards close with the turn, so nothing could ever answer
/// them, and Grok would stay blocked — holding the one-per-connection slot
/// every later ask needs, and, when a prompt is queued behind the turn, that
/// prompt too. The same reclaim the mid-turn Cancel does.
async fn cancel_grok_agent_turn_asks(
    delegation_injection: Option<&DelegationInjection>,
    conn_id: &str,
) {
    if let Some(inj) = delegation_injection {
        inj.questions.cancel_questions_by_parent(conn_id).await;
        inj.plan_approvals
            .cancel_plan_approvals_by_parent(conn_id)
            .await;
    }
}

/// Read one ACP Session Notice out of a raw `session/update` dispatch.
///
/// Runs BEFORE `MatchDispatch`, at the same seam as [`air_async_task_delta`].
/// Unlike those AIR frames a notice DOES deserialize as a typed
/// `SessionUpdate::Notice` since schema 1.9 (`unstable_session_notices`), so the
/// raw read is a choice rather than the only way in: the three seams that call
/// it — the `session/load` replay drain, the idle loop and the in-turn loop —
/// each settle a notice differently (dropped on replay, never counted as turn
/// output), and claiming it ahead of the typed pipeline keeps that policy in
/// one visible place per seam. It is also what keeps a notice with a blank
/// title out, which the typed struct would accept. Method-checked as well as
/// shape-checked, because claiming a dispatch before the typed pipeline is
/// destructive.
///
/// `title` is REQUIRED and non-empty per the RFD; a notice without one is
/// dropped rather than surfaced, since the consumer would have nothing to show
/// but an empty toast. `description` is optional, and blank is treated as
/// absent so a whitespace-only detail cannot open an empty second line.
///
/// Deliberately NOT gated on `agent_type`, matching [`air_async_task_delta`]:
/// only the two agents `build_client_capabilities` advertises `session.notices`
/// to should send these, but if another does, showing the advisory beats
/// dropping it — the vocabulary is the RFD's, not any one adapter's.
fn session_notice(dispatch: &Dispatch) -> Option<SessionNotice> {
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    if msg.method() != "session/update" {
        return None;
    }
    let update = msg.params.get("update")?;
    if update.get("sessionUpdate").and_then(|v| v.as_str())? != "notice" {
        return None;
    }
    let title = air_task_str(update.get("title"))?;
    Some(SessionNotice {
        severity: air_task_str(update.get("severity")).unwrap_or_else(|| "info".to_string()),
        title,
        description: air_task_str(update.get("description")),
    })
}

/// Translate an ACP compaction frame into the `_meta.contextCompaction`
/// synthetic tool call codeg already renders compaction from.
///
/// Same pre-dispatch seam and the same reason as [`session_notice`]. The
/// TRANSLATION, rather than a new event, is the point: `<ContextCompactionCard>`
/// reads `_meta.contextCompaction` and the timeline hoists it to the
/// `"compaction"` render kind, and four history parsers
/// (`parsers::{claude,pi,opencode,deepseek}`) synthesize that exact shape from
/// their logs. Advertising `session.compaction` makes both adapters STOP
/// sending the legacy call, so anything other than translating it back would
/// mean rebuilding the card on a second channel and keeping two renderings of
/// one thing in step. This is the same trick the grok bridge plays for
/// `auto_compact_completed` (see `grok_ext_session_update_event`).
///
/// Field mapping:
/// * `compactionId` → the tool-call id. Stable across the frame pair, which is
///   what lets `in_progress` and its terminal frame share one card.
/// * `status` → `in_progress` | `completed` | `failed` | `cancelled`. An
///   unknown status is passed through verbatim rather than guessed at; the card
///   treats anything non-terminal as running.
/// * `_meta` → forwarded WHOLE. claude fills the reserved
///   `{version, trigger, preTokens, postTokens, durationMs, error}` block that
///   the card's full label needs; codex sends none, so a bare `{version: 1}`
///   stands in — which is exactly what its legacy call carried too, so codex
///   loses nothing by the move and gains the failed/cancelled states and the
///   history-position replay it had no way to express before.
/// * `summary` / `compaction_summary_chunk` → `raw_output`, the channel that
///   already streams (chunks append) and already survives snapshot and
///   promotion. It is the retained summary — the one thing the legacy
///   presentation could never carry — and the card expands to show it. Every
///   `compaction_update` also stamps [`COMPACTION_SUMMARY_META_KEY`], because
///   `raw_output` on its own proves nothing: claude's LEGACY call parks its
///   metadata object there (`rawOutput: metadata` in `finish`), so without an
///   explicit claim an older adapter's counts would render as a "summary".
///
/// Every frame is a `ToolCallUpdate`, including the opening one, because the
/// frontend reducer UPSERTS: an update naming an id it has not seen creates the
/// block (the `existingIndex === -1` arm in `acp-connections-context`, which is
/// why that action carries a `fallback_title` at all). That keeps this reader
/// stateless, and it is also what makes the terminal-only orders work — codex
/// replays a persisted compaction as a lone `completed`, and claude's `settle`
/// notes the opening status can be missed on replay or a terminal-only runtime.
fn session_compaction_event(dispatch: &Dispatch) -> Option<AcpEvent> {
    let Dispatch::Notification(msg) = dispatch else {
        return None;
    };
    if msg.method() != "session/update" {
        return None;
    }
    let update = msg.params.get("update")?;
    let kind = update.get("sessionUpdate").and_then(|v| v.as_str())?;
    let compaction_id = air_task_str(update.get("compactionId"))?;
    match kind {
        "compaction_update" => {
            let status = air_task_str(update.get("status"))?;
            let mut meta = update
                .get("_meta")
                .and_then(|m| m.as_object().cloned())
                .unwrap_or_default();
            // The card keys off `contextCompaction` specifically; the adapters
            // nest their reserved fields under it on the legacy call, and
            // claude repeats that exact block here. Anything else the frame
            // carried rides along untouched. claude-agent-acp 0.82.0 moved the
            // block to `_meta.jetbrains.air.contextCompaction` (and sends the
            // top-level key to no client); without taking it from there, the
            // default below would stand in for it and the card would lose
            // every token count and the duration.
            if let Some(air) = crate::acp::air_contract::air_meta_value(Some(&meta), "contextCompaction")
                .filter(|block| block.is_object())
                .cloned()
            {
                meta.entry("contextCompaction").or_insert(air);
            }
            meta.entry("contextCompaction")
                .or_insert_with(|| serde_json::json!({"version": 1}));
            meta.insert(
                COMPACTION_SUMMARY_META_KEY.to_string(),
                serde_json::Value::Bool(true),
            );
            // `error` is a sibling of `status` on the wire but a member of the
            // card's payload, so fold it in where the card looks for it. A
            // service error envelope is read down to its message first: codex
            // forwards the raw JSON here even where it gives the failure
            // banner the sentence (see `service_error`).
            if let Some(error) = air_task_str(update.get("error")) {
                let error = crate::acp::service_error::readable_service_error_message(&error)
                    .unwrap_or(error);
                if let Some(payload) = meta
                    .get_mut("contextCompaction")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    payload
                        .entry("error")
                        .or_insert_with(|| serde_json::Value::String(error));
                }
            }
            Some(AcpEvent::ToolCallUpdate {
                tool_call_id: compaction_id,
                title: Some(CONTEXT_COMPACTION_TITLE.to_string()),
                status: Some(status),
                content: None,
                raw_input: None,
                raw_output: compaction_summary_text(update.get("summary")),
                raw_output_append: None,
                locations: None,
                meta: Some(serde_json::Value::Object(meta)),
                images: None,
            })
        }
        // Streamed continuation of the same compaction's retained summary.
        // Appended, not replaced — each chunk is a fragment, exactly like the
        // terminal-output bridge's deltas.
        "compaction_summary_chunk" => Some(AcpEvent::ToolCallUpdate {
            tool_call_id: compaction_id,
            title: None,
            status: None,
            content: None,
            raw_input: None,
            raw_output: Some(compaction_summary_text(update.get("content"))?),
            raw_output_append: Some(true),
            locations: None,
            meta: None,
            images: None,
        }),
        _ => None,
    }
}

/// Title the synthetic compaction call carries, matching the one the grok
/// bridge and both adapters' legacy calls use.
const CONTEXT_COMPACTION_TITLE: &str = "Context compaction";

/// Flatten an ACP summary payload — a `ContentBlock` or an array of them — to
/// its text. Non-text blocks (an image in a summary would be novel) are
/// skipped rather than stringified into JSON noise.
fn compaction_summary_text(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    let mut out = String::new();
    let mut push = |block: &serde_json::Value| {
        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
            out.push_str(text);
        }
    };
    match value {
        serde_json::Value::Array(blocks) => blocks.iter().for_each(&mut push),
        block => push(block),
    }
    (!out.is_empty()).then_some(out)
}

/// Convert a SessionUpdate into AcpEvent(s) and emit to frontend.
///
/// `raw_output_cache` is a per-session cache used to detect cumulative
/// snapshots from agents and convert them into incremental deltas so the
/// event pipeline never carries a full N-MB tool output more than once.
///
/// `cb_state` is the per-session `CodeBuddyLiveState`: the authoritative
/// title-rewrite map (so a status-only update can't downgrade an Agent /
/// delegation card and un-nest its children) plus the open-sub-agent window used
/// to suppress a CodeBuddy sub-agent's interleaved thought/message chunks.
/// Mirrors `raw_output_cache`'s lifetime.
async fn emit_conversation_update(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    update: SessionUpdate,
    cwd: Option<&str>,
    raw_output_cache: &mut ToolCallOutputCache,
    cb_state: &mut CodeBuddyLiveState,
) {
    match update {
        SessionUpdate::UserMessageChunk(_) => {
            // User echo chunks are informational for transcript sync and
            // currently not rendered in live ACP UI.
        }
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            meta,
            ..
        }) => {
            // Gemini reports an approval-mode switch as a prose chunk reading
            // `[MODE_UPDATE] <mode>`. Turn it back into the mode event it should
            // have been — see `gemini_mode_update_chunk` for the two guards that
            // keep it from eating prose. No-op for every other agent.
            if let Some(mode_id) = gemini_mode_update_chunk(agent_type, state, &text.text).await {
                emit_with_state(state, emitter, AcpEvent::ModeChanged { mode_id }).await;
                return;
            }
            // pi-acp opens every new session by pushing its markdown startup
            // banner down this same prose channel. It is recognized against the
            // text pi-acp itself reported on the `session/new` response, not by
            // shape — see `pi_take_startup_banner`. No-op for every other agent.
            let is_pi_startup_banner =
                pi_take_startup_banner(agent_type, state, &text.text).await;
            // Drop a CodeBuddy sub-agent's interleaved message text — it belongs
            // to the Agent pill, not the main thread (see
            // `should_suppress_subagent_chunk`). No-op for every other agent.
            if !is_pi_startup_banner
                && !should_suppress_subagent_chunk(
                    agent_type,
                    !cb_state.open_subagents.is_empty(),
                    meta.as_ref(),
                )
            {
                // pi-acp announces its own lifecycle (extension notifies, auto
                // retry, compaction, prompt queue) on this same prose channel,
                // where it splices into the reply — issue #525. Classify before
                // emitting; `Prose` is every other agent's only outcome.
                match pi_message_chunk_route(agent_type, &text.text, meta.as_ref()) {
                    PiChunkRoute::Drop => {}
                    PiChunkRoute::Retrying {
                        attempt,
                        max,
                        delay_ms,
                    } => {
                        // The shared retry banner, not the transcript. `message`
                        // is empty on purpose: pi forwards no error text, and the
                        // banner renders its own localized line from the numbers
                        // (see `AcpEvent::TurnRetrying`).
                        emit_with_state(
                            state,
                            emitter,
                            AcpEvent::TurnRetrying {
                                message: String::new(),
                                error_status: None,
                                attempt,
                                max_retries: max,
                                retry_delay_ms: delay_ms,
                            },
                        )
                        .await;
                    }
                    PiChunkRoute::Prose => {
                        // Claude subagent chunks (claude-agent-acp ≥0.63 with the
                        // `subagent-transcript` capability) are NOT suppressed: they
                        // emit with their parent id so the frontend can route them
                        // into the live Agent capsule.
                        let parent_tool_use_id =
                            claude_chunk_parent_tool_use_id(agent_type, meta.as_ref());
                        emit_with_state(
                            state,
                            emitter,
                            AcpEvent::ContentDelta {
                                text: text.text,
                                parent_tool_use_id,
                            },
                        )
                        .await;
                    }
                }
            }
        }
        SessionUpdate::AgentMessageChunk(_) => {
            // Non-text chunks are currently not surfaced in live streaming UI.
        }
        SessionUpdate::AgentThoughtChunk(ContentChunk {
            content: ContentBlock::Text(text),
            meta,
            ..
        }) => {
            // Same suppression for a sub-agent's interleaved reasoning.
            if !should_suppress_subagent_chunk(
                agent_type,
                !cb_state.open_subagents.is_empty(),
                meta.as_ref(),
            ) {
                let parent_tool_use_id = claude_chunk_parent_tool_use_id(agent_type, meta.as_ref());
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::Thinking {
                        text: text.text,
                        parent_tool_use_id,
                    },
                )
                .await;
            }
        }
        SessionUpdate::AgentThoughtChunk(_) => {
            // Non-text thought chunks are currently ignored.
        }
        SessionUpdate::ToolCall(mut tc) => {
            let tool_call_id = tc.tool_call_id.to_string();
            // The AIR contract sends each `_meta` key once: open the call's
            // record, and read the moved keys under their legacy names from here
            // on (see `crate::acp::air_contract`).
            tc.meta = cb_state
                .air_tool_call_meta
                .open(agent_type, &tool_call_id, tc.meta.take());
            if agent_type == AgentType::Codex
                && crate::acp::codex_context::is_web_search_input(tc.raw_input.as_ref())
            {
                cb_state.codex_context_readings.web_search_ran();
            }
            // codex-acp #304 surfaces codex `subAgentActivity` as a live
            // `tool_call`. A launch becomes an Agent capsule (its own rawInput
            // is orchestration bookkeeping, so it is replaced wholesale); a
            // terminal marker is folded back onto that capsule; the rest stay
            // dropped. See `classify_codex_subagent_activity`.
            let activity_meta =
                codex_activity_classification_meta(agent_type, tc.meta.as_ref(), tc.raw_input.as_ref());
            let mut codex_subagent_thread = None;
            let codex_subagent = match classify_codex_subagent_activity(
                agent_type,
                activity_meta.as_ref().or(tc.meta.as_ref()),
            ) {
                CodexSubagentActivity::None => {
                    // The id now names something else: its follow-ups are its
                    // own, not an earlier activity's.
                    cb_state.codex_activity_calls.remove(&tool_call_id);
                    None
                }
                CodexSubagentActivity::Started { thread_id, input } => {
                    codex_subagent_thread = thread_id;
                    cb_state
                        .codex_activity_calls
                        .insert(tool_call_id.clone(), Some(input.clone()));
                    Some(input)
                }
                CodexSubagentActivity::Terminal { thread_id, kind } => {
                    cb_state.codex_activity_calls.insert(tool_call_id, None);
                    settle_codex_subagent_launch(state, emitter, cb_state, &thread_id, &kind).await;
                    return;
                }
                CodexSubagentActivity::Other => {
                    cb_state.codex_activity_calls.insert(tool_call_id, None);
                    return;
                }
            };
            // Remember which capsule owns this child, so its eventual
            // `completed` / `interrupted` (announced under a synthetic id of its
            // own) can be routed back here.
            if let (Some(thread_id), Some(input)) = (codex_subagent_thread, codex_subagent.as_ref())
            {
                cb_state
                    .codex_subagent_launches
                    .insert(thread_id, (tool_call_id.clone(), input.clone()));
            }
            // Grok emits a redundant `tool_call` for its native ask_user_question
            // alongside the blocking `_x.ai/ask_user_question` ext request codeg
            // answers with the interactive card; drop it here (remembering the id so
            // later status-only updates that lost the meta are dropped too).
            if grok_meta_marks_ask_user(agent_type, tc.meta.as_ref()) {
                cb_state.grok_ask_tool_ids.insert(tool_call_id);
                return;
            }
            // CodeBuddy double-wraps a deferred MCP result as a `{type,text}`
            // content part; peel it (in both the content and raw_output channels)
            // so the dedicated delegation cards parse it instead of showing raw JSON.
            // codex-acp reports file edits as a `Diff` content block with no
            // `raw_input`; synthesize a canonical edit so the call classifies/
            // renders as an edit instead of a tool named after the raw diff
            // header (see synthesize_edit_input_from_diffs). When we do, drop the
            // `Diff` from `content` — it's the same edit re-serialized hunklessly,
            // which would otherwise double the event and skew the header +/- stats.
            // Blank raw_input is treated as absent (matches the frontend guard).
            // Grok wraps every MCP call in a `use_tool` envelope; peel it so the
            // call is correlated/classified/parsed as a direct MCP call — its
            // real `tool_input` becomes `raw_input`, its `tool_name` the title
            // below (see unwrap_grok_use_tool).
            let grok_use_tool = if matches!(agent_type, AgentType::Grok) {
                unwrap_grok_use_tool(tc.raw_input.as_ref())
            } else {
                None
            };
            // pi and codex both host their own terminal and name it by this very
            // tool-call id, so the `Terminal` block is a placeholder nothing can
            // ever supersede from the terminal channel — strip it and let the
            // `_meta` bridge below supply the output. Remember the call: the
            // frames that carry the output name only the id (see
            // `hosted_terminal_meta_marks_shell`).
            let hosted_shell = hosted_terminal_meta_marks_shell(agent_type, tc.meta.as_ref());
            if hosted_shell {
                cb_state
                    .hosted_terminal_calls
                    .entry(tool_call_id.clone())
                    .or_insert(false);
            }
            let hosted_stripped_content = hosted_shell.then(|| strip_terminal_blocks(&tc.content));
            let content_blocks: &[ToolCallContent] =
                hosted_stripped_content.as_deref().unwrap_or(&tc.content);
            let own_raw_input = match &grok_use_tool {
                Some((_, inner)) => {
                    json_value_to_text(&Some(inner.clone())).filter(|t| !t.trim().is_empty())
                }
                None => tool_call_raw_input_text(agent_type, tc.meta.as_ref(), &tc.raw_input)
                    .filter(|t| !t.trim().is_empty()),
            };
            let synthesized_edit = if own_raw_input.is_none() {
                synthesize_edit_input_from_diffs(content_blocks)
            } else {
                claude_complete_file_edit_input(
                    agent_type,
                    tc.meta.as_ref(),
                    own_raw_input.as_deref(),
                    content_blocks,
                )
            };
            // pi sends no `rawInput` for bash at all — its command lives in the
            // title. Synthesize the canonical `{"command"}` shape so the call
            // classifies as `bash` instead of a generic tool named after the
            // command (see `pi_bash_input_from_title`). Stays gated on pi even
            // though `hosted_shell` now also covers codex: codex DOES send
            // `rawInput = {command, cwd}` on the opening frame, so this could
            // only ever fire there on a frame that lost it, where the title
            // would be a worse reconstruction than the reducer's prior input.
            let pi_bash_input = if own_raw_input.is_none()
                && hosted_shell
                && matches!(agent_type, AgentType::Pi)
            {
                pi_bash_input_from_title(Some(tc.title.as_str()))
            } else {
                None
            };
            // gemini sends no `rawInput` for ANY tool — its arguments survive
            // only in the title and locations. Rebuild the canonical shape for
            // the tools whose title format identifies them exactly, so they
            // classify instead of all landing on the generic card (see
            // `gemini_synthesize_tool_input`).
            let gemini_input = if own_raw_input.is_none() {
                gemini_synthesize_tool_input(agent_type, &tc.kind, &tc.title, &tc.locations)
            } else {
                None
            };
            let content =
                serialize_tool_call_content(content_blocks, synthesized_edit.is_none())
                    .map(|c| unwrap_codebuddy_deferred_output(agent_type, &c).unwrap_or(c))
                    // pi announces a command with an empty result, which pi-acp
                    // renders as JSON source (see fn doc).
                    .filter(|_| !pi_result_content_is_stringify_noise(agent_type, &tc.raw_output));
            let images = extract_tool_call_images(content_blocks);
            let codex_subagent_launch = codex_subagent.is_some();
            let raw_input = codex_subagent
                .or(synthesized_edit)
                .or(own_raw_input)
                .or(pi_bash_input)
                .or(gemini_input)
                .map(|text| resolve_live_tool_input(&text, cwd));
            // Initial tool_call notification — the frontend reducer
            // treats `raw_output` as a full replacement, so we bypass
            // the diff path and seed the cache with the current snapshot.
            let raw_output_text = if matches!(agent_type, AgentType::Grok) {
                // Grok's structured rawOutput would shadow `content` and render
                // empty; take the parity path (see grok_live_tool_output).
                grok_live_tool_output(&content, &tc.raw_output)
            } else if matches!(agent_type, AgentType::Pi) {
                // pi's rawOutput is the MCP envelope around the SAME text
                // `content` already carries; shipping it shadows the clean text
                // with JSON source (see pi_live_tool_output).
                pi_live_tool_output(&content, &tc.raw_output)
            } else if matches!(agent_type, AgentType::OpenCode) {
                // OpenCode's rawOutput is the `{output, metadata}` envelope
                // around the SAME text `content` already carries; shipping it
                // shadows the clean text (see opencode_live_tool_output).
                opencode_live_tool_output(&content, &tc.raw_output)
            } else {
                json_value_to_text(&tc.raw_output)
                    .map(|text| unwrap_codebuddy_deferred_output(agent_type, &text).unwrap_or(text))
                    .map(|text| structurize_live_output(&text))
            };
            let raw_output =
                raw_output_text.and_then(|text| raw_output_cache.seed(&tool_call_id, &text));
            let locations = if tc.locations.is_empty() {
                None
            } else {
                serde_json::to_value(&tc.locations).ok()
            };
            // Read the CodeBuddy sub-agent markers from `_meta` BEFORE it's moved
            // into the emitted `Value` below — `meta_marks_subagent` is the early,
            // reliable signal (frame 1) that keeps the Agent pill from flickering;
            // `meta_marks_background` keeps a concurrent sub-agent out of the
            // suppression window (see fn docs).
            // `codex_subagent_launch` joins the CodeBuddy meta signal here for
            // the same reason it exists: it is what records the authoritative
            // "agent" title, so the status-only follow-up (which carries no
            // rawInput at all) re-asserts it instead of downgrading the capsule
            // to a generic tool card.
            let meta_marks_subagent = codebuddy_meta_marks_subagent(agent_type, tc.meta.as_ref())
                || codex_subagent_launch;
            let meta_marks_background = codebuddy_meta_marks_background(agent_type, tc.meta.as_ref());
            let grok_spawn = grok_meta_marks_spawn_subagent(agent_type, tc.meta.as_ref());
            let status = format!("{:?}", tc.status).to_lowercase();
            // OpenCode's only authoritative statement of WHICH tool this is
            // arrives on this opening frame's title (see fn doc).
            let meta = stamp_opencode_tool_name(
                agent_type,
                &status,
                &tc.raw_input,
                &tc.title,
                tc.meta,
            );
            let meta = stamp_codex_search_action(agent_type, &tc.kind, hosted_shell, meta);
            // Later frames hand the frontend the MERGED `_meta`, which it takes
            // whole — so codeg's own stamps must be part of the record, or the
            // first update carrying any `_meta` would wipe them.
            if let Some(meta) = meta.as_ref() {
                cb_state
                    .air_tool_call_meta
                    .remember_stamps(&tool_call_id, meta);
            }
            let meta = meta.map(serde_json::Value::Object);
            raw_output_cache.remove_if_final(&tool_call_id, Some(status.as_str()));
            // Track Grok's spawn_subagent lifecycle for the subagent-notification
            // pairing (progress meta + finished settle). No-op for other agents.
            track_grok_spawn_call(
                cb_state,
                grok_spawn,
                Some(status.as_str()),
                &tool_call_id,
                &raw_input,
            );
            // Avoid logging titles/payloads below — they can be model-generated
            // user task descriptions (PII-adjacent) and would create noise in
            // server-mode log sinks. The opaque tool_call_id is enough to
            // correlate these events with downstream traces.
            // Record the peeled Grok MCP name as an authoritative title override
            // so later sparse `use_tool` updates (which carry the generic wrapper
            // title and no raw_input) re-assert it via resolve_rewritten_title
            // instead of reverting the delegation card to a generic tool. Mirrors
            // the CodeBuddy DeferExecuteTool / sub-agent title persistence.
            if let Some((name, _)) = &grok_use_tool {
                cb_state
                    .title_overrides
                    .insert(tool_call_id.clone(), name.clone());
            }
            // Resolve (and record) any authoritative title rewrite so a later
            // status-only update can't downgrade this card (see fn doc).
            let title = resolve_rewritten_title(
                agent_type,
                &raw_input,
                &tool_call_id,
                false,
                meta_marks_subagent,
                &mut cb_state.title_overrides,
            )
            .unwrap_or(tc.title);
            // Mark Cursor's identity-less MCP announcements as eligible for the
            // completion-time result sniff. Scoping the sniff to ids announced
            // with this exact title keeps a `shell`/`read` call whose OUTPUT
            // echoes a delegation ack from being re-titled.
            if matches!(agent_type, AgentType::Cursor)
                && title == crate::acp::lifecycle::CURSOR_IDENTITYLESS_MCP_TITLE
            {
                cb_state.cursor_generic_mcp_ids.insert(tool_call_id.clone());
            }
            // Open/close the sub-agent suppression window for this call. `title ==
            // "agent"` iff this is a classified native sub-agent (DeferExecuteTool
            // rewrites to an `mcp__…` name, never "agent").
            track_subagent_window(
                agent_type,
                title == "agent",
                meta_marks_background,
                Some(status.as_str()),
                &tool_call_id,
                &mut cb_state.open_subagents,
                &mut cb_state.closed_subagents,
            );
            emit_with_state(
                state,
                emitter,
                AcpEvent::ToolCall {
                    tool_call_id,
                    title,
                    kind: format!("{:?}", tc.kind).to_lowercase(),
                    status,
                    content,
                    raw_input,
                    raw_output,
                    locations,
                    meta,
                    images,
                },
            )
            .await;
        }
        SessionUpdate::ToolCallUpdate(mut tcu) => {
            let tool_call_id = tcu.tool_call_id.to_string();
            // The AIR contract leaves every unchanged `_meta` key off an update;
            // read the call's merged `_meta` instead of the frame's, and hand the
            // frontend (which REPLACES a call's `_meta` with whatever arrives)
            // the whole of it — but only on frames that said something, exactly
            // as before. See `crate::acp::air_contract`.
            let frame_had_meta = tcu.meta.is_some();
            tcu.meta = cb_state
                .air_tool_call_meta
                .update(agent_type, &tool_call_id, tcu.meta.take());
            // codex-acp repeats a search's `rawInput` on its completion frame, so
            // either frame marks the request — one whose opening frame never
            // came still counts (a repeat mark is a no-op).
            if agent_type == AgentType::Codex
                && crate::acp::codex_context::is_web_search_input(tcu.fields.raw_input.as_ref())
            {
                cb_state.codex_context_readings.web_search_ran();
            }
            // Symmetric with the `ToolCall` arm: the follow-up carries the same
            // `_meta.codex.subagent`, so it classifies identically — a launch's
            // completion is forwarded (settling its capsule), any other
            // lifecycle marker's is dropped like its opening frame was. A
            // terminal marker can arrive on either frame, so both route it.
            // codex-acp 2.0.0 sends that follow-up as a bare status, so a frame
            // that classifies as nothing falls back to what its opening frame
            // was classified as (`codex_activity_calls`).
            let activity_meta = codex_activity_classification_meta(
                agent_type,
                tcu.meta.as_ref(),
                tcu.fields.raw_input.as_ref(),
            );
            let mut codex_subagent_thread = None;
            let codex_subagent = match classify_codex_subagent_activity(
                agent_type,
                activity_meta.as_ref().or(tcu.meta.as_ref()),
            ) {
                CodexSubagentActivity::None => match cb_state.codex_activity_calls.get(&tool_call_id) {
                    Some(Some(launch_input)) => Some(launch_input.clone()),
                    Some(None) => return,
                    None => None,
                },
                CodexSubagentActivity::Started { thread_id, input } => {
                    codex_subagent_thread = thread_id;
                    Some(input)
                }
                CodexSubagentActivity::Terminal { thread_id, kind } => {
                    settle_codex_subagent_launch(state, emitter, cb_state, &thread_id, &kind)
                        .await;
                    return;
                }
                CodexSubagentActivity::Other => return,
            };
            if let (Some(thread_id), Some(input)) = (codex_subagent_thread, codex_subagent.as_ref())
            {
                cb_state
                    .codex_subagent_launches
                    .insert(thread_id, (tool_call_id.clone(), input.clone()));
            }
            // Suppress the redundant update stream for grok's ask_user_question
            // (see the ToolCall arm): match the tracked id, or the meta on a late
            // update that still carries it.
            if cb_state.grok_ask_tool_ids.contains(&tool_call_id)
                || grok_meta_marks_ask_user(agent_type, tcu.meta.as_ref())
            {
                return;
            }
            // Peel CodeBuddy's `{type,text}` deferred-MCP wrapper here too — the
            // result often arrives on an update (see raw_output below).
            // Same Diff→canonical-edit hoist as the initial ToolCall path: the
            // edit may first arrive on an update. Drop the redundant Diff from
            // `content` when hoisted. The reducer preserves a prior raw_input on
            // status-only updates (`action.raw_input ?? block.info.raw_input`).
            // Grok `use_tool` unwrap, symmetric with the ToolCall arm — a rare
            // update that re-sends the envelope is peeled the same way (most
            // updates carry no raw_input, so this resolves to None and the
            // reducer keeps the prior unwrapped input).
            let grok_use_tool = if matches!(agent_type, AgentType::Grok) {
                unwrap_grok_use_tool(tcu.fields.raw_input.as_ref())
            } else {
                None
            };
            // Symmetric with the ToolCall arm. `terminal_info` only ever rides the
            // OPENING frame, so the id set is what identifies these updates; the
            // meta check is a cheap guard for a wire that ever reorders them.
            let hosted_shell = cb_state.hosted_terminal_calls.contains_key(&tool_call_id)
                || hosted_terminal_meta_marks_shell(agent_type, tcu.meta.as_ref());
            if hosted_shell {
                cb_state
                    .hosted_terminal_calls
                    .entry(tool_call_id.clone())
                    .or_insert(false);
            }
            let hosted_stripped_content = hosted_shell
                .then(|| tcu.fields.content.as_deref().map(strip_terminal_blocks))
                .flatten();
            let content_blocks: Option<&[ToolCallContent]> = hosted_stripped_content
                .as_deref()
                .or(tcu.fields.content.as_deref());
            let own_raw_input = match &grok_use_tool {
                Some((_, inner)) => {
                    json_value_to_text(&Some(inner.clone())).filter(|t| !t.trim().is_empty())
                }
                None => {
                    tool_call_raw_input_text(agent_type, tcu.meta.as_ref(), &tcu.fields.raw_input)
                        .filter(|t| !t.trim().is_empty())
                }
            };
            let synthesized_edit = if own_raw_input.is_none() {
                content_blocks.and_then(synthesize_edit_input_from_diffs)
            } else {
                content_blocks.and_then(|blocks| {
                    claude_complete_file_edit_input(
                        agent_type,
                        tcu.meta.as_ref(),
                        own_raw_input.as_deref(),
                        blocks,
                    )
                })
            };
            // pi's real command usually arrives on an update, not the opening
            // frame (its first frame's arguments are still partial JSON, so the
            // title is the bare "bash"). Re-synthesize whenever a titled frame
            // shows up; the reducer keeps the prior input on the title-less ones.
            // Gated on pi for the same reason as the ToolCall arm.
            let pi_bash_input = if own_raw_input.is_none()
                && hosted_shell
                && matches!(agent_type, AgentType::Pi)
            {
                pi_bash_input_from_title(tcu.fields.title.as_deref())
            } else {
                None
            };
            // gemini repeats title, kind and locations on its SUCCESS
            // completion frame, so the same reconstruction applies here (see
            // `gemini_synthesize_tool_input`). Both wirings are needed: the
            // opening frame is what the card is first classified from, and the
            // completion frame is all a permission-gated call gets — that one
            // never sends an opening `tool_call` at all.
            //
            // The FAILURE frame is the exception: it carries status, content
            // and kind only. `zip` is what makes that a no-op rather than a
            // wrong guess from a kind with no title behind it.
            let gemini_input = if own_raw_input.is_none() {
                tcu.fields
                    .kind
                    .as_ref()
                    .zip(tcu.fields.title.as_deref())
                    .and_then(|(kind, title)| {
                        gemini_synthesize_tool_input(
                            agent_type,
                            kind,
                            title,
                            tcu.fields.locations.as_deref().unwrap_or_default(),
                        )
                    })
            } else {
                None
            };
            let content = content_blocks
                .and_then(|c| serialize_tool_call_content(c, synthesized_edit.is_none()))
                .map(|c| unwrap_codebuddy_deferred_output(agent_type, &c).unwrap_or(c))
                // Symmetric with the ToolCall arm — and the arm that matters:
                // pi's empty opening frame is a `tool_call_update`.
                .filter(|_| {
                    !pi_result_content_is_stringify_noise(agent_type, &tcu.fields.raw_output)
                });
            let images = content_blocks.and_then(extract_tool_call_images);
            let codex_subagent_launch = codex_subagent.is_some();
            let raw_input = codex_subagent
                .or(synthesized_edit)
                .or(own_raw_input)
                .or(pi_bash_input)
                .or(gemini_input)
                .map(|text| resolve_live_tool_input(&text, cwd));
            // Diff the incoming raw_output against the last snapshot we
            // emitted for this tool call. This turns cumulative snapshots
            // from agents (Claude Code, Codex, …) into incremental deltas
            // with `raw_output_append=true`, collapsing the O(N²) transfer
            // problem to O(N) while capping any single emitted chunk to
            // MAX_SINGLE_EMIT_BYTES.
            // A claude viewed-file result the AIR contract withheld, recovered
            // off the raw SDK stream — only for a completion that carries no
            // result of its own (see `claude_viewed_results`).
            let viewed_result = claude_viewed_result_for_completion(
                agent_type,
                &mut cb_state.claude_viewed_results,
                &tool_call_id,
                tcu.fields.status.as_ref(),
                content.is_some() || tcu.fields.raw_output.is_some(),
            );
            let raw_output_text = if hosted_shell {
                // The `_meta` bridge below owns this call's output channel
                // entirely. A codex that does not honour the advertised
                // `terminal_output_delta` (anything before 1.13.0) repeats the
                // WHOLE aggregated output as `rawOutput` on the completion frame
                // of a call it has already streamed incrementally over `_meta`,
                // so taking it here would re-send everything the card already
                // has. (pi sends no `rawOutput` at all for a `_meta`-hosted
                // call, so this is a no-op there.) Skipped rather than left to
                // be overwritten by the bridge: `raw_output_cache.consume`
                // MUTATES the cache, and seeding it with a snapshot the card
                // never received would make the next diff measure against output
                // that was never sent.
                None
            } else if matches!(agent_type, AgentType::Grok) {
                // Grok's structured rawOutput would shadow `content` and render
                // empty; take the parity path (see grok_live_tool_output).
                grok_live_tool_output(&content, &tcu.fields.raw_output)
            } else if matches!(agent_type, AgentType::Pi) {
                // Symmetric with the ToolCall arm — and the arm that matters:
                // pi delivers the result on the update (see pi_live_tool_output).
                pi_live_tool_output(&content, &tcu.fields.raw_output)
            } else if matches!(agent_type, AgentType::OpenCode) {
                // Symmetric with the ToolCall arm — and the arm that matters:
                // OpenCode delivers every result on the completion update (see
                // opencode_live_tool_output).
                opencode_live_tool_output(&content, &tcu.fields.raw_output)
            } else {
                json_value_to_text(&tcu.fields.raw_output)
                    .or(viewed_result)
                    .map(|text| unwrap_codebuddy_deferred_output(agent_type, &text).unwrap_or(text))
                    .map(|text| structurize_live_output(&text))
            };
            let (raw_output, raw_output_append) = match raw_output_text {
                Some(text) => match raw_output_cache.consume(&tool_call_id, &text) {
                    Some((payload, append)) => (Some(payload), Some(append)),
                    None => (None, None),
                },
                None => (None, None),
            };
            // A self-hosted shell call's output rides `_meta`, so bridge it onto
            // the same `raw_output` stream the host-terminal poller feeds — that
            // channel is what supersedes the placeholder in the frontend store's
            // output precedence. This deliberately BYPASSES `raw_output_cache`:
            // the cache diffs cumulative snapshots, while both adapters already
            // send deltas, which is exactly why `emit_terminal_output_update`
            // bypasses it too. The branch above resolved to `(None, None)` for
            // these calls, so nothing is being overwritten. (Older pi-acp has no
            // `_meta` channel at all: there `bash` streams like any other tool,
            // no `terminal_info` is ever seen, and `pi_live_tool_output` above
            // is what carries its output.)
            let (raw_output, raw_output_append) = match hosted_terminal_chunk(
                agent_type,
                tcu.meta.as_ref(),
                &tool_call_id,
                &mut cb_state.hosted_terminal_calls,
            ) {
                Some((payload, append)) => (Some(payload), Some(append)),
                None => (raw_output, raw_output_append),
            };
            let locations = tcu
                .fields
                .locations
                .as_ref()
                .filter(|l| !l.is_empty())
                .and_then(|l| serde_json::to_value(l).ok());
            let meta_marks_subagent = codebuddy_meta_marks_subagent(agent_type, tcu.meta.as_ref())
                || codex_subagent_launch;
            let meta_marks_background = codebuddy_meta_marks_background(agent_type, tcu.meta.as_ref());
            let grok_spawn = grok_meta_marks_spawn_subagent(agent_type, tcu.meta.as_ref());
            let meta = frame_had_meta
                .then(|| tcu.meta.clone())
                .flatten()
                .map(serde_json::Value::Object);
            let status = tcu.fields.status.map(|s| format!("{:?}", s).to_lowercase());
            raw_output_cache.remove_if_final(&tool_call_id, status.as_deref());
            // Same lifetime as the output cache — and deliberately NOT mirrored in
            // the ToolCall arm: pi's `session/load` replay opens the call ALREADY
            // `completed` and delivers the output on the update that follows, so
            // dropping the entry on the opening frame's status would lose it.
            if matches!(
                status.as_deref(),
                Some("completed" | "failed" | "cancelled" | "error")
            ) {
                cb_state.hosted_terminal_calls.remove(&tool_call_id);
            }
            // Symmetric with the ToolCall arm: an update may carry the terminal
            // status (and, on grok, usually re-carries the `x.ai/tool` meta).
            track_grok_spawn_call(cb_state, grok_spawn, status.as_deref(), &tool_call_id, &raw_input);
            // Ordering variant: `subagent_spawned` can pair BEFORE the launch
            // call's terminal frame arrives. The pairing site skipped its
            // outstanding emission then (call not yet settled), so surface the
            // count here — a completed launch with a paired, still-running
            // child is a background subagent codeg must not idle-sweep. The
            // common ordering (completed first) emits from the pairing site,
            // and `subagent_finished` always re-emits the corrected count.
            if status.as_deref() == Some("completed")
                && cb_state
                    .grok_subagent_to_call
                    .values()
                    .any(|call| call == &tool_call_id)
            {
                let session_id = state.read().await.external_id.clone();
                if let Some(session_id) = session_id {
                    let outstanding = cb_state
                        .grok_subagent_to_call
                        .values()
                        .filter(|call| cb_state.grok_settled_spawn_ids.contains(*call))
                        .count() as u32;
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::BackgroundActivity {
                            session_id,
                            turns: Vec::new(),
                            outstanding,
                            settled: Vec::new(),
                            watermark: 0,
                        },
                    )
                    .await;
                }
            }
            // Re-assert any authoritative title rewrite (see fn doc): an update
            // that carries the subagent/deferred marker classifies (and records)
            // the card, and — the key fix — a later status-only update that LOST
            // the marker but carries the agent's raw (non-agent) title still
            // resolves to the recorded override, so the Agent/delegation card and
            // its child nesting (`getToolName === "agent"`) don't revert to a
            // generic tool call mid-stream. Falls back to the event's own title
            // for never-classified tool calls.
            // Symmetric with the ToolCall arm: a (rare) update that re-sends the
            // envelope records the peeled name so it survives later sparse updates.
            if let Some((name, _)) = &grok_use_tool {
                cb_state
                    .title_overrides
                    .insert(tool_call_id.clone(), name.clone());
            }
            // Cursor loses MCP tool identity on the wire entirely (announced as
            // "MCP: tool" before McpArgs exists; updates never resend title or
            // raw_input). The completion update's result text is the one signal
            // left — recover the codeg-mcp companion identity from it and record
            // it as an authoritative override so the delegation / status cards
            // resolve instead of a generic tool. Gated to ids this connection
            // announced with the identity-less title (see the
            // `cursor_generic_mcp_ids` field doc); the entry is dropped once
            // the call goes terminal.
            if matches!(agent_type, AgentType::Cursor)
                && cb_state.cursor_generic_mcp_ids.contains(&tool_call_id)
            {
                if let Some(name) = cursor_companion_title_from_content(content.as_deref()) {
                    cb_state
                        .title_overrides
                        .insert(tool_call_id.clone(), name.to_string());
                }
                if matches!(status.as_deref(), Some("completed") | Some("failed")) {
                    cb_state.cursor_generic_mcp_ids.remove(&tool_call_id);
                }
            }
            let title = resolve_rewritten_title(
                agent_type,
                &raw_input,
                &tool_call_id,
                true,
                meta_marks_subagent,
                &mut cb_state.title_overrides,
            )
            .or(tcu.fields.title);
            // Keep/close the sub-agent suppression window by status (an update
            // resolving to "agent" is a classified native sub-agent).
            track_subagent_window(
                agent_type,
                title.as_deref() == Some("agent"),
                meta_marks_background,
                status.as_deref(),
                &tool_call_id,
                &mut cb_state.open_subagents,
                &mut cb_state.closed_subagents,
            );
            emit_with_state(
                state,
                emitter,
                AcpEvent::ToolCallUpdate {
                    tool_call_id,
                    title,
                    status,
                    content,
                    raw_input,
                    raw_output,
                    raw_output_append,
                    locations,
                    meta,
                    images,
                },
            )
            .await;
        }
        SessionUpdate::CurrentModeUpdate(update) => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::ModeChanged {
                    mode_id: update.current_mode_id.to_string(),
                },
            )
            .await;
        }
        SessionUpdate::Plan(plan) => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::PlanUpdate {
                    entries: map_plan_entries(&plan),
                },
            )
            .await;
        }
        SessionUpdate::ConfigOptionUpdate(update) => {
            // Agent-initiated push, applied verbatim: it reports what the agent
            // is actually running, and nothing here distinguishes "the user
            // typed /model" from "the agent re-pinned its own default". Logged
            // because it can silently overwrite a value codeg just applied.
            //
            // Whether an establishment-time value gets defended against this is
            // decided one level up, in the idle loop's copy of this arm
            // (`take_asserted_config_drift`) — it has to run BEFORE this emit,
            // which overwrites the state the comparison reads.
            tracing::info!(
                "[ACP] agent pushed config_option_update: model={:?}",
                current_model_id_from_opts(&map_session_config_options(&update.config_options))
            );
            emit_session_config_options_values(state, emitter, update.config_options)
                .await;
        }
        SessionUpdate::AvailableCommandsUpdate(update) => {
            // Drop config-option state toggles (codex `/plan` — see
            // `is_config_option_state_command`): they're already the
            // `collaboration_mode` selector, not invokable commands. Then dedup:
            // some agents (e.g. Claude Code with overlapping user/project slash
            // commands) emit duplicate entries sharing the same name. Keep the
            // first occurrence so downstream consumers don't render duplicates;
            // the frontend reducer also dedupes as a defensive measure.
            let mut seen = HashSet::new();
            let commands: Vec<AvailableCommandInfo> = update
                .available_commands
                .iter()
                .filter(|cmd| !is_config_option_state_command(agent_type, cmd.meta.as_ref()))
                .filter(|cmd| seen.insert(cmd.name.clone()))
                .map(|cmd| {
                    let input_hint = cmd.input.as_ref().map(|input| match input {
                        agent_client_protocol::schema::v1::AvailableCommandInput::Unstructured(u) => u.hint.clone(),
                        _ => String::new(),
                    });
                    AvailableCommandInfo {
                        name: cmd.name.clone(),
                        description: cmd.description.clone(),
                        input_hint,
                    }
                })
                .collect();
            emit_with_state(state, emitter, AcpEvent::AvailableCommands { commands }).await;
        }
        SessionUpdate::UsageUpdate(update) => {
            // A codex request that ran a hosted web search reports its
            // provider's whole search loop: the ring keeps the reading before
            // it (see `crate::acp::codex_context`) — on the window this report
            // states, since a model switch moves the denominator even while the
            // figure is held (the parser tracks the window apart the same way).
            if agent_type == AgentType::Codex
                && !cb_state.codex_context_readings.admit(update.used)
            {
                let rekeyed = state
                    .read()
                    .await
                    .usage
                    .as_ref()
                    .filter(|held| held.size != update.size)
                    .map(|held| held.used);
                if let Some(used) = rekeyed {
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::UsageUpdate {
                            used,
                            size: update.size,
                        },
                    )
                    .await;
                }
                return;
            }
            emit_with_state(
                state,
                emitter,
                AcpEvent::UsageUpdate {
                    used: update.used,
                    size: update.size,
                },
            )
            .await;
        }
        SessionUpdate::SessionInfoUpdate(info) => {
            // codex-acp v1.1.0 (#263) reports `/goal` transitions as structured
            // session metadata instead of live "Goal updated (…)" agent text.
            // The goal object rides under ONE of two meta keys, selected per
            // connection at initialize (`SessionState.neutral_goal_channel`,
            // see `session_info_goal_value`): the provider-neutral
            // `_meta.goal` for adapters advertising the goal extension
            // (claude-agent-acp 0.66+, codex-acp 1.2+ — which dropped the
            // legacy key), else the legacy `_meta.codex.goal`. Either way it
            // maps onto codeg's canonical create_goal/update_goal synthetic
            // tool call so the existing goal-card pipeline
            // (groupGoalRuns/GoalCard) renders it — byte-identical to the
            // history path (parsers/codex.rs). Agents publishing no goal meta
            // no-op here. The neutral snapshot's status vocabulary
            // (active|paused|blocked|limited|complete) passes through
            // `normalize_goal_status` unchanged; its extra fields
            // (createdAt/updatedAt/iterations/lastReason/controlMethod)
            // survive inside the marker's raw goal object for the card.
            // `info.title` is the agent's live session name (Codex thread name,
            // Claude ACP 0.69+ generated titles, anyone else who publishes the
            // field). Apply it immediately via a dedicated lifecycle event
            // rather than waiting for the next conversation fetch. Goal-only
            // updates leave title undefined and emit nothing. Identical repeats
            // are skipped (CodeBuddy resends its fallback after every turn).
            // A title that arrives before the row is bound is dropped, not
            // remembered, so a later resend is still accepted. If it never
            // comes back, the next detail load recovers it only for agents
            // whose own transcript carries the name (Codex's session index,
            // Claude's `ai-title`) — a custom ACP agent's is gone, because
            // `parsers/acp_native.rs` records no `session_info_update` and can
            // only ever title a session by its first prompt. Claude Code's is
            // not written here at all: it is held for the transcript watcher,
            // which writes the transcript's own titles instead whenever it can
            // read the transcript (`session_title::accept_wire_title`).
            if let Some(title) = crate::acp::session_title::native_title_from_session_info(
                info.title.value().map(|s| s.as_str()),
            ) {
                // Shared with the transcript watcher's title path so the
                // skip-cache and the unbound-row drop have exactly one
                // spelling — see `session_title::publish_native_title`.
                crate::acp::session_title::accept_wire_title(state, emitter, agent_type, title)
                    .await;
            }
            let neutral_goal_channel = state.read().await.neutral_goal_channel;
            if let Some(goal) =
                session_info_goal_value(neutral_goal_channel, info.meta.as_ref())
            {
                if let Some(marker) =
                    crate::acp::codex_goal::next_goal_marker(&mut cb_state.codex_open_goal, goal)
                {
                    cb_state.codex_goal_seq += 1;
                    let tool_call_id =
                        crate::acp::codex_goal::goal_tool_call_id(cb_state.codex_goal_seq);
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::ToolCall {
                            tool_call_id,
                            title: marker.title,
                            kind: "other".to_string(),
                            status: "completed".to_string(),
                            content: None,
                            raw_input: Some(marker.input_json),
                            raw_output: Some(marker.output_json),
                            locations: None,
                            meta: None,
                            images: None,
                        },
                    )
                    .await;
                }
                // Mirror "a goal run is open" (⟺ the last snapshot was active,
                // see `next_goal_marker`) onto the session state, where
                // `ConnectionManager::goal_control` can read it: only an ACTIVE
                // goal justifies following a pause/clear with an interrupt. A
                // paused goal is not driving anything, so clearing it must
                // leave whatever the user started themselves alone.
                state.write().await.goal_active = cb_state.codex_open_goal.is_some();
            }
            // JetBrains AIR typed session failure (claude-agent-acp 0.67+/
            // codex-acp 1.2+): published only because codeg advertises
            // `clientCapabilities._meta.jetbrains.air` (see
            // `build_client_capabilities`). Valid upserts are forwarded
            // verbatim — the monotonic id+revision merge runs identically in
            // `SessionState::apply_event` and the frontend reducer, so a
            // stale or replayed record is rejected the same way everywhere.
            // A record without usable identity cannot merge and is dropped.
            if let Some(raw) = air_session_failure(info.meta.as_ref()) {
                match parse_session_failure_record(raw) {
                    Some(record) => {
                        emit_with_state(state, emitter, AcpEvent::SessionFailure { record })
                            .await;
                    }
                    None => tracing::debug!(
                        "[ACP] dropped AIR sessionFailure without usable id/revision: {raw:?}"
                    ),
                }
            }
            // codex-acp #289 (v1.1.3+): a retryable turn error rides under
            // `_meta.codex.error` (only when `willRetry == true`) and the turn
            // stays alive. Surface a transient retry indicator (the frontend
            // reuses the Claude API-retry banner); it is NOT a turn failure.
            // With AIR advertised (above), codex 1.2+ REPLACES this channel
            // with severity-"warning" failure records, so this indicator now
            // serves only non-advertised/legacy paths.
            if let Some((message, error_status)) = codex_retry_indicator(info.meta.as_ref()) {
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::TurnRetrying {
                        message,
                        error_status,
                        // codex reports no retry counters — only pi does.
                        attempt: None,
                        max_retries: None,
                        retry_delay_ms: None,
                    },
                )
                .await;
            }
        }
        other => {
            // Unhandled update types, for debugging. DEBUG, not INFO: this arm
            // runs once per `session/update` notification, and `{other:?}` is the
            // whole payload — for a chunk-shaped variant that is the agent's full
            // text. At INFO a single agent emitting an update kind codeg doesn't
            // map would write the entire conversation to disk several times over
            // under the default level (issue #427). Nothing acts on this line;
            // it exists to be read while adding support for a new variant, which
            // is exactly when `debug` is on.
            //
            // The default-level signal for "a variant we don't map swallowed the
            // reply" is not this line but the empty-turn diagnosis: such an
            // update decodes fine, so `TurnOutputProbe::note_update` files it as
            // metadata and the turn reports `MetadataOnly`. Turn `debug` on from
            // there to find out *which* variant.
            tracing::debug!("[ACP] Unhandled SessionUpdate: {:?}", other);
        }
    }
}

#[cfg(test)]
mod tests;
