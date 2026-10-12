// [crewchat] Team-member presence (Phase 1.4) — the pure resolution behind the
// conversation header's member strip.
//
// A member is a roster role with one CLI body (`agent_type`). Everything the
// strip shows about it is derived from two refresh-safe sources:
//
// * the conversation's child rows (DB; what survived a reload), and
// * the live delegation bindings (in-memory; seeded from the session snapshot
//   for RUNNING tasks, so they survive a reload too — completed ones fall back
//   to the child rows).
//
// "Awaiting permission" is deliberately NOT resolved here: it needs the live
// child connection's snapshot, which the chip subscribes to directly. The
// pure function returns `running` for such a member and the chip upgrades it.

import type { ConversationStatus } from "@/lib/types"

export type TeamMemberPresence =
  | "idle"
  | "queued"
  | "running"
  | "waiting"
  | "done"
  | "failed"
  | "cancelled"

/** The child-row facts of ONE member (rows match the member's `agent_type`). */
export interface MemberChildSignal {
  id: number
  status: ConversationStatus
}

/** The live-binding facts of ONE member (bindings match the member's CLI). */
export interface MemberBindingSignal {
  childConversationId: number
  childConnectionId: string
  status: "running" | "ok" | "err"
  errorCode?: string
  /** Bounded task preview from `delegation_started`, for the chip tooltip. */
  task: string | null
}

export interface TeamMemberSignals {
  /** This member's children, NEWEST FIRST (callers sort by id descending). */
  children: MemberChildSignal[]
  /** Live delegation bindings for this member's `agent_type`. */
  bindings: MemberBindingSignal[]
}

/**
 * The member's current presence:
 *
 * * `running` — a child row is in flight;
 * * `queued` — a live binding is still setting up (its child row isn't
 *   visible yet): the spawn/claim window, which is what "queued" means in a
 *   broker that has no queue;
 * * `done` / `failed` / `cancelled` — its newest task is terminal (a
 *   non-cancel error code on a cancelled child reads as `failed`);
 * * `idle` — nothing has ever been delegated to this member.
 */
export function resolveMemberPresence({
  children,
  bindings,
}: TeamMemberSignals): TeamMemberPresence {
  if (children.some((child) => child.status === "in_progress")) return "running"

  const childIds = new Set(children.map((child) => child.id))
  if (
    bindings.some(
      (binding) =>
        binding.status === "running" &&
        !childIds.has(binding.childConversationId)
    )
  ) {
    return "queued"
  }

  const newest = children[0]
  if (newest) {
    if (newest.status === "completed" || newest.status === "pending_review") {
      return "done"
    }
    if (newest.status === "cancelled") {
      const binding = bindings.find(
        (candidate) => candidate.childConversationId === newest.id
      )
      // A cancel that carried a NON-cancel failure code is a failure wearing a
      // cancellation: surface the failure rather than the cancel.
      if (
        binding?.status === "err" &&
        binding.errorCode &&
        binding.errorCode !== "canceled"
      ) {
        return "failed"
      }
      return "cancelled"
    }
  }

  if (bindings.some((binding) => binding.status === "err")) return "failed"
  if (bindings.some((binding) => binding.status === "ok")) return "done"
  return "idle"
}

/**
 * The child connections a chip should watch for a pending permission ask:
 * one per in-flight child that a live binding points at. Empty when nothing
 * is running (a member can be `queued` with no connection yet — its chip then
 * watches nothing and shows no permission state, which is correct: there is
 * no session to ask).
 */
export function runningChildConnectionIds({
  children,
  bindings,
}: TeamMemberSignals): string[] {
  const runningChildIds = new Set(
    children
      .filter((child) => child.status === "in_progress")
      .map((child) => child.id)
  )
  return bindings
    .filter(
      (binding) =>
        binding.status === "running" &&
        runningChildIds.has(binding.childConversationId)
    )
    .map((binding) => binding.childConnectionId)
}

/** The task text for the chip's tooltip: the running binding's preview when
 *  there is one, else the newest child's — never a stale task from an older
 *  delegation while a newer one is in flight. */
export function memberTaskPreview({
  children,
  bindings,
}: TeamMemberSignals): string | null {
  const running = bindings.find(
    (binding) =>
      binding.status === "running" &&
      children.some(
        (child) =>
          child.id === binding.childConversationId &&
          child.status === "in_progress"
      )
  )
  if (running?.task) return running.task
  const newest = children[0]
  if (!newest) return bindings.find((b) => b.status === "running")?.task ?? null
  const newestBinding = bindings.find(
    (binding) => binding.childConversationId === newest.id
  )
  return newestBinding?.task ?? null
}
