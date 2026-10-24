import SwiftUI

struct AgentAvatar: View {
    let name: String
    var size: CGFloat = 28
    var state: ConversationMemberState?
    var isLeader: Bool = false

    var body: some View {
        Circle()
            .fill(.quaternary)
            .frame(width: size, height: size)
            .overlay {
                Text(String(name.prefix(1)))
                    .font(.system(size: max(10, size * 0.38), weight: .semibold))
            }
            .overlay {
                if isLeader {
                    Circle()
                        .strokeBorder(.tint, lineWidth: 1.5)
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if let state {
                    Circle()
                        .fill(state.color)
                        .frame(width: max(7, size * 0.24), height: max(7, size * 0.24))
                        .overlay {
                            Circle().strokeBorder(.background, lineWidth: 1.5)
                        }
                        .offset(x: 1, y: 1)
                }
            }
            .accessibilityLabel(name)
    }
}

extension ConversationMemberState {
    var label: LocalizedStringKey {
        switch self {
        case .idle: "conversation.member.state.idle"
        case .connecting: "conversation.member.state.connecting"
        case .running: "conversation.member.state.running"
        case .waiting: "conversation.member.state.waiting"
        case .done: "conversation.member.state.done"
        case .failed: "conversation.member.state.failed"
        case .cancelled: "conversation.member.state.cancelled"
        case .offline: "conversation.member.state.offline"
        case .unknown: "conversation.member.state.unknown"
        }
    }

    var color: Color {
        switch self {
        case .idle, .done: .green
        case .connecting, .running: .orange
        case .waiting: .yellow
        case .failed: .red
        case .cancelled, .offline, .unknown: .secondary
        }
    }
}
