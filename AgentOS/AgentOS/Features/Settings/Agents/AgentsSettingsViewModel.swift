import SwiftUI

@MainActor
@Observable
final class AgentsSettingsViewModel {
    private(set) var agents: [AgentRole] = []
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private(set) var face: ModelFace?
    private(set) var skills: [CapabilityOption] = []
    private(set) var mcpServers: [CapabilityOption] = []
    private(set) var isLoadingFace = false
    private(set) var isLoadingCapabilities = false

    private let roster: CoreRoster
    private let enginesClient: CoreEngines
    private let capabilities: CoreCapabilities

    init(roster: CoreRoster, engines: CoreEngines, capabilities: CoreCapabilities) {
        self.roster = roster
        self.enginesClient = engines
        self.capabilities = capabilities
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

    func save(_ role: AgentRole) async throws -> AgentRole {
        let saved = try await roster.saveAgent(role)
        await load()
        return saved
    }

    func loadFace(agentType: String) async {
        guard !agentType.isEmpty else {
            face = nil
            return
        }
        isLoadingFace = true
        face = try? await roster.modelFace(agentType: agentType)
        isLoadingFace = false
    }

    func loadCapabilities(agentType: String) async {
        guard !agentType.isEmpty else {
            skills = []
            return
        }
        isLoadingCapabilities = true
        async let skills = try? capabilities.skills(agentType: agentType)
        async let mcp = try? capabilities.mcpServers()
        self.skills = await skills ?? []
        self.mcpServers = await mcp ?? []
        isLoadingCapabilities = false
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
