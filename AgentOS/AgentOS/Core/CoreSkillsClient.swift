import Foundation

final class CoreSkillsClient: CoreSkills, @unchecked Sendable {
    private let provider: CoreClientProvider

    /// Install/update may fetch Git or archives; the default 60s is too tight.
    private let longTimeout: TimeInterval = 300

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func library() async throws -> [Skill] {
        let response: CoreSkillList = try await provider.client().send(
            "skill_list",
            body: EmptyBody()
        )
        return response.skills.map(\.domain)
    }

    func content(skillID: String) async throws -> SkillContent {
        let response: CoreSkillDetail = try await provider.client().send(
            "skill_read",
            body: CoreSkillReadBody(skillID: skillID)
        )
        return response.domain
    }

    func checkUpdate(skillID: String) async throws -> SkillUpdate {
        let response: CoreSkillUpdateCheck = try await provider.client().send(
            "skill_check",
            body: CoreSkillIdBody(skillID: skillID),
            timeout: 120
        )
        return response.domain
    }

    func update(skillID: String) async throws -> Skill {
        let response: CoreSkillResult = try await provider.client().send(
            "skill_update",
            body: CoreSkillIdBody(skillID: skillID),
            timeout: longTimeout
        )
        return response.domain
    }

    func remove(skillID: String) async throws {
        try await provider.client().sendIgnoring(
            "skill_remove",
            body: CoreSkillIdBody(skillID: skillID)
        )
    }

    func install(_ request: SkillInstallRequest) async throws -> Skill {
        let response: CoreSkillResult = try await provider.client().send(
            "skill_install",
            body: CoreSkillInstallBody(
                path: request.path,
                source: request.source,
                gitRef: request.gitRef,
                subdir: request.subdir
            ),
            timeout: longTimeout
        )
        return response.domain
    }
}
