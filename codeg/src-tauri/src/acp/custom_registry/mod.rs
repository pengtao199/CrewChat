//! Runtime registry for **custom (user-registered) ACP agents**.
//!
//! Built-in agents are compile-time constants in [`crate::acp::registry`].
//! Custom agents are rows in the `custom_agent` table, so their launch
//! metadata only exists at runtime — but every consumer of
//! [`crate::acp::registry::get_agent_meta`] expects a `&'static str`-shaped
//! [`AcpAgentMeta`] from a synchronous, infallible call.
//!
//! This module bridges the two: [`hydrate`] converts database definitions into
//! `&'static AcpAgentMeta` values (via [`crate::intern`]) and publishes them in
//! a process-global map that `get_agent_meta` consults. Consumers are
//! unchanged — `AgentDistribution` keeps its `&'static str` fields, and the
//! 57 destructuring sites across `commands/acp.rs`, `binary_cache.rs`,
//! `preflight.rs` and `connection.rs` need no `Cow` conversion.
//!
//! ## Leak discipline
//!
//! Promoting to `&'static` means leaking. Each definition is fingerprinted
//! (`serde_json` of the stored spec), and [`hydrate`] reuses the previously
//! leaked meta whenever the fingerprint is unchanged — so re-hydrating an
//! untouched registry (which happens on every startup and after every unrelated
//! CRUD call) allocates nothing. Only an actual edit to a definition leaks a
//! fresh meta, bounding the leak by the number of edits in one app run.
//!
//! ## Spec format
//!
//! [`CustomAgentSpec`] is intentionally **paste-compatible with the ACP
//! registry's `distribution` object**
//! (`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`), so
//! a user can copy an entry verbatim. The few things the registry does not
//! carry — the npx/uvx console-script name, and runtime version floors — are
//! optional extra keys with derived defaults.

mod def;
mod error;
mod meta;
mod registry;
mod validate;

pub use self::def::*;
pub use self::error::*;
pub use self::meta::*;
pub use self::registry::*;
pub use self::validate::*;

#[cfg(test)]
mod tests;
