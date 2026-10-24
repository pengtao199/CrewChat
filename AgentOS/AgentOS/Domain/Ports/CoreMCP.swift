import Foundation

protocol CoreMCP: Sendable {
    func servers() async throws -> [MCPServer]
    func upsert(id: String, spec: MCPServerSpec) async throws -> MCPSaveResult
    func remove(id: String) async throws -> MCPDeleteResult
    func check(serverID: String?) async throws
}
