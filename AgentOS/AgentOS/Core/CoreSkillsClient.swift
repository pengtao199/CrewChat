import Foundation

final class CoreSkillsClient: CoreSkills, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func library() async throws -> [Skill] {
        let response: CoreSkillList = try await provider.client().send(
            "skill_list",
            body: EmptyBody()
        )

        var linksBySkill: [String: [SkillGlobalLink]] = [:]
        for link in response.globalLinks {
            linksBySkill[link.skillId, default: []].append(
                SkillGlobalLink(agentType: link.agentType, state: link.state)
            )
        }

        return response.skills.map { skill in
            skill.domain(links: linksBySkill[skill.id] ?? [])
        }
    }

    func content(skillID: String) async throws -> SkillContent {
        let response: CoreSkillDetail = try await provider.client().send(
            "skill_read",
            body: CoreSkillReadBody(skillID: skillID)
        )
        return response.domain
    }
}
