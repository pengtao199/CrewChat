import Foundation

protocol CoreMCP: Sendable {
    func scan() async throws -> MCPScan
    func upsert(serverID: String, spec: JSONValue, apps: [String]) async throws -> MCPServer
    func remove(serverID: String) async throws -> Bool
}
