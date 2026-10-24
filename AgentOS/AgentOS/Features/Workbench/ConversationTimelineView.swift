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
    let bottomInset: CGFloat
    let onMention: (ConversationMember) -> Void
    let onPermission: (PendingPermission, String?) -> Void

    private let bottomID = "conversation-bottom"
    @State private var isPinnedToBottom = true
    @State private var lastFollowAt = Date.distantPast
    @State private var lastContentHeight: CGFloat = 0
    @State private var lastContentOffset: CGFloat = 0

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
                .padding(.top, 20)
                .padding(.bottom, bottomInset)
                .padding(.horizontal, 20)
            }
            .onScrollGeometryChange(for: ScrollSnapshot.self) { geometry in
                ScrollSnapshot(
                    contentHeight: geometry.contentSize.height,
                    contentOffset: geometry.contentOffset.y,
                    containerHeight: geometry.containerSize.height
                )
            } action: { _, snapshot in
                let contentGrew = snapshot.contentHeight > lastContentHeight + 0.5
                let offsetMoved = abs(snapshot.contentOffset - lastContentOffset) > 0.5
                if lastContentHeight > 0, !contentGrew || offsetMoved {
                    let remaining = snapshot.contentHeight
                        - snapshot.contentOffset
                        - snapshot.containerHeight
                    isPinnedToBottom = remaining <= 72
                }
                lastContentHeight = snapshot.contentHeight
                lastContentOffset = snapshot.contentOffset
            }
            .onAppear {
                isPinnedToBottom = true
                follow(proxy, animated: false)
            }
            .onChange(of: detail.turns.count) { _, _ in
                guard isPinnedToBottom else { return }
                follow(proxy, animated: true)
            }
            .onChange(of: pendingUserText) { _, value in
                guard value != nil else { return }
                isPinnedToBottom = true
                follow(proxy, animated: true)
            }
            .onChange(of: liveSignature) { _, _ in
                guard isPinnedToBottom else { return }
                follow(proxy, animated: false)
            }
            .onChange(of: pendingPermissions.count) { _, _ in
                guard isPinnedToBottom else { return }
                follow(proxy, animated: false)
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

    private func follow(_ proxy: ScrollViewProxy, animated: Bool) {
        if !animated {
            let now = Date()
            guard now.timeIntervalSince(lastFollowAt) >= 0.05 else { return }
            lastFollowAt = now
        }
        if animated {
            withAnimation(.easeOut(duration: 0.16)) {
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

private struct ScrollSnapshot: Equatable {
    let contentHeight: CGFloat
    let contentOffset: CGFloat
    let containerHeight: CGFloat
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
