//! [crewchat]-extended internal prompt frames: the upstream routing frame
//! plus the team (1.3) and role (2.2) kinds. Every kind shares one
//! reserved separator and one byte-exact find/strip pass (`strip.rs`); the
//! public surface (`crate::acp::agent_mentions::…`) is unchanged.

use crate::models::agent::{is_valid_custom_agent_id, BUILTIN_AGENT_TYPES};


// Reserved at every Codeg prompt ingress before any routing frame is appended.
// The parser may therefore treat a structurally valid RS-bounded frame as
// Codeg-authored without deleting an API caller's control-character text.
const ROUTE_FRAME_SEPARATOR: char = '\u{001e}';


pub(crate) fn valid_agent_wire_syntax(agent_type: &str) -> bool {
    BUILTIN_AGENT_TYPES
        .iter()
        .any(|candidate| candidate.as_wire() == agent_type)
        || agent_type
            .strip_prefix(crate::models::agent::CUSTOM_AGENT_WIRE_PREFIX)
            .is_some_and(is_valid_custom_agent_id)
}

mod role;
mod routes;
mod strip;
mod team;

pub(crate) use self::role::*;
pub(crate) use self::routes::*;
pub(crate) use self::strip::*;
pub(crate) use self::team::*;

#[cfg(test)]
mod tests;
