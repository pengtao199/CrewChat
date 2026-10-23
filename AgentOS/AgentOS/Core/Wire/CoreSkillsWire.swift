import Foundation

struct CoreSkillCatalog: Decodable, Sendable {
    let skills: [CoreSkillSummary]
    let collections: [CoreSkillCollection]
    let globalLinks: [CoreSkillGlobalLink]
}

struct CoreSkillCollection: Decodable, Sendable {
    let id: String
    let name: String
    let description: String
    let skillIDs: [String]

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case description
        case skillIDs = "skillIds"
    }
}

struct CoreSkillGlobalLink: Decodable, Sendable {
    let skillID: String
    let agentType: String
    let path: String
    let state: String
    let linkedAt: String

    enum CodingKeys: String, CodingKey {
        case skillID = "skillId"
        case agentType
        case path
        case state
        case linkedAt
    }
}

struct CoreSkillSummary: Decodable, Sendable {
    let id: String
    let name: String
    let description: String?
    let currentHash: String?
    let license: String?
    let sourceKind: String?
    let sourceURL: String?
    let sourceRef: String?
    let sourceSubdir: String?
    let hasScripts: Bool?
    let sizeBytes: UInt64?
    let installedAt: String?
    let updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case description
        case currentHash
        case license
        case sourceKind
        case sourceURL = "sourceUrl"
        case sourceRef
        case sourceSubdir
        case hasScripts
        case sizeBytes
        case installedAt
        case updatedAt
    }

    func domain(globalLinks: [CoreSkillGlobalLink]) -> Skill {
        Skill(
            id: id,
            name: name,
            description: description,
            sourceKind: sourceKind,
            sourceURL: sourceURL,
            currentHash: currentHash,
            license: license,
            sizeBytes: sizeBytes,
            hasScripts: hasScripts,
            installedAt: installedAt,
            updatedAt: updatedAt,
            agentsUsing: [],
            globalLinks: globalLinks
                .filter { $0.skillID == id }
                .map { SkillGlobalLink(agentType: $0.agentType, state: $0.state) },
            hasUpdate: false
        )
    }
}

struct CoreSkillDetail: Decodable, Sendable {
    let skill: CoreSkillSummary
    let content: String
}

struct CoreSkillReadBody: Encodable {
    let skillId: String
}
