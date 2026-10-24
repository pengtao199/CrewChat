import Foundation

struct CorePermissionOption: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let kind: String
}

struct CorePermissionRequest: Identifiable, Sendable, Equatable {
    let id: String
    let title: String
    let detail: String?
    let options: [CorePermissionOption]
}

enum CoreEvent: Sendable, Equatable {
    case contentDelta(String)
    case thinking(String)
    case toolCall(id: String, title: String, status: String)
    case toolUpdate(id: String, title: String?, status: String?, output: String?)
    case usage(TokenUsage)
    case permissionRequest(CorePermissionRequest)
    case permissionAutoApproved(requestID: String, optionName: String?)
    case delegationStarted(id: String, memberRoleID: String?, task: String)
    case delegationCompleted(id: String, memberRoleID: String?, status: String, summary: String?, error: String?)
    case delegationCancelled(id: String, memberRoleID: String?)
    case conversationCancelled(cancelledDelegations: Int)
    case memberAdded(roleID: String)
    case turnComplete
    case detached
    case status(String)
    case other(String)
}

struct CoreEventFrame: Sendable, Equatable {
    let event: CoreEvent
    let conversationID: String?
    let speakerRoleID: String?
    let seq: UInt64
    let isReplay: Bool
}
