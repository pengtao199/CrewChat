import AppKit
import SwiftUI

struct ConversationPane: View {
    @Environment(AppEnvironment.self) private var environment
    let conversationID: Int?

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        Group {
            if let conversationID {
                if let detail = model.detail, detail.conversation.id == conversationID {
                    ConversationView(
                        detail: detail,
                        live: model.liveTurn(for: detail.conversation.id),
                        agentName: model.engineName(for: detail.conversation.agentType),
                        isSending: model.isSending || model.isRunning,
                        onSend: { text in
                            await model.sendMessage(prompt: text, conversation: detail.conversation)
                        }
                    )
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
    let live: LiveTurn?
    let agentName: String
    let isSending: Bool
    let onSend: (String) async -> Bool

    @State private var draft = ""
    @State private var focusRequested = false
    @State private var hoveredTurn: String?

    var body: some View {
        VStack(spacing: 0) {
            if detail.turns.isEmpty, live == nil {
                ContentUnavailableView(
                    "conversation.empty.title",
                    systemImage: "bubble.left",
                    description: Text("conversation.empty.description")
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 18) {
                        ForEach(detail.turns) { turn in
                            MessageRow(
                                turn: turn,
                                agentName: agentName,
                                hovered: hoveredTurn == turn.id,
                                onHover: { hovering in
                                    hoveredTurn = hovering ? turn.id : nil
                                },
                                onCopy: { copy(turn.plainText) },
                                onEdit: {
                                    draft = turn.plainText
                                    focusRequested = true
                                }
                            )
                        }

                        if let live {
                            LiveTurnView(live: live, agentName: agentName)
                        }
                    }
                    .padding(20)
                }
            }

            Divider()

            Composer(
                text: $draft,
                focusRequested: $focusRequested,
                placeholder: "conversation.placeholder",
                isSending: isSending
            ) {
                let text = draft
                Task {
                    if await onSend(text) {
                        draft = ""
                    }
                }
            }
            .padding(12)
        }
        .navigationTitle(detail.conversation.displayTitle)
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

private struct MessageRow: View {
    let turn: ConversationTurn
    let agentName: String
    let hovered: Bool
    let onHover: (Bool) -> Void
    let onCopy: () -> Void
    let onEdit: () -> Void

    var body: some View {
        Group {
            switch turn.role {
            case .user:
                userRow
            case .system:
                systemRow
            default:
                assistantRow
            }
        }
        .onHover(perform: onHover)
    }

    private var userRow: some View {
        HStack(alignment: .top, spacing: 8) {
            Spacer(minLength: 80)

            VStack(alignment: .trailing, spacing: 4) {
                HStack(spacing: 6) {
                    if hovered {
                        actionMenu
                    }
                    Text("conversation.role.user")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(turn.plainText)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(
                        RoundedRectangle(cornerRadius: 10)
                            .fill(Color(nsColor: .controlBackgroundColor))
                    )
            }

            MessageAvatar(name: String(localized: "conversation.role.user"))
        }
    }

    private var assistantRow: some View {
        HStack(alignment: .top, spacing: 8) {
            MessageAvatar(name: agentName)

            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Text(agentName)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if hovered {
                        actionMenu
                    }
                }

                ForEach(Array(turn.blocks.enumerated()), id: \.offset) { _, block in
                    blockView(block)
                }
            }

            Spacer(minLength: 40)
        }
    }

    private var systemRow: some View {
        HStack {
            Spacer()
            ForEach(Array(turn.blocks.enumerated()), id: \.offset) { _, block in
                blockView(block)
            }
            Spacer()
        }
    }

    private var actionMenu: some View {
        Menu {
            Button("conversation.copy", action: onCopy)
            if turn.role == .user {
                Button("conversation.editResend", action: onEdit)
            }
        } label: {
            Image(systemName: "ellipsis")
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }

    @ViewBuilder
    private func blockView(_ block: ConversationBlock) -> some View {
        switch block {
        case let .text(text):
            Text(text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

        case let .thinking(text):
            DisclosureGroup("conversation.thinking") {
                Text(text)
                    .italic()
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 4)
            }
            .font(.callout)
            .foregroundStyle(.secondary)

        case let .toolUse(name, preview):
            VStack(alignment: .leading, spacing: 2) {
                Label(name, systemImage: "wrench.and.screwdriver")
                    .font(.callout)
                if let preview, !preview.isEmpty {
                    Text(preview)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(6)
                        .textSelection(.enabled)
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color(nsColor: .controlBackgroundColor).opacity(0.6))
            )

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
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

private struct MessageAvatar: View {
    let name: String

    var body: some View {
        Circle()
            .fill(.quaternary)
            .frame(width: 26, height: 26)
            .overlay {
                Text(String(name.prefix(1)))
                    .font(.caption)
                    .fontWeight(.semibold)
            }
            .accessibilityLabel(name)
    }
}

private struct LiveTurnView: View {
    let live: LiveTurn
    let agentName: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            MessageAvatar(name: agentName)

            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Text(agentName)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if live.isRunning {
                        ProgressView().controlSize(.small)
                    }
                }

                if !live.thinking.isEmpty {
                    DisclosureGroup("conversation.thinking") {
                        Text(live.thinking)
                            .italic()
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 4)
                    }
                    .font(.callout)
                    .foregroundStyle(.secondary)
                }

                ForEach(live.tools) { tool in
                    VStack(alignment: .leading, spacing: 2) {
                        Label(tool.title, systemImage: "wrench.and.screwdriver")
                            .font(.callout)
                        if !tool.status.isEmpty {
                            Text(tool.status)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        if let output = tool.output, !output.isEmpty {
                            Text(output)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(6)
                                .textSelection(.enabled)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                if !live.text.isEmpty {
                    Text(live.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            Spacer(minLength: 40)
        }
    }
}

private extension ConversationTurn {
    var plainText: String {
        blocks.compactMap { block in
            if case let .text(text) = block { return text }
            return nil
        }
        .joined(separator: "\n\n")
    }
}
