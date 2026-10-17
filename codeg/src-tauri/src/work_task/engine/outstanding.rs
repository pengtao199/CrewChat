use super::*;

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// What the user last asked for, and how they meant it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OutstandingKind {
    /// A follow-up on a reviewed task.
    Review(FollowUpIntent),
    /// A note attached to a retry or a re-queue.
    Restart,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outstanding {
    pub(super) kind: OutstandingKind,
    pub(super) text: String,
    /// Raw prompt blocks the user attached to that instruction (images, pasted
    /// bytes). Kept unparsed here — `attachment_blocks` validates at the point
    /// the prompt is composed.
    pub(super) attachments: Vec<serde_json::Value>,
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The user instruction the agent still owes a turn to, if any.
///
/// Scans the task's events newest-first and stops at whichever comes first:
/// - a follow-up `user_action` (return / retry / requeue) → that one is
///   outstanding;
/// - a settle into `review` → a generation ran to completion after anything
///   earlier, so nothing is owed.
///
/// It is a barrier, not a filter. Filtering (e.g. "skip questions") would walk
/// *past* the newest instruction and resurrect an older one the agent already
/// carried out — replaying "add the tests" long after they were added. And the
/// review barrier is what stops a note from being re-injected into every
/// subsequent generation for the rest of the task's life.
/// What a retry launch learns from the event log: the newest instruction the
/// interrupted generation still owes the user (`outstanding`, replayed into
/// the prompt), and the kind of turn that generation was actually running
/// (`interrupted`, which picks the guard's licence). They differ exactly when
/// a retry/requeue note sits on top of a failed follow-up: the note is the
/// newer INSTRUCTION, but the turn underneath is still that follow-up — a
/// note refines the turn it interrupts, it does not change its kind.
pub(super) struct InstructionScan {
    pub(super) outstanding: Option<Outstanding>,
    /// The unsettled `return` intent beneath any retry/requeue notes; `None`
    /// when the generation was (re)doing the task's original order.
    pub(super) interrupted: Option<FollowUpIntent>,
    /// That same `return` as an INSTRUCTION, and only when a newer
    /// retry/requeue note took the `outstanding` slot away from it.
    ///
    /// A resumed session already has those words in its transcript, so the
    /// note alone is the right replay there — that is what "a note refines the
    /// turn it interrupts" means. A session with no context (a re-queued
    /// task's fresh launch, a resume that failed over) has nothing: replaying
    /// only the note hands the agent "take this into account" with no account
    /// to take it into, and for a question it pairs a read-only licence with a
    /// question the agent was never shown.
    pub(super) interrupted_instruction: Option<Outstanding>,
}

pub(super) async fn instruction_scan(
    conn: &sea_orm::DatabaseConnection,
    task_id: i32,
) -> InstructionScan {
    // Newest-first, and narrowed IN THE QUERY to the two kinds that can settle
    // the question. Scanning raw events would let a chatty run bury the answer:
    // `agent_progress` volume is up to the agent, so a few hundred milestones
    // would push the instruction past any limit. Filtered this way the limit
    // bounds a log of decisions, which is small.
    let events = work_task_service::recent_events_of_kinds(
        conn,
        task_id,
        &["user_action", "status_changed"],
        200,
    )
    .await
    .unwrap_or_default();
    let mut scan = InstructionScan {
        outstanding: None,
        interrupted: None,
        interrupted_instruction: None,
    };
    for event in events {
        match event.kind.as_str() {
            "status_changed" => {
                let settled = event
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("to"))
                    .and_then(|v| v.as_str())
                    == Some("review");
                // Reaching review consumed every older instruction: whatever
                // lies beyond was delivered and decided on.
                if settled {
                    break;
                }
            }
            "user_action" => {
                let payload = match event.payload {
                    Some(p) => p,
                    None => continue,
                };
                let Some(action) = payload.get("action").and_then(|v| v.as_str()) else {
                    break;
                };
                match action {
                    "return" => {
                        let intent = FollowUpIntent::from_wire(
                            payload.get("intent").and_then(|v| v.as_str()),
                        )
                        .unwrap_or_default();
                        let returned =
                            payload.get("feedback").and_then(|v| v.as_str()).map(|text| {
                                Outstanding {
                                    kind: OutstandingKind::Review(intent),
                                    text: text.to_string(),
                                    attachments: payload_blocks(&payload),
                                }
                            });
                        if scan.outstanding.is_none() {
                            let Some(returned) = returned else {
                                break;
                            };
                            scan.outstanding = Some(returned);
                        } else {
                            // A retry/requeue note already claimed the newest
                            // slot. Keep the returned words too — see the field.
                            scan.interrupted_instruction = returned;
                        }
                        // The newest unsettled return IS the interrupted turn
                        // (a second return needs another review in between,
                        // which would have settled this scan already).
                        scan.interrupted = Some(intent);
                        break;
                    }
                    "retry" | "requeue" => {
                        if scan.outstanding.is_none() {
                            let Some(text) = payload.get("note").and_then(|v| v.as_str()) else {
                                break;
                            };
                            scan.outstanding = Some(Outstanding {
                                kind: OutstandingKind::Restart,
                                text: text.to_string(),
                                attachments: payload_blocks(&payload),
                            });
                        }
                        // Keep looking: the turn this note interrupted lies
                        // deeper in the log.
                    }
                    // Other user actions (delete, …) neither carry nor consume
                    // an instruction.
                    _ => continue,
                }
            }
            _ => continue,
        }
    }
    scan
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// Replay everything the launch still owes the user, oldest first, with each
/// instruction's own attachments right behind the sentence that framed it.
///
/// `resumed` is the whole difference. A resumed session carries the interrupted
/// turn in its transcript, so the newest instruction is the entire replay — a
/// retry/requeue note refines a turn the agent can still read. A session with
/// no context has to be given that turn as well, or the note lands on nothing:
/// see [`InstructionScan::interrupted_instruction`].
///
/// Shared by both replaying arms — a re-queued task reaches `Fresh` with
/// exactly the same debt to the user as an interrupted one reaching `Retry`,
/// so the two must not drift apart in what they replay or how they word it.
pub(super) fn push_replay(
    blocks: &mut Vec<PromptInputBlock>,
    scan: &InstructionScan,
    resumed: bool,
    task_id: i32,
) {
    let mut push = |instruction: &Outstanding| {
        blocks.push(outstanding_block(instruction));
        blocks.extend(attachment_blocks(&instruction.attachments, task_id));
    };
    if !resumed {
        if let Some(interrupted) = &scan.interrupted_instruction {
            push(interrupted);
        }
    }
    if let Some(outstanding) = &scan.outstanding {
        push(outstanding);
    }
}

// [crewchat] 从 engine.rs 归口（批次 B 第 2 步）
/// The block that replays one outstanding instruction, framed the way the user
/// meant it: an unanswered question must not come back as a work order.
pub(super) fn outstanding_block(outstanding: &Outstanding) -> PromptInputBlock {
    match outstanding.kind {
        OutstandingKind::Restart => restart_note_block(&outstanding.text),
        OutstandingKind::Review(FollowUpIntent::Question) => PromptInputBlock::Text {
            text: format!(
                "The user asked this question before the interruption and never got an \
                 answer. Answer it, and do not change any files for it:\n\n{}",
                outstanding.text
            ),
        },
        OutstandingKind::Review(_) => PromptInputBlock::Text {
            text: format!("Latest review feedback to address:\n{}", outstanding.text),
        },
    }
}
