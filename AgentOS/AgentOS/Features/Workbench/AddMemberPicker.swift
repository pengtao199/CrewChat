import SwiftUI

struct AddMemberPicker: View {
    let roles: [AgentRole]
    let existingIDs: Set<String>
    let addingID: String?
    let error: String?
    let engineName: (String) -> String
    let onSelect: (String) -> Void

    private var available: [AgentRole] {
        roles.filter { !existingIDs.contains($0.id) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("conversation.member.add.title")
                .font(.headline)
                .padding(.horizontal, 14)
                .padding(.vertical, 12)

            Divider()

            if let error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
            }

            if available.isEmpty {
                Text("conversation.member.add.empty")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(available) { role in
                            Button {
                                onSelect(role.id)
                            } label: {
                                HStack(spacing: 9) {
                                    AgentAvatar(name: role.name, size: 24)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(role.name)
                                            .lineLimit(1)
                                        Text(roleSubtitle(role))
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                    }
                                    Spacer()
                                    if addingID == role.id {
                                        ProgressView()
                                            .controlSize(.small)
                                    } else {
                                        Image(systemName: "plus.circle")
                                            .foregroundStyle(.tint)
                                    }
                                }
                                .padding(.horizontal, 14)
                                .padding(.vertical, 7)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(addingID != nil)
                        }
                    }
                }
            }
        }
        .frame(width: 330, height: available.isEmpty ? 150 : 340)
    }

    private func roleSubtitle(_ role: AgentRole) -> String {
        let engine = engineName(role.agentType)
        let model = role.model.isEmpty
            ? String(localized: "conversation.member.defaultModel")
            : role.model
        return "\(engine) · \(model)"
    }
}
