import Foundation

// [crewchat] Read-only bridge to the legacy per-engine skill catalog.
//
// The skill-layer model has one global library (`~/.agents/skills`), but the
// core does not expose it through `skill_list` yet. Codex, OpenCode and Gemini
// all read that shared tree, so the first one that answers gives us the same
// content; the engine never reaches the UI. Swap this client for the skill
// command when it lands.

final class CoreSkillsClient: CoreSkills, @unchecked Sendable {
    private static let candidates = ["codex", "open_code", "gemini", "claude_code"]

    private let provider: CoreClientProvider
    private var resolvedAgentType = "codex"

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func library() async throws -> [Skill] {
        var lastError: Error?

        for candidate in Self.candidates {
            do {
                let catalog: CoreLegacySkillCatalog = try await provider.client().send(
                    "acp_list_agent_skills",
                    body: CoreAgentSkillListBody(
                        agentType: candidate,
                        workspacePath: nil
                    )
                )

                guard catalog.supported else { continue }

                resolvedAgentType = candidate
                return uniqueGlobalSkills(catalog.skills)
            } catch {
                lastError = error
            }
        }

        if let lastError {
            throw lastError
        }
        return []
    }

    func content(skillID: String) async throws -> SkillContent {
        let result: CoreLegacySkillContent = try await provider.client().send(
            "acp_read_agent_skill",
            body: CoreSkillReadBody(
                agentType: resolvedAgentType,
                scope: "global",
                skillID: skillID,
                workspacePath: nil
            )
        )

        return SkillContent(skill: result.skill.domain, content: result.content)
    }

    private func uniqueGlobalSkills(_ rows: [CoreLegacySkill]) -> [Skill] {
        var seen = Set<String>()
        var skills: [Skill] = []

        for row in rows where row.scope == "global" && !row.readOnly {
            guard seen.insert(row.id).inserted else { continue }
            skills.append(row.domain)
        }

        return skills
    }
}
