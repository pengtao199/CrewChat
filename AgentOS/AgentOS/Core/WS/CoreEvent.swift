import Foundation

struct CoreTokenUsage: Sendable, Equatable {
    let input: Int?
    let output: Int?
    let total: Int?

    var displayTotal: Int? {
        total ?? {
            guard input != nil || output != nil else { return nil }
            return (input ?? 0) + (output ?? 0)
        }()
    }
}

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
    case usage(CoreTokenUsage)
    case permissionRequest(CorePermissionRequest)
    case permissionAutoApproved(requestID: String, optionName: String?)
    case delegationStarted(id: String, memberRoleID: String?, task: String)
    case delegationCompleted(id: String, memberRoleID: String?, status: String, summary: String?, error: String?)
    case delegationCancelled(id: String, memberRoleID: String?)
    case conversationCancelled(cancelledDelegations: Int)
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
