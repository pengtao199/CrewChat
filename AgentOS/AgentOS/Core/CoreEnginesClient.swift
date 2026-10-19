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
        let saved = CoreAgentPreferences(
            agentType: agentType,
            enabled: enabled,
            env: current?.env ?? [:]
        )
        try await provider.client().sendIgnoring("acp_update_agent_preferences", body: saved)
    }
}
