import Foundation

final class CoreMCPClient: CoreMCP, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func scan() async throws -> MCPScan {
        let result: CoreMCPScan = try await provider.client().send(
            "mcp_scan_local",
            body: EmptyBody()
        )
        return result.domain
    }

    func upsert(serverID: String, spec: JSONValue, apps: [String]) async throws -> MCPServer {
        let result: CoreMCPServer = try await provider.client().send(
            "mcp_upsert_local_server",
            body: CoreMCPUpsertBody(serverID: serverID, spec: spec, apps: apps)
        )
        return result.domain
    }

    func remove(serverID: String) async throws -> Bool {
        try await provider.client().send(
            "mcp_remove_server",
            body: CoreMCPRemoveBody(serverID: serverID)
        )
    }

}
