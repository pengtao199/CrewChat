import Foundation

// [crewchat] Legacy bridge DTOs for codeg's per-engine skill commands. The
// skill-layer `skill_list` / `skill_read` commands replace these; until they
// land, the client reads the shared `~/.agents/skills` tree through the first
// engine that can see it. The UI never shows the engine.

struct CoreLegacySkillCatalog: Decodable, Sendable {
    let supported: Bool
    let message: String?
    let skills: [CoreLegacySkill]
}

struct CoreLegacySkill: Decodable, Sendable {
    let id: String
    let name: String
    let scope: String
    let layout: String
    let path: String
    let description: String?
    let readOnly: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case scope
        case layout
        case path
        case description
        case readOnly = "read_only"
    }

    var domain: Skill {
        Skill(
            id: id,
            name: name,
            description: description,
            sourceKind: nil,
            sourceURL: nil,
            currentHash: nil,
            license: nil,
            sizeBytes: nil,
            hasScripts: nil,
            installedAt: nil,
            updatedAt: nil,
            agentsUsing: [],
            globalLinks: [],
            hasUpdate: false
        )
    }
}

struct CoreLegacySkillContent: Decodable, Sendable {
    let skill: CoreLegacySkill
    let content: String
}

struct CoreAgentSkillListBody: Encodable {
    let agentType: String
    let workspacePath: String?
}

struct CoreSkillReadBody: Encodable {
    let agentType: String
    let scope: String
    let skillID: String
    let workspacePath: String?

    enum CodingKeys: String, CodingKey {
        case agentType
        case scope
        case skillID = "skillId"
        case workspacePath
    }
}
