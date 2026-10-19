import Foundation

protocol CoreCapabilities: Sendable {
    func skills(agentType: String) async throws -> [CapabilityOption]
    func mcpServers() async throws -> [CapabilityOption]
}
