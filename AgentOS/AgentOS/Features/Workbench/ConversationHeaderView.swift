import SwiftUI

struct ConversationHeaderView: View {
    let conversation: Conversation
    let members: [ConversationMember]
    let liveTurns: [LiveTurn]
    let activeDelegations: [ConversationDelegation]
    let onMention: (ConversationMember) -> Void
    let onAddMember: (() -> Void)?

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text(conversation.displayTitle)
                    .font(.headline)
                    .lineLimit(1)
                if !members.isEmpty {
                    Text(subtitle)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .layoutPriority(1)

            Spacer(minLength: 8)

            ForEach(members) { member in
                MemberChip(
                    member: member,
                    state: effectiveState(member),
                    delegation: delegation(for: member),
                    onMention: { onMention(member) }
                )
            }

            Button {
                onAddMember?()
            } label: {
                Image(systemName: "plus")
                    .frame(width: 24, height: 24)
            }
            .buttonStyle(.borderless)
            .disabled(onAddMember == nil)
            .help(onAddMember == nil
                ? "conversation.member.frozen"
                : "conversation.member.add")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
        .background(.bar)
    }

    private var subtitle: String {
        var parts = ["\(members.count) \(String(localized: "conversation.member.count"))"]
        if !activeDelegations.isEmpty {
            parts.append(
                "\(activeDelegations.count) \(String(localized: "conversation.member.activeTasks"))"
            )
        }
        return parts.joined(separator: " · ")
    }

    private func effectiveState(_ member: ConversationMember) -> ConversationMemberState {
        if liveTurns.contains(where: { $0.speakerRoleID == member.roleID && $0.isRunning }) {
            return .running
        }
        return member.state
    }

    private func delegation(for member: ConversationMember) -> ConversationDelegation? {
        activeDelegations.first { $0.memberRoleID == member.roleID }
    }
}

private struct MemberChip: View {
    let member: ConversationMember
    let state: ConversationMemberState
    let delegation: ConversationDelegation?
    let onMention: () -> Void

    @State private var showProfile = false

    var body: some View {
        Button {
            showProfile = true
        } label: {
            HStack(spacing: 5) {
                AgentAvatar(
                    name: member.name,
                    size: 23,
                    state: state,
                    isLeader: member.isLeader
                )
                Text(member.name)
                    .font(.caption)
                    .lineLimit(1)
                if member.isLeader {
                    Text("L")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(.tint)
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: 12)
                    .fill(Color(nsColor: .controlBackgroundColor))
            )
        }
        .buttonStyle(.plain)
        .help(member.name)
        .popover(isPresented: $showProfile, arrowEdge: .bottom) {
            AgentProfileView(
                member: member,
                delegation: delegation,
                state: state,
                onMention: onMention
            )
        }
    }
}
