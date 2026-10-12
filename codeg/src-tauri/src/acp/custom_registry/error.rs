// custom_registry submodule: CustomAgentError (split from acp/custom_registry.rs —
// mechanical move, see NOTICE.md).




/// Reason a definition cannot be turned into launch metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomAgentError {
    InvalidId(String),
    EmptyName,
    /// The chosen channel has no entry in the spec.
    MissingChannel(&'static str),
    MissingPackage(&'static str),
    /// The binary channel has no release for the machine's platform.
    UnsupportedPlatform(String),
    MissingArchiveUrl(String),
    /// The version label would not be usable as a single directory name.
    UnsafeVersion(String),
    /// A platform's in-archive launch path escapes the extracted tree.
    UnsafeArchiveEntry(String),
    /// The registry id is one of the BUILT-IN agents' registry ids, so the
    /// definition would be permanently shadowed: `from_registry_id` resolves
    /// built-ins first, and `all_acp_agents` would list the agent twice. This
    /// also fails hydration of a definition that predates the id becoming a
    /// built-in (e.g. `deepseek-acp` registered as a custom agent before the
    /// deep integration) — the stored row stays in the database for the user
    /// to delete, but it is never published.
    CollidesWithBuiltin(String),
}

impl std::fmt::Display for CustomAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustomAgentError::InvalidId(id) => write!(
                f,
                "invalid custom agent id {id:?}: use letters, digits, '-', '_' or '.' (max 64), and not a built-in agent name"
            ),
            CustomAgentError::EmptyName => write!(f, "custom agent name must not be empty"),
            CustomAgentError::MissingChannel(kind) => {
                write!(f, "spec has no `{kind}` distribution entry")
            }
            CustomAgentError::MissingPackage(kind) => {
                write!(f, "`{kind}` distribution has an empty package")
            }
            CustomAgentError::UnsupportedPlatform(p) => {
                write!(f, "no binary release for this platform ({p})")
            }
            CustomAgentError::MissingArchiveUrl(p) => {
                write!(f, "binary release for {p} has no archive URL")
            }
            CustomAgentError::UnsafeVersion(v) => write!(
                f,
                "version {v:?} is not usable as a directory name: use letters, digits, '-', '_' or '.' (max 128, no leading dot)"
            ),
            CustomAgentError::UnsafeArchiveEntry(p) => write!(
                f,
                "binary release for {p} has a launch path that escapes the archive: it must be relative, with no '..' segment"
            ),
            CustomAgentError::CollidesWithBuiltin(id) => write!(
                f,
                "{id:?} is a built-in agent's registry id: the built-in integration already covers it, so delete this custom entry"
            ),
        }
    }
}

impl std::error::Error for CustomAgentError {}
