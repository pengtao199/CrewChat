import SwiftUI

@MainActor
@Observable
final class AgentsSettingsViewModel {
    private(set) var agents: [AgentRole] = []
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let roster: CoreRoster
    private let enginesClient: CoreEngines

    init(roster: CoreRoster, engines: CoreEngines) {
        self.roster = roster
        self.enginesClient = engines
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            async let agents = roster.agents()
            async let engines = enginesClient.engines()
            self.agents = try await agents
            self.engines = try await engines
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func save(_ role: AgentRole) async throws {
        _ = try await roster.saveAgent(role)
        await load()
    }

    func delete(_ id: String) async {
        do {
            try await roster.deleteAgent(id: id)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
