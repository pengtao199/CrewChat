import Foundation

struct ConversationDetail: Sendable, Equatable {
    let conversation: Conversation
    let turns: [ConversationTurn]
}

struct ConversationTurn: Identifiable, Sendable, Equatable {
    let id: String
    let role: TurnRole
    let timestamp: String
    let blocks: [ConversationBlock]
}

extension ConversationTurn {
    var plainText: String {
        blocks.compactMap { block in
            if case let .text(text) = block { return text }
            return nil
        }
        .joined(separator: "\n\n")
    }
}

enum TurnRole: String, Decodable, Sendable {
    case user
    case assistant
    case system
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = TurnRole(rawValue: raw) ?? .unknown
    }
}

enum ConversationBlock: Sendable, Equatable {
    case text(String)
    case thinking(String)
    case toolUse(name: String, preview: String?)
    case toolResult(preview: String?, isError: Bool)
    case turnError(String)
    case other(String)
}
