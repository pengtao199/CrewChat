import SwiftUI

struct ConversationTimelineView: View {
    let detail: ConversationDetail
    let liveTurns: [LiveTurn]
    let pendingPermissions: [PendingPermission]
    let pendingUserText: String?
    let members: [ConversationMember]
    let agentName: String
    let engineName: (String) -> String
    let isRunning: Bool
    let onMention: (ConversationMember) -> Void
    let onPermission: (PendingPermission, String?) -> Void

    private let bottomID = "conversation-bottom"

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 18) {
                    ForEach(groups) { group in
                        groupView(group)
                    }

                    if let pendingUserText {
                        UserBubble(
                            text: pendingUserText,
                            members: members,
                            timestamp: nil,
                            onCopy: { copy(pendingUserText) }
                        )
                    }

                    ForEach(liveTurns) { live in
                        liveView(live)
                    }

                    ForEach(pendingPermissions) { permission in
                        PermissionRequestCard(
                            permission: permission,
                            member: detail.member(id: permission.speakerRoleID)
                                ?? (detail.members.isEmpty ? nil : detail.leader),
                            onRespond: { onPermission(permission, $0) }
                        )
                    }

                    Color.clear
                        .frame(height: 1)
                        .id(bottomID)
                }
                .frame(maxWidth: 860, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 20)
                .padding(.horizontal, 20)
            }
            .onAppear {
                scrollToBottom(proxy, animated: false)
            }
            .onChange(of: detail.turns.count) { _, _ in
                scrollToBottom(proxy)
            }
            .onChange(of: pendingUserText) { _, _ in
                scrollToBottom(proxy)
            }
            .onChange(of: liveSignature) { _, _ in
                scrollToBottom(proxy)
            }
            .onChange(of: pendingPermissions.count) { _, _ in
                scrollToBottom(proxy)
            }
        }
    }

    private var groups: [TurnGroup] {
        var result: [TurnGroup] = []

        for turn in detail.turns {
            let speaker = turn.speakerRoleID
            if turn.role == .assistant,
               var last = result.last,
               last.role == .assistant,
               last.speakerRoleID == speaker {
                last.turns.append(turn)
                result[result.count - 1] = last
            } else {
                result.append(
                    TurnGroup(
                        id: turn.id,
                        role: turn.role,
                        speakerRoleID: speaker,
                        turns: [turn]
                    )
                )
            }
        }

        return result
    }

    @ViewBuilder
    private func groupView(_ group: TurnGroup) -> some View {
        switch group.role {
        case .user:
            UserBubble(
                text: group.plainText,
                members: members,
                timestamp: group.turns.first?.timestamp,
                onCopy: { copy(group.plainText) }
            )
        case .system:
            SystemRows(blocks: group.blocks)
        default:
            let info = speakerInfo(for: group.speakerRoleID)
            AssistantBubble(
                member: info.member,
                delegation: detail.delegation(for: info.member?.roleID ?? ""),
                fallbackName: info.name,
                engineName: info.engineName,
                model: info.model,
                timestamp: group.turns.first?.timestamp,
                members: members,
                blocks: group.blocks,
                thinking: group.thinking,
                tools: [],
                text: "",
                isRunning: false,
                usage: nil,
                duration: nil,
                onCopy: { copy(group.plainText) },
                onMention: onMention
            )
        }
    }

    private func liveView(_ live: LiveTurn) -> some View {
        let info = speakerInfo(for: live.speakerRoleID)
        return AssistantBubble(
            member: info.member,
            delegation: detail.delegation(for: info.member?.roleID ?? ""),
            fallbackName: info.name,
            engineName: info.engineName,
            model: info.model,
            timestamp: ConversationTime.text(live.startedAt),
            members: members,
            blocks: [],
            thinking: live.thinking,
            tools: live.tools,
            text: live.text,
            isRunning: live.isRunning,
            usage: live.usage,
            duration: live.duration,
            onCopy: live.text.isEmpty ? nil : { copy(live.text) },
            onMention: onMention
        )
    }

    private func speakerInfo(
        for roleID: String?
    ) -> (member: ConversationMember?, name: String, engineName: String, model: String) {
        let member = detail.member(id: roleID)
            ?? (detail.members.isEmpty ? nil : detail.leader)
        let agentType = member?.agentType ?? detail.conversation.agentType
        let model = member?.model ?? detail.conversation.model ?? ""
        return (
            member,
            member?.name ?? agentName,
            engineName(agentType),
            model
        )
    }

    private var liveSignature: String {
        liveTurns
            .map { "\($0.id):\($0.text.count):\($0.thinking.count):\($0.isRunning)" }
            .joined(separator: "|")
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool = true) {
        if animated {
            withAnimation(.easeOut(duration: 0.15)) {
                proxy.scrollTo(bottomID, anchor: .bottom)
            }
        } else {
            proxy.scrollTo(bottomID, anchor: .bottom)
        }
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

private struct TurnGroup: Identifiable {
    let id: String
    let role: TurnRole
    let speakerRoleID: String?
    var turns: [ConversationTurn]

    var thinking: String {
        turns
            .flatMap(\.blocks)
            .compactMap { block in
                if case let .thinking(text) = block { return text }
                return nil
            }
            .joined(separator: "\n")
    }

    var blocks: [ConversationBlock] {
        turns
            .flatMap(\.blocks)
            .filter { block in
                if case .thinking = block { return false }
                return true
            }
    }

    var plainText: String {
        turns
            .map(\.plainText)
            .filter { !$0.isEmpty }
            .joined(separator: "\n\n")
    }
}
