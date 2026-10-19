import Foundation

struct AgentRole: Identifiable, Sendable, Equatable {
    var id: String
    var name: String
    var soul: String
    var rules: String
    var agentType: String
    var model: String
    var modelProvider: String
    var skills: [String]
    var mcp: [String]
    var permissionMode: String
    var autoApprove: Bool

    static let empty = AgentRole(
        id: "",
        name: "",
        soul: "",
        rules: "",
        agentType: "",
        model: "",
        modelProvider: "",
        skills: [],
        mcp: [],
        permissionMode: "",
        autoApprove: false
    )
}
