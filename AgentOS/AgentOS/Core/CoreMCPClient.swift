import Foundation

final class CoreMCPClient: CoreMCP, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func servers() async throws -> [MCPServer] {
        let result: CoreMCPList = try await provider.client().send(
            "mcp_list",
            body: EmptyBody()
        )
        return result.servers.map(\.domain)
    }

    func upsert(id: String, spec: MCPServerSpec) async throws -> MCPSaveResult {
        let result: CoreMCPUpsertResult = try await provider.client().send(
            "mcp_upsert",
            body: CoreMCPUpsertBody(serverID: id, spec: CoreMCPServerSpec(spec))
        )
        return result.domain
    }

    func remove(id: String) async throws -> MCPDeleteResult {
        let result: CoreMCPDeleteResult = try await provider.client().send(
            "mcp_remove",
            body: CoreMCPRemoveBody(serverID: id)
        )
        return result.domain
    }
}
