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

struct SkillUpdate: Sendable, Equatable {
    let currentHash: String
    let latestHash: String
    let updateAvailable: Bool
}

struct SkillInstallRequest: Sendable {
    let path: String?
    let source: String?
    let gitRef: String?
    let subdir: String?
}
