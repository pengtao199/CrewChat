//! [crewchat] CCR runtime — the two halves of Codeg's side of the model layer.
//!
//! - [`client`] — the management **transport** (`CCR_WEB_URL` /
//!   `CCR_WEB_AUTH_TOKEN` contract, RPC forwarding). The one place that knows
//!   how to reach CCR.
//! - [`supervisor`] — **supervision** (Phase 5.1): probe the management
//!   process, adopt it when it is already up, start it when it is not, and keep
//!   watching so a crash recovers without the user starting CCR by hand.
//!
//! Model *management* itself (providers, profiles, keys) belongs to CCR, not
//! here — see `AGENTS/gateway.md`.

pub mod client;
pub mod providers;
pub mod supervisor;

pub use supervisor::{
    CcrDaemonState, CcrDaemonStatus, CcrSupervisor, CcrSupervisorOptions,
};
