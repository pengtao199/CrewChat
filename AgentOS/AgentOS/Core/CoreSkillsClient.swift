import Foundation

final class CoreSkillsClient: CoreSkills, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func library() async throws -> [Skill] {
        let catalog: CoreSkillCatalog = try await provider.client().send(
            "skill_list",
            body: EmptyBody()
        )
        return catalog.skills.map { $0.domain(globalLinks: catalog.globalLinks) }
    }

    func content(skillID: String) async throws -> SkillContent {
        let detail: CoreSkillDetail = try await provider.client().send(
            "skill_read",
            body: CoreSkillReadBody(skillId: skillID)
        )
        return SkillContent(
            skill: detail.skill.domain(globalLinks: []),
            content: detail.content
        )
    }
}
