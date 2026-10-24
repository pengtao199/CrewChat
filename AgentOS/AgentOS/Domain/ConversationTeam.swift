import Foundation

enum ConversationSeat: String, Sendable, Equatable {
    case leader
    case member
    case unknown

    init(raw: String?) {
        self = ConversationSeat(rawValue: raw ?? "") ?? .unknown
    }
}

enum ConversationMemberState: String, Sendable, Equatable {
    case idle
    case connecting
    case running
    case waiting
    case done
    case failed
    case cancelled
    case offline
    case unknown

    init(raw: String?) {
        self = ConversationMemberState(rawValue: raw ?? "") ?? .unknown
    }
}

struct ConversationMember: Identifiable, Sendable, Equatable {
    let roleID: String
    let name: String
    let seat: ConversationSeat
    let agentType: String
    let model: String
    let state: ConversationMemberState
    let connectionID: String?
    let permissionMode: String
    let autoApprove: Bool
    let skillCount: Int?
    let mcpCount: Int?

    var id: String { roleID }

    var isLeader: Bool { seat == .leader }
}

enum ConversationDelegationStatus: String, Sendable, Equatable {
    case queued
    case running
    case done
    case failed
    case cancelled
    case unknown

    init(raw: String?) {
        self = ConversationDelegationStatus(rawValue: raw ?? "") ?? .unknown
    }

    var isActive: Bool {
        self == .queued || self == .running
    }
}

struct ConversationDelegation: Identifiable, Sendable, Equatable {
    let id: String
    let conversationID: String
    let leaderRoleID: String
    let memberRoleID: String
    let childConversationID: String?
    let task: String
    let context: String?
    let expectedOutput: String?
    let status: ConversationDelegationStatus
    let summary: String?
    let usage: JSONValue?
    let error: String?
    let createdAt: String
    let updatedAt: String
}
