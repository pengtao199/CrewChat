//! [crewchat] CCR supervision (Phase 5.1) — 探活 → 接管 / 拉起 → 守护.
//!
//! The problem this solves: CCR is a *separate* process (its own daemon, its own
//! management server on 3458 and gateway on 3456). Until now the user had to
//! start it by hand, and if it died Codeg's model page just said "unreachable"
//! with nothing to do about it.
//!
//! **We do not re-implement process supervision.** CCR already ships it: `ccr
//! start` spawns a detached `serve --daemon-child`, records `service.json`
//! (pid + url), and `ccr start` is idempotent — it reuses a verified running
//! service. So this module only has to:
//!
//! 1. **探活** — probe the management address (TCP connect, cheap and
//!    token-free, so a token mismatch can never make us start a second daemon
//!    on a port that is already taken).
//! 2. **接管** — a reachable instance is adopted as-is, whoever started it.
//! 3. **拉起** — only when unreachable, only for a loopback URL, only with a
//!    pinned auth token (an unpinned one would come back with a random token
//!    Codeg cannot use), and only with a resolvable `ccr` CLI.
//! 4. **守护** — a background loop re-probes and retries with a growing backoff
//!    so a crash recovers on its own without hammering a broken binary.
//!
//! Scope: it only ever *starts* CCR. Stopping is a user decision (and CCR's own
//! stop lives in its UI), so nothing here will fight a user who stopped it —
//! the loop will, however, start it again on the next tick; `CODEG_CCR_SUPERVISE=0`
//! is the off switch for users who manage CCR themselves.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Mutex;

use super::client;

/// Off switch. `0` / `false` / `off` / `no` disables every probe-driven start;
/// the status card keeps reporting what it sees.
const ENABLED_ENV: &str = "CODEG_CCR_SUPERVISE";
/// Explicit path to the `ccr` CLI. Packaged builds set it; without it we look
/// for `ccr` on `PATH`.
const BIN_ENV: &str = "CCR_BIN";

const GUARD_TICK_BASE_SECS: u64 = 20;
const GUARD_TICK_MAX_SECS: u64 = 300;
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// `ccr start` waits up to 30s for its own daemon to report ready.
const START_TIMEOUT_SECS: u64 = 60;
/// How long we poll for the management server to answer after `ccr start` said
/// it succeeded.
const START_SETTLE: Duration = Duration::from_secs(15);
const START_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CcrDaemonState {
    /// Supervision is switched off and nothing is listening.
    Disabled,
    /// Listening — adopted from whoever started it (or started by us).
    Running,
    /// Not listening, and we are able (or about to try) to start it.
    Stopped,
    /// A start attempt is in flight.
    Starting,
    /// The last start attempt failed; `last_error` says why.
    Failed,
}

/// A snapshot of the management process, for the settings card.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcrDaemonStatus {
    pub enabled: bool,
    pub web_url: String,
    pub token_configured: bool,
    /// Resolved `ccr` CLI; `None` when this process cannot start one.
    pub bin: Option<String>,
    pub state: CcrDaemonState,
    /// True when the instance currently listening was started by this process.
    /// An adopted instance keeps it `false` — we never took it over.
    pub managed: bool,
    /// PID off CCR's own `service.json`, when it names the instance we probed.
    pub pid: Option<u32>,
    pub last_check_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    /// Why a local start is unavailable (`None` = it is available). Distinct
    /// from `last_error`: this one is a standing condition, not a failed try.
    pub blocked_reason: Option<String>,
}

/// Everything the supervisor needs to know, resolved once from the
/// environment so the decisions stay pure and testable.
#[derive(Debug, Clone)]
pub struct CcrSupervisorOptions {
    pub enabled: bool,
    pub web_url: String,
    pub token_configured: bool,
    pub bin: Option<String>,
    pub addr: Option<(String, u16)>,
    pub blocked_reason: Option<String>,
}

impl CcrSupervisorOptions {
    pub fn from_env() -> Self {
        let enabled = env_flag_enabled(ENABLED_ENV);
        let web_url = client::web_url();
        let token_configured = client::web_token().is_some();
        let addr = client::parse_web_addr(&web_url);
        let bin = resolve_bin();
        let blocked_reason = blocked_reason(
            enabled,
            token_configured,
            addr.as_ref(),
            bin.as_deref(),
        );
        Self {
            enabled,
            web_url,
            token_configured,
            bin,
            addr,
            blocked_reason,
        }
    }

    /// Test-only shape: nothing to probe, nothing to start.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn disabled_for_test() -> Self {
        Self {
            enabled: false,
            web_url: client::DEFAULT_CCR_WEB_URL.to_string(),
            token_configured: false,
            bin: None,
            addr: None,
            blocked_reason: Some("supervision is disabled".to_string()),
        }
    }

    pub fn can_start(&self) -> bool {
        self.blocked_reason.is_none()
    }
}

/// Why a local start is unavailable, or `None` when one is allowed.
///
/// Order matters: "disabled" is the user's own decision and should be reported
/// as such rather than as a missing token.
pub(crate) fn blocked_reason(
    enabled: bool,
    token_configured: bool,
    addr: Option<&(String, u16)>,
    bin: Option<&str>,
) -> Option<String> {
    if !enabled {
        return Some(format!("supervision is off ({ENABLED_ENV}=0)"));
    }
    let Some((host, _)) = addr else {
        return Some(format!(
            "{} is not an address this process can dial",
            client::DEFAULT_CCR_WEB_URL
        ));
    };
    if !client::is_loopback_host(host) {
        return Some(format!(
            "CCR management is remote ({host}); start it on that host"
        ));
    }
    if !token_configured {
        return Some(
            "CCR_WEB_AUTH_TOKEN is not set; a locally started CCR would answer with an unknown token"
                .to_string(),
        );
    }
    if bin.is_none() {
        return Some(format!("no ccr CLI found; set {BIN_ENV} to the ccr executable"));
    }
    None
}

pub(crate) fn env_flag_enabled(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no" | ""
        ),
        Err(_) => true,
    }
}

/// The `ccr` CLI to start: explicit path first, then `PATH`.
pub(crate) fn resolve_bin() -> Option<String> {
    if let Some(explicit) = std::env::var(BIN_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(explicit);
    }
    find_in_path("ccr", std::env::var_os("PATH")?.as_os_str())
        .map(|path| path.to_string_lossy().into_owned())
}

/// `which ccr` without a shell. An explicit `CCR_BIN` is trusted as given; this
/// path (and only this path) requires a real executable file.
pub(crate) fn find_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Guard pacing: grow the interval after repeated failures, capped, so a broken
/// `ccr` binary is retried a few times a minute at worst.
pub(crate) fn tick(failures: u32) -> Duration {
    let secs = (GUARD_TICK_BASE_SECS << failures.min(4)).min(GUARD_TICK_MAX_SECS);
    Duration::from_secs(secs)
}

/// A `service.json` entry, as far as we care about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceSnapshot {
    pub pid: u32,
    pub url: String,
}

/// CCR's own state file: `service.json` in its config dir. Best-effort
/// enrichment for the card (the PID); a miss is not an error. Windows'
/// app-data config dir is not derived here, so on Windows this simply yields
/// `None`.
pub(crate) fn service_state_path() -> Option<PathBuf> {
    let home = std::env::var_os("CCR_INTERNAL_HOME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
    Some(home.join(".claude-code-router").join("service.json"))
}

pub(crate) fn parse_service_state(text: &str) -> Option<ServiceSnapshot> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let pid = value.get("pid")?.as_u64()? as u32;
    let url = value.get("url")?.as_str()?.to_string();
    Some(ServiceSnapshot { pid, url })
}

fn read_service_state() -> Option<ServiceSnapshot> {
    let path = service_state_path()?;
    parse_service_state(&std::fs::read_to_string(path).ok()?)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// TCP-connect probe. Deliberately not an RPC: reachability is what decides
/// "adopt vs start", and it must not depend on the token being right.
pub(crate) async fn probe(addr: Option<&(String, u16)>) -> bool {
    let Some((host, port)) = addr else {
        return false;
    };
    matches!(
        tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((host.as_str(), *port))
        )
        .await,
        Ok(Ok(_))
    )
}

/// The management process, probed, adopted and guarded.
pub struct CcrSupervisor {
    options: CcrSupervisorOptions,
    started_by_us: AtomicBool,
    status: Mutex<CcrDaemonStatus>,
}

impl CcrSupervisor {
    pub fn new(options: CcrSupervisorOptions) -> Arc<Self> {
        let status = CcrDaemonStatus {
            enabled: options.enabled,
            web_url: options.web_url.clone(),
            token_configured: options.token_configured,
            bin: options.bin.clone(),
            state: if options.enabled {
                CcrDaemonState::Stopped
            } else {
                CcrDaemonState::Disabled
            },
            managed: false,
            pid: None,
            last_check_ms: None,
            consecutive_failures: 0,
            last_error: None,
            blocked_reason: options.blocked_reason.clone(),
        };
        Arc::new(Self {
            options,
            started_by_us: AtomicBool::new(false),
            status: Mutex::new(status),
        })
    }

    pub fn from_env() -> Arc<Self> {
        Self::new(CcrSupervisorOptions::from_env())
    }

    /// Last known status, no I/O. The handlers refresh first.
    pub async fn snapshot(&self) -> CcrDaemonStatus {
        self.status.lock().await.clone()
    }

    /// 探活: probe and record. Never starts anything.
    pub async fn refresh(&self) -> CcrDaemonStatus {
        let reachable = probe(self.options.addr.as_ref()).await;
        let mut status = self.status.lock().await;
        status.last_check_ms = Some(now_ms());
        if reachable {
            // 接管: whoever started it, we adopt it and never touch it.
            let pid = read_service_state()
                .filter(|service| same_service(&service.url, &status.web_url))
                .map(|service| service.pid);
            status.state = CcrDaemonState::Running;
            status.pid = pid;
            status.managed = self.started_by_us.load(Ordering::Relaxed);
            status.consecutive_failures = 0;
            status.last_error = None;
        } else {
            status.state = if !status.enabled {
                CcrDaemonState::Disabled
            } else if status.consecutive_failures > 0 {
                CcrDaemonState::Failed
            } else {
                CcrDaemonState::Stopped
            };
            status.pid = None;
            status.managed = false;
        }
        status.clone()
    }

    /// 接管 / 拉起: adopt a listening instance, otherwise start one once.
    ///
    /// Returns the status after the attempt; the `Err` carries the reason the
    /// start could not be made (also recorded in `last_error`).
    pub async fn ensure_running(&self) -> Result<CcrDaemonStatus, String> {
        let current = self.refresh().await;
        if current.state == CcrDaemonState::Running {
            return Ok(current);
        }
        if let Some(reason) = self.options.blocked_reason.clone() {
            return Err(reason);
        }
        let Some(bin) = self.options.bin.clone() else {
            return Err(format!("no ccr CLI found; set {BIN_ENV}"));
        };
        self.status.lock().await.state = CcrDaemonState::Starting;
        if let Err(error) = spawn_start(&bin, self.options.addr.as_ref()).await {
            self.record_failure(error.clone()).await;
            return Err(error);
        }
        if !wait_until_reachable(self.options.addr.as_ref()).await {
            let error = format!("`{bin} start` reported success but {0} never answered", current.web_url);
            self.record_failure(error.clone()).await;
            return Err(error);
        }
        self.started_by_us.store(true, Ordering::Relaxed);
        tracing::info!("[ccr] started management server via {bin}");
        Ok(self.refresh().await)
    }

    async fn record_failure(&self, error: String) {
        let mut status = self.status.lock().await;
        status.state = CcrDaemonState::Failed;
        status.consecutive_failures = status.consecutive_failures.saturating_add(1);
        status.last_error = Some(error.clone());
        tracing::warn!("[ccr] start attempt failed: {error}");
    }

    /// 守护: probe on a timer, start when down and startable.
    ///
    /// Never returns, and deliberately does not spawn itself: the desktop app's
    /// `setup` runs outside a tokio reactor (`tokio::spawn` would panic there)
    /// while `codeg-server` runs inside one. Callers pick their runtime —
    /// `tokio::spawn(supervisor.clone().guard_loop())` in server mode,
    /// `tauri::async_runtime::spawn(...)` in the desktop app.
    pub async fn guard_loop(self: Arc<Self>) {
        if !self.options.enabled {
            tracing::info!("[ccr] supervision off; management server is not managed");
            return;
        }
        if let Some(reason) = self.options.blocked_reason.clone() {
            tracing::info!("[ccr] supervision idle: {reason}");
        }
        loop {
            let status = self.refresh().await;
            if status.state != CcrDaemonState::Running && self.options.can_start() {
                let _ = self.ensure_running().await;
            }
            let failures = self.snapshot().await.consecutive_failures;
            tokio::time::sleep(tick(failures)).await;
        }
    }
}

/// True when CCR's `service.json` names the instance at `web_url` — same port
/// is enough; the host may be spelled `localhost` in one place and `127.0.0.1`
/// in the other.
fn same_service(service_url: &str, web_url: &str) -> bool {
    match (
        client::parse_web_addr(service_url),
        client::parse_web_addr(web_url),
    ) {
        (Some((_, service_port)), Some((_, web_port))) => service_port == web_port,
        _ => false,
    }
}

async fn wait_until_reachable(addr: Option<&(String, u16)>) -> bool {
    let deadline = tokio::time::Instant::now() + START_SETTLE;
    loop {
        if probe(addr).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// `ccr start` (daemon mode, gateway included) for the address we probe. The
/// child inherits our environment, which is what pins the management token:
/// CCR uses `CCR_WEB_AUTH_TOKEN` when it is set.
async fn spawn_start(bin: &str, addr: Option<&(String, u16)>) -> Result<(), String> {
    let mut command = tokio::process::Command::new(bin);
    command.arg("start").arg("--no-open");
    if let Some((host, port)) = addr {
        command.arg("--host").arg(host).arg("--port").arg(port.to_string());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(START_TIMEOUT_SECS), command.output())
        .await
        .map_err(|_| format!("`{bin} start` timed out after {START_TIMEOUT_SECS}s"))?
        .map_err(|error| format!("failed to run `{bin} start`: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(if detail.is_empty() {
        format!("`{bin} start` exited {}", output.status)
    } else {
        format!("`{bin} start` exited {}: {detail}", output.status)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> CcrSupervisorOptions {
        CcrSupervisorOptions {
            enabled: true,
            web_url: "http://127.0.0.1:3458".to_string(),
            token_configured: true,
            bin: Some("/opt/ccr".to_string()),
            addr: Some(("127.0.0.1".to_string(), 3458)),
            blocked_reason: None,
        }
    }

    #[test]
    fn a_complete_configuration_allows_starting() {
        assert!(opts().can_start());
        assert_eq!(
            blocked_reason(
                true,
                true,
                Some(&("127.0.0.1".to_string(), 3458)),
                Some("/opt/ccr")
            ),
            None
        );
    }

    #[test]
    fn each_missing_piece_names_itself() {
        // Disabled is the user's decision and takes precedence.
        let disabled = blocked_reason(
            false,
            false,
            Some(&("127.0.0.1".to_string(), 3458)),
            None,
        )
        .unwrap();
        assert!(disabled.contains("supervision is off"), "{disabled}");

        // A remote management server is not ours to start.
        let remote = blocked_reason(
            true,
            true,
            Some(&("10.0.0.7".to_string(), 3458)),
            Some("/opt/ccr"),
        )
        .unwrap();
        assert!(remote.contains("remote"), "{remote}");

        // An unpinned token would come back random and unusable.
        let no_token = blocked_reason(
            true,
            false,
            Some(&("127.0.0.1".to_string(), 3458)),
            Some("/opt/ccr"),
        )
        .unwrap();
        assert!(no_token.contains("CCR_WEB_AUTH_TOKEN"), "{no_token}");

        // Nothing to run.
        let no_bin = blocked_reason(
            true,
            true,
            Some(&("127.0.0.1".to_string(), 3458)),
            None,
        )
        .unwrap();
        assert!(no_bin.contains("ccr CLI"), "{no_bin}");

        // An address we cannot dial at all.
        assert!(blocked_reason(true, true, None, Some("/opt/ccr")).is_some());
    }

    #[test]
    fn path_lookup_wants_a_real_executable() {
        let dir = std::env::temp_dir().join(format!("ccr-bin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exec = dir.join("ccr");
        std::fs::write(&exec, b"#!/bin/sh\n").unwrap();
        let plain = dir.join("README");
        std::fs::write(&plain, b"not a program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let path = std::env::join_paths([dir.as_path()]).unwrap();
        assert_eq!(find_in_path("ccr", &path), Some(exec.clone()));

        // A present-but-not-executable file, and a genuinely absent name, both
        // mean "no ccr here".
        assert_eq!(find_in_path("README", &path), None);
        assert_eq!(find_in_path("definitely-absent", &path), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_guard_backs_off_and_caps() {
        assert_eq!(tick(0), Duration::from_secs(20));
        assert_eq!(tick(1), Duration::from_secs(40));
        assert_eq!(tick(2), Duration::from_secs(80));
        assert_eq!(tick(3), Duration::from_secs(160));
        // Capped: however long the outage, we keep checking every 5 minutes.
        assert_eq!(tick(4), Duration::from_secs(300));
        assert_eq!(tick(40), Duration::from_secs(300));
    }

    #[test]
    fn service_state_is_read_leniently() {
        let parsed = parse_service_state(r#"{"pid": 4242, "url": "http://127.0.0.1:3458"}"#).unwrap();
        assert_eq!(parsed.pid, 4242);
        // Missing / malformed fields are simply "no info", not an error.
        assert!(parse_service_state(r#"{"url": "http://x"}"#).is_none());
        assert!(parse_service_state("not json").is_none());
        assert!(parse_service_state("{}").is_none());
    }

    #[test]
    fn only_the_instance_we_probed_is_credited() {
        assert!(same_service(
            "http://127.0.0.1:3458",
            "http://localhost:3458"
        ));
        assert!(!same_service("http://127.0.0.1:9999", "http://127.0.0.1:3458"));
    }

    #[tokio::test]
    async fn a_blocked_supervisor_never_starts_and_says_why() {
        let supervisor = CcrSupervisor::new(CcrSupervisorOptions {
            blocked_reason: Some("no ccr CLI found".to_string()),
            bin: None,
            addr: None,
            ..opts()
        });
        let error = supervisor.ensure_running().await.unwrap_err();
        assert_eq!(error, "no ccr CLI found");
        // And it did not pretend to be starting.
        let status = supervisor.snapshot().await;
        assert_eq!(status.state, CcrDaemonState::Stopped);
        assert_eq!(status.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn a_dead_address_probes_down_and_reports_stopped() {
        // Port 1 on loopback: nothing listens there.
        let supervisor = CcrSupervisor::new(CcrSupervisorOptions {
            addr: Some(("127.0.0.1".to_string(), 1)),
            web_url: "http://127.0.0.1:1".to_string(),
            blocked_reason: Some("supervision is off (test)".to_string()),
            ..opts()
        });
        let status = supervisor.refresh().await;
        assert_eq!(status.state, CcrDaemonState::Stopped);
        assert!(status.last_check_ms.is_some());
        assert_eq!(status.pid, None);
        assert!(!status.managed);
    }

    #[tokio::test]
    async fn supervision_off_reports_disabled() {
        let supervisor = CcrSupervisor::new(CcrSupervisorOptions {
            enabled: false,
            addr: Some(("127.0.0.1".to_string(), 1)),
            web_url: "http://127.0.0.1:1".to_string(),
            blocked_reason: Some("supervision is off".to_string()),
            ..opts()
        });
        assert_eq!(supervisor.refresh().await.state, CcrDaemonState::Disabled);
    }

    #[test]
    fn status_serializes_camel_case_for_the_wire() {
        let status = CcrDaemonStatus {
            enabled: true,
            web_url: "http://127.0.0.1:3458".to_string(),
            token_configured: true,
            bin: Some("/opt/ccr".to_string()),
            state: CcrDaemonState::Running,
            managed: false,
            pid: Some(7),
            last_check_ms: Some(1),
            consecutive_failures: 0,
            last_error: None,
            blocked_reason: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["webUrl"], "http://127.0.0.1:3458");
        assert_eq!(value["tokenConfigured"], true);
        assert_eq!(value["state"], "running");
        assert_eq!(value["lastCheckMs"], 1);
    }
}
