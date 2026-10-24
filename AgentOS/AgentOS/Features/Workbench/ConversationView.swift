import SwiftUI

struct ConversationView: View {
    let detail: ConversationDetail
    let liveTurns: [LiveTurn]
    let pendingPermissions: [PendingPermission]
    let pendingUserText: String?
    let errorMessage: String?
    let agentName: String
    let isRunning: Bool
    let isSending: Bool
    let engineName: (String) -> String
    let onSend: (String, [String]) async -> Bool
    let onStop: () -> Void
    let onPermission: (PendingPermission, String?) -> Void

    @State private var draft = ""
    @State private var focusRequested = false

    private var members: [ConversationMember] {
        detail.members
    }

    private var isEmpty: Bool {
        detail.turns.isEmpty
            && liveTurns.isEmpty
            && pendingPermissions.isEmpty
            && pendingUserText == nil
    }

    var body: some View {
        VStack(spacing: 0) {
            if !members.isEmpty {
                ConversationHeaderView(
                    conversation: detail.conversation,
                    members: members,
                    liveTurns: liveTurns,
                    activeDelegations: detail.activeDelegations,
                    onMention: insertMention,
                    onAddMember: nil
                )
                Divider()
            }

            if let errorMessage {
                Text(errorMessage)
                    .font(.callout)
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 6)
            }

            ZStack(alignment: .bottom) {
                if isEmpty {
                    ContentUnavailableView(
                        "conversation.empty.title",
                        systemImage: "bubble.left",
                        description: Text("conversation.empty.description")
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ConversationTimelineView(
                        detail: detail,
                        liveTurns: liveTurns,
                        pendingPermissions: pendingPermissions,
                        pendingUserText: pendingUserText,
                        members: members,
                        agentName: agentName,
                        engineName: engineName,
                        isRunning: isRunning,
                        bottomInset: 132,
                        onMention: insertMention,
                        onPermission: onPermission
                    )
                }

                Composer(
                    text: $draft,
                    focusRequested: $focusRequested,
                    placeholder: "conversation.placeholder.team",
                    members: members,
                    isRunning: isRunning,
                    canSend: !draft.isEmpty && !isSending && !isRunning,
                    onSend: {
                        let text = draft
                        let mentions = MentionParser.memberIDs(in: text, members: members)
                        draft = ""
                        Task {
                            if !(await onSend(text, mentions)) {
                                if draft.isEmpty {
                                    draft = text
                                }
                            }
                        }
                    },
                    onStop: onStop
                )
                .frame(maxWidth: 860)
                .padding(.horizontal, 20)
                .padding(.bottom, 14)
            }
        }
        .navigationTitle(detail.conversation.displayTitle)
    }

    private func insertMention(_ member: ConversationMember) {
        if draft.isEmpty {
            draft = "@\(member.name) "
        } else if draft.hasSuffix(" ") || draft.hasSuffix("\n") {
            draft += "@\(member.name) "
        } else {
            draft += " @\(member.name) "
        }
        focusRequested = true
    }
}
