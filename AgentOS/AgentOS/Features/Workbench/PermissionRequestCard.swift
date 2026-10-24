import SwiftUI

struct PermissionRequestCard: View {
    let permission: PendingPermission
    let member: ConversationMember?
    let onRespond: (String?) -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            AgentAvatar(
                name: member?.name ?? String(localized: "conversation.role.assistant"),
                size: 26,
                state: .waiting,
                isLeader: member?.isLeader == true
            )

            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Text(member?.name ?? String(localized: "conversation.role.assistant"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("conversation.permission.title")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }

                Text(permission.request.title)
                    .font(.callout)
                    .textSelection(.enabled)

                if let detail = permission.request.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(4)
                        .textSelection(.enabled)
                }

                HStack(spacing: 8) {
                    ForEach(permission.request.options) { option in
                        Button(option.name) {
                            onRespond(option.id)
                        }
                        .buttonStyle(.bordered)
                    }
                    Button("conversation.permission.reject") {
                        onRespond(nil)
                    }
                    .buttonStyle(.bordered)
                }
            }
            .padding(12)
            .frame(maxWidth: 680, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 12)
                    .fill(.orange.opacity(0.08))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .strokeBorder(.orange.opacity(0.3))
            )

            Spacer(minLength: 40)
        }
    }
}
