// agent_mentions submodule: shared find/strip machinery for every frame kind (split from acp/agent_mentions.rs —
// mechanical move, see NOTICE.md).
use super::*;

use crate::acp::types::PromptInputBlock;


/// One recognised internal control-frame kind. The strip/parse pass is
/// kind-agnostic: a body candidate is accepted when ANY kind's validator
/// reproduces it byte-for-byte (each kind's validator carries its own
/// descriptor check, so the kinds can never claim each other's bodies).
pub(crate) struct InternalFrameKind {
    /// Opening bytes of the kind's descriptor — the unseparated body anchor.
    body_prefix: &'static str,
    /// Byte-exact body validator for the kind.
    accepts_body: fn(&str) -> bool,
}

pub(crate) fn accepts_route_frame_body(body: &str) -> bool {
    parse_internal_agent_routes_body(body).is_some()
}

pub(crate) fn accepts_team_frame_body(body: &str) -> bool {
    parse_internal_team_frame_body(body).is_some()
}

pub(crate) fn accepts_role_frame_body(body: &str) -> bool {
    parse_internal_role_frame_body(body).is_some()
}

/// Every internal frame kind Codeg appends. Both bodies are exactly three
/// newline-terminated lines, which is what [`unseparated_frame_end`] relies
/// on; a kind with a different line count needs that finder parameterized.
pub(crate) const INTERNAL_FRAME_KINDS: &[InternalFrameKind] = &[
    InternalFrameKind {
        body_prefix: ROUTE_FRAME_BODY_PREFIX,
        accepts_body: accepts_route_frame_body,
    },
    InternalFrameKind {
        body_prefix: TEAM_FRAME_BODY_PREFIX,
        accepts_body: accepts_team_frame_body,
    },
    InternalFrameKind {
        body_prefix: ROLE_FRAME_BODY_PREFIX,
        accepts_body: accepts_role_frame_body,
    },
];

/// The next frame at or after `from`, in whichever form the agent stored it.
///
/// Both forms are searched and the EARLIER start wins. That ordering is what
/// keeps the separators inside the removed span whenever they survived: a
/// separated frame's own body is also a valid unseparated match one byte later,
/// so the separated hit is always the earlier one and the stray control
/// characters go with it.
pub(crate) fn find_internal_agent_routes(input: &str, from: usize) -> Option<(usize, usize)> {
    match (
        find_separated_frame(input, from),
        find_unseparated_frame(input, from),
    ) {
        (Some(separated), Some(bare)) if bare.0 < separated.0 => Some(bare),
        (Some(separated), _) => Some(separated),
        (None, bare) => bare,
    }
}

/// A frame the agent gave back exactly as it was sent, separators and all.
/// Kind-agnostic: whichever kind's validator accepts the body wins.
pub(crate) fn find_separated_frame(input: &str, from: usize) -> Option<(usize, usize)> {
    let separator_len = ROUTE_FRAME_SEPARATOR.len_utf8();
    let mut search_from = from;
    while search_from < input.len() {
        let start = search_from + input[search_from..].find(ROUTE_FRAME_SEPARATOR)?;
        let after_start = start + separator_len;
        let mut bounded_end = (after_start + MAX_ROUTE_FRAME_BYTES).min(input.len());
        while !input.is_char_boundary(bounded_end) {
            bounded_end -= 1;
        }
        if let Some(close_rel) = input[after_start..bounded_end].find(ROUTE_FRAME_SEPARATOR) {
            let end = after_start + close_rel + separator_len;
            let body = &input[after_start..after_start + close_rel];
            if INTERNAL_FRAME_KINDS
                .iter()
                .any(|kind| (kind.accepts_body)(body))
            {
                return Some((start, end));
            }
        }
        // Preserve an invalid separator and retry at the next byte. This is
        // what prevents an unmatched user opening marker from consuming a
        // later genuine frame and all prose between them.
        search_from = after_start;
    }
    None
}

/// A frame whose separators the agent dropped or rewritten, matched on its
/// body — for EVERY internal frame kind (routing and team).
///
/// WHY THIS EXISTS. Separators are the cheapest possible proof that a frame is
/// Codeg-authored, but they only reach a transcript intact if the agent stores
/// the prompt verbatim — and one does not. Antigravity's ACP server joins the
/// prompt's text blocks with a space and replaces each separator with one, so a
/// routed turn lands in `conversations/<id>.db` as
/// `…（使用codeg-mcp工具）  {"kind":…delegation tool.\n ` — the same body, both
/// separators now spaces. The separated pass cannot see that, and the whole
/// frame renders inside the user's bubble when the session is reopened.
///
/// The body's own NEWLINES do survive that rewrite, which is load-bearing:
/// [`unseparated_frame_end`] delimits a candidate by counting exactly three of
/// them. Only the separators were observed to be rewritten — an agent that
/// flattened newlines too would need a different anchor, not a wider one.
///
/// What stands in for the separators as evidence is the body itself: three
/// lines that must re-render byte-for-byte from their own parsed contents,
/// nonce and full instruction wording included. Prose that reproduces all of
/// that was copied out of a frame, not typed. The residue an agent leaves in
/// place of the separators is left to the callers' existing trims — it is
/// whitespace, and the frame is always the last thing in the record.
pub(crate) fn find_unseparated_frame(input: &str, from: usize) -> Option<(usize, usize)> {
    let mut search_from = from;
    while search_from < input.len() {
        // Earliest anchor across kinds; ties are irrelevant because kinds are
        // told apart by the body validator, not the anchor.
        let mut anchor: Option<(usize, &InternalFrameKind)> = None;
        for kind in INTERNAL_FRAME_KINDS {
            if let Some(rel) = input[search_from..].find(kind.body_prefix) {
                let at = search_from + rel;
                if anchor.is_none_or(|(best, _)| at < best) {
                    anchor = Some((at, kind));
                }
            }
        }
        let (start, kind) = anchor?;
        if let Some(end) = unseparated_frame_end(input, start) {
            if (kind.accepts_body)(&input[start..end]) {
                return Some((start, end));
            }
        }
        // Same rule as the separated scan: a look-alike opening must not hide a
        // real frame further along.
        search_from = start + 1;
    }
    None
}

/// End of an unseparated candidate: the body is exactly three newline-terminated
/// lines, so there is nothing to search for — the third newline IS the close.
pub(crate) fn unseparated_frame_end(input: &str, start: usize) -> Option<usize> {
    let mut bounded_end = (start + MAX_ROUTE_FRAME_BYTES).min(input.len());
    while !input.is_char_boundary(bounded_end) {
        bounded_end -= 1;
    }
    let mut cursor = start;
    for _ in 0..3 {
        cursor += input.get(cursor..bounded_end)?.find('\n')? + 1;
    }
    Some(cursor)
}

/// Remove only complete internal frames (any kind — routing and, [crewchat],
/// team) that can be parsed and reproduced byte-for-byte.
/// Codeg scrubs the reserved separator from every prompt at ingress, so a frame
/// that still round-trips can only have been appended at the final boundary —
/// true for any agent's transcript, not just the one this was first written for.
///
/// A frame the agent stored without its separators is removed too, on the
/// strength of its body alone (see [`find_unseparated_frame`]); only the body
/// goes, and the whitespace an agent put where the separators were is left to
/// the caller's trim.
pub(crate) fn strip_internal_agent_routes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some((start, end)) = find_internal_agent_routes(input, cursor) {
        let prefix = &input[cursor..start];
        let prefix_end = if start > cursor
            && input.as_bytes()[start - 1] == b'\n'
            && !prefix.contains(ROUTE_FRAME_SEPARATOR)
        {
            start - 1
        } else {
            start
        };
        output.push_str(&input[cursor..prefix_end]);
        cursor = end;
        if prefix_end == start
            && cursor < input.len()
            && input.as_bytes().get(cursor) == Some(&b'\n')
        {
            cursor += 1;
        }
    }
    output.push_str(&input[cursor..]);
    output
}

pub(crate) fn contains_internal_agent_routes(input: &str) -> bool {
    find_internal_agent_routes(input, 0).is_some()
}

/// Cut a string at a frame marker left behind by TRUNCATION, for the title path.
///
/// [`strip_internal_agent_routes`] only removes a byte-exact *complete* frame,
/// which is right for turn text but not for a title: parsers derive one by
/// joining a prompt's text blocks and capping the result (80 chars in
/// `acp_native`, 100 in [`title_from_user_text`]), and that cap lands wherever it
/// lands. A short first prompt therefore truncates mid-frame, leaving an opening
/// marker and half a JSON descriptor with no close — which the strip pass skips,
/// putting internal route metadata in the sidebar. The frame's body is 453
/// bytes even for the shortest agent slug, well past either cap, so a title that
/// reaches a frame at all is always sliced inside one.
///
/// Two markers, because either one can be the first thing left of a frame.
/// Cutting at the SEPARATOR is sound because RS is reserved:
/// `strip_route_separator_from_prompt` removes it from every prompt field at
/// ingress, so a separator that survives into persisted history can only be one
/// Codeg appended itself. The descriptor prefix carries no such guarantee — it
/// is ordinary text a user could in principle type — but it is the only handle
/// left on the agents that rewrite the separators away (Antigravity), and the
/// blast radius of a false positive is one truncated sidebar title, against a
/// certainty of leaking route metadata into every routed conversation's title.
/// Turn text keeps demanding the full round-trip either way; the asymmetry is
/// the same one the separator cut has always had.
///
/// [`title_from_user_text`]: crate::parsers::title_from_user_text
pub(crate) fn cut_at_route_frame_marker(text: &mut String) {
    let Some(cut) = text
        .find(ROUTE_FRAME_SEPARATOR)
        .into_iter()
        .chain(
            INTERNAL_FRAME_KINDS
                .iter()
                .filter_map(|kind| text.find(kind.body_prefix)),
        )
        .chain(
            INTERNAL_FRAME_KINDS
                .iter()
                .filter_map(|kind| find_halved_body_prefix(text, kind.body_prefix)),
        )
        .min()
    else {
        return;
    };
    text.truncate(cut);
    text.truncate(text.trim_end().len());
}

/// Start of a descriptor prefix the CAP itself cut in half.
///
/// The separator is one character and cannot be halved: either it is inside the
/// capped window or no frame content is. The descriptor prefix is 37 bytes and
/// very much can be, so a first prompt whose prose length falls in a 37-wide
/// band leaves `…prose  {"kind":"codeg_int...` — a fragment [`str::find`] cannot
/// see, and the leak this whole cut exists to stop.
///
/// Matched at the END of the string with a trailing run of `.` discarded,
/// because both title paths cap with [`truncate_str`], which marks the cut by
/// appending `...`.
///
/// A fragment shorter than `{"kind":"c` is deliberately left alone: what
/// survives at that length is a generic JSON opening rather than anything that
/// identifies codeg, and matching it would start trimming ordinary prose that
/// happens to end in `{`.
///
/// [`truncate_str`]: crate::parsers::truncate_str
pub(crate) fn find_halved_body_prefix(text: &str, body_prefix: &str) -> Option<usize> {
    const SHORTEST_FRAGMENT: usize = "{\"kind\":\"c".len();
    let capped = text.trim_end_matches('.');
    // Longest fragment first, so the cut lands at the earliest byte. The full
    // marker is excluded — `find` already covers that case.
    (SHORTEST_FRAGMENT..body_prefix.len())
        .rev()
        .find(|&length| capped.ends_with(&body_prefix[..length]))
        .map(|length| capped.len() - length)
}

/// The record contains one or more valid internal frames and no user-visible
/// content. Used by the Codex parser to avoid phantom turns and promotion
/// coverage from adapters that persist ACP text blocks separately.
pub(crate) fn contains_only_internal_agent_routes(input: &str) -> bool {
    contains_internal_agent_routes(input) && strip_internal_agent_routes(input).trim().is_empty()
}

/// RS is reserved for the internal frame, so it is REMOVED from every prompt
/// at ingress — never used to reject the prompt.
///
/// The character is invisible and mostly arrives inside content the user did
/// not author (an attached file's bytes land in `Resource.text`; ASCII-delimited
/// data and some export formats carry RS legitimately). Rejecting made such a
/// message permanently unsendable with no way for the user to find the offending
/// byte, and it also let a model echo the frame back into a `delegate_to_agent`
/// task string and kill the child prompt. Stripping keeps the invariant that
/// motivated the check — a byte-exact frame inside a Codeg-authored turn can
/// only have been appended by [`append_agent_routes`] — and makes it stronger,
/// since no user-supplied RS reaches the wire at all.
///
/// That last claim is only true if EVERY field is covered, so the nominally
/// base64 ones (`Image.data`, `Resource.blob`) are scrubbed too. Nothing
/// validates them as base64 before they are forwarded to the agent, and both are
/// plain `String`s an API caller can fill freely; scrubbing is a no-op on
/// well-formed input, since RS is outside the base64 alphabet.
///
/// Must run before any conversation DB, broadcast, title, ledger, or
/// command-channel side effect, so the stored / broadcast / on-the-wire copies
/// of the message stay identical.
pub(crate) fn strip_route_separator_from_prompt(blocks: &mut [PromptInputBlock]) -> bool {
    fn scrub(value: &mut String) -> bool {
        if !value.contains(ROUTE_FRAME_SEPARATOR) {
            return false;
        }
        value.retain(|c| c != ROUTE_FRAME_SEPARATOR);
        true
    }
    fn scrub_opt(value: &mut Option<String>) -> bool {
        value.as_mut().is_some_and(scrub)
    }

    let mut stripped = false;
    for block in blocks.iter_mut() {
        // Every textual field, not just `text`: a sentinel uri or an attachment
        // name is echoed back into the prompt by the composer's serialization.
        match block {
            PromptInputBlock::Text { text } => stripped |= scrub(text),
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } => {
                stripped |= scrub(data);
                stripped |= scrub(mime_type);
                stripped |= scrub_opt(uri);
            }
            PromptInputBlock::Resource {
                uri,
                mime_type,
                text,
                blob,
            } => {
                stripped |= scrub(uri);
                stripped |= scrub_opt(mime_type);
                stripped |= scrub_opt(text);
                stripped |= scrub_opt(blob);
            }
            PromptInputBlock::ResourceLink {
                uri,
                name,
                mime_type,
                description,
            } => {
                stripped |= scrub(uri);
                stripped |= scrub(name);
                stripped |= scrub_opt(mime_type);
                stripped |= scrub_opt(description);
            }
        }
    }
    stripped
}
