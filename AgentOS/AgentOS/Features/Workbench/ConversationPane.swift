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
                        ForEach(groups) { group in
                            groupView(group)
                        }

                        if let pendingUserText {
                            UserBubble(text: pendingUserText, onCopy: nil, onEdit: nil)
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
                    .frame(maxWidth: 860, alignment: .leading)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 20)
                    .padding(.horizontal, 20)
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
            .frame(maxWidth: 860)
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .navigationTitle(detail.conversation.displayTitle)
    }

    private var groups: [TurnGroup] {
        var result: [TurnGroup] = []

        for turn in detail.turns {
            if turn.role == .assistant, var last = result.last, last.role == .assistant {
                last.turns.append(turn)
                result[result.count - 1] = last
            } else {
                result.append(TurnGroup(id: turn.id, role: turn.role, turns: [turn]))
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
                onCopy: { copy(group.plainText) },
                onEdit: {
                    draft = group.plainText
                    focusRequested = true
                }
            )
        case .system:
            SystemRows(blocks: group.blocks)
        default:
            AssistantBubble(
                agentName: agentName,
                blocks: group.blocks,
                thinking: group.thinking,
                isRunning: false,
                onCopy: { copy(group.plainText) }
            )
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

private struct UserBubble: View {
    let text: String
    let onCopy: (() -> Void)?
    let onEdit: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Spacer(minLength: 80)

            VStack(alignment: .trailing, spacing: 4) {
                Text("conversation.role.user")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                HStack(alignment: .bottom, spacing: 6) {
                    if let onCopy {
                        BubbleActions(onCopy: onCopy, onEdit: onEdit)
                    }

                    Text(text)
                        .textSelection(.enabled)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .background(
                            RoundedRectangle(cornerRadius: 12)
                                .fill(Color.primary.opacity(0.07))
                        )
                }
            }

            MessageAvatar(name: String(localized: "conversation.role.user"))
        }
    }
}

private struct AssistantBubble: View {
    let agentName: String
    var blocks: [ConversationBlock] = []
    var thinking: String = ""
    var tools: [LiveTool] = []
    var text: String = ""
    let isRunning: Bool
    let onCopy: (() -> Void)?

    @State private var thinkingExpanded = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            MessageAvatar(name: agentName)

            VStack(alignment: .leading, spacing: 4) {
                Text(agentName)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                HStack(alignment: .bottom, spacing: 6) {
                    VStack(alignment: .leading, spacing: 8) {
                        if !thinking.isEmpty {
                            DisclosureGroup(isExpanded: $thinkingExpanded) {
                                Text(thinking)
                                    .font(.callout)
                                    .italic()
                                    .foregroundStyle(.secondary)
                                    .lineSpacing(2)
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(8)
                                    .background(
                                        RoundedRectangle(cornerRadius: 8)
                                            .fill(Color.primary.opacity(0.04))
                                    )
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
                    .frame(maxWidth: 680, alignment: .leading)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(Color.primary.opacity(0.07))
                    )

                    if let onCopy {
                        BubbleActions(onCopy: onCopy, onEdit: nil)
                    }
                }
            }

            Spacer(minLength: 40)
        }
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

private struct BubbleActions: View {
    let onCopy: () -> Void
    let onEdit: (() -> Void)?

    var body: some View {
        HStack(spacing: 2) {
            Button(action: onCopy) {
                Image(systemName: "doc.on.doc")
                    .font(.caption)
            }
            .buttonStyle(.borderless)
            .help("conversation.copy")

            if let onEdit {
                Button(action: onEdit) {
                    Image(systemName: "pencil")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .help("conversation.editResend")
            }
        }
        .foregroundStyle(.secondary)
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
