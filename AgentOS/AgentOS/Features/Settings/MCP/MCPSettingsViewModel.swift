import Foundation

@MainActor
@Observable
final class MCPSettingsViewModel {
    private(set) var scanResult = MCPScan.empty
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let client: CoreMCP

    init(mcp: CoreMCP) {
        self.client = mcp
    }

    var servers: [MCPServer] { scanResult.servers }
    var warnings: [MCPSourceWarning] { scanResult.warnings }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            scanResult = try await client.scan()
        } catch {
            errorMessage = error.localizedDescription
            scanResult = .empty
        }

        isLoading = false
    }

    func save(serverID: String, spec: JSONValue, apps: [String]) async throws -> MCPServer {
        let saved = try await client.upsert(serverID: serverID, spec: spec, apps: apps)
        await load()
        return saved
    }

    func remove(serverID: String) async throws {
        _ = try await client.remove(serverID: serverID)
        await load()
    }
}
