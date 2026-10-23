import Foundation

protocol CoreSkills: Sendable {
    func library() async throws -> [Skill]
    func content(skillID: String) async throws -> SkillContent
}
