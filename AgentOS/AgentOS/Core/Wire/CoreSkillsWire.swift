import Foundation

struct CoreSkillCatalog: Decodable, Sendable {
    let supported: Bool
    let message: String?
    let locations: [CoreSkillLocation]
    let skills: [CoreSkillDetail]

    enum CodingKeys: String, CodingKey {
        case supported
        case message
        case locations
        case skills
    }

    var domain: SkillCatalog {
        SkillCatalog(
            supported: supported,
            message: message,
            locations: locations.map(\.domain),
            skills: skills.map(\.domain)
        )
    }
}

struct CoreSkillLocation: Decodable, Sendable {
    let scope: SkillScope
    let path: String
    let exists: Bool

    var domain: SkillLocation {
        SkillLocation(scope: scope, path: path, exists: exists)
    }
}

struct CoreSkillDetail: Decodable, Sendable {
    let id: String
    let name: String
    let scope: SkillScope
    let layout: SkillLayout
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

    var domain: SkillItem {
        SkillItem(
            id: id,
            name: name,
            scope: scope,
            layout: layout,
            path: path,
            description: description,
            readOnly: readOnly
        )
    }
}

struct CoreSkillContent: Decodable, Sendable {
    let skill: CoreSkillDetail
    let content: String

    var domain: SkillContent {
        SkillContent(skill: skill.domain, content: content)
    }
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

struct CoreSkillSaveBody: Encodable {
    let agentType: String
    let scope: String
    let skillID: String
    let content: String
    let workspacePath: String?
    let layout: String?

    enum CodingKeys: String, CodingKey {
        case agentType
        case scope
        case skillID = "skillId"
        case content
        case workspacePath
        case layout
    }
}

struct CoreSkillDeleteBody: Encodable {
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
