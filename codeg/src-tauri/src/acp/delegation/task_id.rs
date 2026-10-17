//! [crewchat] Repair of task ids echoed back by the parent model.
//!
//! Codeg mints a task id as a lowercase UUID (`lifecycle.rs`:
//! `Uuid::new_v4().to_string()`, or the `delegation-<uuid>` fallback), and the
//! parent agent is expected to hand it back verbatim to
//! `get_delegation_status` / `cancel_delegation` / `resume_delegation`.
//!
//! Models do not always do that. A 2026-10-17 session log shows every `-` of a
//! task id arriving as CR — `3a9a21ad\r-3071\r-4328\r-ac03\r-4c32b1e9747a`
//! (0x0D is 0x2D with bit 5 cleared, i.e. a case-flip applied to a hyphen).
//! That id misses both the in-memory maps and the DB lookup, so the broker
//! answers `Unknown task id` for a task that is alive: the parent burns a
//! round-trip re-polling, and the delegation card shows 未知任务 until it does.
//!
//! [`canonical_task_id`] repairs **separators only**. The hex digits are the
//! identity, so a repaired id can never name a different task — an id with one
//! different digit still misses, exactly as before. Anything that isn't a UUID
//! (or the `delegation-` fallback form) returns `None`, and the caller keeps
//! the original string.

/// Hex digit count in a UUID.
const UUID_HEX: usize = 32;

/// UUID group widths, 8-4-4-4-12.
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// Prefix of the fallback id form minted when the original call id is missing
/// (`lifecycle.rs` / `resume.rs`).
const FALLBACK_PREFIX: &str = "delegation";

/// Rewrite `raw` into the id form Codeg minted, or `None` when it cannot be
/// recognized as one.
///
/// Handles: control characters and whitespace where separators belong (the
/// observed CR corruption), missing separators, surrounding whitespace, and
/// hex case. Separator-free input of any other shape is refused rather than
/// guessed at, so a non-UUID id (`task-1` in tests, a future id scheme) passes
/// through untouched.
pub fn canonical_task_id(raw: &str) -> Option<String> {
    // Drop what can never be part of an id: whitespace and control characters.
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    // The `delegation-` prefix is split off first: its letters are not hex, so
    // the hex scan below would otherwise reject the whole id. The prefix's own
    // separator may be mangled away, which is why this matches the bare word.
    let head = cleaned.get(..FALLBACK_PREFIX.len());
    let (prefix, body) = match head {
        Some(word) if word.eq_ignore_ascii_case(FALLBACK_PREFIX) => {
            (FALLBACK_PREFIX, &cleaned[FALLBACK_PREFIX.len()..])
        }
        _ => ("", cleaned.as_str()),
    };

    let mut hex = String::with_capacity(UUID_HEX);
    for ch in body.chars() {
        if ch.is_ascii_hexdigit() {
            hex.push(ch.to_ascii_lowercase());
        } else if ch != '-' {
            // Neither a separator nor a hex digit: not an id we can repair.
            return None;
        }
    }
    if hex.len() != UUID_HEX {
        return None;
    }

    let mut out = String::with_capacity(prefix.len() + UUID_HEX + UUID_GROUPS.len());
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push('-');
    }
    let mut at = 0;
    for (index, width) in UUID_GROUPS.iter().enumerate() {
        if index > 0 {
            out.push('-');
        }
        out.push_str(&hex[at..at + width]);
        at += width;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact id a parent model sent on 2026-10-17, and its true value.
    const MANGLED: &str = "3a9a21ad\r-3071\r-4328\r-ac03\r-4c32b1e9747a";
    const TRUE_ID: &str = "3a9a21ad-3071-4328-ac03-4c32b1e9747a";

    #[test]
    fn repairs_the_separator_corruption_seen_in_the_wild() {
        assert_eq!(canonical_task_id(MANGLED).as_deref(), Some(TRUE_ID));
    }

    #[test]
    fn leaves_a_well_formed_id_alone() {
        assert_eq!(canonical_task_id(TRUE_ID).as_deref(), Some(TRUE_ID));
        // The broker's fallback form keeps its prefix.
        let fallback = format!("delegation-{TRUE_ID}");
        assert_eq!(canonical_task_id(&fallback).as_deref(), Some(fallback.as_str()));
    }

    #[test]
    fn tolerates_other_separator_and_case_noise() {
        // No separators at all, uppercase hex, surrounding whitespace.
        let bare = TRUE_ID.replace('-', "");
        assert_eq!(canonical_task_id(&bare).as_deref(), Some(TRUE_ID));
        assert_eq!(canonical_task_id(&bare.to_uppercase()).as_deref(), Some(TRUE_ID));
        assert_eq!(canonical_task_id(&format!(" {TRUE_ID}\n")).as_deref(), Some(TRUE_ID));
        // The fallback prefix with its separator mangled away.
        let fallback = format!("delegation{bare}");
        assert_eq!(
            canonical_task_id(&fallback).as_deref(),
            Some(format!("delegation-{TRUE_ID}").as_str())
        );
    }

    /// The safety property that makes the repair acceptable at all: repairing
    /// separators can never re-point a lookup at a *different* task.
    #[test]
    fn never_aliases_one_task_onto_another() {
        let other = "3a9a21ad-3071-4328-ac03-4c32b1e9747b";
        let repaired = canonical_task_id(&other.replace('-', "\r")).unwrap();
        assert_ne!(repaired, TRUE_ID);
        assert_eq!(repaired, other);
    }

    #[test]
    fn refuses_anything_that_is_not_a_uuid() {
        for raw in [
            "",
            "task-1",
            "delegation",
            "delegation-not-a-uuid",
            // 31 hex digits: one short of a UUID, so not repairable.
            "3a9a21ad-3071-4328-ac03-4c32b1e9747",
            // A non-separator, non-hex character in the middle.
            "3a9a21ad_3071_4328_ac03_4c32b1e9747a",
        ] {
            assert_eq!(canonical_task_id(raw), None, "raw={raw:?}");
        }
    }
}
