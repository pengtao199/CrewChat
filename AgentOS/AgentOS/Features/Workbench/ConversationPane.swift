import SwiftUI

struct ConversationPane: View {
    @Environment(AppEnvironment.self) private var environment
    let conversationID: Int?

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        Group {
            if let conversationID {
                if let detail = model.detail, detail.conversation.id == conversationID {
                    ConversationView(detail: detail)
                } else if let message = model.errorMessage {
                    ContentUnavailableView(
                        "common.error",
                        systemImage: "exclamationmark.triangle",
                        description: Text(message)
                    )
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else if model.workspaces.isEmpty {
                ContentUnavailableView(
                    "workbench.noWorkspace.title",
                    systemImage: "folder",
                    description: Text("workbench.noWorkspace.description")
                )
            } else {
                ContentUnavailableView(
                    "workbench.noConversation.title",
                    systemImage: "bubble.left.and.bubble.right",
                    description: Text("workbench.noConversation.description")
                )
            }
        }
        .task(id: conversationID) {
            guard let conversationID else { return }
            await model.open(conversationID)
        }
    }
}

struct ConversationView: View {
    let detail: ConversationDetail

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                ForEach(detail.turns) { turn in
                    TurnView(turn: turn)
                    Divider()
                }
            }
            .padding(20)
        }
        .navigationTitle(detail.conversation.displayTitle)
    }
}

private struct TurnView: View {
    let turn: ConversationTurn

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(roleKey)
                    .font(.caption)
                    .fontWeight(.semibold)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(turn.timestamp.prefix(16).replacingOccurrences(of: "T", with: " "))
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }

            ForEach(Array(turn.blocks.enumerated()), id: \.offset) { _, block in
                blockView(block)
            }
        }
    }

    private var roleKey: LocalizedStringKey {
        switch turn.role {
        case .user: "conversation.role.user"
        case .assistant: "conversation.role.assistant"
        case .system: "conversation.role.system"
        case .unknown: "conversation.role.unknown"
        }
    }

    @ViewBuilder
    private func blockView(_ block: ConversationBlock) -> some View {
        switch block {
        case let .text(text):
            Text(text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

        case let .thinking(text):
            Text(text)
                .italic()
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

        case let .toolUse(name, preview):
            VStack(alignment: .leading, spacing: 4) {
                Label(name, systemImage: "wrench.and.screwdriver")
                    .font(.callout)
                if let preview, !preview.isEmpty {
                    Text(preview)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(8)
                        .textSelection(.enabled)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

        case let .toolResult(preview, isError):
            if let preview, !preview.isEmpty {
                Text(preview)
                    .font(.caption)
                    .foregroundStyle(isError ? .red : .secondary)
                    .lineLimit(8)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

        case let .turnError(message):
            Text(message)
                .foregroundStyle(.red)
                .textSelection(.enabled)

        case let .other(text):
            Text(text)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
    }
}
