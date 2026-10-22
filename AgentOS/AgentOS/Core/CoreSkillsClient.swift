import Foundation

final class CoreSkillsClient: CoreSkills, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func catalog(agentType: String, workspacePath: String?) async throws -> SkillCatalog {
        let result: CoreSkillCatalog = try await provider.client().send(
            "acp_list_agent_skills",
            body: CoreAgentSkillListBody(agentType: agentType, workspacePath: workspacePath)
        )
        return result.domain
    }

    func content(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        workspacePath: String?
    ) async throws -> SkillContent {
        let result: CoreSkillContent = try await provider.client().send(
            "acp_read_agent_skill",
            body: CoreSkillReadBody(
                agentType: agentType,
                scope: scope.rawValue,
                skillID: skillID,
                workspacePath: workspacePath
            )
        )
        return result.domain
    }

    func save(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        content: String,
        workspacePath: String?,
        layout: SkillLayout?
    ) async throws {
        try await provider.client().sendIgnoring(
            "acp_save_agent_skill",
            body: CoreSkillSaveBody(
                agentType: agentType,
                scope: scope.rawValue,
                skillID: skillID,
                content: content,
                workspacePath: workspacePath,
                layout: layout?.rawValue
            )
        )
    }

    func delete(
        agentType: String,
        scope: SkillScope,
        skillID: String,
        workspacePath: String?
    ) async throws {
        try await provider.client().sendIgnoring(
            "acp_delete_agent_skill",
            body: CoreSkillDeleteBody(
                agentType: agentType,
                scope: scope.rawValue,
                skillID: skillID,
                workspacePath: workspacePath
            )
        )
    }
}
