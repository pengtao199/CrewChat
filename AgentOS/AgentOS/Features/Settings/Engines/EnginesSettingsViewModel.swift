import Foundation

@MainActor
@Observable
final class EnginesSettingsViewModel {
    private(set) var engines: [Engine] = []
    private(set) var agents: [AgentRole] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let client: CoreEngines
    private let roster: CoreRoster

    init(engines: CoreEngines, roster: CoreRoster) {
        self.client = engines
        self.roster = roster
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        async let engineRows = client.engines()
        async let agentRows = roster.agents()

        do {
            engines = try await engineRows
        } catch {
            errorMessage = error.localizedDescription
        }

        // The roster is only used for impact copy ("N 个 Agent 在用"); a
        // failure here must not hide the engines themselves.
        agents = (try? await agentRows) ?? []

        isLoading = false
    }

    func agentsUsing(_ agentType: String) -> [AgentRole] {
        agents.filter { $0.agentType == agentType }
    }

    func update(_ engine: Engine, enabled: Bool, env: [String: String]) async throws {
        try await client.setPreferences(
            EnginePreferences(
                agentType: engine.agentType,
                enabled: enabled,
                env: env
            )
        )
        await load()
    }
}
