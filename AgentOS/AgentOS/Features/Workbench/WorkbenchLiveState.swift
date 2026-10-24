import Foundation

struct LiveTool: Identifiable, Equatable {
    let id: String
    var title: String
    var status: String
    var output: String?
}

struct LiveTurn: Identifiable, Equatable {
    let id: String
    let speakerRoleID: String?
    var text: String
    var thinking: String
    var tools: [LiveTool]
    var isRunning: Bool
    var usage: CoreTokenUsage?
    var startedAt: Date
    var duration: TimeInterval?

    var displayTokens: Int? {
        usage?.displayTotal
    }
}

struct PendingPermission: Identifiable, Equatable {
    let request: CorePermissionRequest
    let speakerRoleID: String?

    var id: String { request.id }
}
