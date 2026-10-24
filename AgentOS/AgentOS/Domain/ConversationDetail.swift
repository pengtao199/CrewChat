import Foundation

struct ConversationDetail: Sendable, Equatable {
    let conversation: Conversation
    let turns: [ConversationTurn]
    let members: [ConversationMember]
    let delegations: [ConversationDelegation]

    var leader: ConversationMember? {
        members.first { $0.isLeader }
    }

    var teamMembers: [ConversationMember] {
        members.filter { !$0.isLeader }
    }

    var activeDelegations: [ConversationDelegation] {
        delegations.filter(\.status.isActive)
    }

    func member(id: String?) -> ConversationMember? {
        guard let id else { return nil }
        return members.first { $0.roleID == id }
    }

    func delegation(for roleID: String) -> ConversationDelegation? {
        delegations
            .filter { $0.memberRoleID == roleID }
            .max { $0.updatedAt < $1.updatedAt }
    }
}

struct ConversationTurn: Identifiable, Sendable, Equatable {
    let id: String
    let role: TurnRole
    let speakerRoleID: String?
    let timestamp: String
    let blocks: [ConversationBlock]
    let usage: TokenUsage?
    let duration: TimeInterval?
    let model: String?
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
    case toolUse(
        id: String,
        name: String,
        preview: String?,
        status: String,
        duration: TimeInterval?
    )
    case toolResult(
        id: String,
        name: String,
        preview: String?,
        isError: Bool,
        status: String,
        duration: TimeInterval?
    )
    case turnError(String)
    case other(String)
}
