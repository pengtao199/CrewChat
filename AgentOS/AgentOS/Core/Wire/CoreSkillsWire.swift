import Foundation

// cli-layer `skill_*` wire shapes. They proxy skill-layer; the Agent never
// talks to the daemon directly.

struct CoreSkillList: Decodable, Sendable {
    let skills: [CoreSkillSummary]
}

struct CoreSkillSummary: Decodable, Sendable {
    let id: String
    let name: String
    let description: String
    let currentHash: String
    let license: String?
    let sourceKind: String
    let sourceUrl: String?
    let sourceRef: String?
    let sourceSubdir: String?
    let hasScripts: Bool
    let sizeBytes: UInt64
    let installedAt: String
    let updatedAt: String

    var domain: Skill {
        Skill(
            id: id,
            name: name,
            description: description,
            sourceKind: sourceKind,
            sourceURL: sourceUrl,
            sourceRef: sourceRef,
            sourceSubdir: sourceSubdir,
            currentHash: currentHash,
            license: license,
            sizeBytes: sizeBytes,
            hasScripts: hasScripts,
            installedAt: installedAt,
            updatedAt: updatedAt
        )
    }
}

struct CoreSkillDetail: Decodable, Sendable {
    let skill: CoreSkillSummary
    let content: String
    let files: [CoreSkillFile]

    var domain: SkillContent {
        SkillContent(
            skill: skill.domain,
            content: content,
            files: files.map(\.domain)
        )
    }
}

struct CoreSkillFile: Decodable, Sendable {
    let path: String
    let size: UInt64

    var domain: SkillFile {
        SkillFile(path: path, size: size)
    }
}

struct CoreSkillResult: Decodable, Sendable {
    let skill: CoreSkillSummary

    var domain: Skill {
        skill.domain
    }
}

struct CoreSkillUpdateCheck: Decodable, Sendable {
    let id: String
    let currentHash: String
    let latestHash: String
    let updateAvailable: Bool

    var domain: SkillUpdate {
        SkillUpdate(
            currentHash: currentHash,
            latestHash: latestHash,
            updateAvailable: updateAvailable
        )
    }
}

struct CoreSkillIdBody: Encodable {
    let skillID: String

    enum CodingKeys: String, CodingKey {
        case skillID = "skillId"
    }
}

struct CoreSkillReadBody: Encodable {
    let skillID: String

    enum CodingKeys: String, CodingKey {
        case skillID = "skillId"
    }
}

struct CoreSkillInstallBody: Encodable {
    let path: String?
    let source: String?
    let gitRef: String?
    let subdir: String?
}
