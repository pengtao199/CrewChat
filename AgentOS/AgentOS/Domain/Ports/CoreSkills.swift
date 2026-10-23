import Foundation

protocol CoreSkills: Sendable {
    func library() async throws -> [Skill]
    func content(skillID: String) async throws -> SkillContent
    func checkUpdate(skillID: String) async throws -> SkillUpdate
    func update(skillID: String) async throws -> Skill
    func remove(skillID: String) async throws
    func install(_ request: SkillInstallRequest) async throws -> Skill
}
