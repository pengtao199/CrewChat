import Foundation

final class CoreEnginesClient: CoreEngines, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func engines() async throws -> [Engine] {
        let rows: [CoreAcpAgent] = try await provider.client().send("acp_list_agents", body: EmptyBody())
        return rows.map(\.domain)
    }

    func setEngineEnabled(_ agentType: String, enabled: Bool) async throws {
        let current = try await engines().first { $0.agentType == agentType }
        try await setPreferences(
            EnginePreferences(
                agentType: agentType,
                enabled: enabled,
                env: current?.env ?? [:]
            )
        )
    }

    func setPreferences(_ preferences: EnginePreferences) async throws {
        let saved = CoreAgentPreferences(
            agentType: preferences.agentType,
            enabled: preferences.enabled,
            env: preferences.env
        )
        try await provider.client().sendIgnoring("acp_update_agent_preferences", body: saved)
    }
}
