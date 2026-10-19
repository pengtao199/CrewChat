import Foundation

struct CoreAgentRole: Codable, Sendable {
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

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case soul
        case rules
        case agentType = "agent_type"
        case model
        case modelProvider = "model_provider"
        case skills
        case mcp
        case permissionMode = "permission_mode"
        case autoApprove = "auto_approve"
    }

    init(_ role: AgentRole) {
        id = role.id
        name = role.name
        soul = role.soul
        rules = role.rules
        agentType = role.agentType
        model = role.model
        modelProvider = role.modelProvider
        skills = role.skills
        mcp = role.mcp
        permissionMode = role.permissionMode
        autoApprove = role.autoApprove
    }

    var domain: AgentRole {
        AgentRole(
            id: id,
            name: name,
            soul: soul,
            rules: rules,
            agentType: agentType,
            model: model,
            modelProvider: modelProvider,
            skills: skills,
            mcp: mcp,
            permissionMode: permissionMode,
            autoApprove: autoApprove
        )
    }
}

struct CoreAgentRoleSave: Encodable {
    let role: CoreAgentRole
}
