import SwiftUI

struct MessageMetadataView: View {
    let engineName: String
    let model: String
    let usage: CoreTokenUsage?
    let duration: TimeInterval?
    let isRunning: Bool
    let onCopy: (() -> Void)?
    let copyVisible: Bool

    var body: some View {
        HStack(spacing: 6) {
            Text(verbatim: engineName)
            Text(verbatim: "·")
            Text(verbatim: model.isEmpty
                ? String(localized: "conversation.member.defaultModel")
                : model)
            if let tokens = usage?.displayTotal {
                Text(verbatim: "·")
                Text(verbatim: tokenLabel(tokens))
            }
            if let duration {
                Text(verbatim: "·")
                Text(verbatim: durationLabel(duration))
            }
            if isRunning {
                Text("conversation.meta.streaming")
            }
            if let onCopy {
                Button(action: onCopy) {
                    Image(systemName: "doc.on.doc")
                        .font(.system(size: 10, weight: .regular))
                        .frame(width: 16, height: 16)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .opacity(copyVisible ? 1 : 0)
                .allowsHitTesting(copyVisible)
                .help("conversation.copy")
            }
        }
        .font(.caption2)
        .frame(height: 16)
        .foregroundStyle(.tertiary)
        .lineLimit(1)
    }

    private func tokenLabel(_ count: Int) -> String {
        if count >= 1000 {
            return String(format: "%.1fk tok", Double(count) / 1000)
        }
        return "\(count) tok"
    }

    private func durationLabel(_ duration: TimeInterval) -> String {
        if duration >= 60 {
            return String(format: "%dm %.0fs", Int(duration) / 60, duration.truncatingRemainder(dividingBy: 60))
        }
        return String(format: "%.0fs", duration)
    }
}

enum ConversationTime {
    static func text(_ date: Date) -> String {
        let output = DateFormatter()
        output.dateFormat = "HH:mm"
        return output.string(from: date)
    }

    static func text(_ value: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        var date = formatter.date(from: value)
        if date == nil {
            formatter.formatOptions = [.withInternetDateTime]
            date = formatter.date(from: value)
        }
        guard let date else {
            return value.count >= 16 ? String(value.suffix(11).prefix(5)) : value
        }
        let output = DateFormatter()
        output.dateFormat = "HH:mm"
        return output.string(from: date)
    }
}
