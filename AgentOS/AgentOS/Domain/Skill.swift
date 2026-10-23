import Foundation

struct Skill: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let description: String?
    let sourceKind: String?
    let sourceURL: String?
    let sourceRef: String?
    let sourceSubdir: String?
    let currentHash: String?
    let license: String?
    let sizeBytes: UInt64?
    let hasScripts: Bool?
    let installedAt: String?
    let updatedAt: String?
    let agentsUsing: [String]
    let globalLinks: [SkillGlobalLink]
    let hasUpdate: Bool
}

struct SkillGlobalLink: Identifiable, Sendable, Equatable {
    let agentType: String
    let state: String

    var id: String { agentType }
}

struct SkillFile: Identifiable, Sendable, Equatable {
    let path: String
    let size: UInt64

    var id: String { path }
}

struct SkillContent: Sendable, Equatable {
    let skill: Skill
    let content: String
    let files: [SkillFile]
}
