import SwiftUI

struct AgentProfileView: View {
    let member: ConversationMember
    let delegation: ConversationDelegation?
    let state: ConversationMemberState
    let onMention: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                AgentAvatar(
                    name: member.name,
                    size: 38,
                    state: state,
                    isLeader: member.isLeader
                )
                VStack(alignment: .leading, spacing: 2) {
                    Text(member.name)
                        .font(.headline)
                    HStack(spacing: 6) {
                        Text(member.isLeader
                            ? "conversation.member.leader"
                            : "conversation.member.member")
                        Text(verbatim: "·")
                        Text(state.label)
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }

            Divider()

            LabeledContent("conversation.member.engine", value: member.agentType)
            LabeledContent(
                "conversation.member.model",
                value: member.model.isEmpty
                    ? String(localized: "conversation.member.defaultModel")
                    : member.model
            )
            LabeledContent(
                "conversation.member.autoApprove",
                value: member.autoApprove
                    ? String(localized: "common.yes")
                    : String(localized: "common.no")
            )

            if let skillCount = member.skillCount {
                LabeledContent("conversation.member.skills", value: "\(skillCount)")
            }
            if let mcpCount = member.mcpCount {
                LabeledContent("conversation.member.mcp", value: "\(mcpCount)")
            }

            if let delegation, delegation.status.isActive {
                VStack(alignment: .leading, spacing: 3) {
                    Text("conversation.member.currentTask")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(delegation.task)
                        .font(.callout)
                        .lineLimit(3)
                }
            }

            HStack {
                Button {
                    onMention()
                    dismiss()
                } label: {
                    Label("conversation.member.mention", systemImage: "at")
                }
                Spacer()
            }
        }
        .padding(14)
        .frame(width: 290)
    }
}
