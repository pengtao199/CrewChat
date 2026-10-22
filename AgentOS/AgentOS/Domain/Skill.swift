import Foundation

enum SkillScope: String, Codable, CaseIterable, Identifiable, Sendable {
    case global
    case project

    var id: String { rawValue }
}

enum SkillLayout: String, Codable, Sendable {
    case markdownFile = "markdown_file"
    case skillDirectory = "skill_directory"
}

struct SkillItem: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let scope: SkillScope
    let layout: SkillLayout
    let path: String
    let description: String?
    let readOnly: Bool
}

struct SkillLocation: Identifiable, Sendable, Equatable {
    let scope: SkillScope
    let path: String
    let exists: Bool

    var id: String { "\(scope.rawValue):\(path)" }
}

struct SkillCatalog: Sendable, Equatable {
    let supported: Bool
    let message: String?
    let locations: [SkillLocation]
    let skills: [SkillItem]

    static let empty = SkillCatalog(
        supported: true,
        message: nil,
        locations: [],
        skills: []
    )
}

struct SkillContent: Sendable, Equatable {
    let skill: SkillItem
    let content: String
}
