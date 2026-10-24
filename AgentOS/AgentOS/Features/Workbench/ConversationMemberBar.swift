import SwiftUI

struct ConversationMemberBar: View {
    let members: [ConversationMember]
    let liveTurns: [LiveTurn]
    let activeDelegations: [ConversationDelegation]
    let onMention: (ConversationMember) -> Void
    let onAddMember: (() -> Void)?

    var body: some View {
        HStack(spacing: 6) {
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
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.borderless)
            .disabled(onAddMember == nil)
            .help(onAddMember == nil
                ? "conversation.member.frozen"
                : "conversation.member.add")
        }
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
                    size: 21,
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
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                RoundedRectangle(cornerRadius: 11)
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
