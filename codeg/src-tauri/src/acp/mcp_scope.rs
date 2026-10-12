//! [crewchat] Role-scoped MCP selection (Phase 2.3).
//!
//! A session that runs as a roster role mounts exactly the MCP servers that
//! role selected; a missing/empty selection inherits the agent-type's whole
//! configured set (the pre-2.3 behavior). The choice is made where the role is
//! known — the delegation spawn path resolves the member role — and travels to
//! `session/new` through `SessionState::role_mcp`.
//!
//! Kept as a pure function so the narrowing rules are testable without a
//! connection, an agent store, or a DB.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Narrow an agent-type's configured servers to the role's selection.
///
/// Returns the kept map (name → spec) plus the selected names this agent has
/// no configuration for, so the caller can log a role that references another
/// agent's servers instead of silently mounting nothing.
pub fn scope_servers_to_role(
    entries: BTreeMap<String, Value>,
    allowed: Option<&[String]>,
) -> (BTreeMap<String, Value>, Vec<String>) {
    let Some(allowed) = allowed.filter(|names| !names.is_empty()) else {
        return (entries, Vec::new());
    };
    let allow: BTreeSet<&str> = allowed.iter().map(String::as_str).collect();
    let mut kept = BTreeMap::new();
    let mut missing = Vec::new();
    for name in allow {
        match entries.get(name) {
            Some(spec) => {
                kept.insert(name.to_string(), spec.clone());
            }
            None => missing.push(name.to_string()),
        }
    }
    (kept, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn servers(names: &[&str]) -> BTreeMap<String, Value> {
        names
            .iter()
            .map(|name| (name.to_string(), json!({ "command": name })))
            .collect()
    }

    #[test]
    fn no_selection_inherits_the_agents_whole_set() {
        let entries = servers(&["playwright", "xcodebuild"]);
        let (kept, missing) = scope_servers_to_role(entries.clone(), None);
        assert_eq!(kept, entries);
        assert!(missing.is_empty());

        let (kept, _) = scope_servers_to_role(entries.clone(), Some(&[]));
        assert_eq!(kept, entries, "an empty selection is 'not chosen', not 'none'");
    }

    #[test]
    fn a_selection_narrows_to_exactly_the_named_servers() {
        let (kept, missing) = scope_servers_to_role(
            servers(&["playwright", "xcodebuild", "filesystem"]),
            Some(&["filesystem".into(), "playwright".into()]),
        );
        assert_eq!(
            kept.keys().cloned().collect::<Vec<_>>(),
            vec!["filesystem".to_string(), "playwright".to_string()],
            "kept servers stay name-sorted (BTreeMap)"
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn names_this_agent_has_no_config_for_are_reported_not_mounted() {
        let (kept, missing) = scope_servers_to_role(
            servers(&["playwright"]),
            Some(&["playwright".into(), "only-on-codex".into()]),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(missing, vec!["only-on-codex".to_string()]);
    }

    #[test]
    fn duplicate_selections_collapse() {
        let (kept, missing) = scope_servers_to_role(
            servers(&["playwright"]),
            Some(&["playwright".into(), "playwright".into()]),
        );
        assert_eq!(kept.len(), 1);
        assert!(missing.is_empty());
    }
}
