import AppKit
import SwiftUI

struct UserBubble: View {
    let text: String
    let members: [ConversationMember]
    let timestamp: String?
    let onCopy: (() -> Void)?

    @State private var isHovered = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Spacer(minLength: 80)

            VStack(alignment: .trailing, spacing: 4) {
                HStack(spacing: 6) {
                    if let timestamp {
                        Text(ConversationTime.text(timestamp))
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                    Text("conversation.role.user")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                HStack(alignment: .bottom, spacing: 6) {
                    BubbleActions(onCopy: onCopy)
                        .opacity(isHovered ? 1 : 0)
                        .allowsHitTesting(isHovered)
                    MentionText(text: text, members: members)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .background(
                            RoundedRectangle(cornerRadius: 12)
                                .fill(Color.primary.opacity(0.07))
                        )
                }
            }

            AgentAvatar(name: String(localized: "conversation.role.user"), size: 26)
        }
        .onHover { isHovered = $0 }
        .textSelection(.enabled)
    }
}

struct AssistantBubble: View {
    let member: ConversationMember?
    let delegation: ConversationDelegation?
    let fallbackName: String
    let engineName: String
    let model: String
    let timestamp: String?
    let members: [ConversationMember]
    let blocks: [ConversationBlock]
    let thinking: String
    let tools: [LiveTool]
    let text: String
    let isRunning: Bool
    let usage: TokenUsage?
    let duration: TimeInterval?
    let onCopy: (() -> Void)?
    let onMention: (ConversationMember) -> Void

    @State private var showProfile = false
    @State private var isHovered = false

    private var displayName: String {
        member?.name ?? fallbackName
    }

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Button {
                if member != nil {
                    showProfile = true
                }
            } label: {
                AgentAvatar(
                    name: displayName,
                    size: 26,
                    state: isRunning ? .running : member?.state,
                    isLeader: member?.isLeader == true
                )
            }
            .buttonStyle(.plain)
            .disabled(member == nil)
            .popover(isPresented: $showProfile, arrowEdge: .bottom) {
                if let member {
                    AgentProfileView(
                        member: member,
                        delegation: delegation,
                        state: isRunning ? .running : member.state,
                        onMention: { onMention(member) }
                    )
                }
            }

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Text(displayName)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if member?.isLeader == true {
                        Text("conversation.member.leader")
                            .font(.caption2)
                            .foregroundStyle(.tint)
                    }
                    if let timestamp {
                        Text(ConversationTime.text(timestamp))
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                }

                if !isRunning {
                    MentionBadges(text: bubbleText, members: members)
                }

                HStack(alignment: .bottom, spacing: 6) {
                    VStack(alignment: .leading, spacing: 8) {
                        if !thinking.isEmpty {
                            ThinkingView(text: thinking, isRunning: isRunning)
                        }

                        ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                            blockView(block)
                        }

                        ToolActivityView(tools: toolItems, isRunning: isRunning)

                        if !text.isEmpty {
                            if isRunning {
                                Text(text)
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            } else {
                                StructuredText(markdown: text)
                                    .textual.structuredTextStyle(.gitHub)
                                    .textual.textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
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
                    .textual.textSelection(.enabled)
                }

                MessageMetadataView(
                    engineName: engineName,
                    model: model,
                    usage: usage,
                    duration: duration,
                    isRunning: isRunning,
                    onCopy: onCopy,
                    copyVisible: isHovered
                )
            }

            Spacer(minLength: 40)
        }
        .onHover { isHovered = $0 }
        .textSelection(.enabled)
    }

    private var toolItems: [ToolActivityItem] {
        if !tools.isEmpty {
            return tools.map(ToolActivityItem.init(live:))
        }

        var items: [ToolActivityItem] = []
        var indexByID: [String: Int] = [:]
        for (index, block) in blocks.enumerated() {
            switch block {
            case let .toolUse(id, name, preview, status, duration):
                let title = toolTitle(name: name, preview: preview)
                let item = ToolActivityItem(
                    id: id.isEmpty ? "block-tool-\(index)" : id,
                    title: title,
                    status: ToolActivityStatus.from(status.isEmpty ? "completed" : status),
                    output: nil,
                    duration: duration
                )
                indexByID[item.id] = items.count
                items.append(item)
            case let .toolResult(id, name, preview, isError, status, duration):
                let key = id.isEmpty ? "block-tool-\(index)" : id
                if let itemIndex = indexByID[key] {
                    items[itemIndex] = ToolActivityItem(
                        id: key,
                        title: items[itemIndex].title,
                        status: isError ? .failed : ToolActivityStatus.from(status),
                        output: preview,
                        duration: duration ?? items[itemIndex].duration
                    )
                } else {
                    items.append(
                        ToolActivityItem(
                            id: key,
                            title: toolTitle(name: name, preview: preview),
                            status: isError ? .failed : ToolActivityStatus.from(status),
                            output: preview,
                            duration: duration
                        )
                    )
                }
            default:
                continue
            }
        }
        return items
    }

    private func toolTitle(name: String, preview: String?) -> String {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let firstLine = preview?
            .split(whereSeparator: \.isNewline)
            .first
            .map(String.init)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if trimmedName.isEmpty || trimmedName == "tool" {
            return firstLine.isEmpty ? String(localized: "conversation.tools.untitled") : firstLine
        }
        return firstLine.isEmpty ? trimmedName : "\(trimmedName) · \(firstLine)"
    }

    private var bubbleText: String {
        if !text.isEmpty { return text }
        return blocks.compactMap { block in
            if case let .text(value) = block { return value }
            return nil
        }
        .joined(separator: "\n\n")
    }

    @ViewBuilder
    private func blockView(_ block: ConversationBlock) -> some View {
        switch block {
        case let .text(value):
            StructuredText(markdown: value)
                .textual.structuredTextStyle(.gitHub)
                .textual.textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)

        case .toolUse, .toolResult:
            EmptyView()

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

struct SystemRows: View {
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

struct BubbleActions: View {
    let onCopy: (() -> Void)?

    var body: some View {
        if let onCopy {
            Button(action: onCopy) {
                Image(systemName: "doc.on.doc")
                    .font(.caption)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .frame(width: 18, height: 18)
            .contentShape(Rectangle())
            .help("conversation.copy")
        }
    }
}
