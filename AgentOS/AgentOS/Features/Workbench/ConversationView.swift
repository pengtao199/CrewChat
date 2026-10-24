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
    let availableRoles: [AgentRole]
    let onSend: (String, [String]) async -> Bool
    let onStop: () -> Void
    let onAddMember: (String) async throws -> Void
    let onPermission: (PendingPermission, String?) -> Void

    @State private var draft = ""
    @State private var focusRequested = false
    @State private var composerHeight: CGFloat = 100
    @State private var showAddMember = false
    @State private var addingMemberID: String?
    @State private var addMemberError: String?

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
                        bottomInset: composerHeight + 28,
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
                .onGeometryChange(for: CGFloat.self) { proxy in
                    proxy.size.height
                } action: { height in
                    if abs(height - composerHeight) > 0.5 {
                        composerHeight = height
                    }
                }
            }
        }
        .navigationTitle(detail.conversation.displayTitle)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                if !members.isEmpty {
                    ConversationMemberBar(
                        members: members,
                        liveTurns: liveTurns,
                        activeDelegations: detail.activeDelegations,
                        onMention: insertMention,
                        onAddMember: {
                            addMemberError = nil
                            showAddMember = true
                        }
                    )
                    .popover(isPresented: $showAddMember, arrowEdge: .bottom) {
                        AddMemberPicker(
                            roles: availableRoles,
                            existingIDs: Set(members.map(\.roleID)),
                            addingID: addingMemberID,
                            error: addMemberError,
                            engineName: engineName,
                            onSelect: addMember
                        )
                    }
                }
            }
        }
    }

    private func addMember(_ roleID: String) {
        guard addingMemberID == nil else { return }
        addingMemberID = roleID
        addMemberError = nil
        Task {
            do {
                try await onAddMember(roleID)
                addingMemberID = nil
                showAddMember = false
            } catch {
                addingMemberID = nil
                addMemberError = error.localizedDescription
            }
        }
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
