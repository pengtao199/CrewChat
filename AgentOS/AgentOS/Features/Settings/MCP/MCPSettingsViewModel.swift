import Foundation

@MainActor
@Observable
final class MCPSettingsViewModel {
    private(set) var servers: [MCPServer] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    private(set) var usageError: String?

    @ObservationIgnored private var agents: [AgentRole] = []
    @ObservationIgnored private let client: CoreMCP
    @ObservationIgnored private let roster: CoreRoster

    init(mcp: CoreMCP, roster: CoreRoster) {
        self.client = mcp
        self.roster = roster
    }

    func load() async {
        isLoading = true

        async let serverRows = client.servers()
        async let agentRows = roster.agents()

        do {
            servers = try await serverRows.sorted(by: Self.sortByName)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }

        do {
            agents = try await agentRows
            usageError = nil
        } catch {
            usageError = error.localizedDescription
        }

        isLoading = false
    }

    func save(id: String, spec: MCPServerSpec) async throws -> MCPSaveResult {
        let result = try await client.upsert(id: id, spec: spec)
        apply(result.server)
        return result
    }

    func remove(id: String) async throws -> MCPDeleteResult {
        let result = try await client.remove(id: id)
        if result.removed {
            servers.removeAll { $0.id == id }
            agents = agents.map { agent in
                var agent = agent
                agent.mcp.removeAll { $0 == id }
                return agent
            }
        }
        return result
    }

    func isInUse(_ id: String) -> Bool {
        agents.contains { $0.mcp.contains(id) }
    }

    func contains(id: String) -> Bool {
        servers.contains { $0.id == id }
    }

    private func apply(_ server: MCPServer) {
        if let index = servers.firstIndex(where: { $0.id == server.id }) {
            servers[index] = server
        } else {
            servers.append(server)
        }
        servers.sort(by: Self.sortByName)
    }

    private static func sortByName(_ lhs: MCPServer, _ rhs: MCPServer) -> Bool {
        lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
    }
}
