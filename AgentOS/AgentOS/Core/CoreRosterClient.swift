import Foundation

final class CoreRosterClient: CoreRoster, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func agents() async throws -> [AgentRole] {
        let rows: [CoreAgentRole] = try await provider.client().send("agent_roles_list", body: EmptyBody())
        return rows.map(\.domain)
    }

    func saveAgent(_ role: AgentRole) async throws -> AgentRole {
        let saved: CoreAgentRole = try await provider.client().send(
            "agent_roles_save",
            body: CoreAgentRoleSave(role: CoreAgentRole(role))
        )
        return saved.domain
    }

    func deleteAgent(id: String) async throws {
        try await provider.client().sendIgnoring("agent_roles_delete", body: CoreAgentIdBody(id: id))
    }

    func modelFace(agentType: String) async throws -> ModelFace {
        let face: CoreModelFace = try await provider.client().send(
            "agent_role_model_face",
            body: CoreFaceBody(agentType: agentType)
        )
        return ModelFace(
            supported: face.supported,
            reason: face.reason,
            models: face.models.map {
                FaceModel(provider: $0.provider, providerID: $0.providerID, model: $0.model)
            }
        )
    }
}

struct CoreAgentIdBody: Encodable {
    let id: String
}
