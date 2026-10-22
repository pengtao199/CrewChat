import Foundation

protocol CoreSkills: Sendable {
    func catalog(agentType: String, workspacePath: String?) async throws -> SkillCatalog
    func content(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        workspacePath: String?
    ) async throws -> SkillContent
    func save(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        content: String,
        workspacePath: String?,
        layout: SkillLayout?
    ) async throws
    func delete(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        workspacePath: String?
    ) async throws
}
