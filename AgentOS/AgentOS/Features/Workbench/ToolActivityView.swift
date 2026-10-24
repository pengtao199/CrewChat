import SwiftUI
import AppKit

enum ToolActivityStatus: Sendable, Equatable {
    case pending
    case running
    case completed
    case failed
    case unknown

    static func from(_ raw: String) -> ToolActivityStatus {
        let value = raw.lowercased()
        if value.contains("fail") || value.contains("error") || value.contains("cancel") {
            return .failed
        }
        if value.contains("complet") || value.contains("success") || value.contains("done") {
            return .completed
        }
        if value.contains("run") || value.contains("progress") || value.contains("start") {
            return .running
        }
        if value.contains("queue") || value.contains("pending") || value.contains("wait") {
            return .pending
        }
        return .unknown
    }
}

struct ToolActivityItem: Identifiable, Equatable {
    let id: String
    let title: String
    let status: ToolActivityStatus
    let output: String?
    let duration: TimeInterval?

    init(
        id: String,
        title: String,
        status: ToolActivityStatus,
        output: String?,
        duration: TimeInterval? = nil
    ) {
        self.id = id
        self.title = title
        self.status = status
        self.output = output
        self.duration = duration
    }

    init(live tool: LiveTool) {
        self.init(
            id: tool.id,
            title: tool.title,
            status: ToolActivityStatus.from(tool.status),
            output: tool.output,
            duration: tool.duration
        )
    }
}

struct ToolActivityView: View {
    let tools: [ToolActivityItem]
    let isRunning: Bool

    @State private var expanded: Bool

    init(tools: [ToolActivityItem], isRunning: Bool) {
        self.tools = tools
        self.isRunning = isRunning
        _expanded = State(initialValue: isRunning)
    }

    var body: some View {
        if !tools.isEmpty {
            DisclosureGroup(isExpanded: $expanded) {
                VStack(spacing: 0) {
                    ForEach(tools) { tool in
                        ToolActivityRow(tool: tool)
                        if tool.id != tools.last?.id {
                            Divider()
                        }
                    }
                }
                .padding(.top, 4)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "wrench.and.screwdriver")
                    Text("conversation.tools.title")
                    Text("\(tools.count)")
                        .foregroundStyle(.secondary)
                    if let failed = failureCount, failed > 0 {
                        Text(String(format: String(localized: "conversation.tools.failed"), failed))
                            .foregroundStyle(.red)
                    }
                    Spacer()
                    if isRunning {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .onChange(of: isRunning) { _, running in
                expanded = running
            }
        }
    }

    private var failureCount: Int? {
        let count = tools.filter { $0.status == .failed }.count
        return count > 0 ? count : nil
    }
}

private struct ToolActivityRow: View {
    let tool: ToolActivityItem

    @State private var outputExpanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                statusIcon
                Text(displayTitle)
                    .font(.system(.caption, design: .monospaced))
                    .lineLimit(2)
                    .textSelection(.enabled)

                Spacer(minLength: 4)

                if let duration = tool.duration {
                    Text(durationLabel(duration))
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }

                if tool.output != nil {
                    Button {
                        outputExpanded.toggle()
                    } label: {
                        Image(systemName: outputExpanded
                            ? "chevron.down"
                            : "chevron.right")
                            .font(.caption2)
                    }
                    .buttonStyle(.borderless)
                    .help("conversation.tools.output")
                }
            }

            if outputExpanded, let output = tool.output, !output.isEmpty {
                ScrollView {
                    Text(preview(output))
                        .font(.system(.caption2, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 220)
                .padding(8)
                .background(
                    RoundedRectangle(cornerRadius: 7)
                        .fill(.primary.opacity(0.04))
                )

                HStack {
                    Button("conversation.tools.copyOutput") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(output, forType: .string)
                    }
                    .buttonStyle(.link)
                    .font(.caption2)
                    Spacer()
                }
            }
        }
        .padding(.vertical, 5)
        .onAppear {
            outputExpanded = tool.status == .failed
        }
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch tool.status {
        case .running:
            ProgressView()
                .controlSize(.mini)
        case .completed:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .failed:
            Image(systemName: "xmark.octagon.fill")
                .foregroundStyle(.red)
        case .pending:
            Image(systemName: "clock")
                .foregroundStyle(.orange)
        case .unknown:
            Image(systemName: "circle")
                .foregroundStyle(.secondary)
        }
    }

    private var displayTitle: String {
        tool.title
            .split(whereSeparator: { $0.isNewline })
            .first
            .map(String.init)?
            .trimmingCharacters(in: .whitespaces)
            .nilIfEmpty ?? String(localized: "conversation.tools.untitled")
    }

    private func preview(_ output: String) -> String {
        if output.count > 4_000 {
            return String(output.prefix(4_000)) + "\n…"
        }
        return output
    }

    private func durationLabel(_ duration: TimeInterval) -> String {
        if duration >= 60 {
            return String(format: "%dm %.0fs", Int(duration) / 60, duration.truncatingRemainder(dividingBy: 60))
        }
        return String(format: "%.1fs", duration)
    }
}

struct ThinkingView: View {
    let text: String
    let isRunning: Bool

    @State private var expanded = false

    var body: some View {
        if !text.isEmpty {
            DisclosureGroup(isExpanded: $expanded) {
                ScrollView {
                    Text(text)
                        .font(.callout)
                        .italic()
                        .foregroundStyle(.secondary)
                        .lineSpacing(2)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
                .frame(maxHeight: 220)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(.primary.opacity(0.04))
                )
            } label: {
                HStack(spacing: 6) {
                    if isRunning {
                        ProgressView()
                            .controlSize(.mini)
                    }
                    Text(isRunning
                        ? "conversation.thinking.running"
                        : "conversation.thinking")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .onChange(of: isRunning) { _, running in
                if !running { expanded = false }
            }
        }
    }
}

private extension String {
    var nilIfEmpty: String? {
        isEmpty ? nil : self
    }
}
