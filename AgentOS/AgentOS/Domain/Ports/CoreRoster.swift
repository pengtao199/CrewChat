import Foundation

protocol CoreRoster: Sendable {
    func agents() async throws -> [AgentRole]
    func saveAgent(_ role: AgentRole) async throws -> AgentRole
    func deleteAgent(id: String) async throws
    func modelFace(agentType: String) async throws -> ModelFace
}
