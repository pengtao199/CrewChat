import Foundation

@MainActor
@Observable
final class AgentsSettingsViewModel {
    private(set) var agents: [AgentRole] = []
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    /// Set by the editor; the settings container asks before leaving the page.
    var hasUnsavedChanges = false
    private(set) var enginesError: String?

    private(set) var face: ModelFace?
    private(set) var faceError: String?
    private(set) var skills: [CapabilityOption] = []
    private(set) var skillsError: String?
    private(set) var mcpServers: [CapabilityOption] = []
    private(set) var mcpError: String?
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
        enginesError = nil

        async let agentRows = roster.agents()
        async let engineRows = enginesClient.engines()

        do {
            agents = try await agentRows
        } catch {
            errorMessage = error.localizedDescription
        }

        do {
            engines = try await engineRows
        } catch {
            enginesError = error.localizedDescription
        }

        isLoading = false
    }

    func save(_ role: AgentRole) async throws -> AgentRole {
        let saved = try await roster.saveAgent(role)
        await load()
        return saved
    }

    func delete(_ id: String) async throws {
        try await roster.deleteAgent(id: id)
        await load()
    }

    func loadFace(agentType: String) async {
        guard !agentType.isEmpty else {
            face = nil
            faceError = nil
            return
        }

        isLoadingFace = true
        do {
            face = try await roster.modelFace(agentType: agentType)
            faceError = nil
        } catch {
            face = nil
            faceError = error.localizedDescription
        }
        isLoadingFace = false
    }

    func loadCapabilities(agentType: String) async {
        guard !agentType.isEmpty else {
            skills = []
            mcpServers = []
            skillsError = nil
            mcpError = nil
            return
        }

        isLoadingCapabilities = true

        async let skillRows = capabilities.skills(agentType: agentType)
        async let mcpRows = capabilities.mcpServers()

        do {
            skills = try await skillRows
            skillsError = nil
        } catch {
            skills = []
            skillsError = error.localizedDescription
        }

        do {
            mcpServers = try await mcpRows
            mcpError = nil
        } catch {
            mcpServers = []
            mcpError = error.localizedDescription
        }

        isLoadingCapabilities = false
    }

    func engineName(for agentType: String) -> String {
        engines.first { $0.agentType == agentType }?.name ?? agentType
    }
}
