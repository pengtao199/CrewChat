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
                        agentName: model.agentDisplayName(for: detail.conversation),
                        pendingUserText: model.pendingUserText,
                        isRunning: model.isRunning,
                        isSending: model.isSending,
                        onSend: { text in
                            await model.sendMessage(prompt: text, conversation: detail.conversation)
                        },
                        onStop: {
                            Task { await model.cancelRun() }
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
    let pendingUserText: String?
    let isRunning: Bool
    let isSending: Bool
    let onSend: (String) async -> Bool
    let onStop: () -> Void

    @State private var draft = ""
    @State private var focusRequested = false

    var body: some View {
        VStack(spacing: 0) {
            if detail.turns.isEmpty, live == nil, pendingUserText == nil {
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
                            TurnRow(
                                turn: turn,
                                agentName: agentName,
                                onEdit: turn.role == .user
                                    ? {
                                        draft = turn.plainText
                                        focusRequested = true
                                    }
                                    : nil
                            )
                        }

                        if let pendingUserText {
                            UserBubble(
                                text: pendingUserText,
                                hovered: false,
                                onCopy: nil,
                                onEdit: nil
                            )
                        }

                        if let live {
                            AssistantBubble(
                                agentName: agentName,
                                thinking: live.thinking,
                                tools: live.tools,
                                text: live.text,
                                isRunning: live.isRunning,
                                onCopy: nil
                            )
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
                isRunning: isRunning,
                canSend: !draft.isEmpty && !isSending && !isRunning,
                onSend: {
                    let text = draft
                    Task {
                        if await onSend(text) {
                            draft = ""
                        }
                    }
                },
                onStop: onStop
            )
            .padding(12)
        }
        .navigationTitle(detail.conversation.displayTitle)
    }
}

private struct TurnRow: View {
    let turn: ConversationTurn
    let agentName: String
    let onEdit: (() -> Void)?

    @State private var hovered = false

    var body: some View {
        Group {
            switch turn.role {
            case .user:
                UserBubble(
                    text: turn.plainText,
                    hovered: hovered,
                    onCopy: { copy(turn.plainText) },
                    onEdit: onEdit
                )
            case .system:
                SystemRows(blocks: turn.blocks)
            default:
                AssistantBubble(
                    agentName: agentName,
                    blocks: turn.blocks,
                    thinking: thinkingText,
                    tools: [],
                    text: "",
                    isRunning: false,
                    onCopy: { copy(turn.plainText) }
                )
            }
        }
        .onHover { hovered = $0 }
    }

    private var thinkingText: String {
        turn.blocks.compactMap { block in
            if case let .thinking(text) = block { return text }
            return nil
        }
        .joined(separator: "\n")
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

private struct UserBubble: View {
    let text: String
    let hovered: Bool
    let onCopy: (() -> Void)?
    let onEdit: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Spacer(minLength: 80)

            VStack(alignment: .trailing, spacing: 4) {
                Text("conversation.role.user")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Text(text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color(nsColor: .controlBackgroundColor))
                    )

                if let onCopy, hovered {
                    HStack(spacing: 2) {
                        IconAction(symbol: "doc.on.doc", help: "conversation.copy", action: onCopy)
                        if let onEdit {
                            IconAction(symbol: "pencil", help: "conversation.editResend", action: onEdit)
                        }
                    }
                }
            }

            MessageAvatar(name: String(localized: "conversation.role.user"))
        }
    }
}

private struct AssistantBubble: View {
    let agentName: String
    var blocks: [ConversationBlock] = []
    let thinking: String
    var tools: [LiveTool] = []
    var text: String = ""
    let isRunning: Bool
    let onCopy: (() -> Void)?

    @State private var thinkingExpanded = false
    @State private var hovered = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            MessageAvatar(name: agentName)

            VStack(alignment: .leading, spacing: 4) {
                Text(agentName)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 8) {
                    if !thinking.isEmpty {
                        DisclosureGroup(isExpanded: $thinkingExpanded) {
                            Text(thinking)
                                .italic()
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.top, 4)
                        } label: {
                            Text("conversation.thinking")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                        }
                    }

                    ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                        blockView(block)
                    }

                    ForEach(tools) { tool in
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

                    if !text.isEmpty {
                        Text(text)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }

                    if isRunning {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Color(nsColor: .controlBackgroundColor))
                )

                if let onCopy, hovered {
                    HStack(spacing: 2) {
                        IconAction(symbol: "doc.on.doc", help: "conversation.copy", action: onCopy)
                    }
                }
            }

            Spacer(minLength: 40)
        }
        .onHover { hovered = $0 }
        .onAppear {
            thinkingExpanded = isRunning
        }
        .onChange(of: isRunning) { _, running in
            thinkingExpanded = running
        }
    }

    @ViewBuilder
    private func blockView(_ block: ConversationBlock) -> some View {
        switch block {
        case let .text(value):
            Text(value)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

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

        case let .other(value):
            Text(value)
                .font(.caption)
                .foregroundStyle(.secondary)

        case .thinking:
            EmptyView()
        }
    }
}

private struct SystemRows: View {
    let blocks: [ConversationBlock]

    var body: some View {
        VStack(spacing: 4) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                if case let .turnError(message) = block {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
        }
        .frame(maxWidth: .infinity)
    }
}

private struct IconAction: View {
    let symbol: String
    let help: LocalizedStringKey
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.caption)
        }
        .buttonStyle(.borderless)
        .foregroundStyle(.secondary)
        .help(help)
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
