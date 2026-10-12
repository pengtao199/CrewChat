// [crewchat] Pure turn builders split out of conversation-detail-panel
// (mechanical move — see NOTICE.md split ledger).

import { randomUUID } from "@/lib/utils"
import {
  extractUserImagesFromDraft,
  getPromptDraftDisplayText,
} from "@/lib/prompt-draft"
import {
  type ContentBlock,
  type MessageTurn,
  type PromptDraft,
  type UserMessageBlock,
} from "@/lib/types"
import { contentBlocksFromUserMessage } from "@/lib/user-message-blocks"

export function buildOptimisticUserTurnFromDraft(
  draft: PromptDraft,
  attachedResourcesFallback: string
): MessageTurn {
  // `draft.displayText` is the composer's full Markdown, which already renders
  // every inline file/resource badge as a `[label](uri)` link (see
  // `referenceToMarkdown`). Re-appending the resource blocks here would duplicate
  // each attached file in the optimistic bubble, so the display text is used
  // as-is — images are the only out-of-band content left to add as blocks.
  const text = getPromptDraftDisplayText(draft, attachedResourcesFallback)

  const blocks: ContentBlock[] = []
  for (const image of extractUserImagesFromDraft(draft)) {
    blocks.push({
      type: "image",
      data: image.data,
      mime_type: image.mime_type,
      uri: image.uri ?? null,
    })
  }
  blocks.push({ type: "text", text })

  return {
    id: `optimistic-${randomUUID()}`,
    role: "user",
    blocks,
    timestamp: new Date().toISOString(),
  }
}

/** Build a user `MessageTurn` from a broadcast `user_message` (event or
 *  snapshot `pending_user_message`). Used by cross-client VIEWERS to render the
 *  sender's prompt. The turn `id` is the broadcast `message_id` so the runtime
 *  reducer can dedup it idempotently. */
export function buildUserTurnFromMessageBlocks(
  messageId: string,
  blocks: UserMessageBlock[]
): MessageTurn {
  return {
    id: messageId,
    role: "user",
    blocks: contentBlocksFromUserMessage(blocks),
    timestamp: new Date().toISOString(),
  }
}

export function buildVirtualConversationId(seed: string): number {
  let hash = 0
  for (let i = 0; i < seed.length; i += 1) {
    hash = (hash * 31 + seed.charCodeAt(i)) | 0
  }
  const normalized = Math.abs(hash) + 1
  return -normalized
}
